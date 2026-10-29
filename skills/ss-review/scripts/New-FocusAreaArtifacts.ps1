#Requires -Version 7.6

# Assembles both focus-area reading views from the complete validated categorization map.
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
. "$PSScriptRoot\lib\FocusAreaPreparation.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FocusAreaExpansion.ps1"

$sourceLock = Open-PullRequestFileLock -Path (Join-Path $SessionStateDirectory 'focus-area-cache-operation') -Shared
try {
    $expansion = Read-WorkflowData -Kind expansion -Path (Join-Path $SessionStateDirectory 'focus-area-expansion-status.json')
    Assert-FocusAreaExpansion -Document $expansion -SessionStateDirectory $SessionStateDirectory -RequireFinal

    $validated = Read-FocusAreaMap -MapPath $MapPath -SessionStateDirectory $SessionStateDirectory -RequireComplete
    $documents = ConvertTo-FocusAreaDocuments -ValidatedMap $validated
    Write-FocusAreaDocuments -Documents $documents -SessionStateDirectory $SessionStateDirectory
    [ordered]@{
        status = 'Assembled'
        categoryCount = $validated.Map['categories'].Count
        chapterCount = $validated.Map['chapters'].Count
    } | ConvertTo-Json -Compress
} finally { $sourceLock.Dispose() }
