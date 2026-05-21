#requires -Version 7.0
<#
.SYNOPSIS
    Program-grader script. Verifies the agent did not modify any source
    files in the per-task workspace — only Cargo.toml / Cargo.lock are
    allowed to differ from the upstream cache.

.DESCRIPTION
    Invoked by waza's `program` grader. Reads `WAZA_WORKSPACE_DIR` from
    the environment and the cache root from `-CacheDir`. Hashes every
    file in both trees (excluding Cargo.toml, Cargo.lock, .git, target),
    compares, and exits 0 only if every non-manifest file is byte-
    identical.

    Mismatches are printed to stderr (waza captures it in grader notes).
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$CacheDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$workspace = $env:WAZA_WORKSPACE_DIR
if (-not $workspace) { Write-Error 'WAZA_WORKSPACE_DIR not set'; exit 2 }
if (-not (Test-Path $workspace)) { Write-Error "workspace '$workspace' missing"; exit 2 }
if (-not (Test-Path $CacheDir))  { Write-Error "cache '$CacheDir' missing"; exit 2 }

function Get-TreeHashes {
    param([string]$Root)
    $rootFull = (Resolve-Path $Root).Path
    $map = @{}
    Get-ChildItem -Path $rootFull -Recurse -File -Force `
        | Where-Object {
            $_.FullName -notmatch '[\\/](\.git|target)[\\/]' -and
            $_.Name -ne 'Cargo.toml' -and
            $_.Name -ne 'Cargo.lock'
        } `
        | ForEach-Object {
            $rel = $_.FullName.Substring($rootFull.Length + 1) -replace '\\', '/'
            $map[$rel] = (Get-FileHash -Algorithm SHA256 -Path $_.FullName).Hash
        }
    return $map
}

$expected = Get-TreeHashes -Root $CacheDir
$actual   = Get-TreeHashes -Root $workspace

$problems = @()
foreach ($key in $expected.Keys) {
    if (-not $actual.ContainsKey($key)) {
        $problems += "MISSING: $key"
    } elseif ($actual[$key] -ne $expected[$key]) {
        $problems += "MODIFIED: $key"
    }
}
foreach ($key in $actual.Keys) {
    if (-not $expected.ContainsKey($key)) {
        $problems += "ADDED: $key"
    }
}

if ($problems.Count -gt 0) {
    [Console]::Error.WriteLine("Source files were modified ($($problems.Count) issues):")
    $problems | Select-Object -First 25 | ForEach-Object { [Console]::Error.WriteLine("  $_") }
    if ($problems.Count -gt 25) {
        [Console]::Error.WriteLine("  ...and $($problems.Count - 25) more")
    }
    exit 1
}

Write-Output "All $($expected.Count) source files byte-identical to upstream."
exit 0
