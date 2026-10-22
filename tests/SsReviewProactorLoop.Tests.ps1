BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $script:SkillRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'agents\.github\skills\ss-review-proactor'
    $script:ProactorPrompt = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
    $script:ReviewPrompt = Get-Content -LiteralPath (Join-Path $script:SkillRoot '..\ss-review\SKILL.md') -Raw
    $script:RunControl = [regex]::Match($script:ProactorPrompt, '(?s)# Run control(.*?)# Stage 1:').Groups[1].Value
    $script:Continuation = [regex]::Match($script:ProactorPrompt, '(?s)# Stage 5: summary and continuation(.*)').Groups[1].Value
}

Describe 'Proactor loop instructions' {
    It 'defaults to one review and requires an explicit user opt-in rather than a schedule alone' {
        $script:ProactorPrompt | Should -Match 'By default, each invocation starts at most one review'
        $script:RunControl | Should -Match 'Enable loop\s+mode only when the user explicitly requests it'
        $script:RunControl | Should -Match 'user-authored schedule prompt, or a later direct message'
        $script:RunControl | Should -Match 'A schedule alone\s+does not enable looping'
        $script:RunControl | Should -Match '> Run ss-review-proactor in loop mode\.'
    }

    It 'honors the latest stop-after-current instruction throughout review and compaction' {
        $script:RunControl | Should -Match '"the current review is the last one", "stop after this review"'
        $script:RunControl | Should -Match 'disable looping immediately but continue\s+the current review through its remaining phases'
        $script:RunControl | Should -Match 'across handoff, user-input pauses, and\s+compaction'
        $script:RunControl | Should -Match 'restarting stage 1 must not re-enable looping from that original prompt'
        $script:RunControl | Should -Match 'current review has already finished, do not select another PR'
        $script:RunControl | Should -Match 'this invocation only, not future scheduled invocations'
        $script:ReviewPrompt | Should -Match '\[run-control rules\]\(\.\./ss-review-proactor/SKILL.md#run-control\)'
        $script:ReviewPrompt | Should -Match 'disables further cycles without ending this review or answering a pending audit or merge question'
    }

    It 'returns from learning before restarting the entire cycle under the latest instruction' {
        $learning = [regex]::Match($script:ReviewPrompt, '(?s)# Phase 10: learning(.*)').Groups[1].Value
        $learning | Should -Match 'After this phase completes, a proactor handoff returns to \[proactor stage 5\]'
        $learning | Should -Match 'SKILL.md#stage-5-summary-and-continuation'
        $learning | Should -Match 'Preserve the latest user loop instruction'
        $script:Continuation | Should -Match 'completes all phases, including publication\s+\(or its defined nothing-to-publish outcome\) and learning'
        $script:Continuation | Should -Match 'Apply the latest user instruction before selecting more\s+work'
        $script:Continuation | Should -Match 'If loop mode remains enabled, return to stage 1 in this same primary\s+session'
        $script:Continuation | Should -Match 'Otherwise finish after the current review'
        $learning | Should -Match 'Standalone `ss-review` invocations finish here without selecting another review'
        $script:ProactorPrompt | Should -Not -Match 'Do not acquire another PR while this review is\s+paused or after it finishes'
    }

    It 'never advances past empty selection, an unanswered question, or incomplete work' {
        $script:ProactorPrompt | Should -Match '`noCandidate` \|[^\r\n]*finish even in loop mode\. Do not poll'
        $script:ProactorPrompt | Should -Match 'Every question to the user is a hard suspension point'
        $script:ProactorPrompt | Should -Match 'Do not acquire another PR while this review is\s+active, paused, failed, cancelled, or awaiting reconciliation'
        $script:ProactorPrompt | Should -Match 'A `confirmed`\s+selection result or a completed agent turn is not a completed review'
        $script:Continuation | Should -Match 'with no unresolved\s+question or failure'
        $script:Continuation | Should -Match 'An empty selection ends the invocation'
        $script:Continuation | Should -Match 'A pause, failure, or cancellation does not start another cycle'
    }

    It 'isolates cycle artifacts while retaining the invocation configuration' {
        $script:ProactorPrompt | Should -Match 'On later cycles, retain that\s+configuration unless the user changes it'
        $script:ProactorPrompt | Should -Match 'do not infer a different repository\s+from a review checkout or a learning worktree'
        $script:RunControl | Should -Match 'original invocation directory'
        $script:ProactorPrompt | Should -Match 'create a fresh cycle subdirectory'
        $script:ProactorPrompt | Should -Match 'Use new paths for every cycle; never\s+overwrite a previous cycle''s journal or review artifacts'
        $script:ReviewPrompt | Should -Match 'use its absolute review-state directory for all review artifacts and worker outputs otherwise placed directly in session state'
        $recipe = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'tool-calls\select-pull-request.md') -Raw
        $recipe | Should -Match 'Reuse this exact path for confirmation, never for a new cycle or selection run'
    }
}
