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
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
$item = ConvertFrom-JsonVerbatim -Text ((Read-PullRequestFileText -Path ((Resolve-PullRequestPath -Path $ItemPath))))
$field = Get-WorkflowListName -Kind $Kind
Assert-WorkflowData -Kind $Kind -Value ([ordered]@{ schemaVersion = 1; $field = @($item) })
# Resolve document references at the draft boundary, so moving the assembled plan cannot change them.
if ($Kind -in @('mergePlan', 'updates') -and (Test-JsonProperty -Node $item -Name 'findingPath')) {
    $findingPath = $item['findingPath']
    if ($findingPath -is [string] -and -not [IO.Path]::IsPathFullyQualified($findingPath)) {
        $item['findingPath'] = [IO.Path]::GetFullPath((Join-Path (Split-Path -Parent (Resolve-PullRequestPath -Path $ItemPath)) $findingPath))
    }
}
$index = if ($PSBoundParameters.ContainsKey('ReplaceIndex')) { $ReplaceIndex } else { -1 }
Add-WorkflowListItem -Kind $Kind -Path $Path -Item $item -ReplaceIndex $index
@{ status = 'Recorded'; path = (Resolve-PullRequestPath -Path $Path); kind = $Kind } | ConvertTo-Json -Compress
