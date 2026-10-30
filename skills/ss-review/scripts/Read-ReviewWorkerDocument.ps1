#Requires -Version 7.6

# Preserves the existing script entry point for bounded native document reads.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $AssignmentPath,
    [Parameter(Mandatory)][string] $DocumentId,
    [ValidateRange(0, [int]::MaxValue)][int] $Offset = 0
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"

Invoke-NativeReviewWorkflow -Arguments @(
    'worker-document', "--assignment-path=$AssignmentPath", "--document-id=$DocumentId", "--offset=$Offset"
)
