#Requires -Version 7.6

# Verifies that the focus-area artifacts in session state are complete and usable.
#
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $SessionStateDirectory,

    [Parameter(Mandatory)]
    [string] $PreparationIdentity
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$session = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($SessionStateDirectory)
Invoke-NativeReviewWorkflow -Arguments @('validate-focus-area-artifacts', "--session-state=$session", "--preparation-identity=$PreparationIdentity")
