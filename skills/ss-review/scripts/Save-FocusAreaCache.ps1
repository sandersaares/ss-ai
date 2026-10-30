#Requires -Version 7.6

# Publishes the session's focus-area set to the cache (docs/design.md, "Focus-area reuse").
#
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $SessionStateDirectory,

    [Parameter(Mandatory)]
    [string] $InputDirectory,

    [Parameter(Mandatory)]
    [string] $PreparationIdentity,

    # Defaults to the skill's own recipe, which is the document that defines how focus areas are
    # generated. It is hashed into the cache key, so editing it invalidates prior entries.
    [string] $RecipePath = (Join-Path (Split-Path -Parent $PSScriptRoot) 'prepare-focus-areas.md'),

    [string] $CacheDirectory,

    # Partitions the cache by generating model. Output from a different model is not interchangeable,
    # so it must never be served across profiles. The default matches the final scope-verification
    # model in prepare-focus-areas.md.
    [string] $GeneratorProfile = 'gpt-6-luna',

    [ValidateRange(1, 3650)]
    [int] $FreshnessDays = 7,

    # Injectable so tests can exercise retention and freshness-bucket boundaries without depending on
    # the real clock.
    [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
if (-not $CacheDirectory) {
    $CacheDirectory = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)) '.copilot\cache\ss-review\focus-areas\v1'
}
$resolve = { param($Path) $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path) }
Invoke-NativeReviewWorkflow -Arguments @(
    'save-focus-area-cache',
    "--session-state=$(& $resolve $SessionStateDirectory)",
    "--input-directory=$(& $resolve $InputDirectory)",
    "--preparation-identity=$PreparationIdentity",
    "--skill-root=$(Split-Path -Parent $PSScriptRoot)",
    "--recipe-path=$(& $resolve $RecipePath)",
    "--cache-directory=$(& $resolve $CacheDirectory)",
    "--generator-profile=$GeneratorProfile",
    "--freshness-days=$FreshnessDays",
    "--now-utc=$($NowUtc.ToUniversalTime().ToString('o', [Globalization.CultureInfo]::InvariantCulture))"
)
