#Requires -Version 7.6

# The primary skill obtains installation permission before invoking this command.
[CmdletBinding(PositionalBinding = $false)]
param(
    [switch] $InstallBrowserDependencies
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
. "$PSScriptRoot\lib\ReportTools.ps1"

$skillRoot = Split-Path -Parent $PSScriptRoot
$arguments = @('install-review-report-tools', "--skill-root=$skillRoot",
    "--tools-directory=$(Get-ReportToolsDirectory)")
if ($InstallBrowserDependencies) { $arguments += '--install-browser-dependencies' }
Invoke-NativeReviewWorkflow -Arguments $arguments
