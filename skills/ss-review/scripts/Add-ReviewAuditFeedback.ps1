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
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"
. "$PSScriptRoot\lib\ReviewAuditState.ps1"
Add-ReviewAuditFeedback -Path (Join-Path $OutputDirectory 'audit-feedback.json') -FindingsPath $FindingsPath `
    -ResponseId $ResponseId -FindingId $FindingId -Text ((Read-PullRequestFileText -Path ((Resolve-PullRequestPath -Path $TextPath))))
@{ status = 'Recorded'; findingId = $FindingId; responseId = $ResponseId } | ConvertTo-Json -Compress
