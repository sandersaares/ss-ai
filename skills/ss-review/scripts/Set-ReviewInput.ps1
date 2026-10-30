#Requires -Version 7.6

# Validates complete inventory drafts before publishing preparation inputs to other workers.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][ValidateSet('Inventory', 'Groups')][string] $Kind,
    [Parameter(Mandatory)][string] $InputPath,
    [Parameter(Mandatory)][string] $Path,
    [string] $InventoryPath,
    [switch] $Replace
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$arguments = @('set-review-input', "--kind=$($Kind.ToLowerInvariant())", "--input-path=$InputPath", "--path=$Path")
if ($InventoryPath) { $arguments += "--inventory-path=$InventoryPath" }
if ($Replace) { $arguments += '--replace' }
Invoke-NativeReviewWorkflow -Arguments $arguments
