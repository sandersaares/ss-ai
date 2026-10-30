#Requires -Version 7.6

# Snapshots the skill's built-in focus areas and the caller's configured instructions
# before cache lookup (docs/implementation.md, "Worker input boundary").
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $SessionStateDirectory,
    [string] $RepositoryRoot,
    [string[]] $RepositoryInstruction
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$session = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($SessionStateDirectory)
$skillRoot = Split-Path -Parent $PSScriptRoot
$arguments = @(
    'snapshot-instructions', "--session-state=$session", "--skill-root=$skillRoot"
)
if ($RepositoryRoot) {
    $arguments += "--repository-root=$($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($RepositoryRoot))"
}
if ($null -ne $RepositoryInstruction) {
    foreach ($path in $RepositoryInstruction) {
        $arguments += "--repository-instruction=$path"
    }
}
Invoke-NativeReviewWorkflow -Arguments $arguments
