#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Reproduce the legacy managed block only to recognize ownership during cleanup.
function Get-LegacyBashEnvironmentContent {
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
        [Parameter(Mandatory)][AllowEmptyString()][string] $Content
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

# Remove recognized ss-ai exports and loaders without changing user-owned shell content.
function Remove-LegacyBashEnvironment {
    param(
        [Parameter(Mandatory)][string] $AgentsDirectory,
        [string] $HomeDirectory = $HOME
    )

    # Commas were rejected by the old installer, so such homes have no managed block.
    if ($AgentsDirectory.Contains(',')) { return }
    $content = Get-LegacyBashEnvironmentContent -AgentsDirectory $AgentsDirectory
    $environmentPath = Join-Path $HomeDirectory '.config\ss-ai\environment.sh'
    [void][IO.Directory]::CreateDirectory((Split-Path -Parent $environmentPath))
    $lock = Open-BashEnvironmentLock -Path $environmentPath
    $startupLock = $null
    try {
        # Both independent installers acquire their private lock before this shared
        # retained lock. Discovery and the entire startup transaction need protection.
        $startupLock = Open-BashEnvironmentLock -Path (Join-Path $HomeDirectory '.config\shell-startup')
        $environmentFile = Get-Item -LiteralPath $environmentPath -Force -ErrorAction SilentlyContinue
        # A manually substituted file link is not an installer-owned payload.
        if ($null -ne $environmentFile -and $environmentFile.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { return }
        $existingEnvironment = Read-BashConfigurationText -Path $environmentPath
        $remaining = $existingEnvironment
        # Require the complete exact managed block, never a similar user-written export.
        if ($existingEnvironment.StartsWith($content, [StringComparison]::Ordinal)) {
            $remaining = $existingEnvironment.Substring($content.Length)
            if ($remaining.Length -eq 0) {
                Remove-Item -LiteralPath $environmentPath
            } else {
                Write-BashConfigurationText -Path $environmentPath -Content $remaining
            }
        }
        # User additions still need their loader; unrecognized files remain untouched.
        if (Test-Path -LiteralPath $environmentPath) { return }
        $sourceLine = '[ ! -f "$HOME/.config/ss-ai/environment.sh" ] || . "$HOME/.config/ss-ai/environment.sh"'
        foreach ($name in @('.profile', '.bashrc', '.bash_profile', '.bash_login')) {
            $path = Join-Path $HomeDirectory $name
            if (-not (Test-Path -LiteralPath $path)) { continue }
            $existing = Read-BashConfigurationText -Path $path
            $pattern = '(?m)^(?:# Load the repository''s per-user AI environment\.\r?\n)?' +
                [regex]::Escape($sourceLine) + '(?:\r?\n|$)'
            $updated = [regex]::Replace($existing, $pattern, '')
            Write-BashConfigurationText -Path $path -Content $updated
        }
    } finally {
        if ($null -ne $startupLock) { $startupLock.Dispose() }
        $lock.Dispose()
    }
}
