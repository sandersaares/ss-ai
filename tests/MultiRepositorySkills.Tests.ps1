#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $root = Split-Path -Parent $PSScriptRoot
    $script:ProactorSkill = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-review-proactor\SKILL.md')
    )
    $script:ProactorDesign = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-review-proactor\docs\design.md')
    )
    $script:ProactorCommands = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-review-proactor\tool-calls\select-pull-request.md')
    )
    $script:ReviewSkill = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-review\SKILL.md')
    )
    $script:ReviewImplementation = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-review\docs\implementation.md')
    )
    $script:ProactorImplementation = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-review-proactor\docs\implementation.md')
    )
    $script:AutoreapproveSkill = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-autoreapprove\SKILL.md')
    )
    $script:AutoreapproveCommands = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-autoreapprove\commands.md')
    )
}

Describe 'Multi-repository skill contracts' {
    It 'accepts an ordered explicit repository list in both skills' {
        foreach ($content in @($script:ProactorSkill, $script:AutoreapproveSkill)) {
            $content | Should -Match 'One or more explicit repositories'
            $content | Should -Match 'preserve the\s+user''s\s+(?:repository\s+)?order'
            $content | Should -Match 'Resolve (?:all of them|every explicitly\s+supplied repository) before (?:remote work|starting selection)'
        }
    }

    It 'keeps proactor reviews in one repository until it is exhausted' {
        $script:ProactorSkill |
            Should -Match 'continues reviewing the current repository until selection reports no candidate'
        $script:ProactorDesign |
            Should -Match 'A completed review returns to the same active\s+repository\.'
        $script:ProactorSkill |
            Should -Match 'On `noCandidate`, switch to the next repository'
        $script:ProactorSkill |
            Should -Match 'Do not cycle back to an earlier repository during the same invocation'
        $script:ProactorSkill |
            Should -Match 'Keep the cross-repository aggregate only in the primary-session report'
        $script:ProactorSkill |
            Should -Match 'include only\s+information from that summary''s destination repository'
    }

    It 'isolates sequential autoreapproval runs by repository' {
        $script:AutoreapproveSkill |
            Should -Match 'For each repository, create a fresh absolute run path'
        $script:AutoreapproveSkill |
            Should -Match 'Do not overlap repository runs'
        $script:AutoreapproveSkill |
            Should -Match 'run stage 1 `discover` with that repository''s URL and tenant'
        $script:AutoreapproveCommands |
            Should -Match 'The native commands accept one repository per run'
        $script:AutoreapproveCommands |
            Should -Match 'Use a separate path for every repository'
    }

    It 'supports assignment-filtered proactor selection' {
        $script:ProactorSkill |
            Should -Match 'Use `assigned` only when the user explicitly asks'
        $script:ProactorSkill |
            Should -Match 'requires an assignment to the signed-in user and excludes an existing active\s+submitted review'
        $script:ProactorSkill |
            Should -Match 'a matching completed marker prevents\s+repeat selection'
        $script:ProactorCommands |
            Should -Match '--selection-mode "\{\{SELECTION_MODE\}\}"'
        $script:ProactorCommands |
            Should -Match '`assigned` to require an assignment without an active submitted review'
        $script:ProactorCommands |
            Should -Match 'replace `review select` with\s+`review complete`'
        $script:ReviewSkill |
            Should -Match 'run the active-reservation `review check` command'
        $script:ReviewSkill |
            Should -Match 'Append\s+`--publish-to-closed-pull-request` only when the user'
        $script:ProactorCommands |
            Should -Match 'append `--publish-to-closed-pull-request` only for this check'
        $script:ProactorCommands |
            Should -Match '--reviewed-source-commit "\{\{REVIEWED_SOURCE_COMMIT\}\}"'
        $script:ProactorDesign |
            Should -Match 'One resolved mode applies to every\s+configured repository'
    }

    It 'suspends when a user prompt is skipped or otherwise unanswered' {
        foreach ($content in @($script:ProactorSkill, $script:ReviewSkill)) {
            $content | Should -Match 'skipped, dismissed, cancelled, timed-out, or unrecognized'
            $content | Should -Match '\[Skipped by user\]'
        }
        $script:ProactorSkill | Should -Match 'do not start another cycle'
        $script:ReviewSkill | Should -Match 'do not write decision state, start a new phase'
        $script:ProactorDesign |
            Should -Match 'Skipped, dismissed, cancelled, timed-out, or unrecognized user prompts remain\s+unanswered'
    }

    It 'binds resumed work to exactly one active review cycle' {
        $script:ProactorSkill | Should -Match 'one \*\*active review cycle\*\*'
        $script:ProactorSkill |
            Should -Match 'Stages 1 through 3 may construct and populate a pending identity'
        $script:ProactorSkill |
            Should -Match 'From stage 4 onward, require an existing, matching\s+identity'
        $script:ProactorSkill |
            Should -Match 'Do\s+not\s+combine paths or queued actions from different cycles'
        $script:ProactorSkill |
            Should -Match 'run the read-only\s+active-reservation `review check` command from\s+the active cycle'
        $script:ProactorImplementation |
            Should -Match 'one active-cycle identity'
        $script:ProactorImplementation |
            Should -Match 'The agent never mixes two cycle roots'
        $script:ReviewSkill |
            Should -Match 'bind this invocation to one review identity'
        $script:ReviewSkill |
            Should -Match 'Never reuse an artifact from a completed earlier\s+cycle'
    }

    It 'uses stage-aware review identity and ownership validation' {
        $script:ProactorSkill |
            Should -Match 'before final review\s+publication'
        $script:ProactorSkill |
            Should -Match 'do not rerun\s+`review check`'
        $script:ProactorSkill | Should -Match 'produces `alreadyReviewed`'
        $script:ProactorSkill |
            Should -Match '`review complete`\s+command performs the remaining remote ownership'
        $script:ProactorCommands |
            Should -Match 'After interruption or compaction before final review publication'
        $script:ProactorCommands |
            Should -Match 'Do not run this check to resume phase 9'
        $script:ReviewSkill |
            Should -Match 'After phase 8 completes'
        $script:ReviewImplementation |
            Should -Match 'After phase 8 completes'
        $script:ReviewImplementation |
            Should -Match 'does not rerun the\s+candidate-eligibility check'
    }

    It 'records no-write outcomes as phase-8 completion evidence' {
        $script:ReviewSkill |
            Should -Match 'record a durable\s+completion entry in the maintained review checkpoint'
        $script:ReviewSkill |
            Should -Match '`nothingToPublish` completion entry is valid'
        $script:ReviewImplementation |
            Should -Match 'publication result or a maintained\s+checkpoint entry'
        $script:ProactorImplementation |
            Should -Match 'maintained checkpoint entry that binds the cycle\s+identity'
    }

    It 'limits proactor handoff evidence to proactor reviews' {
        $script:ReviewSkill |
            Should -Match 'For a proactor handoff, if interruption or compaction pauses this phase'
        $script:ReviewSkill |
            Should -Match 'Standalone `ss-review` invocations resume from their own review checkpoint'
        $script:ReviewImplementation |
            Should -Match 'Standalone reviews have no proactor\s+journal or handoff identity requirement'
        $script:ReviewImplementation |
            Should -Match 'A standalone review\s+finishes phase 9 without a handoff'
    }

    It 'aggregates autoreapproval results across repository runs' {
        $script:AutoreapproveSkill |
            Should -Match 'from every repository'
        $script:AutoreapproveSkill |
            Should -Match 'Repository`, `PR`, `Decision`, and `Required detail'
    }
}
