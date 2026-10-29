#Requires -Version 7.6

# Validates categorizer output before the preparation agent accepts the handoff.
# Ref: docs/implementation.md, "Focus-area preparation".
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $SessionStateDirectory,
    [Parameter(Mandatory)][string] $MapPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\FocusAreaPreparation.ps1"

$sourceLock = Open-PullRequestFileLock -Path (Join-Path $SessionStateDirectory 'focus-area-cache-operation') -Shared
try {
    $validated = Read-FocusAreaMap -MapPath $MapPath -SessionStateDirectory $SessionStateDirectory
    [ordered]@{
        status = 'Valid'
        categoryCount = $validated.Map['categories'].Count
        chapterCount = $validated.Map['chapters'].Count
    } | ConvertTo-Json -Compress
} finally { $sourceLock.Dispose() }
