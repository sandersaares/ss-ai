#Requires -Version 7
#Requires -Modules @{ ModuleName = 'Pester'; ModuleVersion = '5.5.0' }

# Cross-cutting checks for the ss-cargo-deps-tidy eval suite. These tests do
# not exercise the skill itself; they enforce internal consistency between
# files that must agree (e.g. the canonical upstream-bug-workaround text).

Set-StrictMode -Version Latest

BeforeAll {
    $script:RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).ProviderPath
    $script:EvalDir = Join-Path $script:RepoRoot 'evals\ss-cargo-deps-tidy'
    $script:WorkaroundPath = Join-Path $script:EvalDir 'prompts\upstream-bug-workaround.md'
    $script:CargoOnlyTask = Join-Path $script:EvalDir 'tasks\folo\folo-3e1f241-cargo-only.yaml'
}

Describe 'Upstream-bug-workaround prompt is the single source of truth' {
    It 'has a canonical prompt file' {
        Test-Path -LiteralPath $script:WorkaroundPath | Should -BeTrue
    }

    It 'is mirrored verbatim in folo-3e1f241-cargo-only.yaml (line-by-line, indentation- and bullet-insensitive)' {
        $canonical = (Get-Content -LiteralPath $script:WorkaroundPath -Raw).Trim() -split "`r?`n"
        $task = Get-Content -LiteralPath $script:CargoOnlyTask
        # Strip leading whitespace and any list-bullet marker from each line
        # so a "- WORKAROUND ..." bullet in the task matches the bare
        # "WORKAROUND ..." in the canonical text.
        $stripPrefix = { param($s) ($s -replace '^\s*(?:-\s+)?', '') }
        $taskNormalized = $task | ForEach-Object { & $stripPrefix $_ }

        foreach ($line in $canonical) {
            if ([string]::IsNullOrWhiteSpace($line)) { continue }
            $expected = & $stripPrefix $line
            $taskNormalized | Should -Contain $expected -Because "canonical line '$line' must appear in cargo-only task prompt"
        }
    }
}
