#Requires -Version 7.6

# Captures a pull request and its discussion (docs/design.md, "Deterministic discussion capture").
#
# Phase 1 of the review runs this instead of asking a subagent to read and retell the discussion,
# because a review that deduplicates its findings against prior feedback has to trust that the
# feedback it sees is what was written. Pre-audit and publication refreshes compare against the
# last checked capture, preserving the initial baseline for follow-up accounting.
#
# This is the composition root for the capture: it dot-sources the libraries and does nothing else
# that a library could do, so that every decision it reaches is reachable by the test suite.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. The captured document goes to a file and errors to the
# error stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    # Browser URL of the pull request. Either this or explicit coordinates are required.
    [string] $Url,

    [ValidateSet('azureDevOps', 'gitHub')]
    [string] $Platform,

    [string] $Organization,

    [string] $Project,

    [string] $Repository,

    [int] $PullRequestId,

    [string] $CollectionUrl,

    [Parameter(Mandatory)]
    [string] $OutputDirectory,

    # Every refresh uses a new prefix so audit and intervening-delta baselines remain immutable.
    [string] $OutputPrefix = '',

    # An earlier capture to compare against. Supplying it adds a delta to the emitted result.
    [string] $ComparePath,

    # Azure DevOps only. The access token is issued for the current subscription's tenant, which is
    # usually but not always the organization's tenant.
    [string] $Tenant,

    [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PublicationFailure.ps1"
. "$PSScriptRoot\lib\PullRequestTarget.ps1"
. "$PSScriptRoot\lib\PullRequestIdentity.ps1"
. "$PSScriptRoot\lib\AdoRestTransport.ps1"
. "$PSScriptRoot\lib\AdoPullRequestApi.ps1"
. "$PSScriptRoot\lib\GitHubCliTransport.ps1"
. "$PSScriptRoot\lib\GitHubPullRequestApi.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\PullRequestTransportHost.ps1"
. "$PSScriptRoot\lib\PullRequestPlatform.ps1"

$targetParameters = Get-PullRequestTargetParameter -BoundParameter $PSBoundParameters
$target = Resolve-PullRequestTarget @targetParameters

$contextPath = Resolve-PullRequestPath -Path (Join-Path $OutputDirectory "${OutputPrefix}pr-context.json")

# The earlier capture is validated and read before anything is captured or written, so that a run
# which cannot produce a usable comparison costs nothing and cannot be affected by its own output.
$previous = $null
$previousDecisions = $null
if ($ComparePath) {
    Assert-PullRequestComparisonPath -ComparePath $ComparePath -OutputPath @($contextPath)
    $previous = Read-PullRequestContextDocument -Path $ComparePath
    $previousRaw = Read-PullRequestContextDocument -Path $ComparePath -WithoutAuthorship
    $previousDecisions = Read-CommentAuthorship -Path $ComparePath -Document $previousRaw
}

$transport = New-PullRequestTransport -Target $target -Tenant $Tenant
$context = Get-PullRequestContextForTarget -Transport $transport -Target $target

$document = New-PullRequestContextDocument -Context $context -NowUtc $NowUtc

if ($previous -and $previous['pullRequestUrl'] -cne $document['pullRequestUrl']) {
    throw 'Cannot compare captures of different pull requests.'
}
$reused = Write-PullRequestCapture -Path $contextPath -Document $document -PreviousDecisions $previousDecisions

$delta = $null
if ($previous) {
    $delta = Get-PullRequestContextDelta -Previous $previous -Current $document
}

$result = New-PullRequestCaptureResult `
    -Document $document `
    -ContextPath $contextPath `
    -Delta $delta `
    -ReusedClassificationCount $reused

$result | ConvertTo-Json -Depth 32 -Compress
