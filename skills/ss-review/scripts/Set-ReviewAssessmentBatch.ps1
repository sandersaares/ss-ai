#Requires -Version 7.6

# Persists a claimed group's complete assessment batch and completion in one transaction.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $QueuePath,
    [Parameter(Mandatory)][string] $InventoryPath,
    [Parameter(Mandatory)][string] $FileGroupsPath,
    [Parameter(Mandatory)][string] $ArtifactPath,
    [Parameter(Mandatory)][string] $BatchPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"
. "$PSScriptRoot\lib\ReviewWork.ps1"
$batch = ConvertFrom-JsonVerbatim -Text ((Read-PullRequestFileText -Path ((Resolve-PullRequestPath -Path $BatchPath))))
Set-ReviewAssessmentBatch -QueuePath $QueuePath -InventoryPath $InventoryPath -FileGroupsPath $FileGroupsPath -ArtifactPath $ArtifactPath -Batch $batch |
    ConvertTo-Json -Depth 32 -Compress
