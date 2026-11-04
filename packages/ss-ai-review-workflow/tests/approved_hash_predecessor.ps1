#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Root,
    [Parameter(Mandatory)][string] $ContextPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$Root/skills/ss-review/scripts/lib/JsonValue.ps1"
. "$Root/skills/ss-review/scripts/lib/PullRequestModel.ps1"

$capture = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($ContextPath, [Text.Encoding]::UTF8))
Get-PullRequestEvidenceHash -Document $capture
