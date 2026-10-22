#Requires -Version 7.6

# Validates complete inventory drafts before publishing preparation inputs to other workers.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][ValidateSet('Inventory', 'Groups')][string] $Kind,
    [Parameter(Mandatory)][string] $InputPath,
    [Parameter(Mandatory)][string] $Path,
    [string] $InventoryPath,
    [switch] $Replace
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\ReviewWork.ps1"
$records = if ($Kind -ceq 'Inventory') {
    Read-ReviewFileInventory -Path $InputPath
} else {
    if (-not $InventoryPath) { throw 'Groups requires -InventoryPath.' }
    Read-ReviewFileGroups -Path $InputPath -FileInventory (Read-ReviewFileInventory -Path $InventoryPath)
}
$lock = Open-WorkflowDataLock -Path $Path
try {
    if ((Test-Path -LiteralPath $Path) -and -not $Replace) { throw "Input '$Path' already exists; only explicit regeneration may replace it." }
    Write-PullRequestOutputFileAtomically -Path $Path -Content (ConvertTo-JsonText -Value $records)
} finally { $lock.Dispose() }
@{ status = 'Recorded'; path = (Resolve-PullRequestPath -Path $Path); count = $records.Count } | ConvertTo-Json -Compress
