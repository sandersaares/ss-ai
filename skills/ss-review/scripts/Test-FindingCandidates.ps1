#Requires -Version 7.6

# Refuses malformed or stale candidate IDs before dispatching adjudicators.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Path,
    [Parameter(Mandatory)][string] $IndexPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$candidates = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
$index = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($IndexPath)
Invoke-NativeReviewWorkflow -Arguments @('validate-candidates', "--path=$candidates", "--index-path=$index")
