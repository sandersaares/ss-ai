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
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\ReviewWork.ps1"
$assessment = Read-WorkflowData -Kind assessment -Path $AssessmentPath
Set-ReviewFileAssessment -QueuePath $QueuePath -InventoryPath $InventoryPath -FileGroupsPath $FileGroupsPath -Assessment $assessment
@{ status = 'Recorded'; path = $assessment['path']; disposition = $assessment['disposition'] } | ConvertTo-Json -Compress
