#Requires -Version 7.6

# Maintains expansion outcomes and reference inheritance; agents supply only typed registrations.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $SessionStateDirectory,
    [Parameter(Mandatory)][ValidateSet('Initialize', 'Register', 'Complete', 'Fail', 'Finalize', 'Read')][string] $Operation,
    [string] $InputPath,
    [string] $ReferenceId
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FocusAreaPreparation.ps1"
. "$PSScriptRoot\lib\FocusAreaExpansion.ps1"
$inputRecord = $null
$outputs = @()
$failure = ''
if ($Operation -in @('Register', 'Complete', 'Fail') -and -not $InputPath) { throw "$Operation requires -InputPath." }
if ($Operation -ceq 'Register') { $inputRecord = Read-WorkflowData -Kind referenceInput -Path $InputPath }
if ($Operation -in @('Complete', 'Fail')) {
    $result = Read-WorkflowData -Kind referenceResult -Path $InputPath
    $ReferenceId = $result['referenceId']
    $outputs = $result['outputs']
    $failure = $result['error']
    if ($Operation -ceq 'Complete' -and $null -ne $failure) { throw 'Successful expansion must not carry an error.' }
    if ($Operation -ceq 'Fail' -and $outputs.Count -ne 0) { throw 'Failed expansion must not claim outputs.' }
}
Update-FocusAreaExpansion -Path (Join-Path $SessionStateDirectory 'focus-area-expansion-status.json') `
    -SessionStateDirectory $SessionStateDirectory -Operation $Operation -InputRecord $inputRecord `
    -ReferenceId $ReferenceId -OutputPath $outputs -Failure $failure | ConvertTo-Json -Depth 32 -Compress
