#Requires -Version 7.6

# Refuses malformed or stale candidate IDs before dispatching adjudicators.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Path,
    [Parameter(Mandatory)][string] $IndexPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
$candidates = Read-WorkflowData -Kind mergeCandidates -Path $Path
$index = Read-WorkflowData -Kind findingIndex -Path $IndexPath
$known = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
foreach ($finding in $index['findings']) {
    if (-not $known.Add($finding['id'])) { throw "Finding index repeats identifier '$($finding['id'])'." }
}
foreach ($cluster in $candidates['clusters']) {
    foreach ($id in $cluster['findingIds']) {
        if (-not $known.Contains($id)) { throw "Candidate names unknown finding '$id'." }
    }
}
@{ status = 'Valid'; clusterCount = $candidates['clusters'].Count } | ConvertTo-Json -Compress
