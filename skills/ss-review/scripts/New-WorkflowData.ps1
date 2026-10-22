#Requires -Version 7.6

# Creates an explicit empty machine-readable plan; never resets an existing document.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][ValidateSet('mergeCandidates', 'mergePlan', 'updates', 'removals', 'decisions')][string] $Kind,
    [Parameter(Mandatory)][string] $Path
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
New-WorkflowList -Kind $Kind -Path $Path
@{ status = 'Created'; path = (Resolve-PullRequestPath -Path $Path); kind = $Kind } | ConvertTo-Json -Compress
