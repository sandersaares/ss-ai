#Requires -Version 7.6

# Persists requested-scope analysis or exclusion declarations before group completion.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $QueuePath,
    [Parameter(Mandatory)][string] $InventoryPath,
    [Parameter(Mandatory)][string] $FileGroupsPath,
    [Parameter(Mandatory)][string] $AssessmentPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$QueuePath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($QueuePath)
$InventoryPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InventoryPath)
$FileGroupsPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($FileGroupsPath)
$AssessmentPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($AssessmentPath)
Invoke-NativeReviewWorkflow -Arguments @('set-review-assessment', "--queue-path=$QueuePath",
    "--inventory-path=$InventoryPath", "--file-groups-path=$FileGroupsPath", "--assessment-path=$AssessmentPath")
