#Requires -Version 7.6

# Focus-area cache library for the ss-review skill.
#
# Role in the overall design (see docs/design.md, "Focus-area reuse"): focus-area generation is
# expensive because it expands and categorizes the entire governing instruction corpus. That work
# is deterministic with respect to its instruction inputs, so equivalent inputs may reuse a
# previously generated artifact set. This library implements that reuse.
#
# The cache is explicitly NOT a relevance filter. It stores and restores the complete focus-area
# set, including section scope metadata, so Phase 2 accounts for every focus area and inventory file
# (docs/design.md, "Review coverage"). Nothing here may select, filter, sample, or omit focus areas.
#
# Cache reuse must be behaviorally transparent: a hit has to leave session state exactly as a
# generation pass would have left it, or it must not be treated as a hit at all. Every decision
# below favors correctness over hit rate, because a wrong hit silently corrupts a review while a
# miss only costs time.
#
# This is a library file: it is dot-sourced rather than executed, and defines only functions and
# constants. Because dot-sourcing runs in the caller's scope, it does impose Set-StrictMode and a
# terminating $ErrorActionPreference on its host - which the functions below rely on, since they
# treat any cmdlet failure during a copy or a hash as a failed operation rather than a partial one.
# Restore-FocusAreaCache.ps1, Save-FocusAreaCache.ps1, and Test-FocusAreaArtifacts.ps1 in the parent
# directory are the executable entry points agents call, one per cache operation.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# The generation contract includes executable behavior, not just the orchestration prompt.
# Ref: docs/implementation.md, "Cache inputs".
$script:FocusAreaGenerationRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$script:FocusAreaGenerationFiles = @(
    'scripts\Copy-ReviewInstructionSources.ps1'
    'scripts\lib\InstructionSources.ps1'
    'pwsh-calls\copy-instruction-sources.md'
    'focus-area-format.md'
    'schemas\focus-area-map.schema.json'
    'scripts\lib\FocusAreaPreparation.ps1'
    'scripts\lib\JsonValue.ps1'
    'scripts\lib\PullRequestModel.ps1'
    'scripts\Test-FocusAreaMap.ps1'
    'scripts\New-FocusAreaArtifacts.ps1'
    'pwsh-calls\validate-focus-area-map.md'
    'pwsh-calls\assemble-focus-areas.md'
    'workflow-data.md'
    'data-guides\focus-areas.md'
    'schemas\workflow-data.schema.json'
    'scripts\lib\WorkflowData.ps1'
    'scripts\lib\FocusAreaExpansion.ps1'
    'scripts\Update-FocusAreaExpansion.ps1'
    'scripts\Update-FocusAreaMap.ps1'
    'pwsh-calls\update-focus-area-expansion.md'
    'pwsh-calls\update-focus-area-map.md'
    'scripts\Save-FocusAreaCache.ps1'
)

# Participates in the cache key so that changing the entry layout or manifest shape cannot cause an
# old-format entry to be misread as a current-format one. Increment when either changes. Changes to
# manifest content rather than shape - the order records are written in, for instance - need no
# increment, because validation rehashes the payload and discards anything that no longer matches.
$script:FocusAreaCacheSchemaVersion = 1

# Domain separator in the key material. Prevents a hash computed here from ever colliding with a
# hash computed for some other purpose over similar bytes.
$script:FocusAreaCacheDomain = 'ss-review-focus-area-cache'

# Retention bounds. Payloads contain fully expanded guideline libraries, and a new key is minted
# every freshness window, so entries would otherwise accumulate without limit. Both an age bound
# and a count bound are applied: age discards abandoned instruction sets, count bounds worst-case
# growth for a user who reviews many differently-governed repositories.
$script:FocusAreaCacheRetentionDays = 30
$script:FocusAreaCacheMaximumEntries = 64

# Staging directories are published by rename, so a leftover one means a writer died mid-store.
# Correctness comes from the per-key lock rather than from this bound - collection takes the lock
# and skips a directory that a live writer still holds. The age bound only keeps an unlocked
# remnant from being collected the instant it appears.
$script:FocusAreaCacheStagingRetentionDays = 1

# Matches the staging directory names Save-FocusAreaCache creates: a leading dot, the 64-character
# entry key, the hyphenated GUID that makes concurrent attempts distinct, and a .tmp suffix. Kept
# beside the constants so it stays in step with the name construction it must recognize.
$script:FocusAreaCacheStagingNamePattern =
    '^\.[0-9a-f]{64}\.[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.tmp$'

# The complete set of generated artifacts the cache is responsible for, per docs/implementation.md
# ("Cached artifacts"). Restore and store both treat this as an all-or-nothing unit: partial state
# is never valid, because a focus-area set that disagrees with its category map or raw instruction
# set would silently change review behavior.
$script:FocusAreaArtifacts = @(
    [pscustomobject]@{ Path = 'instruction-set-raw'; Kind = 'Directory' }
    [pscustomobject]@{ Path = 'instruction-set-categories.md'; Kind = 'File' }
    [pscustomobject]@{ Path = 'focus-areas'; Kind = 'Directory' }
)

# Resolves the default cache location.
#
# Cache data stays under the user profile because generated guidance may contain repository-specific
# or user-specific instructions (docs/design.md, "Local data"). It must never be shared or committed.
# The trailing version segment lets an incompatible future layout coexist instead of colliding.
function Get-DefaultFocusAreaCacheDirectory {
    $homeDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
    Join-Path $homeDirectory '.copilot\cache\ss-review\focus-areas\v1'
}

# Maps a cache key to its lock file path.
#
# Locks live in a dedicated .locks subdirectory rather than inside entry directories so that a lock
# survives deletion of the entry it guards. That is what makes it safe for maintenance to delete an
# entry while another process is trying to read it.
function Get-FocusAreaCacheLockPath {
    param(
        [Parameter(Mandatory)]
        [string] $CacheDirectory,

        [Parameter(Mandatory)]
        [string] $Key
    )

    Join-Path $CacheDirectory ".locks\$Key.lock"
}

# Acquires an exclusive advisory lock. Always returns a result object: Lock holds the open handle
# on success and $null otherwise, with Reason naming why acquisition failed.
#
# The two failure reasons are kept apart because they call for opposite operator responses.
# CacheBusy means a peer session holds the lock; it is transient and the next review succeeds.
# CacheUnavailable means the cache tree could not be created, or the lock file could not be opened
# because of permissions or because something other than a lock file occupies its path - all of
# which persist until someone repairs the filesystem. Collapsing both into one reason would make a
# misconfigured cache look like ordinary contention in the diagnostics the review summary reports.
#
# Creating the lock's parent directory is part of the guarded region, and it is the only place the
# cache directory tree is created. That is what lets a review whose cache location cannot be
# created at all degrade to "no caching" rather than failing: callers translate any failure reason
# into a miss or bypass and generate normally.
#
# Concurrency model (docs/implementation.md, "Publication and restore"): multiple review sessions
# may run against the same cache directory at once. Exclusivity comes from FileShare::None, which
# the OS enforces and which is released even if the process is killed - no stale lock state to reap.
#
# Ordinary operations wait through temporary contention. Opportunistic maintenance uses NoWait
# so it never waits for a key held by a store that is itself requesting maintenance.
#
# Lock files are deliberately never deleted. Deleting them would introduce an unlink/recreate race
# in which two processes each hold an exclusive handle to a different inode at the same path and
# both believe they have exclusive access. The files are empty, so leaving them costs nothing.
function Enter-FocusAreaCacheLock {
    param(
        [Parameter(Mandatory)]
        [string] $LockPath,

        [switch] $NoWait
    )

    # Every way this can fail carries the same meaning - the cache tree cannot be brought into
    # existence - so the catch is deliberately untyped. Typed catches would let provider-level
    # failures such as an unmounted drive letter escape and fail the review, and the exception type
    # varies by cause, so enumerating them would be an open-ended list rather than a fixed one.
    try {
        New-Item -ItemType Directory -Path (Split-Path -Parent $LockPath) -Force | Out-Null
    } catch {
        return [pscustomobject]@{ Lock = $null; Reason = 'CacheUnavailable' }
    }

    try {
        $open = { [IO.File]::Open($LockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None) }
        $lock = if ($NoWait) { & $open } else { Invoke-PullRequestFileOperation -Path $LockPath -Operation $open }
        [pscustomobject]@{ Lock = $lock; Reason = $null }
    } catch [IO.DirectoryNotFoundException] {
        # Reached when a file occupies the cache tree rather than a directory: the creation above
        # silently no-ops in that case, so this is where the misconfiguration first becomes visible.
        # It must precede the IOException clause, which is its base type.
        [pscustomobject]@{ Lock = $null; Reason = 'CacheUnavailable' }
    } catch [IO.IOException] {
        $reason = if (Test-PullRequestFileContention -Exception $_.Exception) { 'CacheBusy' } else { 'CacheUnavailable' }
        [pscustomobject]@{ Lock = $null; Reason = $reason }
    } catch [UnauthorizedAccessException] {
        [pscustomobject]@{ Lock = $null; Reason = 'CacheUnavailable' }
    }
}

# Enforces the cache size and age bounds described in docs/implementation.md ("Publication and
# restore").
#
# Runs opportunistically after a store completes - whether it published a new entry or found an
# equivalent one - rather than as a scheduled job, because the skill has no daemon. Restore
# deliberately does not sweep: a review waits on restore, and a sweep would add unbounded latency to
# the path the cache exists to make fast. Maintenance is best-effort throughout: it holds its own
# lock so concurrent sessions do not duplicate the sweep, and it simply returns if another session
# is already sweeping.
#
# CurrentKey is excluded from removal so a session can never delete the entry it just wrote.
function Invoke-FocusAreaCacheMaintenance {
    param(
        [Parameter(Mandatory)]
        [string] $CacheDirectory,

        [Parameter(Mandatory)]
        [DateTimeOffset] $NowUtc,

        [string] $CurrentKey
    )

    $maintenanceLock = (Enter-FocusAreaCacheLock `
        -LockPath (Join-Path $CacheDirectory '.locks\maintenance.lock') -NoWait).Lock
    if (-not $maintenanceLock) {
        return
    }

    try {
        # Only hash-named directories are entries. The name pattern is the discriminator that keeps
        # maintenance away from .locks, staging directories, and anything else sharing the root.
        $entryRecords = @(
            foreach ($directory in (
                Get-ChildItem -LiteralPath $CacheDirectory -Directory -Force |
                    Where-Object { $_.Name -cmatch '^[0-9a-f]{64}$' }
            )) {
                # Age comes from the manifest rather than filesystem timestamps, which reads and
                # restores can perturb. An entry whose manifest cannot be read is dated to MinValue,
                # making it unconditionally eligible for removal: an unreadable manifest is
                # unusable anyway.
                $createdAt = [DateTimeOffset]::MinValue
                try {
                    $manifest = Get-Content `
                        -LiteralPath (Join-Path $directory.FullName 'manifest.json') `
                        -Raw |
                        ConvertFrom-Json
                    # ConvertFrom-Json already materializes ISO-8601 timestamps as [datetime], and
                    # round-tripping those through string parsing loses the offset. Handle both the
                    # converted and the raw-string shape.
                    if ($manifest.createdAtUtc -is [datetime]) {
                        $createdAt = [DateTimeOffset]$manifest.createdAtUtc
                    } else {
                        $createdAt = [DateTimeOffset]::Parse(
                            [string]$manifest.createdAtUtc,
                            [Globalization.CultureInfo]::InvariantCulture,
                            [Globalization.DateTimeStyles]::RoundtripKind
                        )
                    }
                } catch {
                    # Invalid entries are eligible for removal while holding their key lock.
                }

                [pscustomobject]@{
                    Directory = $directory
                    CreatedAt = $createdAt
                }
            }
        )

        # Age and count bounds are unioned through a hashtable keyed by path so an entry that
        # violates both is only removed once.
        $removalCandidates = @{}
        $retentionCutoff = $NowUtc.AddDays(-$script:FocusAreaCacheRetentionDays)
        foreach ($record in $entryRecords) {
            if ($record.CreatedAt -lt $retentionCutoff) {
                $removalCandidates[$record.Directory.FullName] = $record
            }
        }
        foreach ($record in @(
            $entryRecords |
                Sort-Object CreatedAt -Descending |
                Select-Object -Skip $script:FocusAreaCacheMaximumEntries
        )) {
            $removalCandidates[$record.Directory.FullName] = $record
        }

        foreach ($record in $removalCandidates.Values) {
            $key = $record.Directory.Name
            if ($key -ceq $CurrentKey) {
                continue
            }

            # Each removal takes that entry's own key lock, so an entry is never deleted out from
            # under a concurrent restore. A busy entry is skipped and collected on a later sweep.
            $keyLock = (Enter-FocusAreaCacheLock `
                -LockPath (Get-FocusAreaCacheLockPath -CacheDirectory $CacheDirectory -Key $key) -NoWait).Lock
            if (-not $keyLock) {
                continue
            }

            try {
                if (Test-Path -LiteralPath $record.Directory.FullName) {
                    Remove-Item -LiteralPath $record.Directory.FullName -Recurse -Force
                }
            } finally {
                $keyLock.Dispose()
            }
        }

        # Staging directories abandoned by a crashed writer. These are keyed by the entry they were
        # being staged for, so they are collected under that entry's lock for the same reason.
        # The pattern must track the name Save-FocusAreaCache builds, whose GUID segment is in
        # hyphenated form.
        $stagingCutoff = $NowUtc.UtcDateTime.AddDays(-$script:FocusAreaCacheStagingRetentionDays)
        foreach ($directory in (
            Get-ChildItem -LiteralPath $CacheDirectory -Directory -Force |
                Where-Object {
                    $_.Name -cmatch $script:FocusAreaCacheStagingNamePattern -and
                    $_.LastWriteTimeUtc -lt $stagingCutoff
                }
        )) {
            $directory.Name -cmatch '^\.(?<key>[0-9a-f]{64})\.' | Out-Null
            $key = $Matches.key
            if ($key -ceq $CurrentKey) {
                continue
            }

            $keyLock = (Enter-FocusAreaCacheLock `
                -LockPath (Get-FocusAreaCacheLockPath -CacheDirectory $CacheDirectory -Key $key) -NoWait).Lock
            if (-not $keyLock) {
                continue
            }

            try {
                if (Test-Path -LiteralPath $directory.FullName) {
                    Remove-Item -LiteralPath $directory.FullName -Recurse -Force
                }
            } finally {
                $keyLock.Dispose()
            }
        }
    } finally {
        $maintenanceLock.Dispose()
    }
}

# Produces the stable relative path used in manifests, and refuses paths that escape their root.
#
# Separators are normalized to forward slashes so a manifest written on one platform compares
# equal on another; the path string is part of the hashed key material, so an unnormalized
# separator would present as a content change.
#
# The containment check is a safety boundary for record derivation: enumeration must never yield a
# record whose path leaves the hashed root, because such a record would misdescribe the tree in both
# the cache key and the manifest.
function Get-NormalizedRelativePath {
    param(
        [Parameter(Mandatory)]
        [string] $BasePath,

        [Parameter(Mandatory)]
        [string] $Path
    )

    $relativePath = [IO.Path]::GetRelativePath(
        [IO.Path]::GetFullPath($BasePath),
        [IO.Path]::GetFullPath($Path)
    ).Replace('\', '/')

    if ([IO.Path]::IsPathRooted($relativePath) -or
        $relativePath -eq '..' -or
        $relativePath.StartsWith('../', [StringComparison]::Ordinal)) {
        throw "Path '$Path' is outside '$BasePath'."
    }

    $relativePath
}

# Rejects symlinks and junctions anywhere in a tree.
#
# Reparse points break the cache's central assumption that a hashed file set fully describes the
# tree: the target can change without any change under the hashed root, so a stale entry would
# validate. They also let a copy escape its root. Rather than resolve and hash link targets, the
# cache refuses them outright - they have no legitimate role in generated focus-area artifacts, so
# rejection costs nothing and removes the whole class of problems.
function Assert-NoReparsePoints {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $root = Get-Item -LiteralPath $Path -Force
    if ($root.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
        throw "Reparse points are not supported in focus-area cache inputs or artifacts: '$Path'."
    }

    foreach ($item in (Get-ChildItem -LiteralPath $Path -Recurse -Force)) {
        if ($item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
            throw "Reparse points are not supported in focus-area cache inputs or artifacts: '$($item.FullName)'."
        }
    }
}

# Orders file records by ordinal path comparison.
#
# Record order is hashed into the cache key and is compared positionally by Test-FileRecordEquality,
# so it must depend on nothing but the path characters. PowerShell's `Sort-Object -CaseSensitive` is
# linguistic and therefore culture-sensitive: under it, identical inputs key differently on machines
# with different cultures, and a manifest written under one culture appears to disagree with a
# byte-identical tree read under another.
function Sort-FileRecord {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Record
    )

    $ordered = [object[]]$Record.Clone()
    [Array]::Sort(
        $ordered,
        [Comparison[object]] { param($left, $right) [string]::CompareOrdinal($left.path, $right.path) }
    )
    @($ordered)
}

# Describes a directory tree as an ordered list of (path, length, hash) records.
#
# This is the record shape the whole cache reasons about: it is hashed into the key over inputs and
# compared field by field to verify artifacts. Length participates in both - it is part of the key
# material and a hard equality term - so a length-only change is detected even in the theoretical
# case of a digest collision.
#
# Get-FocusAreaArtifactRecords produces the same shape for artifact trees but does not call this
# function, because it must enumerate a fixed artifact list rather than a whole tree. Changes to the
# record shape must be applied to both.
function Get-FileRecords {
    param(
        [Parameter(Mandatory)]
        [string] $RootPath
    )

    if (-not (Test-Path -LiteralPath $RootPath -PathType Container)) {
        throw "Directory does not exist: '$RootPath'."
    }

    Assert-NoReparsePoints -Path $RootPath

    $records = @(
        foreach ($file in (Get-ChildItem -LiteralPath $RootPath -Recurse -File -Force)) {
            [pscustomobject]@{
                path = Get-NormalizedRelativePath -BasePath $RootPath -Path $file.FullName
                length = $file.Length
                sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
    )

    Sort-FileRecord -Record $records
}

# Derives the cache key and the input record set recorded in the manifest.
#
# The key is a hash over everything that can change the generated output (docs/implementation.md,
# "Cache inputs"):
#   - the schema version, so entry-format changes partition the cache;
#   - the generator profile, so output from one model is never served to another;
#   - the recipe and generation tools, so edits invalidate everything they produced;
#   - every source instruction file's path, length, and content hash;
#   - a coarse time bucket.
#
# Hashing full file content rather than paths or timestamps is what makes reuse safe: an entry can
# only be served when the instruction corpus is byte-identical to the one that produced it. This is
# also why the caller must materialize inputs verbatim before calling - a summarized input directory
# would key differently on every run and never hit.
#
# Volatile per-review state (the reviewed diff, discussion, session identifiers) is deliberately
# absent from the key, since focus areas depend only on the governing instructions. Including it
# would reduce the hit rate to zero.
function Get-FocusAreaCacheDescriptor {
    param(
        [Parameter(Mandatory)]
        [string] $InputDirectory,

        [Parameter(Mandatory)]
        [string] $RecipePath,

        [Parameter(Mandatory)]
        [string] $GeneratorProfile,

        [Parameter(Mandatory)]
        [ValidateRange(1, 3650)]
        [int] $FreshnessDays,

        [Parameter(Mandatory)]
        [DateTimeOffset] $NowUtc
    )

    if (-not (Test-Path -LiteralPath $RecipePath -PathType Leaf)) {
        throw "Focus-area generation recipe does not exist: '$RecipePath'."
    }

    # An empty input set would key identically for every repository, so it is treated as a caller
    # error rather than as a cacheable state.
    $inputRecords = @(Get-FileRecords -RootPath $InputDirectory)
    if ($inputRecords.Count -eq 0) {
        throw "Focus-area cache inputs are empty: '$InputDirectory'."
    }

    $recipe = Get-Item -LiteralPath $RecipePath
    if ($recipe.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
        throw "The focus-area generation recipe must not be a reparse point: '$RecipePath'."
    }

    # The recipe is folded in under a synthetic path prefix so it is distinguishable from the
    # materialized input files rather than appearing as one of them.
    $recipeRecord = [pscustomobject]@{
        path = '__recipe__/prepare-focus-areas.md'
        length = $recipe.Length
        sha256 = (Get-FileHash -LiteralPath $recipe.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $generationRecords = @(
        $recipeRecord
        foreach ($relativePath in $script:FocusAreaGenerationFiles) {
            $file = Get-Item -LiteralPath (Join-Path $script:FocusAreaGenerationRoot $relativePath)
            if ($file.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
                throw "Focus-area generation inputs must not be links: '$($file.FullName)'."
            }
            [pscustomobject]@{
                path = '__generation__/' + $relativePath.Replace('\', '/')
                length = $file.Length
                sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
    )

    # The time bucket periodically invalidates entries whose source documents still contain
    # unchanged URLs, allowing external guidance to be refreshed without network access here.
    # Bucketing rather than storing a per-entry expiry means all sessions within a window agree on
    # the key, so crossing a boundary costs a regeneration rather than staggering expiry per session.
    $windowSeconds = [int64]$FreshnessDays * 24 * 60 * 60
    $freshnessBucket = [Math]::Floor($NowUtc.ToUnixTimeSeconds() / $windowSeconds)

    # Records are joined with a NUL field separator so a path can never be confused with the length
    # or digest that follow it.
    $keyLines = [Collections.Generic.List[string]]::new()
    $keyLines.Add($script:FocusAreaCacheDomain)
    $keyLines.Add("schema=$script:FocusAreaCacheSchemaVersion")
    $keyLines.Add("generator=$GeneratorProfile")
    $keyLines.Add("freshnessBucket=$freshnessBucket")
    foreach ($record in @($inputRecords) + $generationRecords) {
        $keyLines.Add("$($record.path)`0$($record.length)`0$($record.sha256)")
    }

    $keyBytes = [Text.Encoding]::UTF8.GetBytes(($keyLines -join "`n"))
    $key = [Convert]::ToHexString(
        [Security.Cryptography.SHA256]::HashData($keyBytes)
    ).ToLowerInvariant()

    [pscustomobject]@{
        Key = $key
        FreshnessBucket = $freshnessBucket
        Inputs = @($inputRecords) + $generationRecords
    }
}

# Validates that a directory holds a well-formed artifact set and describes it as file records.
#
# Structural validation is enforced here rather than only at store time because this is the single
# chokepoint every artifact set passes through - on store, on restore staging, and on standalone
# validation. Centralizing it means a malformed set cannot enter the cache and equally cannot leave
# it unnoticed.
#
# The checks encode what "usable focus areas" means in Phase 2: the raw instruction set must be
# non-empty, the category map must exist and be non-empty, and focus-areas must contain only
# non-empty Markdown files. An empty focus-area file would silently produce a content-free review
# pass, which is worse than a cache miss.
function Get-FocusAreaArtifactRecords {
    param(
        [Parameter(Mandatory)]
        [string] $RootPath
    )

    $rawPath = Join-Path $RootPath 'instruction-set-raw'
    $categoriesPath = Join-Path $RootPath 'instruction-set-categories.md'
    $focusAreasPath = Join-Path $RootPath 'focus-areas'

    if (-not (Test-Path -LiteralPath $rawPath -PathType Container)) {
        throw "Missing instruction-set-raw directory under '$RootPath'."
    }
    if (-not (Get-ChildItem -LiteralPath $rawPath -Recurse -File -Force | Select-Object -First 1)) {
        throw "The instruction-set-raw directory is empty under '$RootPath'."
    }
    if (-not (Test-Path -LiteralPath $categoriesPath -PathType Leaf) -or
        (Get-Item -LiteralPath $categoriesPath).Length -eq 0) {
        throw "Missing or empty instruction-set-categories.md under '$RootPath'."
    }
    if (-not (Test-Path -LiteralPath $focusAreasPath -PathType Container)) {
        throw "Missing focus-areas directory under '$RootPath'."
    }

    $focusAreaFiles = @(Get-ChildItem -LiteralPath $focusAreasPath -File -Force)
    if ($focusAreaFiles.Count -eq 0) {
        throw "The focus-areas directory is empty under '$RootPath'."
    }
    foreach ($file in $focusAreaFiles) {
        if ($file.Extension -ne '.md') {
            throw "Unexpected file in focus-areas: '$($file.FullName)'."
        }
        if ([string]::IsNullOrWhiteSpace((Read-PullRequestFileText -Path ($file.FullName)))) {
            throw "Focus-area file is empty: '$($file.FullName)'."
        }
    }

    foreach ($artifact in $script:FocusAreaArtifacts) {
        Assert-NoReparsePoints -Path (Join-Path $RootPath $artifact.Path)
    }

    # Paths are recorded relative to the artifact root, so the same record set describes the payload
    # inside a cache entry and the materialized artifacts in session state. That equivalence is what
    # lets a restore be verified against the manifest written at store time.
    #
    # Enumeration is recursive while the focus-area checks above are not, so a nested file under
    # focus-areas/ would be hashed and reproduced faithfully but would not be counted as a focus
    # area. Generation produces a flat focus-areas/ directory, so this asymmetry is unreachable in
    # practice; it is recorded here because the two loops must stay consistent if that changes.
    $records = @(
        foreach ($artifact in $script:FocusAreaArtifacts) {
            $artifactPath = Join-Path $RootPath $artifact.Path
            if ($artifact.Kind -eq 'File') {
                $file = Get-Item -LiteralPath $artifactPath
                [pscustomobject]@{
                    path = $artifact.Path.Replace('\', '/')
                    length = $file.Length
                    sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                }
                continue
            }

            foreach ($file in (Get-ChildItem -LiteralPath $artifactPath -Recurse -File -Force)) {
                $relativePath = Get-NormalizedRelativePath -BasePath $RootPath -Path $file.FullName
                [pscustomobject]@{
                    path = $relativePath
                    length = $file.Length
                    sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                }
            }
        }
    )

    Sort-FileRecord -Record $records
}

# Compares two record sets for exact equality.
#
# Both sides are ordered by the same ordinal rule, so a positional walk is sufficient and detects
# added, removed, and modified files alike. Path and digest are compared ordinally to match that
# ordering: PowerShell's -ceq is linguistic, and treats characters such as soft hyphens and
# zero-width joiners as ignorable, so it would accept a tree that is not actually identical.
function Test-FileRecordEquality {
    param(
        [Parameter(Mandatory)]
        [object[]] $Expected,

        [Parameter(Mandatory)]
        [object[]] $Actual
    )

    if ($Expected.Count -ne $Actual.Count) {
        return $false
    }

    for ($index = 0; $index -lt $Expected.Count; $index++) {
        if (-not [string]::Equals(
                [string]$Expected[$index].path,
                [string]$Actual[$index].path,
                [StringComparison]::Ordinal
            ) -or
            [int64]$Expected[$index].length -ne [int64]$Actual[$index].length -or
            -not [string]::Equals(
                [string]$Expected[$index].sha256,
                [string]$Actual[$index].sha256,
                [StringComparison]::Ordinal
            )) {
            return $false
        }
    }

    $true
}

# Returns a usable cache entry, or $null when the entry is absent, stale, or damaged.
#
# An entry is only usable when its manifest matches the current schema, its recorded key matches the
# key just derived, and its payload still hashes to the outputs the manifest declares. The payload
# is re-verified on every read rather than trusted, because cache directories live in a user-writable
# location and may be modified or truncated between store and read.
#
# All failure modes collapse to $null so the caller treats damage exactly like absence: regenerate.
# Throwing instead would turn recoverable cache damage into a failed review.
function Get-ValidFocusAreaCacheEntry {
    param(
        [Parameter(Mandatory)]
        [string] $EntryPath,

        [Parameter(Mandatory)]
        [string] $ExpectedKey
    )

    $manifestPath = Join-Path $EntryPath 'manifest.json'
    $payloadPath = Join-Path $EntryPath 'payload'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf) -or
        -not (Test-Path -LiteralPath $payloadPath -PathType Container)) {
        return $null
    }

    try {
        $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
        if ([int]$manifest.schemaVersion -ne $script:FocusAreaCacheSchemaVersion -or
            -not [string]::Equals(
                [string]$manifest.key,
                $ExpectedKey,
                [StringComparison]::Ordinal
            )) {
            return $null
        }

        $actualOutputs = @(Get-FocusAreaArtifactRecords -RootPath $payloadPath)
        if (-not (Test-FileRecordEquality -Expected @($manifest.outputs) -Actual $actualOutputs)) {
            return $null
        }

        [pscustomobject]@{
            Manifest = $manifest
            PayloadPath = $payloadPath
        }
    } catch {
        $null
    }
}

# Copies the artifact set between two roots.
#
# Copying only the declared artifacts, rather than mirroring whole directories, keeps unrelated
# session-state files out of cache payloads and keeps unrelated cache-entry files (the manifest) out
# of session state.
function Copy-FocusAreaArtifacts {
    param(
        [Parameter(Mandatory)]
        [string] $SourceRoot,

        [Parameter(Mandatory)]
        [string] $DestinationRoot
    )

    New-Item -ItemType Directory -Path $DestinationRoot -Force | Out-Null
    foreach ($artifact in $script:FocusAreaArtifacts) {
        Copy-Item -LiteralPath (Join-Path $SourceRoot $artifact.Path) -Destination $DestinationRoot -Recurse
    }
}

# Materializes a cached artifact set into session state. One of the three operations the skill
# invokes (Restore, Store, Validate).
#
# Never throws for an ordinary miss. Every non-hit path returns a structured result carrying a
# machine-readable reason, because the caller's correct response to any miss is identical - generate
# normally - while the reason feeds the diagnostics the review summary must report
# (docs/implementation.md, "Publication and restore"). Reasons distinguish RefreshRequested,
# CacheBusy, CacheUnavailable, SessionBusy, NotFound, and InvalidEntry so an operator can tell a
# cold cache from a contended, misconfigured, or damaged one.
#
# Restore aims to leave session state holding either the complete cached artifact set or the
# artifacts it had before, since a mixed set would blend two generations and produce a review with
# inconsistent focus areas. When installation fails, the prior artifacts are rolled back; when the
# rollback itself cannot complete, the backup directory is retained and its path is reported in the
# error so recovery remains possible by hand.
function Restore-FocusAreaCache {
    param(
        [Parameter(Mandatory)]
        [string] $SessionStateDirectory,

        [Parameter(Mandatory)]
        [string] $InputDirectory,

        [Parameter(Mandatory)]
        [string] $RecipePath,

        [Parameter(Mandatory)]
        [string] $CacheDirectory,

        [Parameter(Mandatory)]
        [string] $GeneratorProfile,

        [Parameter(Mandatory)]
        [int] $FreshnessDays,

        [Parameter(Mandatory)]
        [DateTimeOffset] $NowUtc,

        [switch] $Refresh
    )

    $descriptor = Get-FocusAreaCacheDescriptor `
        -InputDirectory $InputDirectory `
        -RecipePath $RecipePath `
        -GeneratorProfile $GeneratorProfile `
        -FreshnessDays $FreshnessDays `
        -NowUtc $NowUtc
    $entryPath = Join-Path $CacheDirectory $descriptor.Key

    # Refresh suppresses only the read, forcing this session to regenerate. The key is still reported
    # so the session stays keyed with everyone else; note that the subsequent store finds the
    # existing entry valid and leaves it in place, so refresh regenerates for the current review
    # rather than replacing what is cached. It exists for an explicit user request to regenerate.
    if ($Refresh) {
        return [pscustomobject]@{
            operation = 'Restore'
            status = 'Miss'
            reason = 'RefreshRequested'
            key = $descriptor.Key
            focusAreaCount = 0
        }
    }

    $cacheLockPath = Get-FocusAreaCacheLockPath `
        -CacheDirectory $CacheDirectory `
        -Key $descriptor.Key
    $cacheLockResult = Enter-FocusAreaCacheLock -LockPath $cacheLockPath
    $cacheLock = $cacheLockResult.Lock
    if (-not $cacheLock) {
        return [pscustomobject]@{
            operation = 'Restore'
            status = 'Miss'
            reason = $cacheLockResult.Reason
            key = $descriptor.Key
            focusAreaCount = 0
        }
    }

    try {
        $entry = Get-ValidFocusAreaCacheEntry -EntryPath $entryPath -ExpectedKey $descriptor.Key
        if (-not $entry) {
            return [pscustomobject]@{
                operation = 'Restore'
                status = 'Miss'
                reason = if (Test-Path -LiteralPath $entryPath) { 'InvalidEntry' } else { 'NotFound' }
                key = $descriptor.Key
                focusAreaCount = 0
            }
        }

        # Cache key before session artifacts is the common store/restore lock order.
        # Assembly and validation use the same artifact-set lock without taking a cache key.
        $sessionLock = Open-PullRequestFileLock -Path (Join-Path $SessionStateDirectory 'focus-areas')

        try {
            # The swap is staged so that the slow, failure-prone part (copying out of the cache) is
            # complete and verified before any existing session state is disturbed. Both scratch
            # directories carry a GUID so concurrent or previously abandoned attempts cannot collide.
            $stagingPath = Join-Path $SessionStateDirectory ".focus-area-cache-restore-$([guid]::NewGuid())"
            $backupPath = Join-Path $SessionStateDirectory ".focus-area-cache-backup-$([guid]::NewGuid())"
            $installedArtifacts = [Collections.Generic.List[string]]::new()
            $preserveBackup = $false

            try {
                Copy-FocusAreaArtifacts -SourceRoot $entry.PayloadPath -DestinationRoot $stagingPath

                # The staged copy is rehashed against the manifest rather than trusting the earlier
                # verification of the payload. This is what catches a truncated or partial copy, and
                # it is the last point at which a bad restore can still be downgraded to a plain
                # miss instead of corrupting session state.
                $stagedOutputs = @(Get-FocusAreaArtifactRecords -RootPath $stagingPath)
                if (-not (
                    Test-FileRecordEquality `
                        -Expected @($entry.Manifest.outputs) `
                        -Actual $stagedOutputs
                )) {
                    return [pscustomobject]@{
                        operation = 'Restore'
                        status = 'Miss'
                        reason = 'InvalidEntry'
                        key = $descriptor.Key
                        focusAreaCount = 0
                    }
                }
                New-Item -ItemType Directory -Path $backupPath | Out-Null

                # Existing artifacts are moved aside rather than deleted, so the previous state
                # remains recoverable until the new set is fully installed.
                foreach ($artifact in $script:FocusAreaArtifacts) {
                    $targetPath = Join-Path $SessionStateDirectory $artifact.Path
                    if (Test-Path -LiteralPath $targetPath) {
                        Move-Item -LiteralPath $targetPath -Destination $backupPath
                    }
                }

                # Installation is a sequence of moves within one volume, keeping the window in which
                # session state is incomplete as short as the filesystem allows.
                foreach ($artifact in $script:FocusAreaArtifacts) {
                    $stagedPath = Join-Path $stagingPath $artifact.Path
                    Move-Item -LiteralPath $stagedPath -Destination $SessionStateDirectory
                    $installedArtifacts.Add($artifact.Path)
                }
            } catch {
                # Rollback removes whatever was installed and returns the backed-up artifacts.
                # Failures are collected rather than rethrown immediately so that one unrecoverable
                # artifact does not abandon the remaining recoverable ones.
                $restoreError = $_.Exception
                $rollbackErrors = [Collections.Generic.List[string]]::new()

                foreach ($artifactPath in $installedArtifacts) {
                    $installedPath = Join-Path $SessionStateDirectory $artifactPath
                    if (Test-Path -LiteralPath $installedPath) {
                        try {
                            Remove-Item -LiteralPath $installedPath -Recurse -Force
                        } catch {
                            $rollbackErrors.Add($_.Exception.Message)
                        }
                    }
                }

                foreach ($artifact in $script:FocusAreaArtifacts) {
                    $backupArtifact = Join-Path $backupPath $artifact.Path
                    if (Test-Path -LiteralPath $backupArtifact) {
                        try {
                            Move-Item -LiteralPath $backupArtifact -Destination $SessionStateDirectory
                        } catch {
                            $rollbackErrors.Add($_.Exception.Message)
                        }
                    }
                }

                # When rollback itself failed, session state is in an unknown condition. The backup
                # is kept and its path surfaced in the error so the original artifacts remain
                # recoverable by hand; deleting it would be the only truly unrecoverable outcome.
                if ($rollbackErrors.Count -gt 0) {
                    $preserveBackup = $true
                    throw "Focus-area restore failed: $($restoreError.Message) Rollback was incomplete. Backup retained at '$backupPath'. $($rollbackErrors -join ' ')"
                }

                throw $restoreError
            } finally {
                if (Test-Path -LiteralPath $stagingPath) {
                    Remove-Item -LiteralPath $stagingPath -Recurse -Force
                }
                if (-not $preserveBackup -and (Test-Path -LiteralPath $backupPath)) {
                    Remove-Item -LiteralPath $backupPath -Recurse -Force
                }
            }

            # Counted from what actually landed in session state, not from the manifest, so the
            # number reported in diagnostics reflects observed reality.
            $focusAreaCount = @(
                Get-ChildItem `
                    -LiteralPath (Join-Path $SessionStateDirectory 'focus-areas') `
                    -Filter '*.md' `
                    -File
            ).Count

            [pscustomobject]@{
                operation = 'Restore'
                status = 'Hit'
                reason = $null
                key = $descriptor.Key
                focusAreaCount = $focusAreaCount
            }
        } finally {
            $sessionLock.Dispose()
        }
    } finally {
        $cacheLock.Dispose()
    }
}

# Publishes the artifact set currently in session state as a cache entry.
#
# Cache-level conditions - a busy lock, degraded inputs - produce a Bypassed status rather than an
# error, because a review that produced good focus areas must not be failed by an inability to
# memoize them. Two conditions are deliberately raised as errors instead, because both indicate the
# caller's artifacts are not in the state it believes: a structurally invalid session artifact set,
# and artifacts changing underneath an in-progress store.
function Save-FocusAreaCache {
    param(
        [Parameter(Mandatory)]
        [string] $SessionStateDirectory,

        [Parameter(Mandatory)]
        [string] $InputDirectory,

        [Parameter(Mandatory)]
        [string] $RecipePath,

        [Parameter(Mandatory)]
        [string] $CacheDirectory,

        [Parameter(Mandatory)]
        [string] $GeneratorProfile,

        [Parameter(Mandatory)]
        [int] $FreshnessDays,

        [Parameter(Mandatory)]
        [DateTimeOffset] $NowUtc,

        [Parameter(Mandatory)]
        [ValidateSet('Complete', 'Degraded')]
        [string] $InputState
    )

    $descriptor = Get-FocusAreaCacheDescriptor `
        -InputDirectory $InputDirectory `
        -RecipePath $RecipePath `
        -GeneratorProfile $GeneratorProfile `
        -FreshnessDays $FreshnessDays `
        -NowUtc $NowUtc
    $sessionLock = Open-PullRequestFileLock -Path (Join-Path $SessionStateDirectory 'focus-areas')
    try {
        $focusAreaCount = @(
            Get-ChildItem -LiteralPath (Join-Path $SessionStateDirectory 'focus-areas') -Filter '*.md' -File
        ).Count
    } finally { $sessionLock.Dispose() }

    # Degraded means the generator could not obtain every input it wanted (typically an unreachable
    # referenced document). The resulting focus areas are good enough for the current review but must
    # not be persisted: the key describes only the inputs that were available, so a degraded entry
    # would be served to later sessions that could have fetched everything. Failing closed here
    # costs one regeneration; failing open would silently degrade every subsequent review sharing
    # the key.
    if ($InputState -eq 'Degraded') {
        return [pscustomobject]@{
            operation = 'Store'
            status = 'Bypassed'
            reason = 'DegradedInputs'
            key = $descriptor.Key
            focusAreaCount = $focusAreaCount
        }
    }

    $cacheLockPath = Get-FocusAreaCacheLockPath `
        -CacheDirectory $CacheDirectory `
        -Key $descriptor.Key
    $cacheLockResult = Enter-FocusAreaCacheLock -LockPath $cacheLockPath
    $cacheLock = $cacheLockResult.Lock
    if (-not $cacheLock) {
        return [pscustomobject]@{
            operation = 'Store'
            status = 'Bypassed'
            reason = $cacheLockResult.Reason
            key = $descriptor.Key
            focusAreaCount = $focusAreaCount
        }
    }

    try {
        $sessionLock = Open-PullRequestFileLock -Path (Join-Path $SessionStateDirectory 'focus-areas')
        try {
            # Keep the source set stable through hashing and copying, using restore's lock order.
            $sessionOutputs = @(Get-FocusAreaArtifactRecords -RootPath $SessionStateDirectory)
            $focusAreaCount = @(Get-ChildItem -LiteralPath (Join-Path $SessionStateDirectory 'focus-areas') -Filter '*.md' -File).Count
            $entryPath = Join-Path $CacheDirectory $descriptor.Key

            # An entry that is already valid for this key describes the same inputs, so it is left
            # untouched and reported as Existing; entries are immutable once published. Maintenance
            # still runs, since this session's activity is a fresh opportunity to enforce the bounds.
            if (Get-ValidFocusAreaCacheEntry -EntryPath $entryPath -ExpectedKey $descriptor.Key) {
                Invoke-FocusAreaCacheMaintenance `
                    -CacheDirectory $CacheDirectory `
                    -NowUtc $NowUtc `
                    -CurrentKey $descriptor.Key
                return [pscustomobject]@{
                    operation = 'Store'
                    status = 'Existing'
                    reason = $null
                    key = $descriptor.Key
                    focusAreaCount = $focusAreaCount
                }
            }

            # Anything present at this point is an entry that failed validation, so it is discarded
            # rather than repaired. Staging directories for this key are also cleared: they can only be
            # remnants of a failed earlier attempt, since the lock excludes any live writer.
            if (Test-Path -LiteralPath $entryPath) {
                Remove-Item -LiteralPath $entryPath -Recurse -Force
            }
            foreach ($abandonedStagingPath in (
                Get-ChildItem `
                    -LiteralPath $CacheDirectory `
                    -Directory `
                    -Filter ".$($descriptor.Key).*.tmp" `
                    -Force
            )) {
                Remove-Item -LiteralPath $abandonedStagingPath.FullName -Recurse -Force
            }

            # The entry is assembled under a hidden staging name and published with a single directory
            # rename. Readers therefore only ever observe an entry that is complete, without needing to
            # take a lock to avoid seeing a half-written one.
            $stagingPath = Join-Path $CacheDirectory ".$($descriptor.Key).$([guid]::NewGuid()).tmp"
            try {
                New-Item -ItemType Directory -Path $stagingPath | Out-Null
                $payloadPath = Join-Path $stagingPath 'payload'
                Copy-FocusAreaArtifacts -SourceRoot $SessionStateDirectory -DestinationRoot $payloadPath
                $cachedOutputs = @(Get-FocusAreaArtifactRecords -RootPath $payloadPath)
                if (-not (Test-FileRecordEquality -Expected $sessionOutputs -Actual $cachedOutputs)) {
                    throw 'Focus-area artifacts changed while the cache entry was being staged.'
                }

                # The manifest records the resolved inputs as well as the outputs. The inputs are not
                # needed to serve a hit - the key already encodes them - but they make a cached entry
                # diagnosable, answering "which instruction set produced this?" without re-deriving it.
                $manifest = [ordered]@{
                    schemaVersion = $script:FocusAreaCacheSchemaVersion
                    key = $descriptor.Key
                    generatorProfile = $GeneratorProfile
                    freshnessBucket = $descriptor.FreshnessBucket
                    createdAtUtc = $NowUtc.ToUniversalTime().ToString('O')
                    inputs = $descriptor.Inputs
                    outputs = $cachedOutputs
                }
                $manifest | ConvertTo-Json -Depth 6 |
                    Set-Content -LiteralPath (Join-Path $stagingPath 'manifest.json') -Encoding utf8

                [IO.Directory]::Move($stagingPath, $entryPath)
            } finally {
                # Reached only when publication failed, since a successful move leaves nothing behind.
                if (Test-Path -LiteralPath $stagingPath) {
                    Remove-Item -LiteralPath $stagingPath -Recurse -Force
                }
            }

            Invoke-FocusAreaCacheMaintenance `
                -CacheDirectory $CacheDirectory `
                -NowUtc $NowUtc `
                -CurrentKey $descriptor.Key

            [pscustomobject]@{
                operation = 'Store'
                status = 'Stored'
                reason = $null
                key = $descriptor.Key
                focusAreaCount = $focusAreaCount
            }
        } finally { $sessionLock.Dispose() }
    } finally {
        $cacheLock.Dispose()
    }
}

# Confirms that session state holds a structurally complete artifact set, independent of the cache.
#
# prepare-focus-areas.md calls this at two points: after a restore hit, to confirm the reused
# artifacts survived, and after generation, to confirm the new artifacts are fit to store. It
# touches no cache state and derives no key, which is why the key field is null: the question it
# answers is "are these artifacts usable?", not "where did they come from?".
#
# Unlike Restore and Store, this throws on invalid artifacts. The caller's documented recovery
# differs by stage - discard and regenerate after a hit, correct and revalidate after generation -
# so the failure is surfaced as an error rather than folded into a status the caller might ignore.
function Test-FocusAreaArtifacts {
    param(
        [Parameter(Mandatory)]
        [string] $SessionStateDirectory
    )

    $transactionLock = Open-PullRequestFileLock -Path (Join-Path $SessionStateDirectory 'focus-areas')
    try {
        $records = @(Get-FocusAreaArtifactRecords -RootPath $SessionStateDirectory)
        [pscustomobject]@{
            operation = 'Validate'
            status = 'Valid'
            reason = $null
            key = $null
            focusAreaCount = @(
                Get-ChildItem -LiteralPath (Join-Path $SessionStateDirectory 'focus-areas') -Filter '*.md' -File
            ).Count
            artifactFileCount = $records.Count
        }
    } finally { $transactionLock.Dispose() }
}
