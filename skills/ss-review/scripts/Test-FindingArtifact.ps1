#Requires -Version 7.6

# Reports whether recorded findings are fit for the next phase
# (docs/implementation.md, "Finding artifacts").
#
# The review runs this as a gate before consolidating, so an invalid set exits unsuccessfully rather
# than merely reporting a status the caller might read past. Naming the reviewers that were dispatched
# turns the check from a count of files into a statement about coverage: a reviewer whose subagent never
# wrote an artifact and a reviewer whose subagent died before completing it are the two failures a
# file count cannot see.
#
# This is a composition root: it dot-sources the libraries and reaches no decision a library could
# reach, so every rule it enforces is exercisable offline.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. Errors go to the error stream.

[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = 'Identifiers')]
param(
    # One artifact, or the directory holding a run's reviewer artifacts.
    [Parameter(Mandatory)]
    [string] $Path,

    # The reviewers that were dispatched, named as one comma separated value. Supplying them reports the
    # ones that are missing and the ones that were never dispatched.
    [Parameter(ParameterSetName = 'Identifiers')]
    [string[]] $ExpectedReviewerId = @(),

    # The phase gate reads the complete coverage authority instead of a model-transcribed ID list.
    [Parameter(Mandatory, ParameterSetName = 'Manifest')]
    [string] $ManifestPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"
. "$PSScriptRoot\lib\ReviewWork.ps1"

$result = if ($PSCmdlet.ParameterSetName -eq 'Manifest') {
    Get-ReviewWorkValidation -ManifestPath $ManifestPath -ArtifactDirectory $Path
} else {
    Get-FindingArtifactValidation -Path $Path -ExpectedReviewerId (ConvertTo-FindingIdentifierList -Value $ExpectedReviewerId)
}

$result | ConvertTo-Json -Depth 32 -Compress

if (-not $result['isValid']) {
    exit 1
}
