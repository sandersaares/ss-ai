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

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\ReviewWork.ps1"

Complete-ReviewFileGroup -QueuePath $QueuePath -GroupId $GroupId | ConvertTo-Json -Depth 32 -Compress
