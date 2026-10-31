#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $InventoryPath,

    [Parameter(Mandatory)]
    [string] $FileGroupsPath,

    [Parameter(Mandatory)]
    [string] $QueuePath,

    [switch] $Resume
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$arguments = @('initialize-review-queue', "--inventory-path=$InventoryPath", "--file-groups-path=$FileGroupsPath", "--queue-path=$QueuePath")
if ($Resume) { $arguments += '--resume' }
Invoke-NativeReviewWorkflow -Arguments $arguments
