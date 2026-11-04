#Requires -Version 7.6

# Projects audited findings through the native freshness and anchor checks.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $Path,

    [Parameter(Mandatory)]
    [string] $DecisionsPath,

    [Parameter(Mandatory)]
    [string] $OutputPath,

    [string] $ContextPath,

    [string] $RepositoryPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"

$arguments = @(
    'approved', 'build',
    "--path=$($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path))",
    "--decisions-path=$($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($DecisionsPath))",
    "--output-path=$($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath))"
)
if ($ContextPath) {
    $arguments += "--context-path=$($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ContextPath))"
}
if ($RepositoryPath) {
    $arguments += "--repository-path=$($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($RepositoryPath))"
}
Invoke-NativeReviewWorkflow -Arguments $arguments
