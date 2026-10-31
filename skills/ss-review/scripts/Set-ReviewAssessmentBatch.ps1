#Requires -Version 7.6

# Persists a claimed group's complete assessment batch and completion in one transaction.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $QueuePath,
    [Parameter(Mandatory)][string] $InventoryPath,
    [Parameter(Mandatory)][string] $FileGroupsPath,
    [Parameter(Mandatory)][string] $ArtifactPath,
    [Parameter(Mandatory)][string] $BatchPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
Invoke-NativeReviewWorkflow -Arguments @('complete-review-batch', "--queue-path=$QueuePath",
    "--inventory-path=$InventoryPath", "--file-groups-path=$FileGroupsPath",
    "--artifact-path=$ArtifactPath", "--batch-path=$BatchPath")
