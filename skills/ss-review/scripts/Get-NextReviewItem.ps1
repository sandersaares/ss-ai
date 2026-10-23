#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $QueuePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\ReviewWork.ps1"

Get-NextReviewWorkItem -QueuePath $QueuePath | ConvertTo-Json -Depth 32 -Compress
