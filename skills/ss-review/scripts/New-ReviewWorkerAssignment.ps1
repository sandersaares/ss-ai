#Requires -Version 7.6

# Supplies the active skill assets to the native assignment operation.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][ValidateSet('references', 'fileGroups', 'focusAreas', 'overview', 'review')][string] $Phase,
    [Parameter(Mandatory)][string] $SessionState,
    [Parameter(Mandatory)][string] $ScopePath,
    [Parameter(Mandatory)][string] $OutputDirectory,
    [string] $ManifestPath,
    [string] $ReviewerId,
    [string] $ContextPath,
    [switch] $Resume,
    [switch] $RepositoryInstructions,
    [switch] $IncludePreExisting
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"

$arguments = @(
    'worker-assignment', "--phase=$Phase", "--session-state=$SessionState",
    "--scope-path=$ScopePath", "--output-directory=$OutputDirectory",
    "--skill-root=$(Split-Path -Parent $PSScriptRoot)"
)
if ($ManifestPath) { $arguments += "--manifest-path=$ManifestPath" }
if ($ReviewerId) { $arguments += "--reviewer-id=$ReviewerId" }
if ($ContextPath) { $arguments += "--context-path=$ContextPath" }
if ($Resume) { $arguments += '--resume' }
if ($RepositoryInstructions) { $arguments += '--repository-instructions' }
if ($IncludePreExisting) { $arguments += '--include-pre-existing' }
Invoke-NativeReviewWorkflow -Arguments $arguments
