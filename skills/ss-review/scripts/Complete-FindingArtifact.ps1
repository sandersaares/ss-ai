#Requires -Version 7.6

# Records that a focus-area reviewer has reviewed the full inventory
# (docs/implementation.md, "Finding artifacts").
#
# A reviewer that found nothing and a reviewer that died partway both leave an artifact with an
# empty finding list, and only this command tells the two apart. The separate validation gate checks
# completion before the review publishes its findings.
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
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$artifact = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ArtifactPath)
$queue = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($FileGroupQueuePath)
Invoke-NativeReviewWorkflow -Arguments @('complete-finding', "--artifact-path=$artifact", "--queue-path=$queue")
