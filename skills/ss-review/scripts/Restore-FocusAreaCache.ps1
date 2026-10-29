#Requires -Version 7.6

# Restores a cached focus-area set into session state (docs/design.md, "Focus-area reuse").
#
# Stage 1 of prepare-focus-areas.md runs this and branches on the result: a hit skips generation
# entirely, a miss falls through to the full workflow. The review agent drives the cache by running
# commands and parsing their output rather than by dot-sourcing PowerShell, which is why each cache
# operation is its own executable over the shared library in lib\FocusAreaCache.ps1.
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
    [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow,

    [switch] $Refresh
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\FocusAreaCache.ps1"
. "$PSScriptRoot\lib\FocusAreaCacheDiagnostics.ps1"

$transactionLock = Open-PullRequestFileLock -Path (Join-Path $SessionStateDirectory 'focus-area-cache-operation')
try {
    if (-not $CacheDirectory) {
        $CacheDirectory = Get-DefaultFocusAreaCacheDirectory
    }

    $result = Restore-FocusAreaCache `
        -SessionStateDirectory $SessionStateDirectory `
        -InputDirectory $InputDirectory `
        -RecipePath $RecipePath `
        -CacheDirectory $CacheDirectory `
        -GeneratorProfile $GeneratorProfile `
        -FreshnessDays $FreshnessDays `
        -NowUtc $NowUtc `
        -Refresh:$Refresh
    $userInstructionFileCount = Get-ReviewUserInstructionFileCount -InputDirectory $InputDirectory
    $result | Add-Member -NotePropertyName userInstructionFileCount -NotePropertyValue $userInstructionFileCount

    # A restore begins the record rather than extending an existing one, so a repeated restore never
    # reports a superseded attempt alongside the current one.
    Write-FocusAreaCacheDiagnostics `
        -SessionStateDirectory $SessionStateDirectory `
        -Diagnostics @{
            cache = $result.status
            reason = $result.reason
            userInstructionFileCount = $userInstructionFileCount
        }

    $result | ConvertTo-Json -Compress
} finally { $transactionLock.Dispose() }
