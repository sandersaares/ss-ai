#Requires -Version 7.6

# Builds the set of findings the audit approved (SKILL.md, "Phase 7: audit"; "Phase 8: freshness
# check").
#
# A finding with no decision is a refusal rather than an omission: an interrupted audit would
# otherwise produce a set that looks complete while quietly leaving out everything the user never
# saw, and nothing after this point could tell that from a review where those findings were
# rejected.
#
# Supplying the captured pull request binds the approved set to the commits it was reviewed against,
# revalidates every follow-up target and resolves every approved anchor against the reviewed diff.
# Phase 7 uses the audited capture and the local clone; phase 8 re-runs against the refreshed capture
# and that clone so a thread that has since disappeared and an anchor the diff cannot carry both
# surface while the run can still act on them. Reviews without a pull request omit both inputs.
# A line anchor that resolves as a file-level downgrade stays approved
# because publication can still report it.
#
# This is a composition root: it dot-sources the libraries and reaches no decision a library could
# reach, so every rule it enforces is exercisable offline.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. The approved set goes to a file and errors to the
# error stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    # The consolidated artifact, as written by New-ConsolidatedFindings.ps1.
    [Parameter(Mandatory)]
    [string] $Path,

    # The decision log, as written by Add-FindingDecision.ps1.
    [Parameter(Mandatory)]
    [string] $DecisionsPath,

    [Parameter(Mandatory)]
    [string] $OutputPath,

    # The captured discussion, as written by Get-PullRequestContext.ps1. It is supplied together
    # with -RepositoryPath or not at all.
    [string] $ContextPath,

    # A local clone holding the reviewed commits. The diff between the commits the capture names is
    # read from here, so an approved finding whose anchor publication would reject is reported as
    # unanchored while the run can still name it.
    [string] $RepositoryPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"
. "$PSScriptRoot\lib\GitDiff.ps1"
. "$PSScriptRoot\lib\PullRequestAnchor.ps1"
. "$PSScriptRoot\lib\PullRequestTransportHost.ps1"

Assert-FindingFreshnessInput -ContextPath $ContextPath -RepositoryPath $RepositoryPath

$artifact = Read-FindingArtifact -Path $Path
$decisions = Open-FindingDecisionDocument -Path $DecisionsPath

$context = $null
if ($ContextPath) {
    $context = Read-PullRequestContextDocument -Path $ContextPath
}

$summary = Get-FindingDecisionSummary -Artifact $artifact -Decision $decisions
$document = New-ApprovedFindingDocument -Artifact $artifact -Decision $decisions -Context $context

$unanchoredFinding = @()
if ($RepositoryPath) {
    $diff = Get-GitDiff `
        -RepositoryPath $RepositoryPath `
        -MergeBaseCommit ([string] (Get-JsonProperty -Node $document -Name 'reviewedMergeBaseCommit')) `
        -SourceCommit ([string] (Get-JsonProperty -Node $document -Name 'reviewedSourceCommit'))

    $unanchoredFinding = Remove-ApprovedFindingOutsideDiff -Document $document -Diff $diff
}

Write-PullRequestOutputFile -Path $OutputPath -Content (ConvertTo-JsonText -Value $document)

# The rejected and dropped counts are reported as the audit reached them, but the approved count, the
# approval split and the identifiers describe the set that was actually written: a run that reported
# an approval the freshness check then marked unanchored would have the orchestrator tell the user
# about a finding nothing will publish.
#
# The approval is reported split into what the user approved in person and what the audit
# auto-approved as obvious, taken over the written set so the two add up to the approved count
# (SKILL.md, "Phase 7: audit"). The rejected and dropped counts do not join that sum, because they
# name findings the approved set never held.
#
# A finding the audit dropped and one that cannot be anchored are reported separately and named
# differently because they are different events: the first is a verdict somebody reached, the second
# is a finding nobody rejected that has nowhere to go.
$approved = Get-JsonArrayProperty -Node $document -Name 'findings'
$approvalSplit = Get-ApprovedFindingApprovalSplit -Document $document -Decision $decisions

[ordered]@{
    outputPath              = Resolve-PullRequestPath -Path $OutputPath
    approved                = $approved.Count
    manuallyApproved        = $approvalSplit['manuallyApproved']
    autoApproved            = $approvalSplit['autoApproved']
    rejected                = $summary['rejected']
    dropped                 = $summary['dropped']
    unanchored              = $unanchoredFinding.Count
    unanchoredFinding       = $unanchoredFinding
    findingId               = Get-FindingIdentifierList -Document $document
    reviewedSourceCommit    = Get-JsonProperty -Node $document -Name 'reviewedSourceCommit'
    reviewedMergeBaseCommit = Get-JsonProperty -Node $document -Name 'reviewedMergeBaseCommit'
} | ConvertTo-Json -Depth 32 -Compress
