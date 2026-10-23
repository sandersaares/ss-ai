#Requires -Version 7.6

# Creates the artifact one focus-area reviewer writes its findings into
# (docs/implementation.md, "Finding artifacts").
#
# Each reviewer subagent owns exactly one artifact and is dispatched with the path this reports, so
# that findings arrive as structured records rather than as prose a later phase has to parse back
# into records.
#
# This is a composition root: it dot-sources the libraries and reaches no decision a library could
# reach, so every rule it enforces is exercisable offline.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. Errors go to the error stream.

[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = 'Replace')]
param(
    # The directory the artifact is written into, which is the directory every later phase
    # enumerates. It is created if it does not exist.
    [Parameter(Mandatory)]
    [string] $OutputDirectory,

    # Names the focus-area reviewer and the artifact file.
    [Parameter(Mandatory)]
    [string] $ReviewerId,

    # The focus area the reviewer applies.
    [Parameter(Mandatory)]
    [string] $FocusArea,

    # Replacement is for a deliberately new review, never recovery of an interrupted worker.
    [Parameter(ParameterSetName = 'Replace')]
    [switch] $Force,

    # Recovery preserves all findings, including those from a partially processed group.
    [Parameter(ParameterSetName = 'Resume')]
    [switch] $Resume,

    [Parameter(Mandatory, ParameterSetName = 'Resume')]
    [Parameter(ParameterSetName = 'Replace')]
    [string] $FileGroupQueuePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"
. "$PSScriptRoot\lib\ReviewWork.ps1"

$artifactPath = Get-FindingArtifactPath -OutputDirectory $OutputDirectory -ReviewerId $ReviewerId
$lock = Open-WorkflowDataLock -Path $artifactPath
try {
    if ($FileGroupQueuePath) {
        Assert-ReviewWorkerArtifactState -ArtifactPath $artifactPath -QueuePath $FileGroupQueuePath
    }
    $resumedExisting = $Resume -and (Test-Path -LiteralPath $artifactPath -PathType Leaf)
    if ($resumedExisting) {
        $document = Open-FindingArtifactForResume -Path $artifactPath -ReviewerId $ReviewerId -FocusArea $FocusArea
    } else {
        Assert-FindingArtifactWritable -Path $artifactPath -Force:$Force
        $document = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId $ReviewerId -FocusArea $FocusArea
    }
    Write-FindingArtifact -Path $artifactPath -Document $document
} finally { $lock.Dispose() }

[ordered]@{
    artifactPath = $artifactPath
    reviewerId   = $ReviewerId
    findingCount = (Get-JsonArrayProperty -Node $document -Name 'findings').Count
    resumedExisting = $resumedExisting
} | ConvertTo-Json -Depth 32 -Compress
