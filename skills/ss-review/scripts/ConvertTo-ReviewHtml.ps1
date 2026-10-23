#Requires -Version 7.6
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Path,
    [Parameter(Mandatory)][string] $OutputPath,
    [string] $Title = 'Review report'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\PullRequestTransportHost.ps1"
. "$PSScriptRoot\lib\ReportTools.ps1"
. "$PSScriptRoot\lib\ReviewHtml.ps1"

$skillRoot = Split-Path -Parent $PSScriptRoot
Write-ReviewHtml -Path $Path -OutputPath $OutputPath -Title $Title -SkillRoot $skillRoot `
    -Tools (Get-ReviewReportTools -SkillRoot $skillRoot) -InvokeProcess (Get-ReportProcessInvoker) |
    ConvertTo-Json -Depth 8 -Compress
