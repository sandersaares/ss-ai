#Requires -Version 7.6
[CmdletBinding(PositionalBinding = $false)]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
. "$PSScriptRoot\lib\ReportTools.ps1"

$skillRoot = Split-Path -Parent $PSScriptRoot
Invoke-NativeReviewWorkflow -Arguments @('test-review-report-tools', "--skill-root=$skillRoot",
    "--tools-directory=$(Get-ReportToolsDirectory)")
