#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][ValidateSet('Initialize', 'Capture', 'Approve', 'AddFile', 'RemoveFile', 'RemoveReviewer')]
    [string] $Step,
    [Parameter(Mandatory)][uri] $PullRequestUrl,
    [Parameter(Mandatory)][string] $StatePath,
    [string] $Tenant,
    [string] $ExpectedAccountId,
    [string] $ExpectedSourceRef,
    [string] $ExpectedTargetRef,
    [string] $ExpectedSourceCommit,
    [string] $ExpectedTargetCommit,
    [switch] $AllowRemoteWrites
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

Import-Module (Join-Path $PSScriptRoot 'AdoExperiment.psm1') -Force
Invoke-AdoExperiment @PSBoundParameters
