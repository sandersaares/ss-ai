#Requires -Version 7.6
<#
.SYNOPSIS
    Clones folo at a pinned commit into a local cache and generates the
    iteration-3 full-workspace task YAML enumerating every file as an
    `inputs.files` entry.

.DESCRIPTION
    Wired as the `before_run` hook for the ss-cargo-deps-tidy eval. Runs
    every `waza run` but is fully idempotent - re-clone is skipped when
    the cache HEAD already matches the pinned SHA.

    Outputs:
      - evals/ss-cargo-deps-tidy/fixtures/.eval-cache/folo-3e1f241/
        (gitignored full git checkout, .git kept for diagnostic tooling)
      - evals/ss-cargo-deps-tidy/tasks/folo/folo-3e1f241-full.yaml
        (gitignored generated task spec)

    Invoke from the active checkout root, including a worktree.
#>

[CmdletBinding(PositionalBinding = $false)]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$PSNativeCommandUseErrorActionPreference = $true

$pinnedSha    = '3e1f241ddfb6de7ab0ecca1a54ca923807135d83'
$shortSha     = '3e1f241'
$repoUrl      = 'https://github.com/folo-rs/folo.git'

$evalDir      = 'evals/ss-cargo-deps-tidy'
$graderDir    = 'dev/ss-cargo-deps-tidy/evals'
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

$prompt = @"
Tidy Cargo dependencies across this Rust workspace. The full skill flow
applies, including stage 5 ``cargo check`` verification and stage 7
``cargo generate-lockfile`` minimum-version verification.

Important constraints:

- Modify only Cargo.toml files and Cargo.lock. Do not edit any source
  files (.rs, .md, .json, etc.) - they must remain byte-identical to
  the upstream commit.
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
            name = 'cargo-tidy-sentinels'
            type = 'file'
            config = [ordered]@{
                content_patterns = @(
                    [ordered]@{
                        path = 'Cargo.toml'
                        must_match = @(
                            'cpu-time\s*=',
                            'arc-swap\s*=\s*\{[^}]*version\s*=\s*"1\.7\.\d+"',
                            'argh\s*=\s*\{[^}]*"default"'
                        )
                        must_not_match = @(
                            'argh\s*=\s*\{[^}]*"help"',
                            'futures\s*=\s*\{[^}]*"executor"',
                            'tokio\s*=\s*\{[^}]*version\s*=\s*"1\.43(\.0)?"',
                            'tracing\s*=\s*\{[^}]*version\s*=\s*"0\.1(\.0)?"',
                            'syn\s*=\s*\{[^}]*version\s*=\s*"2\.0(\.0)?"'
                        )
                    }
                )
            }
        },
        [ordered]@{
            name = 'cargo-tidiness-checks'
            type = 'program'
            config = [ordered]@{
                command = 'pwsh'
                args = @(
                    '-NoProfile', '-File',
                    "$graderDir/Test-CargoTidied.ps1",
                    '-ExpectationsPath', "$evalDir/fixtures/folo-3e1f241-expectations.json",
                    '-RequireBumps'
                )
                timeout = 60
            }
        },
        [ordered]@{
            name = 'source-files-untouched'
            type = 'program'
            config = [ordered]@{
                command = 'pwsh'
                args = @('-NoProfile', '-File', "$graderDir/Test-SourceUntouched.ps1", '-CacheDir', $cacheDir)
                timeout = 120
            }
        },
        [ordered]@{
            name = 'lockfile-modified-or-created'
            type = 'program'
            config = [ordered]@{
                command = 'pwsh'
                args = @('-NoProfile', '-File', "$graderDir/Test-LockfileModified.ps1", '-CacheDir', $cacheDir)
                timeout = 30
            }
        }
    )
}

$header = @(
    '# AUTOGENERATED by dev/ss-cargo-deps-tidy/evals/Generate-FoloFullTask.ps1 - do not edit.',
    '# Regenerated by the `before_run` hook on every `waza run`.',
    ''
) -join "`n"

$yaml = ConvertTo-Yaml -Data $task

New-Item -ItemType Directory -Force -Path (Split-Path $taskFile -Parent) | Out-Null
Set-Content -Path $taskFile -Value ($header + $yaml) -Encoding utf8 -NoNewline
Write-Host "[prepare-folo] wrote $taskFile ($($files.Count) files)"
