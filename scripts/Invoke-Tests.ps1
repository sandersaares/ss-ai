#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [string[]] $Path = @((Join-Path $PSScriptRoot '..\tests')),
    [ValidateSet('All', 'Pester', 'Rust')]
    [string] $Suite = 'All',
    [string[]] $FullNameFilter,
    [ValidateSet('All', 'Prepare', 'Execute')]
    [string] $Phase = 'All',
    [string] $PrerequisitePath,
    [string[]] $RustTest,
    [string] $RustFilter
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Native failures are collected explicitly so independent suites can still finish.
$PSNativeCommandUseErrorActionPreference = $false
. "$PSScriptRoot/lib/RustTestPreparation.ps1"

# A focused path selects Pester only, unless the caller deliberately selects All.
if ($PSBoundParameters.ContainsKey('Path') -and -not $PSBoundParameters.ContainsKey('Suite')) {
    $Suite = 'Pester'
}
if ($Suite -eq 'Rust' -and $PSBoundParameters.ContainsKey('Path')) {
    throw '-Path selects Pester files and cannot be combined with -Suite Rust.'
}
if ($Phase -ne 'All' -and ($Suite -ne 'Rust' -or -not $PrerequisitePath)) {
    throw 'Split -Phase Prepare/Execute requires -Suite Rust and an absolute -PrerequisitePath.'
}
if ($Suite -eq 'Pester' -and ($RustTest -or $RustFilter -or $PrerequisitePath)) {
    throw 'Rust selection and prerequisites require -Suite Rust or All.'
}

$failed = $false
if ($Suite -in @('All', 'Pester')) {
    # Review suites invoke the real shared parser, including through child scripts.
    # Build only when those suites are selected, never use a developer's installed
    # executable or run unrelated Rust tests for a focused Pester invocation.
    $nativeConsumers = @(
        'Generate-Inventory.Tests.ps1', 'Graders.Tests.ps1',
        'SsAutoreapprove.Tests.ps1',
        'SsReviewCoverageBatch.Tests.ps1', 'SsReviewFindingConsolidation.Tests.ps1',
        'SsReviewFindingDecisions.Tests.ps1',
        'SsReviewFindingMarkdown.Tests.ps1', 'SsReviewFindingModel.Tests.ps1',
        'SsReviewGitDiff.Tests.ps1', 'SsReviewPublicationAzureDevOps.Tests.ps1',
        'SsReviewPublicationGitHub.Tests.ps1', 'SsReviewPublicationPlan.Tests.ps1',
        'SsReviewPullRequestAnchor.Tests.ps1', 'SsReviewTrackerPublication.Tests.ps1',
        'SsReviewTrackerGitHub.Tests.ps1', 'SsReviewTrackerAzureDevOps.Tests.ps1',
        'SsReviewPullRequestQuery.Tests.ps1', 'SsReviewReports.Tests.ps1', 'SsReviewSelfReview.Tests.ps1',
        'SsReviewSharedFiles.Tests.ps1', 'SsReviewWorkflowData.Tests.ps1',
        'SsReviewWork.Tests.ps1', 'SsReviewWorkerInput.Tests.ps1'
    )
    $selected = @(Get-ChildItem -LiteralPath $Path -Recurse -File | Where-Object Name -In $nativeConsumers)
    $blockedPesterPaths = @()
    $nativeReady = $false
    $sharedTestExecutables = @{}

    if ($selected.Count -gt 0) {
        Push-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
        try {
            & cargo build --package ss-ai --locked --quiet
            if ($LASTEXITCODE -ne 0) {
                throw 'Building the native review test prerequisite failed.'
            }

            $metadataJson = & cargo metadata --no-deps --format-version 1 --locked
            if ($LASTEXITCODE -ne 0) {
                throw 'Locating the native review test executable failed.'
            }
            $metadata = $metadataJson | ConvertFrom-Json
            $nativeReady = $true
        } catch {
            # Only native-consuming Pester files depend on this build; unrelated checks still run.
            Write-Host "Pester prerequisite failed: $_"
            $blockedPesterPaths = @($selected.FullName)
            $failed = $true
        } finally {
            Pop-Location
        }
    }

    $sharedFileTests = @($selected | Where-Object Name -EQ 'SsReviewSharedFiles.Tests.ps1')
    if ($nativeReady -and $sharedFileTests.Count -gt 0) {
        Push-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
        try {
            # Prepare the process helper before tests acquire any shared locks.
            $messages = @(& cargo test --package ss-ai-review-workflow --test finding_plans --test audit --no-run --locked --message-format json)
            if ($LASTEXITCODE -ne 0) {
                throw 'Building the concurrency test prerequisites failed.'
            }
            foreach ($target in @('finding_plans', 'audit')) {
                $executables = @($messages | ForEach-Object {
                    $message = $_ | ConvertFrom-Json
                    if ($message.reason -eq 'compiler-artifact' -and $message.target.name -eq $target -and $message.executable) {
                        $message.executable
                    }
                })
                if ($executables.Count -ne 1 -or -not (Test-Path -LiteralPath $executables[0] -PathType Leaf)) {
                    throw "Could not identify the $target concurrency test executable."
                }
                $sharedTestExecutables[$target] = $executables[0]
            }
        } catch {
            Write-Host "Pester prerequisite failed: $_"
            $blockedPesterPaths += @($sharedFileTests.FullName)
            $failed = $true
        } finally {
            Pop-Location
        }
    }

    $pester = Get-Module -ListAvailable Pester |
        Where-Object { $_.Version.Major -eq 5 } |
        Sort-Object Version -Descending |
        Select-Object -First 1
    if (-not $pester) {
        throw 'Pester 5 is required. Install it for the current user before running this offline test suite.'
    }

    Import-Module $pester.Path -Force
    $configuration = [PesterConfiguration]::Default
    $configuration.Run.Path = $Path
    $configuration.Run.ExcludePath = $blockedPesterPaths
    if ($FullNameFilter) {
        $configuration.Filter.FullName = $FullNameFilter
    }

    # Pester must return control so a failing suite does not suppress Rust checks.
    $configuration.Run.Exit = $false
    $configuration.Run.PassThru = $true
    $configuration.Output.Verbosity = 'Detailed'

    $originalPath = $env:PATH
    $originalFindingTestExecutable = $env:SS_AI_FINDING_TEST_EXECUTABLE
    $originalAuditTestExecutable = $env:SS_AI_AUDIT_TEST_EXECUTABLE
    $sharedTestCopies = @{}
    try {
        if ($nativeReady) {
            $env:PATH = (Join-Path $metadata.target_directory 'debug') + [IO.Path]::PathSeparator + $env:PATH
        }
        foreach ($target in $sharedTestExecutables.Keys) {
            # Each invocation retains its own immutable helper while Cargo may rebuild its output.
            $executable = $sharedTestExecutables[$target]
            $extension = [IO.Path]::GetExtension($executable)
            $copy = Join-Path ([IO.Path]::GetTempPath()) "ss-ai-$target-test-$([guid]::NewGuid().ToString('N'))$extension"
            [IO.File]::Copy($executable, $copy)
            $sharedTestCopies[$target] = $copy
            if (-not $IsWindows) {
                [IO.File]::SetUnixFileMode($copy, [IO.File]::GetUnixFileMode($executable))
            }
        }
        if ($sharedTestCopies.Count -gt 0) {
            $env:SS_AI_FINDING_TEST_EXECUTABLE = $sharedTestCopies['finding_plans']
            $env:SS_AI_AUDIT_TEST_EXECUTABLE = $sharedTestCopies['audit']
        }

        $result = Invoke-Pester -Configuration $configuration
    } finally {
        $env:PATH = $originalPath
        $env:SS_AI_FINDING_TEST_EXECUTABLE = $originalFindingTestExecutable
        $env:SS_AI_AUDIT_TEST_EXECUTABLE = $originalAuditTestExecutable
        foreach ($copy in $sharedTestCopies.Values) {
            if ([IO.File]::Exists($copy)) { [IO.File]::Delete($copy) }
        }
    }
    $failed = $failed -or $result.Result -ne 'Passed'
}

if ($Suite -in @('All', 'Rust')) {
    if (-not (Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue)) {
        throw 'Cargo is required on PATH. See packages\ss-ai\README.md for build prerequisites.'
    }

    Push-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
    try {
        if ($Phase -ne 'Execute') {
            Invoke-TestPhase -Name 'Clippy' -Action {
                & cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
                if ($LASTEXITCODE -ne 0) {
                    $script:failed = $true
                }
            }
        }

        # Only implicitly allocated prerequisites belong to this runner's cleanup.
        $temporaryRoot = $null
        try {
            if (-not $PrerequisitePath) {
                $temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ('ss-ai-tests-' + [guid]::NewGuid().ToString('N'))
                New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
                $PrerequisitePath = Join-Path $temporaryRoot 'prerequisites.json'
            }

            $root = (Get-Location).Path
            if ($Phase -ne 'Execute') {
                $preparation = Invoke-TestPhase -Name 'Rust preparation' -Action {
                    New-RustTestPreparation -RepositoryRoot $root -PrerequisitePath $PrerequisitePath -TestTarget $RustTest
                }
                Write-Host "Rust prerequisites: $PrerequisitePath"
                if ($preparation.status -ne 'complete') {
                    $failed = $true
                }
            }

            if ($Phase -ne 'Prepare') {
                $preparation = Read-RustTestPreparation -RepositoryRoot $root -PrerequisitePath $PrerequisitePath -TestTarget $RustTest -AllowProviderFailure
                Invoke-TestPhase -Name 'Rust execution' -Action {
                    Invoke-PreparedRustTests -Preparation $preparation -PrerequisitePath $PrerequisitePath -TestTarget $RustTest -TestFilter $RustFilter
                }
            }
        } catch {
            Write-Host "Rust phase failed: $_"
            $failed = $true
        } finally {
            if ($temporaryRoot) {
                Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
            }
        }
    } finally {
        Pop-Location
    }
}

if ($failed) {
    exit 1
}
exit 0
