#Requires -Version 7.6

# Reports measured input costs and records failures outside the prepared commands.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Path,
    [ValidateSet('setupCalls', 'repeatedSetupCalls', 'documentReadCalls', 'firstReadFailures', 'malformedQueryFailures', 'cursorRestarts')]
    [string] $Record,
    [ValidateRange(1, [int]::MaxValue)][int] $Count = 1
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\ReviewInput.ps1"
if ($PSBoundParameters.ContainsKey('Count') -and -not $Record) { throw 'Count requires Record.' }
if ([IO.Directory]::Exists($Path)) {
    if ($Record) { throw 'Record requires one diagnostics file, not a directory.' }
    # Only current-run records use this filename; measured baselines are kept separately.
    $files = @(Get-ChildItem -LiteralPath $Path -Filter input-diagnostics.json -File -Recurse)
    if (-not $files.Count) { throw 'No input diagnostics records exist in the selected run directory.' }
    $total = Read-WorkflowData -Kind inputDiagnostics -Path $files[0].FullName
    foreach ($file in ($files | Select-Object -Skip 1)) {
        $recorded = Read-WorkflowData -Kind inputDiagnostics -Path $file.FullName
        foreach ($name in @($total['counts'].Keys)) { $total['counts'][$name] += $recorded['counts'][$name] }
    }
    Assert-WorkflowData -Kind inputDiagnostics -Value $total
    ConvertTo-Json -InputObject $total -Depth 10 -Compress
    return
}
if ($Record) { Add-ReviewInputDiagnostic -Path $Path -Name $Record -Count $Count }
$data = Read-WorkflowData -Kind inputDiagnostics -Path $Path
ConvertTo-Json -InputObject $data -Depth 10 -Compress
