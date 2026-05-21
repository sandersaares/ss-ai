#requires -Version 7.0
<#
.SYNOPSIS
    Clones folo at a pinned commit into a local cache and generates the
    iteration-3 full-workspace task YAML enumerating every file as an
    `inputs.files` entry.

.DESCRIPTION
    Wired as the `before_run` hook for the ss-cargo-deps-tidy eval. Runs
    every `waza run` but is fully idempotent — re-clone is skipped when
    the cache HEAD already matches the pinned SHA.

    Outputs:
      - evals/ss-cargo-deps-tidy/fixtures/.eval-cache/folo-3e1f241/
        (gitignored full git checkout, .git kept for diagnostic tooling)
      - evals/ss-cargo-deps-tidy/tasks/folo/folo-3e1f241-full.yaml
        (gitignored generated task spec)

    Designed to be invoked from the repo root (CWD = c:\Source\ss-ai).
#>

[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$pinnedSha    = '3e1f241ddfb6de7ab0ecca1a54ca923807135d83'
$shortSha     = '3e1f241'
$repoUrl      = 'https://github.com/folo-rs/folo.git'

$evalDir      = 'evals/ss-cargo-deps-tidy'
$cacheDir     = "$evalDir/fixtures/.eval-cache/folo-$shortSha"
$taskFile     = "$evalDir/tasks/folo/folo-$shortSha-full.yaml"

# 1. Ensure cache exists and is at the pinned SHA.
$needClone = $true
if (Test-Path (Join-Path $cacheDir '.git')) {
    $current = (& git -C $cacheDir rev-parse HEAD 2>$null).Trim()
    if ($LASTEXITCODE -eq 0 -and $current -eq $pinnedSha) {
        Write-Host "[prepare-folo] cache at $shortSha already up to date"
        $needClone = $false
    } else {
        Write-Host "[prepare-folo] cache HEAD '$current' != '$pinnedSha', refetching"
    }
}

if ($needClone) {
    if (Test-Path $cacheDir) {
        # Fetch + checkout into existing clone for speed.
        & git -C $cacheDir fetch --depth 1 origin $pinnedSha 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "git fetch failed" }
        & git -C $cacheDir checkout --detach $pinnedSha 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "git checkout failed" }
    } else {
        Write-Host "[prepare-folo] cloning $repoUrl into $cacheDir"
        New-Item -ItemType Directory -Force -Path (Split-Path $cacheDir -Parent) | Out-Null
        & git clone --filter=blob:none $repoUrl $cacheDir 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "git clone failed" }
        & git -C $cacheDir checkout --detach $pinnedSha 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "git checkout failed (commit not in default fetch?)" }
    }

    $verify = (& git -C $cacheDir rev-parse HEAD).Trim()
    if ($verify -ne $pinnedSha) {
        throw "cache HEAD '$verify' does not match pinned '$pinnedSha' after checkout"
    }
}

# 2. Enumerate every file under cache (excluding .git, target).
$cacheFull = (Resolve-Path $cacheDir).Path
$files = Get-ChildItem -Path $cacheFull -Recurse -File -Force `
    | Where-Object { $_.FullName -notmatch '[\\/](\.git|target)[\\/]' } `
    | ForEach-Object { $_.FullName.Substring($cacheFull.Length + 1) -replace '\\', '/' } `
    | Sort-Object

if (-not $files) { throw "no files found in cache $cacheDir" }
Write-Host "[prepare-folo] enumerated $($files.Count) files"

# 3. Generate the task YAML. Inline graders to avoid leaking strict
#    grader config onto the manifest-only iteration-1 task.
Import-Module powershell-yaml -ErrorAction Stop

$expectedRel = 'fixtures/folo-3e1f241-expected'
$snapshots = Get-ChildItem -Path (Join-Path $evalDir $expectedRel) -Recurse -File -Filter 'Cargo.toml' `
    | ForEach-Object {
        $rel = $_.FullName.Substring((Resolve-Path (Join-Path $evalDir $expectedRel)).Path.Length + 1) -replace '\\', '/'
        # Snapshot is resolved against the diff grader's context_dir (set
        # to "$evalDir/fixtures" below), so prefix with the expected-tree
        # name only — NOT with `fixtures/`, which would double up.
        [ordered]@{ path = $rel; snapshot = "folo-3e1f241-expected/$rel" }
    }

$workaroundPath = "$evalDir/prompts/upstream-bug-workaround.md"
$workaroundRaw = (Get-Content -LiteralPath $workaroundPath -Raw).TrimEnd()
# Indent continuation lines so the workaround reads as a single bullet
# in the rendered prompt.
$workaroundLines = $workaroundRaw -split "`r?`n"
$workaroundIndented = ($workaroundLines[0] + "`n" + (($workaroundLines | Select-Object -Skip 1 | ForEach-Object { "  $_" }) -join "`n")).TrimEnd()

$prompt = @"
Tidy Cargo dependencies across this Rust workspace. The full skill flow
applies, including stage 5 ``cargo check`` verification and stage 7
``cargo generate-lockfile`` minimum-version verification.

Important constraints:

- Modify only Cargo.toml files and Cargo.lock. Do not edit any source
  files (.rs, .md, .json, etc.) — they must remain byte-identical to
  the upstream commit.

- $workaroundIndented
"@

$task = [ordered]@{
    id = "folo-$shortSha-full"
    name = "Folo @ $shortSha - full workspace tidy"
    description = @"
Iteration-3 full-workspace eval extracted from folo-rs/folo at commit
$pinnedSha. The agent receives the complete source tree (sources, lockfile,
AGENTS.md, justfile) and is expected to apply ss-cargo-deps-tidy with
``cargo`` verification enabled. Only Cargo.toml files and Cargo.lock may be
modified; source files must be byte-identical to upstream.
"@
    tags = @('workflow', 'heavy')
    timeout_seconds = 3600
    context_dir = $cacheDir
    inputs = [ordered]@{
        prompt = $prompt
        files = @($files | ForEach-Object { [ordered]@{ path = $_ } })
    }
    expected = [ordered]@{
        outcomes = @( [ordered]@{ type = 'task_completed' } )
    }
    graders = @(
        [ordered]@{
            name = 'cargo-manifests-match-expected'
            type = 'diff'
            config = [ordered]@{
                context_dir = "$evalDir/fixtures"
                expected_files = @($snapshots)
            }
        },
        [ordered]@{
            name = 'source-files-untouched'
            type = 'program'
            config = [ordered]@{
                command = 'pwsh'
                args = @('-NoProfile', '-File', "$evalDir/scripts/Test-SourceUntouched.ps1", '-CacheDir', $cacheDir)
                timeout = 120
            }
        },
        [ordered]@{
            name = 'lockfile-modified-or-created'
            type = 'program'
            config = [ordered]@{
                command = 'pwsh'
                args = @('-NoProfile', '-File', "$evalDir/scripts/Test-LockfileModified.ps1", '-CacheDir', $cacheDir)
                timeout = 30
            }
        },
        [ordered]@{
            name = 'stage-7-min-version-bumps'
            type = 'file'
            config = [ordered]@{
                content_patterns = @(
                    [ordered]@{
                        path = 'Cargo.toml'
                        must_match = @(
                            'libc\s*=\s*\{\s*version\s*=\s*"0\.2\.[1-9]\d*"',
                            'proc-macro2\s*=\s*\{\s*version\s*=\s*"1\.0\.[1-9]\d*"',
                            'tracing\s*=\s*\{\s*version\s*=\s*"0\.1\.[1-9]\d*"',
                            'syn\s*=\s*\{\s*version\s*=\s*"2\.0\.[1-9]\d*"',
                            'fake_headers\s*=\s*\{\s*version\s*=\s*"0\.0\.[1-9]\d*"'
                        )
                    }
                )
            }
        }
    )
}

$header = @(
    '# AUTOGENERATED by scripts/Generate-FoloFullTask.ps1 — do not edit.',
    '# Regenerated by the `before_run` hook on every `waza run`.',
    ''
) -join "`n"

$yaml = ConvertTo-Yaml -Data $task

New-Item -ItemType Directory -Force -Path (Split-Path $taskFile -Parent) | Out-Null
Set-Content -Path $taskFile -Value ($header + $yaml) -Encoding utf8 -NoNewline
Write-Host "[prepare-folo] wrote $taskFile ($($files.Count) files, $(@($snapshots).Count) snapshots)"

