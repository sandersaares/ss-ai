BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $script:TestRunner = Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts\Invoke-Tests.ps1'

    # A child hosts the actual runner and Pester. Cargo is a command-boundary fixture,
    # keeping test dispatch independent of native builds or a global tool installation.
    function Invoke-TestRunnerFixture {
        param(
            [string] $Suite,
            [switch] $FailPester,
            [switch] $FailClippy,
            [switch] $FailPrepare,
            [switch] $FailNativePester,
            [switch] $FailHelper,
            [switch] $NativePrerequisiteSucceeds,
            [switch] $ReplaceSharedExecutable,
            [switch] $MissingNativeExecutable,
            [switch] $MissingHelper,
            [string] $NativePesterName = 'SsReviewGitDiff.Tests.ps1',
            [int] $PreviousExit = 0,
            [int] $TestExit = 0
        )

        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $root | Out-Null
        # Keep the actual runner and preparation logic, replacing only the Cargo
        # execution boundary alongside the existing compiler command fixtures.
        $scripts = Join-Path $root 'scripts'
        $libraryDirectory = Join-Path $scripts 'lib'
        $support = Join-Path $root 'packages/ss-ai/tests/support'
        New-Item -ItemType Directory -Path $libraryDirectory, $support -Force | Out-Null
        [IO.File]::WriteAllText((Join-Path $support 'provider.rs'), 'provider fixture')
        $runner = Join-Path $scripts 'Invoke-Tests.ps1'
        Copy-Item -LiteralPath $script:TestRunner -Destination $runner
        $library = Get-Content -LiteralPath (Join-Path (Split-Path $script:TestRunner) 'lib/RustTestPreparation.ps1') -Raw
        $library += @'

# The runner dispatch fixture records Cargo calls without starting native tests.
function Invoke-RustTestCargo {
    param([string[]] $ArgumentList, [string] $PrerequisitePath)
    cargo @ArgumentList
}
'@
        [IO.File]::WriteAllText((Join-Path $libraryDirectory 'RustTestPreparation.ps1'), $library)
        $test = Join-Path $root 'Sample.Tests.ps1'
        $log = Join-Path $root 'cargo-calls.txt'
        $targetLog = Join-Path $root 'target-paths.txt'
        $truth = if ($FailPester) { '$false' } else { '$true' }
        [IO.File]::WriteAllText($test, "Describe 'fixture' { It 'reports the requested outcome' { `$global:LASTEXITCODE = $TestExit; $truth | Should -BeTrue } }")
        if ($FailNativePester) {
            [IO.File]::WriteAllText((Join-Path $root $NativePesterName), "throw 'A blocked native consumer must not run'")
        }
        if ($NativePrerequisiteSucceeds -and $ReplaceSharedExecutable) {
            [IO.File]::WriteAllText((Join-Path $root $NativePesterName), @'
#Requires -Version 7.6

BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $PSNativeCommandUseErrorActionPreference = $true
}

Describe 'private native executable' {
    It 'remains available while the shared executable is replaced' {
        $name = if ($IsWindows) { 'ss-ai.exe' } else { 'ss-ai' }
        $shared = Join-Path '__ROOT__' "target/debug/$name"
        $private = Join-Path (($env:PATH -split [IO.Path]::PathSeparator)[0]) $name
        $private | Should -Not -Be $shared
        [IO.File]::Delete($shared)
        try {
            [IO.File]::ReadAllText($private) | Should -Be 'private executable'
            Test-Path -LiteralPath $shared | Should -BeFalse
        } finally {
            [IO.File]::WriteAllText($shared, 'replacement')
        }
    }
}
'@.Replace('__ROOT__', $root.Replace("'", "''")))
        } elseif ($NativePrerequisiteSucceeds) {
            [IO.File]::WriteAllText((Join-Path $root $NativePesterName), @'
#Requires -Version 7.6

BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $PSNativeCommandUseErrorActionPreference = $true
}

Describe 'prepared helper' {
    It 'receives the prebuilt test executable' {
        foreach ($name in @(__HELPER_NAMES__)) {
            $helper = [Environment]::GetEnvironmentVariable($name)
            $helper | Should -Not -BeNullOrEmpty
            Test-Path -LiteralPath $helper -PathType Leaf | Should -BeTrue
            [IO.File]::ReadAllText($helper) | Should -Be 'fixture'
            (Split-Path -Parent $helper) | Should -Be (Get-Content -LiteralPath '__TARGET_LOG__' | Select-Object -First 1)
        }
    }
}
'@.Replace('__TARGET_LOG__', $targetLog.Replace("'", "''")).Replace('__HELPER_NAMES__', $(if ($NativePesterName -eq 'SsReviewWorkerInput.Tests.ps1') {
                    "'SS_AI_WORKER_INPUT_TEST_EXECUTABLE'"
                } else {
                    "'SS_AI_FINDING_TEST_EXECUTABLE', 'SS_AI_AUDIT_TEST_EXECUTABLE', 'SS_AI_REVIEW_WORK_TEST_EXECUTABLE'"
                })))
        }
        $testPath = if ($FailNativePester -or $NativePrerequisiteSucceeds) { $root } else { $test }
        $suiteArgument = if ($Suite) { " -Suite $Suite" } else { '' }

        # Distinct nonzero statuses identify which command boundary supplied a failure.
        $clippyExit = if ($FailClippy) { 19 } else { 0 }
        $code = @'
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
if ($PSVersionTable.PSVersion -lt [version]'7.6') {
    throw 'PowerShell 7.6 or later is required.'
}

# Supply Cargo discovery without depending on an installed native toolchain.
function Get-Command {
    param($Name, $CommandType, $ErrorAction)

    if ($Name -eq 'cargo') {
        return [pscustomobject]@{ Name = 'cargo' }
    }
    Microsoft.PowerShell.Core\Get-Command @PSBoundParameters
}

# Emit only the build artifacts and statuses that the runner consumes.
function cargo {
    if ($args[0] -eq '--version') {
        $global:LASTEXITCODE = 0
        return 'cargo fixture'
    }

    [IO.File]::AppendAllText('__LOG__', "$($args[0])`n")
    if ($args[0] -eq 'build') {
        $global:LASTEXITCODE = if (__NATIVE_SUCCEEDS__) { 0 } else { 29 }
        $target = $args[[array]::IndexOf($args, '--target-dir') + 1]
        [IO.File]::AppendAllText('__TARGET_LOG__', "$target`n")
        if (__NATIVE_SUCCEEDS__) {
            $name = if ($IsWindows) { 'ss-ai.exe' } else { 'ss-ai' }
            $private = Join-Path $target 'debug'
            $shared = Join-Path '__ROOT__' 'target/debug'
            [IO.Directory]::CreateDirectory($private) | Out-Null
            [IO.Directory]::CreateDirectory($shared) | Out-Null
            [IO.File]::WriteAllText((Join-Path $private $name), 'private executable')
            [IO.File]::WriteAllText((Join-Path $shared $name), 'shared executable')
            if (__MISSING_NATIVE__) { [IO.File]::Delete((Join-Path $private $name)) }
            @{
                reason = 'compiler-artifact'
                target = @{ name = 'ss-ai'; kind = @('bin') }
                executable = Join-Path $private $name
            } | ConvertTo-Json -Compress
        }
        return
    }
    if ($args[0] -eq 'test' -and $args -contains '--test') {
        $global:LASTEXITCODE = if (__FAIL_HELPER__) { 31 } else { 0 }
        $targetDirectory = $args[[array]::IndexOf($args, '--target-dir') + 1]
        [IO.File]::AppendAllText('__TARGET_LOG__', "$targetDirectory`n")
        if (__FAIL_HELPER__) { return }
        for ($index = 0; $index -lt $args.Count; $index++) {
            if ($args[$index] -ne '--test') { continue }
            $target = $args[$index + 1]
            $executable = Join-Path $targetDirectory "$target-helper"
            [IO.File]::WriteAllText($executable, 'fixture')
            # The shared artifact disappears before the runner acquires its copy.
            $sharedHelper = Join-Path '__ROOT__' "target/$target-helper"
            [IO.File]::WriteAllText($sharedHelper, 'shared helper')
            [IO.File]::Delete($sharedHelper)
            if (__MISSING_HELPER__ -and $target -in @('review_work', 'worker_input')) {
                [IO.File]::Delete($executable)
            }
            @{
                reason = 'compiler-artifact'
                target = @{ name = $target }
                executable = $executable
            } | ConvertTo-Json -Compress
        }
        return
    }

    $global:LASTEXITCODE = if ($args[0] -eq 'clippy') { __CLIPPY_EXIT__ } else { 0 }
    if ($args -contains '--no-run') {
        if (__FAIL_PREPARE__) {
            $global:LASTEXITCODE = 27
            return
        }

        $target = $args[[array]::IndexOf($args, '--target-dir') + 1]
        [IO.Directory]::CreateDirectory($target) | Out-Null
        $executable = Join-Path $target 'fixture'
        [IO.File]::WriteAllText($executable, 'fixture')
        @{
            reason = 'compiler-artifact'
            target = @{ name = 'fixture' }
            executable = $executable
            fresh = $false
        } | ConvertTo-Json -Compress
    }
}

# Stand in for the prebuilt provider without invoking a compiler.
function rustc {
    $global:LASTEXITCODE = 0
    if ($args[0] -eq '-vV') {
        return 'rustc fixture'
    }

    $executable = $args[[array]::IndexOf($args, '-o') + 1]
    [IO.File]::WriteAllText($executable, 'provider')
}

$global:LASTEXITCODE = __PREVIOUS_EXIT__
& '__RUNNER__' -Path '__TEST__'__SUITE__
exit $LASTEXITCODE
'@
        $code = $code.Replace('__LOG__', $log.Replace("'", "''")).Replace('__TARGET_LOG__', $targetLog.Replace("'", "''")).
            Replace('__CLIPPY_EXIT__', [string]$clippyExit).
            Replace('__RUNNER__', $runner.Replace("'", "''")).Replace('__TEST__', $testPath.Replace("'", "''")).
            Replace('__SUITE__', $suiteArgument).Replace('__PREVIOUS_EXIT__', [string]$PreviousExit).
            Replace('__ROOT__', $root.Replace("'", "''")).
            Replace('__NATIVE_SUCCEEDS__', $(if ($NativePrerequisiteSucceeds) { '$true' } else { '$false' })).
            Replace('__MISSING_NATIVE__', $(if ($MissingNativeExecutable) { '$true' } else { '$false' })).
            Replace('__MISSING_HELPER__', $(if ($MissingHelper) { '$true' } else { '$false' })).
            Replace('__FAIL_HELPER__', $(if ($FailHelper) { '$true' } else { '$false' })).
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
                targets = @(if (Test-Path $targetLog) { Get-Content $targetLog })
            }
        } finally {
            $process.Dispose()
        }
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

    It 'builds the native prerequisite for <Name>' -ForEach @(
        @{ Name = 'SsReviewFindingModel.Tests.ps1' }
        @{ Name = 'SsReviewReports.Tests.ps1' }
        @{ Name = 'SsReviewReportTools.Tests.ps1' }
        @{ Name = 'SsReviewNativePublicationPreflight.Tests.ps1' }
        @{ Name = 'SsReviewPublishOverviewCaller.Tests.ps1' }
        @{ Name = 'SsReviewPullRequestQueryExamples.Tests.ps1' }
        @{ Name = 'SsReviewPullRequestContext.Tests.ps1' }
        @{ Name = 'SsReviewPullRequestShape.Tests.ps1' }
        @{ Name = 'SsReviewSelfReview.Tests.ps1' }
        @{ Name = 'SsReviewSharedFiles.Tests.ps1' }
        @{ Name = 'SsReviewWork.Tests.ps1' }
        @{ Name = 'SsReviewCoverageBatch.Tests.ps1' }
    ) {
        $result = Invoke-TestRunnerFixture -FailNativePester -NativePesterName $Name
        $result.calls | Should -Be @('build') -Because $result.output
        $result.code | Should -Not -Be 0
        $result.output | Should -Not -Match 'A blocked native consumer must not run'
    }

    It 'prepares private concurrency helpers for <Name>' -ForEach @(
        @{ Name = 'SsReviewSharedFiles.Tests.ps1' }
        @{ Name = 'SsReviewWorkerInput.Tests.ps1' }
    ) {
        $result = Invoke-TestRunnerFixture -NativePrerequisiteSucceeds -NativePesterName $Name
        $result.code | Should -Be 0 -Because $result.output
        $result.calls | Should -Be @('build', 'test') -Because $result.output
        $result.targets.Count | Should -Be 2
        $result.targets[0] | Should -Be $result.targets[1]
        Test-Path -LiteralPath $result.targets[0] | Should -BeFalse
        $result.output | Should -Match 'receives the prebuilt test executable'
    }

    It 'blocks only <Name> when its helper preparation fails' -ForEach @(
        @{ Name = 'SsReviewSharedFiles.Tests.ps1' }
        @{ Name = 'SsReviewWorkerInput.Tests.ps1' }
    ) {
        $result = Invoke-TestRunnerFixture -NativePrerequisiteSucceeds -FailHelper -NativePesterName $Name
        $result.code | Should -Not -Be 0
        $result.calls | Should -Be @('build', 'test') -Because $result.output
        Test-Path -LiteralPath $result.targets[0] | Should -BeFalse
        $result.output | Should -Match 'Building the concurrency test prerequisites failed'
        $result.output | Should -Match 'reports the requested outcome'
        $result.output | Should -Not -Match 'receives the prebuilt test executable'
    }

    It 'isolates the native executable while the shared Cargo output is replaced' {
        $result = Invoke-TestRunnerFixture -NativePrerequisiteSucceeds -ReplaceSharedExecutable
        $result.code | Should -Be 0 -Because $result.output
        $result.calls | Should -Be @('build') -Because $result.output
        $result.output | Should -Match 'remains available while the shared executable is replaced'
        Test-Path -LiteralPath $result.targets[0] | Should -BeFalse
    }

    It 'fails explicitly if the built native executable cannot be acquired' {
        $result = Invoke-TestRunnerFixture -NativePrerequisiteSucceeds -ReplaceSharedExecutable -MissingNativeExecutable
        $result.code | Should -Not -Be 0
        $result.output | Should -Match 'Could not identify the native review test executable'
        $result.output | Should -Not -Match 'remains available while the shared executable is replaced'
        Test-Path -LiteralPath $result.targets[0] | Should -BeFalse
    }

    It 'fails explicitly if a built concurrency helper cannot be acquired' {
        $result = Invoke-TestRunnerFixture -NativePrerequisiteSucceeds -MissingHelper -NativePesterName 'SsReviewWorkerInput.Tests.ps1'
        $result.code | Should -Not -Be 0
        $result.output | Should -Match 'Could not identify the worker_input concurrency test executable'
        $result.output | Should -Not -Match 'receives the prebuilt test executable'
        Test-Path -LiteralPath $result.targets[0] | Should -BeFalse
    }

    It 'discards partially acquired helpers and still runs independent tests' {
        $result = Invoke-TestRunnerFixture -NativePrerequisiteSucceeds -MissingHelper -NativePesterName 'SsReviewSharedFiles.Tests.ps1'
        $result.code | Should -Not -Be 0
        $result.output | Should -Match 'Could not identify the review_work concurrency test executable'
        $result.output | Should -Match 'reports the requested outcome'
        $result.output | Should -Not -Match 'receives the prebuilt test executable'
        Test-Path -LiteralPath $result.targets[0] | Should -BeFalse
    }

    It 'rejects combining Rust-only selection with Pester paths' {
        $result = Invoke-TestRunnerFixture -Suite Rust
        $result.code | Should -Not -Be 0
        $result.output | Should -Match 'cannot be combined'
        $result.calls | Should -BeNullOrEmpty
    }
}
