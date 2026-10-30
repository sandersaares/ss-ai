#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $QueuePath,

    [Parameter(Mandatory)]
    [string] $GroupId
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
Invoke-NativeReviewWorkflow -Arguments @('complete-review-group', "--queue-path=$QueuePath", "--group-id=$GroupId")
