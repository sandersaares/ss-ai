#Requires -Version 7
#Requires -Modules @{ ModuleName = 'Pester'; ModuleVersion = '5.5.0' }

# Cross-cutting checks for the ss-cargo-deps-tidy eval suite. These tests do
# not exercise the skill itself; they enforce internal consistency between
# files that must agree (e.g. the canonical upstream-bug-workaround text).

Set-StrictMode -Version Latest

BeforeDiscovery {
    $script:RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).ProviderPath
    $script:EvalDir = Join-Path $script:RepoRoot 'evals\ss-cargo-deps-tidy'
    $script:WorkaroundPath = Join-Path $script:EvalDir 'prompts\upstream-bug-workaround.md'
    # Tracked, hand-authored workflow tasks that must mirror the canonical
    # workaround text. Excludes the generated folo full-tree task
    # (`folo-3e1f241-full.yaml`) because it is rewritten on every run by
    # `scripts/Generate-FoloFullTask.ps1`; the generator handles its own
    # workaround insertion.
    $script:TasksWithWorkaround = @(
        Join-Path $script:EvalDir 'tasks\folo\folo-3e1f241-cargo-only.yaml'
        Join-Path $script:EvalDir 'tasks\inherited-dev-deps\inherited-with-path.yaml'
        Join-Path $script:EvalDir 'tasks\inherited-dev-deps\inherited-name-only.yaml'
    )
}

BeforeAll {
    $script:RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).ProviderPath
    $script:EvalDir = Join-Path $script:RepoRoot 'evals\ss-cargo-deps-tidy'
    $script:WorkaroundPath = Join-Path $script:EvalDir 'prompts\upstream-bug-workaround.md'
}

Describe 'Upstream-bug-workaround prompt is the single source of truth' {
    It 'has a canonical prompt file' {
        Test-Path -LiteralPath $script:WorkaroundPath | Should -BeTrue
    }

    It 'is mirrored verbatim in <_> (line-by-line, indentation- and bullet-insensitive)' -ForEach $script:TasksWithWorkaround {
        $taskPath = $_
        Test-Path -LiteralPath $taskPath | Should -BeTrue -Because "task file $taskPath must exist"
        $canonical = (Get-Content -LiteralPath $script:WorkaroundPath -Raw).Trim() -split "`r?`n"
        $task = Get-Content -LiteralPath $taskPath
        # Strip leading whitespace and any list-bullet marker from each line
        # so a "- WORKAROUND ..." bullet in the task matches the bare
        # "WORKAROUND ..." in the canonical text.
        $stripPrefix = { param($s) ($s -replace '^\s*(?:-\s+)?', '') }
        $taskNormalized = $task | ForEach-Object { & $stripPrefix $_ }

        foreach ($line in $canonical) {
            if ([string]::IsNullOrWhiteSpace($line)) { continue }
            $expected = & $stripPrefix $line
            $taskNormalized | Should -Contain $expected -Because "canonical line '$line' must appear in $taskPath"
        }
    }
}
