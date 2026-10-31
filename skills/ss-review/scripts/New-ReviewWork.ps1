#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $InventoryPath,

    [Parameter(Mandatory)]
    [string] $FileGroupsPath,

    [Parameter(Mandatory)]
    [string] $FocusAreaDirectory,

    [Parameter(Mandatory)]
    [string] $OutputDirectory,

    [string] $RepositoryRoot,
    [string] $ArtifactDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$arguments = @('create-review-work', "--inventory-path=$InventoryPath", "--file-groups-path=$FileGroupsPath",
    "--focus-area-directory=$FocusAreaDirectory", "--output-directory=$OutputDirectory")
if ($RepositoryRoot) { $arguments += "--repository-root=$RepositoryRoot" }
if ($ArtifactDirectory) { $arguments += "--artifact-directory=$ArtifactDirectory" }
Invoke-NativeReviewWorkflow -Arguments $arguments
