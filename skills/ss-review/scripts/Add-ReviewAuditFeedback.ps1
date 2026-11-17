#Requires -Version 7.6

# Preserves a user's feedback independently of whether it settles the finding.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $OutputDirectory,
    [Parameter(Mandatory)][string] $FindingsPath,
    [Parameter(Mandatory)][string] $ResponseId,
    [Parameter(Mandatory)][string] $FindingId,
    [Parameter(Mandatory)][string] $TextPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$path = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath((Join-Path $OutputDirectory 'audit-feedback.json'))
$findings = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($FindingsPath)
$text = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($TextPath)
Invoke-NativeReviewWorkflow -Arguments @('audit-add-feedback', "--path=$path", "--findings-path=$findings", "--response-id=$ResponseId", "--finding-id=$FindingId", "--text-path=$text")
