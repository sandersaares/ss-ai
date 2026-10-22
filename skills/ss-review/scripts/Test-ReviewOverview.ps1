#Requires -Version 7.6

# Checks the generated overview contract without requiring a model-authored parser.
[CmdletBinding(PositionalBinding = $false)]
param([Parameter(Mandatory)][string] $Path, [string] $ContextPath)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\OverviewPublication.ps1"

$text = (Read-PullRequestFileText -Path ((Resolve-PullRequestPath -Path $Path)) -Encoding ([Text.UTF8Encoding]::new($false, $true)))
Assert-ReviewOverviewStructure -Text $text
$lines = @([regex]::Split($text, '\r\n|\r|\n') | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
if ($lines[0] -cne '# Content overview') { throw 'Expected the Content overview title.' }
if ($ContextPath) {
    $context = Read-PullRequestContextDocument -Path $ContextPath
    if ($lines[1] -cne "**Pull request:** $($context['pullRequestUrl'])") { throw 'Expected the captured pull request URL immediately beneath the title.' }
} elseif ($lines[1].StartsWith('**Pull request:**', [StringComparison]::Ordinal)) {
    throw 'A pull request link requires ContextPath.'
}
[ordered]@{ isValid = $true; path = Resolve-PullRequestPath -Path $Path } | ConvertTo-Json -Compress
