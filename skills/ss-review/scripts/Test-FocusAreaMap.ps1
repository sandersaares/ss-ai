#Requires -Version 7.6

# Validates a worker map through the native workflow.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $SessionStateDirectory,
    [Parameter(Mandatory)][string] $MapPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"

Invoke-NativeReviewWorkflow -Arguments @('validate-focus-area-map', "--session-state=$SessionStateDirectory", "--map-path=$MapPath")
