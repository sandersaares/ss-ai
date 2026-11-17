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
# Decision plans belong to the audit family, which retains its PowerShell writer
# until the audit and Markdown migration described in TODO.md, "Skill design".
if ($Kind -ceq 'decisions') {
    . "$PSScriptRoot\lib\JsonValue.ps1"
    . "$PSScriptRoot\lib\PullRequestModel.ps1"
    . "$PSScriptRoot\lib\WorkflowData.ps1"
    $item = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path (Resolve-PullRequestPath -Path $ItemPath))
    Assert-WorkflowData -Kind $Kind -Value ([ordered]@{ schemaVersion = 1; decisions = @($item) })
    $index = if ($PSBoundParameters.ContainsKey('ReplaceIndex')) { $ReplaceIndex } else { -1 }
    Add-WorkflowListItem -Kind $Kind -Path $Path -Item $item -ReplaceIndex $index
    @{ status = 'Recorded'; path = (Resolve-PullRequestPath -Path $Path); kind = $Kind } | ConvertTo-Json -Compress
    return
}
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$fullPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
$fullItemPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ItemPath)
$arguments = @('plan-append', "--kind=$Kind", "--path=$fullPath", "--item-path=$fullItemPath")
if ($PSBoundParameters.ContainsKey('ReplaceIndex')) { $arguments += "--replace-index=$ReplaceIndex" }
Invoke-NativeReviewWorkflow -Arguments $arguments
