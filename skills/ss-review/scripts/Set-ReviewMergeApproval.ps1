#Requires -Version 7.6

# Records only the explicit human decision or the workflow's permitted revalidation carry-forward.
[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = 'Decision')]
param(
    [Parameter(Mandatory)][string] $OutputDirectory,
    [Parameter(Mandatory)][string] $ContextPath,
    [Parameter(Mandatory, ParameterSetName = 'Decision')][ValidateSet('Approve', 'DoNotApprove')][string] $Decision,
    [Parameter(Mandatory, ParameterSetName = 'Carry')][switch] $CarryForward,
    [Parameter(Mandatory, ParameterSetName = 'Carry')][string] $PreviousContextPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$path = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath((Join-Path $OutputDirectory 'merge-approval.json'))
$context = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ContextPath)
$arguments = @('audit-set-merge-approval', "--path=$path", "--context-path=$context")
if ($CarryForward) {
    $previous = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($PreviousContextPath)
    $arguments += @('--carry-forward', "--previous-context-path=$previous")
} elseif ($Decision -ceq 'Approve') {
    $arguments += '--approve'
}
Invoke-NativeReviewWorkflow -Arguments $arguments
