#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Exports at shell startup, rather than copying the installer's inherited environment.
# Quoted patterns match whole entries literally, including shell metacharacters in paths.
function Get-BashEnvironmentContent {
    param(
        [Parameter(Mandatory)][string] $AgentsDirectory
    )

    if ($AgentsDirectory.Contains(',')) {
        throw 'The agents path must not contain a comma: it delimits the environment list.'
    }

    $agentsLiteral = "'" + $AgentsDirectory.Replace("'", "'`"'`"'") + "'"
    $template = @'
# Managed by ss-ai Install.ps1; re-run it to update agent instruction paths.
case ",${COPILOT_CUSTOM_INSTRUCTIONS_DIRS-}," in
    *,__AGENTS__,*) ;;
    *) COPILOT_CUSTOM_INSTRUCTIONS_DIRS="${COPILOT_CUSTOM_INSTRUCTIONS_DIRS:+$COPILOT_CUSTOM_INSTRUCTIONS_DIRS,}"__AGENTS__ ;;
esac
# Startup files may define a shell-local list; child agents need it exported.
export COPILOT_CUSTOM_INSTRUCTIONS_DIRS
'@
    # Bash requires LF even when a Windows checkout supplies this library.
    $template.Replace('__AGENTS__', $agentsLiteral).Replace("`r`n", "`n") + "`n"
}

# Serializes startup-file edits with the environment they load. Keep the sibling lock
# on disk so contenders always lock the same file.
function Open-BashEnvironmentLock {
    param([Parameter(Mandatory)][string] $Path)

    # Allow ordinary short setup transactions to finish, but report a persistent lock.
    $retryCount = 600
    for ($attempt = 0; $attempt -le $retryCount; $attempt++) {
        try {
            return [IO.File]::Open("$Path.lock", [IO.FileMode]::OpenOrCreate,
                [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        } catch [IO.IOException] {
            $cause = $_.Exception.GetBaseException()
            $contention = ($IsWindows -and $cause.HResult -eq [int]0x80070020) -or
                ($IsLinux -and $cause.HResult -eq 11)
            if (-not $contention) { throw }
            if ($attempt -eq $retryCount) { throw "Bash environment '$Path' remains busy." }
            # Contention alone warrants a short wait; other I/O failures stay explicit.
            Start-Sleep -Milliseconds 50
        }
    }
}

# Readers accept other readers and atomic replacement of a complete startup file.
function Read-BashConfigurationText {
    param([Parameter(Mandatory)][string] $Path)

    if (-not (Test-Path -LiteralPath $Path)) { return '' }
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read,
        [IO.FileShare]::Read -bor [IO.FileShare]::Delete)
    try {
        $reader = [IO.StreamReader]::new($stream, [Text.UTF8Encoding]::new($false, $true), $true)
        try { $reader.ReadToEnd() } finally { $reader.Dispose() }
    } finally { $stream.Dispose() }
}

# Publishes under the caller's environment lock, preserving dotfile symlinks and modes.
function Write-BashConfigurationText {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][string] $Content
    )

    $file = [IO.FileInfo]::new($Path)
    if ($null -ne $file.LinkTarget) {
        $file = $file.ResolveLinkTarget($true)
        if (-not $file.Exists) { throw "Bash configuration symlink '$Path' has no existing target." }
    }
    $target = $file.FullName
    if ((Test-Path -LiteralPath $target) -and (Read-BashConfigurationText -Path $target) -ceq $Content) { return }

    $temporary = "$target.$([guid]::NewGuid().ToString('N')).tmp"
    try {
        [IO.File]::WriteAllText($temporary, $Content, [Text.UTF8Encoding]::new($false))
        if ($IsLinux -and $file.Exists) {
            [IO.File]::SetUnixFileMode($temporary, [IO.File]::GetUnixFileMode($target))
        }
        if ($file.Exists) {
            [IO.File]::Replace($temporary, $target, [NullString]::Value)
        } else {
            [IO.File]::Move($temporary, $target)
        }
    } finally {
        if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) }
    }
}

# Ubuntu Bash loads .profile at login and .bashrc for interactive/SSH shells.
# A user-supplied Bash login profile takes precedence over .profile, so wire it too.
function Set-BashUserEnvironment {
    param(
        [Parameter(Mandatory)][string] $AgentsDirectory,
        [string] $HomeDirectory = $HOME
    )

    $content = Get-BashEnvironmentContent -AgentsDirectory $AgentsDirectory
    $environmentPath = Join-Path $HomeDirectory '.config\ss-ai\environment.sh'
    [void][IO.Directory]::CreateDirectory((Split-Path -Parent $environmentPath))
    $lock = Open-BashEnvironmentLock -Path $environmentPath
    $startupLock = $null
    try {
        # Both independent installers acquire their private lock before this shared
        # retained lock. Discovery and the entire startup transaction need protection.
        $startupLock = Open-BashEnvironmentLock -Path (Join-Path $HomeDirectory '.config\shell-startup')
        $profiles = [Collections.Generic.List[string]]::new()
        $profiles.Add((Join-Path $HomeDirectory '.profile'))
        $profiles.Add((Join-Path $HomeDirectory '.bashrc'))
        foreach ($name in @('.bash_profile', '.bash_login')) {
            $path = Join-Path $HomeDirectory $name
            if (Test-Path -LiteralPath $path) {
                $profiles.Add($path)
                break
            }
        }

        $sourceLine = '[ ! -f "$HOME/.config/ss-ai/environment.sh" ] || . "$HOME/.config/ss-ai/environment.sh"'
        $sourceBlock = "# Load the repository's per-user AI environment.`n$sourceLine`n"
        Write-BashConfigurationText -Path $environmentPath -Content $content
        foreach ($path in $profiles) {
            $existing = Read-BashConfigurationText -Path $path
            if (($existing -split '\r?\n') -ccontains $sourceLine) { continue }

            # Ubuntu's .bashrc returns early for noninteractive SSH commands.
            # Load before that guard; login profiles load after their own PATH edits.
            $updated = if ((Split-Path -Leaf $path) -eq '.bashrc') {
                $sourceBlock + $existing
            } else {
                $existing + "`n" + $sourceBlock
            }
            Write-BashConfigurationText -Path $path -Content $updated
        }
    } finally {
        if ($null -ne $startupLock) { $startupLock.Dispose() }
        $lock.Dispose()
    }

    Write-Host "Configured Bash Copilot instructions. Start a new Bash session to load them." -ForegroundColor Yellow
}
