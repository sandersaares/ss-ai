#Requires -Version 7.6

# Publishes the session's focus-area set to the cache (docs/design.md, "Focus-area reuse").
#
# Stage 3 of prepare-focus-areas.md runs this once the artifacts are generated and validated. The
# review agent drives the cache by running commands and parsing their output rather than by
# dot-sourcing PowerShell, which is why each cache operation is its own executable over the shared
# library in lib\FocusAreaCache.ps1.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. Diagnostics go to a file and errors to the error
# stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $SessionStateDirectory,

    [Parameter(Mandatory)]
    [string] $InputDirectory,

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

. "$PSScriptRoot\lib\FocusAreaCache.ps1"
. "$PSScriptRoot\lib\FocusAreaCacheDiagnostics.ps1"
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FocusAreaPreparation.ps1"
. "$PSScriptRoot\lib\FocusAreaExpansion.ps1"

$transactionLock = Open-PullRequestFileLock -Path (Join-Path $SessionStateDirectory 'focus-area-cache-operation')
try {
    $expansion = Read-WorkflowData -Kind expansion -Path (Join-Path $SessionStateDirectory 'focus-area-expansion-status.json')
    Assert-FocusAreaExpansion -Document $expansion -SessionStateDirectory $SessionStateDirectory -RequireFinal
    $InputState = $expansion['state']

    if (-not $CacheDirectory) {
        $CacheDirectory = Get-DefaultFocusAreaCacheDirectory
    }

    $result = Save-FocusAreaCache `
        -SessionStateDirectory $SessionStateDirectory `
        -InputDirectory $InputDirectory `
        -RecipePath $RecipePath `
        -CacheDirectory $CacheDirectory `
        -GeneratorProfile $GeneratorProfile `
        -FreshnessDays $FreshnessDays `
        -NowUtc $NowUtc `
        -InputState $InputState

    # The store extends the record the restore left behind, and occupies distinct fields, so the summary
    # can state both what was reused and what was contributed back - independent facts about the session.
    Write-FocusAreaCacheDiagnostics `
        -SessionStateDirectory $SessionStateDirectory `
        -Merge -Diagnostics @{ inputState = $InputState; store = $result.status; storeReason = $result.reason }

    $result | ConvertTo-Json -Compress
} finally { $transactionLock.Dispose() }
