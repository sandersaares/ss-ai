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

# Walks linked instruction trees as well as ordinary directories. An ancestry check
# rejects cycles deterministically rather than hanging or silently omitting guidance.
function Get-ReviewInstructionFile {
    param([Parameter(Mandatory)][string] $Root)

    $pending = [Collections.Generic.Stack[object]]::new()
    $pending.Push(@{ Path = $Root; Relative = ''; Ancestors = @() })
    while ($pending.Count -gt 0) {
        $current = $pending.Pop()
        $directory = Get-Item -LiteralPath $current.Path -Force
        if ($null -ne $directory.LinkTarget) {
            $directory = $directory.ResolveLinkTarget($true)
        }
        if ($null -eq $directory -or -not $directory.Exists) {
            throw "Configured instruction directory has a missing link target: $($current.Path)"
        }
        $identity = [IO.Path]::TrimEndingDirectorySeparator($directory.FullName)
        if ($current.Ancestors -ccontains $identity) {
            throw "Configured instruction directory contains a link cycle: $($current.Path)"
        }
        $ancestors = @($current.Ancestors) + $identity
        foreach ($child in (Get-ChildItem -LiteralPath $directory.FullName -Force | Sort-Object -Property Name -CaseSensitive)) {
            $relative = if ($current.Relative) { "$($current.Relative)/$($child.Name)" } else { $child.Name }
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
    # Finish discovery before creating staging: a configured root may contain session state.
    $discoveredRoots = @(foreach ($root in $roots) {
        @{ Root = $root; Files = @(Get-ReviewInstructionFile -Root $root.Path) }
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
