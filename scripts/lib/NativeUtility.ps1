#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

function New-SkillsUtilityCargoStartInfo {
    [CmdletBinding(PositionalBinding = $false)]
    param([Parameter(Mandatory)][string] $UtilityPath)

    $cargo = Get-Command cargo -CommandType Application | Select-Object -First 1
    $start = [Diagnostics.ProcessStartInfo]::new($cargo.Source)
    $start.UseShellExecute = $false
    $start.WorkingDirectory = (Get-Location).Path
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in @('install', '--path', $UtilityPath, '--locked', '--message-format=json-render-diagnostics')) {
        $start.ArgumentList.Add($argument)
    }
    # Encoded flags take precedence over plain flags in Cargo. Keep caller flags and
    # append the local-CPU choice because this installation is not a portable distribution.
    $separator = [string][char]0x1f
    $flags = if ($start.Environment.ContainsKey('CARGO_ENCODED_RUSTFLAGS')) {
        $start.Environment['CARGO_ENCODED_RUSTFLAGS']
    } elseif ($start.Environment.ContainsKey('RUSTFLAGS')) {
        ($start.Environment['RUSTFLAGS'] -split '\s+' | Where-Object { $_ }) -join $separator
    } else { '' }
    $start.Environment['CARGO_ENCODED_RUSTFLAGS'] = if ($flags) {
        "$flags${separator}-C${separator}target-cpu=native"
    } else { "-C${separator}target-cpu=native" }
    return $start
}

function Invoke-SkillsUtilityCargo {
    [CmdletBinding(PositionalBinding = $false)]
    param([Parameter(Mandatory)][string] $UtilityPath)

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = New-SkillsUtilityCargoStartInfo -UtilityPath $UtilityPath
    try {
        if (-not $process.Start()) { throw 'Could not start Cargo installation.' }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        [Console]::Error.Write($stderr.GetAwaiter().GetResult())
        if ($process.ExitCode -ne 0) { throw "ss-ai compilation or installation failed (exit $($process.ExitCode))." }
        $stdout.GetAwaiter().GetResult() -split '\r?\n' | Where-Object { $_ }
    } finally {
        $process.Dispose()
    }
}

function Install-SkillsUtility {
    [CmdletBinding(PositionalBinding = $false)]
    param()

    foreach ($command in @('rustup', 'cargo')) {
        if (-not (Get-Command $command -CommandType Application -ErrorAction SilentlyContinue)) {
            throw "$command is required on PATH. Install Rustup/Cargo and the native C/C++ build tools described in packages\ss-ai\README.md."
        }
    }
    $repositoryRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $utilityPath = Join-Path $repositoryRoot 'packages\ss-ai'
    if (-not (Test-Path -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -PathType Leaf)) {
        throw 'The repository Cargo.lock is required; restore it before running setup.'
    }

    # Rustup selects the toolchain from the working directory, not --path.
    Push-Location -LiteralPath $repositoryRoot
    try {
        Write-Host 'Building and installing ss-ai. Please wait; this may take a few minutes.' -ForegroundColor Yellow
        # Cargo's artifact messages identify the exact build without duplicating its
        # install-root precedence. Compare bytes, not version banners, after installation.
        $artifacts = @(Invoke-SkillsUtilityCargo -UtilityPath $utilityPath |
            ForEach-Object {
                $message = ConvertFrom-Json -InputObject $_ -AsHashtable
                if ($message['reason'] -eq 'compiler-artifact' -and
                    $message['target']['name'] -ceq 'ss-ai' -and
                    $message['target']['kind'] -contains 'bin') {
                    if ([string]::IsNullOrWhiteSpace($message['executable'])) {
                        throw 'Cargo did not identify the ss-ai executable artifact.'
                    }
                    $message['executable']
                }
            })
        if ($artifacts.Count -ne 1) { throw 'Cargo must report exactly one ss-ai executable artifact.' }
        $application = Get-Command ss-ai -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $application) {
            throw "ss-ai is not on PATH. Add Cargo's configured installation binary directory to PATH and rerun setup."
        }
        $builtHash = (Get-FileHash -LiteralPath $artifacts[0] -Algorithm SHA256).Hash
        $pathHash = (Get-FileHash -LiteralPath $application.Source -Algorithm SHA256).Hash
        if ($builtHash -cne $pathHash) {
            throw "PATH resolves ss-ai to '$($application.Source)', which does not match Cargo's artifact '$($artifacts[0])'. Put Cargo's configured installation binary directory before older copies on PATH and rerun setup."
        }
        $version = & $application.Source --version
        if ($LASTEXITCODE -ne 0 -or ($version -join "`n") -notmatch '^ss-ai \S+') {
            throw 'The PATH ss-ai invocation failed. Check Cargo installation and PATH before linking skills.'
        }
        Write-Host "Installed $version from this repository." -ForegroundColor Green
    } finally {
        Pop-Location
    }
}
