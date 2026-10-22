#Requires -Version 7.6
[CmdletBinding(PositionalBinding = $false)]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\PullRequestTransportHost.ps1"
. "$PSScriptRoot\lib\ReportTools.ps1"
. "$PSScriptRoot\lib\ReviewHtml.ps1"

$skillRoot = Split-Path -Parent $PSScriptRoot
$tools = Get-ReviewReportTools -SkillRoot $skillRoot
if ($tools.missing.Count -eq 0) {
    Assert-ReviewReportRendering -SkillRoot $skillRoot -Tools $tools
}
$tools | ConvertTo-Json -Depth 8 -Compress
