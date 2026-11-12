#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $QueuePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$QueuePath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($QueuePath)
Invoke-NativeReviewWorkflow -Arguments @('next-review-item', "--queue-path=$QueuePath")
