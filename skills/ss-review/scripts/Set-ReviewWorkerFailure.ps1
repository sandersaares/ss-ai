#Requires -Version 7.6

# Distinguishes a stopped failed reviewer from an active or merely interrupted assignment.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $QueuePath,
    [Parameter(Mandatory)][string] $Reason
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
Invoke-NativeReviewWorkflow -Arguments @('fail-review-worker', "--queue-path=$QueuePath", "--reason=$Reason")
