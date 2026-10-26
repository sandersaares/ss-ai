BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $script:TestRunner = Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts\Invoke-Tests.ps1'

    # A child hosts the actual runner and Pester. Cargo is a command-boundary fixture,
    # keeping test dispatch independent of native builds or a global tool installation.
    function Invoke-TestRunnerFixture {
        param([string] $Suite, [switch] $FailPester, [switch] $FailClippy, [switch] $FailPrepare,
            [switch] $FailNativePester, [int] $PreviousExit = 0, [int] $TestExit = 0)
        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $root | Out-Null
        $test = Join-Path $root 'Sample.Tests.ps1'
        $log = Join-Path $root 'cargo-calls.txt'
        $truth = if ($FailPester) { '$false' } else { '$true' }
        [IO.File]::WriteAllText($test, "Describe 'fixture' { It 'reports the requested outcome' { `$global:LASTEXITCODE = $TestExit; $truth | Should -BeTrue } }")
        if ($FailNativePester) {
            [IO.File]::WriteAllText((Join-Path $root 'SsReviewGitDiff.Tests.ps1'), "throw 'A blocked native consumer must not run'")
        }
        $testPath = if ($FailNativePester) { $root } else { $test }
        # Provider compilation exercises dispatch; the release lifecycle is verified separately.
        $suiteArgument = if ($Suite) { " -Suite $Suite -RustTest native" } else { '' }
        $clippyExit = if ($FailClippy) { 19 } else { 0 }
        $code = @'
function Get-Command {
    param($Name, $CommandType, $ErrorAction)
    if ($Name -eq 'cargo') { return [pscustomobject]@{ Name = 'cargo' } }
    Microsoft.PowerShell.Core\Get-Command @PSBoundParameters
}
function cargo {
    if ($args[0] -eq '--version') {
        $global:LASTEXITCODE = 0
        return 'cargo fixture'
    }
    [IO.File]::AppendAllText('__LOG__', "$($args[0])`n")
    if ($args[0] -eq 'build') { $global:LASTEXITCODE = 29; return }
    $global:LASTEXITCODE = if ($args[0] -eq 'clippy') { __CLIPPY_EXIT__ } else { 0 }
    if ($args -contains '--no-run') {
        if (__FAIL_PREPARE__) { $global:LASTEXITCODE = 27; return }
        $target = $args[[array]::IndexOf($args, '--target-dir') + 1]
        [IO.Directory]::CreateDirectory($target) | Out-Null
        $executable = Join-Path $target 'fixture'
        [IO.File]::WriteAllText($executable, 'fixture')
        @{ reason = 'compiler-artifact'; target = @{ name = 'fixture' }; executable = $executable; fresh = $false } | ConvertTo-Json -Compress
    }
}
function rustc {
    $global:LASTEXITCODE = 0
    if ($args[0] -eq '-vV') { return 'rustc fixture' }
    $executable = $args[[array]::IndexOf($args, '-o') + 1]
    [IO.File]::WriteAllText($executable, 'provider')
}
$global:LASTEXITCODE = __PREVIOUS_EXIT__
& '__RUNNER__' -Path '__TEST__'__SUITE__
exit $LASTEXITCODE
'@
        $code = $code.Replace('__LOG__', $log.Replace("'", "''")).Replace('__CLIPPY_EXIT__', [string]$clippyExit).
            Replace('__RUNNER__', $script:TestRunner.Replace("'", "''")).Replace('__TEST__', $testPath.Replace("'", "''")).
            Replace('__SUITE__', $suiteArgument).Replace('__PREVIOUS_EXIT__', [string]$PreviousExit).
            Replace('__FAIL_PREPARE__', $(if ($FailPrepare) { '$true' } else { '$false' }))
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-EncodedCommand', [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code)))) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $errorTask = $process.StandardError.ReadToEndAsync()
            $output = $process.StandardOutput.ReadToEnd()
            $process.WaitForExit()
            @{
                code = $process.ExitCode
                output = $output + $errorTask.GetAwaiter().GetResult()
                calls = if (Test-Path $log) { @(Get-Content $log) } else { @() }
            }
        } finally { $process.Dispose() }
    }
}

Describe 'Repository test entry point' {
    It 'keeps an explicit Pester path focused' {
        $result = Invoke-TestRunnerFixture
        $result.code | Should -Be 0 -Because $result.output
        $result.calls | Should -BeNullOrEmpty
    }

    It 'clears a successful Pester run after nonzero native exit values' {
        $result = Invoke-TestRunnerFixture -PreviousExit 17 -TestExit 23
        $result.code | Should -Be 0 -Because $result.output
        $result.calls | Should -BeNullOrEmpty
    }

    It 'runs both Rust checks after Pester and preserves a <Failure> failure' -ForEach @(
        @{ Failure = 'Pester'; FailPester = $true; FailClippy = $false }
        @{ Failure = 'Clippy'; FailPester = $false; FailClippy = $true }
    ) {
        $result = Invoke-TestRunnerFixture -Suite All -FailPester:$FailPester -FailClippy:$FailClippy
        $result.calls | Should -Be @('clippy', 'test', 'test') -Because $result.output
        $result.code | Should -Not -Be 0
    }

    It 'does not execute Rust tests after failed preparation and still fails the run' {
        $result = Invoke-TestRunnerFixture -Suite All -FailPrepare
        $result.calls | Should -Be @('clippy', 'test') -Because $result.output
        $result.code | Should -Not -Be 0
        $result.output | Should -Match 'Compiling Rust test prerequisites failed'
    }

    It 'blocks only dependent Pester files after a prerequisite failure and preserves it across Rust checks' {
        $result = Invoke-TestRunnerFixture -Suite All -FailNativePester
        $result.calls | Should -Be @('build', 'clippy', 'test', 'test') -Because $result.output
        $result.code | Should -Not -Be 0
        $result.output | Should -Match 'reports the requested outcome'
        $result.output | Should -Not -Match 'A blocked native consumer must not run'
    }

    It 'rejects combining Rust-only selection with Pester paths' {
        $result = Invoke-TestRunnerFixture -Suite Rust
        $result.code | Should -Not -Be 0
        $result.output | Should -Match 'cannot be combined'
        $result.calls | Should -BeNullOrEmpty
    }
}
