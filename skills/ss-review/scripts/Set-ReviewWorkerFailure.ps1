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
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\ReviewWork.ps1"
Set-ReviewWorkerFailure -QueuePath $QueuePath -Reason $Reason | ConvertTo-Json -Depth 32 -Compress
