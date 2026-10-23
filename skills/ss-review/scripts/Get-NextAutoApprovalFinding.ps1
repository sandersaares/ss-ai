#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $Path,

    [Parameter(Mandatory)]
    [string] $DecisionsPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"

$artifact = Read-FindingArtifact -Path $Path
$decisions = Open-FindingDecisionDocument -Path $DecisionsPath
Get-FindingAutoApprovalWork -Artifact $artifact -Decision $decisions | ConvertTo-Json -Depth 32 -Compress
