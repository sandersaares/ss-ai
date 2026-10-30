#Requires -Version 7.6

# Bridges reference expansion transactions to the native workflow.
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
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"

$arguments = @('update-focus-area-expansion', "--session-state=$SessionStateDirectory", "--operation=$($Operation.ToLowerInvariant())")
if ($InputPath) { $arguments += "--input-path=$InputPath" }
# Completed and failed results select their own reference, as in the original wrapper.
if ($ReferenceId -and $Operation -notin @('Complete', 'Fail')) { $arguments += "--reference-id=$ReferenceId" }
Invoke-NativeReviewWorkflow -Arguments $arguments
