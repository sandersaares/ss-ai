#Requires -Version 7.6

# Discards findings, keeping an account of why
# (docs/implementation.md, "Finding artifacts").
#
# A discarded finding is moved rather than deleted, because the pass that reports on the review's
# own quality has no other evidence of what quality improvement rejected and would otherwise only
# ever see what survived.
#
# The batch form exists because the passes that discard findings work over the whole consolidated
# set and would otherwise start one process per finding.
#
# This is a composition root: it dot-sources the libraries and reaches no decision a library could
# reach, so every rule it enforces is exercisable offline.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. Errors go to the error stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $ArtifactPath,

    # The finding to discard. Either this with -Reason, or -PlanPath.
    [string] $FindingId,

    # Why the finding was discarded.
    [string] $Reason,

    # A JSON file holding `{ removals: [ { findingId, reason } ] }`.
    [string] $PlanPath,

    [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"

$removals = Get-FindingRemovalRequest -FindingId $FindingId -Reason $Reason -PlanPath $PlanPath
$lock = Open-WorkflowDataLock -Path $ArtifactPath
try {
    $document = Read-FindingArtifact -Path $ArtifactPath
    $removed = Remove-FindingBatchFromArtifact -Document $document -Removal $removals -NowUtc $NowUtc

    # Rejected batches leave the previously committed artifact unchanged.
    Write-FindingArtifact -Path $ArtifactPath -Document $document
} finally { $lock.Dispose() }

[ordered]@{
    artifactPath = Resolve-PullRequestPath -Path $ArtifactPath
    removed      = $removed
} | ConvertTo-Json -Depth 32 -Compress
