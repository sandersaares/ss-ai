#Requires -Version 7.6

# Owns the focus-area cache diagnostics record that the review summary reports (docs/design.md,
# "Focus-area reuse"; docs/implementation.md, "Publication and restore").
#
# This lives apart from lib\FocusAreaCache.ps1 because the two answer different questions. The cache
# library decides what may be reused; this file decides what a session tells its operator afterwards.
# Keeping them separate is what lets the cache library stay free of session-state layout and
# reporting concerns, and it is shared by all three cache entry points, none of which would otherwise
# have a reason to know where the record is kept or how it is written.
#
# Like lib\FocusAreaCache.ps1, this imposes a terminating $ErrorActionPreference on its host scope,
# which the functions below rely on: a diagnostics write that fails must be visible rather than
# silently leaving the record unwritten.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Counts only discovered user sources in the current snapshot, not the skill's
# built-in guidance or separately authorized repository instructions.
function Get-ReviewUserInstructionFileCount {
    param([Parameter(Mandatory)][string] $InputDirectory)

    $external = Join-Path $InputDirectory 'external'
    if (-not (Test-Path -LiteralPath $external)) { return 0 }
    return @(Get-ChildItem -LiteralPath $external -Recurse -Force -File).Count
}

# Locates the record. All entry points must agree on this path, since a store extends the record a
# restore left behind. It sits in session state rather than in the cache because it describes what
# one review did, not what is cached.
function Get-FocusAreaCacheDiagnosticsPath {
    param(
        [Parameter(Mandatory)]
        [string] $SessionStateDirectory
    )

    Join-Path $SessionStateDirectory 'focus-area-cache-diagnostics.json'
}

# Reads the record a restore left behind, or an empty record when there is none.
#
# A store extends the restore's record so the summary can state both what was reused and what was
# contributed back. A restore instead starts fresh: a session may restore more than once, and
# carrying fields forward from a superseded attempt would report a stale outcome alongside the
# current one.
function Get-FocusAreaCacheDiagnostics {
    param(
        [Parameter(Mandatory)]
        [string] $SessionStateDirectory
    )

    $path = Get-FocusAreaCacheDiagnosticsPath -SessionStateDirectory $SessionStateDirectory
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        return @{}
    }

    Read-PullRequestFileText -Path $path | ConvertFrom-Json -AsHashtable
}

# Publishes the record.
#
# Whole-record replacement and field updates use the same lock; store updates preserve restore
# fields read within that transaction. Ref: docs/implementation.md, "Shared workflow files".
function Write-FocusAreaCacheDiagnostics {
    param(
        [Parameter(Mandatory)]
        [string] $SessionStateDirectory,

        [Parameter(Mandatory)]
        [hashtable] $Diagnostics,

        [switch] $Merge
    )

    $path = Get-FocusAreaCacheDiagnosticsPath -SessionStateDirectory $SessionStateDirectory
    $lock = Open-PullRequestFileLock -Path $path
    try {
        if ($Merge) {
            $merged = Get-FocusAreaCacheDiagnostics -SessionStateDirectory $SessionStateDirectory
            foreach ($key in $Diagnostics.Keys) { $merged[$key] = $Diagnostics[$key] }
            $Diagnostics = $merged
        }
        Write-PullRequestOutputFileAtomically -Path $path -Content (ConvertTo-Json -InputObject $Diagnostics -Depth 4)
    } finally { $lock.Dispose() }
}
