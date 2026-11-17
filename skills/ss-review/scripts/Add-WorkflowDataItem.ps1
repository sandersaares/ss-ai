#Requires -Version 7.6

# Persists one schema-defined draft through the shared plan writer rather than agent JSON mutation.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][ValidateSet('mergeCandidates', 'mergePlan', 'updates', 'removals', 'decisions')][string] $Kind,
    [Parameter(Mandatory)][string] $Path,
    [Parameter(Mandatory)][string] $ItemPath,
    [ValidateRange(0, [int]::MaxValue)][int] $ReplaceIndex
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$fullPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
$fullItemPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ItemPath)
$arguments = @('plan-append', "--kind=$Kind", "--path=$fullPath", "--item-path=$fullItemPath")
if ($PSBoundParameters.ContainsKey('ReplaceIndex')) { $arguments += "--replace-index=$ReplaceIndex" }
Invoke-NativeReviewWorkflow -Arguments $arguments
