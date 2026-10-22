#Requires -Version 7.6

# Rewrites findings that are already recorded
# (docs/implementation.md, "Finding artifacts"; "Finding quality refinement").
#
# Quality improvement rewrites what a reviewer wrote without changing which finding it is: the
# identifier and the provenance survive, so a rewritten finding is still recognizable as the one its
# reviewer reported.
#
# The batch form exists because the passes that rewrite findings work over the whole consolidated
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

    # The finding to rewrite. Either this with -FindingPath, or -PlanPath.
    [string] $FindingId,

    # A JSON file holding the replacement content, carrying no identifier and no provenance.
    [string] $FindingPath,

    # A JSON file holding `{ updates: [ { findingId, findingPath } ] }`. A finding path in it that is
    # relative is taken against the plan's own directory.
    [string] $PlanPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"

$updates = Get-FindingUpdateRequest -FindingId $FindingId -FindingPath $FindingPath -PlanPath $PlanPath
$lock = Open-WorkflowDataLock -Path $ArtifactPath
try {
    $document = Read-FindingArtifact -Path $ArtifactPath
    $updated = Set-FindingBatchInArtifact -Document $document -Update $updates

    # Rejected batches leave the previously committed artifact unchanged.
    Write-FindingArtifact -Path $ArtifactPath -Document $document
} finally { $lock.Dispose() }

[ordered]@{
    artifactPath = Resolve-PullRequestPath -Path $ArtifactPath
    updated      = $updated
} | ConvertTo-Json -Depth 32 -Compress
