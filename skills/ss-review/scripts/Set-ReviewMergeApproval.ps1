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
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\ReviewAuditState.ps1"
$context = Read-PullRequestContextDocument -Path $ContextPath
$previous = if ($CarryForward) { Read-PullRequestContextDocument -Path $PreviousContextPath } else { $null }
$status = Set-ReviewMergeApproval -Path (Join-Path $OutputDirectory 'merge-approval.json') -Context $context `
    -ApproveMerge ($Decision -ceq 'Approve') -PreviousContext $previous -CarryForward:$CarryForward
@{ status = $status; sourceCommit = $context['pullRequest']['reviewedSourceCommit'] } | ConvertTo-Json -Compress
