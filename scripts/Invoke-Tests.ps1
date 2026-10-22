#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [string[]] $Path = @((Join-Path $PSScriptRoot '..\tests')),
    [ValidateSet('All', 'Pester', 'Rust')]
    [string] $Suite = 'All',
    [string[]] $FullNameFilter
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$PSNativeCommandUseErrorActionPreference = $false

# A focused path selects Pester only, unless the caller deliberately selects All.
if ($PSBoundParameters.ContainsKey('Path') -and -not $PSBoundParameters.ContainsKey('Suite')) {
    $Suite = 'Pester'
}
if ($Suite -eq 'Rust' -and $PSBoundParameters.ContainsKey('Path')) {
    throw '-Path selects Pester files and cannot be combined with -Suite Rust.'
}
$failed = $false
if ($Suite -in @('All', 'Pester')) {
    # Review suites invoke the real shared parser, including through child scripts.
    # Build only when those suites are selected, never use a developer's installed
    # executable or run unrelated Rust tests for a focused Pester invocation.
    $nativeConsumers = @(
        'SsAutoreapprove.Tests.ps1',
        'SsReviewFindingDecisions.Tests.ps1', 'SsReviewFindingMarkdown.Tests.ps1',
        'SsReviewGitDiff.Tests.ps1', 'SsReviewPublicationAzureDevOps.Tests.ps1',
        'SsReviewPublicationGitHub.Tests.ps1', 'SsReviewPublicationPlan.Tests.ps1',
        'SsReviewPullRequestAnchor.Tests.ps1', 'SsReviewTrackerPublication.Tests.ps1',
        'SsReviewTrackerGitHub.Tests.ps1', 'SsReviewTrackerAzureDevOps.Tests.ps1'
    )
    $selected = @(Get-ChildItem -LiteralPath $Path -Recurse -File | Where-Object Name -In $nativeConsumers)
    if ($selected.Count -gt 0) {
        Push-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
        try {
            & cargo build --package ss-skills-util --locked --quiet
            if ($LASTEXITCODE -ne 0) { throw 'Building the native review test prerequisite failed.' }
            $metadata = & cargo metadata --no-deps --format-version 1 --locked | ConvertFrom-Json
            if ($LASTEXITCODE -ne 0) { throw 'Locating the native review test executable failed.' }
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
    if ($FullNameFilter) { $configuration.Filter.FullName = $FullNameFilter }
    # Pester must return control so a failing suite does not suppress Rust checks.
    $configuration.Run.Exit = $false
    $configuration.Run.PassThru = $true
    $configuration.Output.Verbosity = 'Detailed'
    $originalPath = $env:PATH
    try {
        if ($selected.Count -gt 0) {
            $env:PATH = (Join-Path $metadata.target_directory 'debug') + [IO.Path]::PathSeparator + $env:PATH
        }
        $result = Invoke-Pester -Configuration $configuration
    } finally {
        $env:PATH = $originalPath
    }
    $failed = $result.Result -ne 'Passed'
}

if ($Suite -in @('All', 'Rust')) {
    if (-not (Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue)) {
        throw 'Cargo is required on PATH. See packages\ss-skills-util\README.md for build prerequisites.'
    }
    Push-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
    try {
        & cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
        if ($LASTEXITCODE -ne 0) { $failed = $true }
        & cargo test --workspace --all-features --locked
        if ($LASTEXITCODE -ne 0) { $failed = $true }
    } finally {
        Pop-Location
    }
}
if ($failed) { exit 1 }
exit 0
