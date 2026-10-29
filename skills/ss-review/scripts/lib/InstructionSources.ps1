#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Finds the configured corpus without importing a particular user's instruction names.
# Ref: docs/implementation.md, "Worker input boundary".
function Get-ReviewInstructionRoot {
    param(
        [AllowNull()][AllowEmptyString()][string] $DirectoryList,
        [Parameter(Mandatory)][string] $BaseDirectory
    )

    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in ($DirectoryList -split ',')) {
        $path = $entry.Trim()
        if ($path.Length -eq 0) { continue }
        $fullPath = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($path, $BaseDirectory))
        if (-not $seen.Add($fullPath)) { continue }
        $directory = Get-Item -LiteralPath $fullPath -Force
        if ($directory -isnot [IO.DirectoryInfo]) {
            throw "Configured instruction root is not a directory: $fullPath"
        }

        # A path-bound namespace keeps equally named files from different roots distinct,
        # and keeps source provenance in the cache key even when their bytes are identical.
        $identity = [Convert]::ToHexStringLower(
            [Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($fullPath)))
        [pscustomobject]@{
            Path = $fullPath
            Prefix = "external/$identity"
        }
    }
}

# Resolves linked ancestors as well as the final directory so session exclusions
# use filesystem-resolved paths without assuming case sensitivity from the OS.
function Resolve-ReviewInstructionDirectory {
    param([Parameter(Mandatory)][string] $Path, [switch] $AllowMissing)

    try { $directory = Get-Item -LiteralPath $Path -Force -ErrorAction Stop }
    catch [Management.Automation.ItemNotFoundException] {
        if (-not $AllowMissing) { throw }
        # Missing destinations are resolved only to classify excluded session aliases;
        # they never become instruction inputs.
        $missing = [IO.DirectoryInfo]::new($Path)
        if ($null -eq $missing.Parent) { throw }
        $parent = Resolve-ReviewInstructionDirectory -Path $missing.Parent.FullName -AllowMissing
        return [IO.DirectoryInfo]::new((Join-Path $parent.FullName $missing.Name))
    }
    if ($null -ne $directory.LinkTarget) {
        $target = $directory.ResolveLinkTarget($true)
        if ($null -eq $target -or (-not $target.Exists -and -not $AllowMissing)) {
            throw "Configured instruction directory has a missing link target: $Path"
        }
        return Resolve-ReviewInstructionDirectory -Path $target.FullName -AllowMissing:$AllowMissing
    }
    if ($directory -isnot [IO.DirectoryInfo]) {
        throw "Instruction directory is not a directory: $Path"
    }
    if ($null -eq $directory.Parent) { return $directory }
    $parent = Resolve-ReviewInstructionDirectory -Path $directory.Parent.FullName
    return Get-Item -LiteralPath (Join-Path $parent.FullName $directory.Name) -Force
}

# Walks linked instruction trees as well as ordinary directories. An ancestry check
# rejects cycles deterministically rather than hanging or silently omitting guidance.
function Get-ReviewInstructionFile {
    param(
        [Parameter(Mandatory)][string] $Root,
        [Parameter(Mandatory)][string] $ExcludedDirectory
    )

    $pending = [Collections.Generic.Stack[object]]::new()
    $pending.Push(@{ Path = (Resolve-ReviewInstructionDirectory -Path $Root).FullName; Relative = ''; Ancestors = @() })
    $excludedPrefix = $ExcludedDirectory.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    while ($pending.Count -gt 0) {
        $current = $pending.Pop()
        $directory = Get-Item -LiteralPath $current.Path -Force
        if ($null -ne $directory.LinkTarget) {
            $directory = Resolve-ReviewInstructionDirectory -Path $directory.FullName
        }
        if ($null -eq $directory -or -not $directory.Exists) {
            throw "Configured instruction directory has a missing link target: $($current.Path)"
        }
        $identity = [IO.Path]::TrimEndingDirectorySeparator($directory.FullName)
        # Generated instructions are outputs, never new user guidance. Check resolved
        # descendants too, because a link can enter below the session directory itself.
        if ($identity -ceq $ExcludedDirectory -or $identity.StartsWith($excludedPrefix, [StringComparison]::Ordinal)) {
            continue
        }
        if ($current.Ancestors -ccontains $identity) {
            throw "Configured instruction directory contains a link cycle: $($current.Path)"
        }
        $ancestors = @($current.Ancestors) + $identity
        foreach ($child in (Get-ChildItem -LiteralPath $directory.FullName -Force | Sort-Object -Property Name -CaseSensitive)) {
            $relative = if ($current.Relative) { "$($current.Relative)/$($child.Name)" } else { $child.Name }
            if ($null -ne $child.LinkTarget) {
                # A dangling directory symlink can appear as FileInfo. Resolve links before
                # filtering by type or filename, while excluding generated targets even after removal.
                $target = $child.ResolveLinkTarget($true)
                if ($null -eq $target) {
                    throw "Configured instruction entry has a missing link target: $($child.FullName)"
                }
                $targetParent = Resolve-ReviewInstructionDirectory -Path (Split-Path -Parent $target.FullName) -AllowMissing
                $targetPath = Join-Path $targetParent.FullName $target.Name
                if ($targetPath -ceq $ExcludedDirectory -or $targetPath.StartsWith($excludedPrefix, [StringComparison]::Ordinal)) {
                    continue
                }
                if (-not $target.Exists) {
                    throw "Configured instruction entry has a missing link target: $($child.FullName)"
                }
            }
            if ($child -is [IO.DirectoryInfo]) {
                $pending.Push(@{ Path = $child.FullName; Relative = $relative; Ancestors = $ancestors })
            } elseif ($child.Name -ceq 'AGENTS.md' -or $child.Name.EndsWith('.instructions.md', [StringComparison]::Ordinal)) {
                [pscustomobject]@{ Path = $child.FullName; Relative = $relative }
            }
        }
    }
}

# Copies bytes while allowing concurrent readers and atomic source replacement.
# This deliberately does not round-trip instruction text through the agent or an encoder.
function Copy-ReviewInstructionFile {
    param(
        [Parameter(Mandatory)][string] $Source,
        [Parameter(Mandatory)][string] $Destination
    )

    [void][IO.Directory]::CreateDirectory((Split-Path -Parent $Destination))
    $inputStream = [IO.File]::Open($Source, [IO.FileMode]::Open, [IO.FileAccess]::Read,
        [IO.FileShare]::Read -bor [IO.FileShare]::Delete)
    try {
        $outputStream = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $inputStream.CopyTo($outputStream) } finally { $outputStream.Dispose() }
    } finally { $inputStream.Dispose() }
}

# Publishes a complete fresh snapshot; the executable holds the same retained lock
# as cache operations, so no reader observes a partially replaced source corpus.
function Copy-ReviewInstructionSources {
    param(
        [Parameter(Mandatory)][string] $SessionStateDirectory,
        [Parameter(Mandatory)][string] $SkillRoot,
        [AllowNull()][AllowEmptyString()][string] $DirectoryList,
        [Parameter(Mandatory)][string] $BaseDirectory
    )

    $roots = @(Get-ReviewInstructionRoot -DirectoryList $DirectoryList -BaseDirectory $BaseDirectory)
    [void][IO.Directory]::CreateDirectory($SessionStateDirectory)
    $excluded = [IO.Path]::TrimEndingDirectorySeparator(
        (Resolve-ReviewInstructionDirectory -Path $SessionStateDirectory).FullName)
    # Finish discovery before creating staging: a configured root may contain session state.
    $discoveredRoots = @(foreach ($root in $roots) {
        @{ Root = $root; Files = @(Get-ReviewInstructionFile -Root $root.Path -ExcludedDirectory $excluded) }
    })
    $destination = Join-Path $SessionStateDirectory 'instruction-set-source'
    $staging = Join-Path $SessionStateDirectory ("instruction-set-source.$([guid]::NewGuid().ToString('N')).tmp")
    $backup = "$staging.previous"
    $published = $false
    [void][IO.Directory]::CreateDirectory($staging)
    try {
        Copy-ReviewInstructionFile -Source (Join-Path $SkillRoot 'focus-areas.md') `
            -Destination (Join-Path $staging 'skill\focus-areas.md')
        $total = 0
        $rootResults = @(foreach ($discovered in $discoveredRoots) {
            $root = $discovered.Root
            foreach ($file in $discovered.Files) {
                Copy-ReviewInstructionFile -Source $file.Path `
                    -Destination (Join-Path $staging "$($root.Prefix)/$($file.Relative)")
            }
            $total += $discovered.Files.Count
            [ordered]@{ path = $root.Path; prefix = $root.Prefix; fileCount = $discovered.Files.Count }
        })

        if (Test-Path -LiteralPath $destination) {
            $existing = Get-Item -LiteralPath $destination -Force
            if ($existing -isnot [IO.DirectoryInfo] -or $null -ne $existing.LinkTarget) {
                throw "Instruction snapshot must be an ordinary session-state directory: $destination"
            }
            [IO.Directory]::Move($destination, $backup)
        }
        try {
            [IO.Directory]::Move($staging, $destination)
            $published = $true
        } catch {
            if ([IO.Directory]::Exists($backup)) { [IO.Directory]::Move($backup, $destination) }
            throw
        }
        [ordered]@{
            schemaVersion = 1
            sourceDirectory = $destination
            roots = @($rootResults)
            externalFileCount = $total
        }
    } finally {
        if ([IO.Directory]::Exists($staging)) { Remove-Item -LiteralPath $staging -Recurse -Force }
        if ($published -and [IO.Directory]::Exists($backup)) { Remove-Item -LiteralPath $backup -Recurse -Force }
    }
}
