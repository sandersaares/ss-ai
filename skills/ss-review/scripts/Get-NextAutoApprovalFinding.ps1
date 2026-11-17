#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $Path,

    [Parameter(Mandatory)]
    [string] $DecisionsPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$artifact = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
$decisions = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($DecisionsPath)
Invoke-NativeReviewWorkflow -Arguments @('audit-poll-classification', "--artifact-path=$artifact", "--decisions-path=$decisions")
