#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Root,
    [Parameter(Mandatory)][string] $ContextPath,
    [Parameter(Mandatory)][string] $FindingPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$Root/skills/ss-review/scripts/lib/JsonValue.ps1"
. "$Root/skills/ss-review/scripts/lib/FindingModel.ps1"
. "$Root/skills/ss-review/scripts/lib/TrackerItem.ps1"

$capture = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($ContextPath, [Text.Encoding]::UTF8))
$finding = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($FindingPath, [Text.Encoding]::UTF8))
Assert-TrackerFollowUpTarget -Finding $finding -Context $capture
$pullRequestRejected = $false
try {
    Assert-FindingFollowUpTarget -Finding $finding -Context $capture
} catch {
    $pullRequestRejected = $true
}
if (-not $pullRequestRejected) {
    throw 'A reply to a non-opening comment must not pass pull request publication.'
}
