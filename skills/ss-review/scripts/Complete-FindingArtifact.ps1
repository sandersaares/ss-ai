#Requires -Version 7.6

# Records that a focus-area reviewer has reviewed the full inventory
# (docs/implementation.md, "Finding artifacts").
#
# A reviewer that found nothing and a reviewer that died partway both leave an artifact with an
# empty finding list, and only this command tells the two apart. The consolidation that follows
# refuses to proceed on a reviewer that never reported completion, because merging one would silently
# publish a review with a hole in its coverage.
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

    [Parameter(Mandatory)]
    [string] $FileGroupQueuePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"
. "$PSScriptRoot\lib\ReviewWork.ps1"

$lock = Open-WorkflowDataLock -Path $ArtifactPath
try {
    $document = Read-FindingArtifact -Path $ArtifactPath
    Assert-ReviewFileGroupQueueComplete -Path $FileGroupQueuePath
    Complete-FindingArtifactDocument -Document $document
    Write-FindingArtifact -Path $ArtifactPath -Document $document
} finally { $lock.Dispose() }

$findings = Get-JsonArrayProperty -Node $document -Name 'findings'

[ordered]@{
    artifactPath = Resolve-PullRequestPath -Path $ArtifactPath
    findingCount = $findings.Count
    status       = Get-JsonProperty -Node $document -Name 'status'
} | ConvertTo-Json -Depth 32 -Compress
