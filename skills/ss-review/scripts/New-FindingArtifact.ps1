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
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$directory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
$arguments = @('initialize-finding', "--output-directory=$directory", "--reviewer-id=$ReviewerId", "--focus-area=$FocusArea")
if ($Force) { $arguments += '--force' }
if ($Resume) { $arguments += '--resume' }
if ($FileGroupQueuePath) {
    $queue = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($FileGroupQueuePath)
    $arguments += "--queue-path=$queue"
}
Invoke-NativeReviewWorkflow -Arguments $arguments
