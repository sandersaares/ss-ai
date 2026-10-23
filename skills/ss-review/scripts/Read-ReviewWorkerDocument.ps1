#Requires -Version 7.6

# Bounded first reads and reloads of shared worker guidance, independent of viewer defaults.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $AssignmentPath,
    [Parameter(Mandatory)][string] $DocumentId,
    [ValidateRange(0, [int]::MaxValue)][int] $Offset = 0
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\ReviewInput.ps1"

$assignment = Read-WorkflowData -Kind workerAssignment -Path $AssignmentPath
Add-ReviewInputDiagnostic -Path $assignment['diagnosticsPath'] -Name documentReadCalls
try {
    $page = Read-ReviewSharedDocument -Assignment $assignment -DocumentId $DocumentId -Offset $Offset
    [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
    [Console]::WriteLine((ConvertTo-Json -InputObject $page -Depth 100 -Compress))
} catch {
    if ($Offset -eq 0) { Add-ReviewInputDiagnostic -Path $assignment['diagnosticsPath'] -Name firstReadFailures }
    throw
}
