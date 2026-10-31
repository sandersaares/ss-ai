#Requires -Version 7.6

# Bridges authored map transactions to the native workflow.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $SessionStateDirectory,
    [Parameter(Mandatory)][string] $Path,
    [Parameter(Mandatory)][ValidateSet('Append', 'Replace', 'Regroup')][string] $Operation,
    [string] $InputPath,
    [string] $CategoryId,
    [string] $NewCategoryId,
    [string] $Title
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"

$arguments = @('update-focus-area-map', "--session-state=$SessionStateDirectory", "--path=$Path", "--operation=$($Operation.ToLowerInvariant())")
if ($InputPath) { $arguments += "--input-path=$InputPath" }
if ($CategoryId) { $arguments += "--category-id=$CategoryId" }
if ($NewCategoryId) { $arguments += "--new-category-id=$NewCategoryId" }
if ($Title) { $arguments += "--title=$Title" }
Invoke-NativeReviewWorkflow -Arguments $arguments
