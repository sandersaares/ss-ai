#Requires -Version 7.6

# Captures a pull request and its discussion (docs/design.md, "Deterministic discussion capture").
#
# Phase 1 of the review runs this instead of asking a subagent to read and retell the discussion,
# because a review that deduplicates its findings against prior feedback has to trust that the
# feedback it sees is what was written. Pre-audit and publication refreshes compare against the
# last checked capture, preserving the initial baseline for follow-up accounting.
#
# The native context command owns provider reads and immutable publication.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. The captured document goes to a file and errors to the
# error stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    # Browser URL of the pull request. Either this or explicit coordinates are required.
    [string] $Url,

    [ValidateSet('azureDevOps', 'gitHub')]
    [string] $Platform,

    [string] $Organization,

    [string] $Project,

    [string] $Repository,

    [int] $PullRequestId,

    [string] $CollectionUrl,

    [Parameter(Mandatory)]
    [string] $OutputDirectory,

    # Every refresh uses a new prefix so audit and intervening-delta baselines remain immutable.
    [string] $OutputPrefix = '',

    # An earlier capture to compare against. Supplying it adds a delta to the emitted result.
    [string] $ComparePath,

    # Azure DevOps only. The access token is issued for the current subscription's tenant, which is
    # usually but not always the organization's tenant.
    [string] $Tenant,

    [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeCaptureBridge.ps1"

$arguments = [Collections.Generic.List[string]]::new()
$arguments.Add('create')
$arguments.Add("--output-directory=$OutputDirectory")
$arguments.Add("--output-prefix=$OutputPrefix")
foreach ($entry in @(
    @('Url', 'url'), @('Platform', 'platform'), @('Organization', 'organization'),
    @('Project', 'project'), @('Repository', 'repository'), @('PullRequestId', 'pull-request-id'),
    @('CollectionUrl', 'collection-url'), @('ComparePath', 'compare-path'), @('Tenant', 'tenant')
)) {
    if ($PSBoundParameters.ContainsKey($entry[0])) { $arguments.Add("--$($entry[1])=$($PSBoundParameters[$entry[0]])") }
}
if ($PSBoundParameters.ContainsKey('NowUtc')) {
    $arguments.Add("--now-utc=$($NowUtc.ToString('o', [cultureinfo]::InvariantCulture))")
}
Invoke-NativeCaptureContext -Arguments $arguments.ToArray()
