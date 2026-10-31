#Requires -Version 7.6

BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $PSNativeCommandUseErrorActionPreference = $true
}

Describe 'Automatic reapproval repository scope instructions' {
    BeforeAll {
        $skill = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-autoreapprove'
        $script:Prompt = Get-Content -LiteralPath (Join-Path $skill 'SKILL.md') -Raw
        $script:Recipes = Get-Content -LiteralPath (Join-Path $skill 'commands.md') -Raw
    }

    It 'defaults to the invocation repository while honoring explicit invocation and schedule scope' {
        $script:Prompt | Should -Match 'An explicit repository in the invocation or user-authored schedule prompt\s+takes precedence'
        $script:Prompt | Should -Match "default to the Git repository containing the\s+invocation's working directory, including a linked worktree or a subdirectory\s+of a checkout"
        $script:Prompt | Should -Match 'Use the repository identity supplied by the session when\s+available'
        $script:Prompt | Should -Match "Do not use the skill's installation directory or a temporary\s+checkout"
        $script:Prompt | Should -Match 'Keep the resolved URL fixed throughout the run'
        $script:Prompt | Should -Not -Match 'Require an explicit HTTPS repository browser URL|Do not infer one from'
        $script:Recipes | Should -Match 'REPOSITORY.*resolved in skill stage 1 from the explicit repository or invocation checkout'
    }

    It 'resolves supported fetch remotes without requesting confirmation for a resolved repository' {
        $script:Prompt | Should -Match 'git -C "\{\{INVOCATION_DIRECTORY\}\}" remote -v'
        $script:Prompt | Should -Match 'Use the fetch URL of `origin`, or the sole remote if there is no `origin`'
        $script:Prompt | Should -Match 'remove GitHub''s clone\s+suffix `\.git`'
        $script:Prompt | Should -Match 'git@github\.com:OWNER/REPO\.git'
        $script:Prompt | Should -Match 'git@ssh\.dev\.azure\.com:v3/ORG/PROJECT/REPO'
        $script:Prompt | Should -Match 'corresponding `ssh://` forms'
        $script:Prompt | Should -Match 'https://ORG\.visualstudio\.com/PROJECT/_git/REPO'
        $script:Prompt | Should -Match 'Do not include credentials in the resolved URL'
        $script:Prompt | Should -Match 'Do not ask the user to supply or confirm the repository URL when it resolves\s+from the invocation''s Git repository'
    }

    It 'asks only for unresolved scope and never falls back from an invalid explicit repository' {
        $script:Prompt | Should -Match 'Ask for a repository URL only when no\s+supported repository can be resolved'
        $script:Prompt | Should -Match 'outside a Git checkout, after\s+a failed remote inspection, or with ambiguous remotes'
        $script:Prompt | Should -Match 'Do not silently replace\s+an invalid explicit repository with the current checkout'
        $script:Prompt | Should -Match 'schedule running in a Git repository can simply invoke\s+`ss-autoreapprove`'
        $script:Prompt | Should -Match 'Report the resolved repository and whether it was explicit or defaulted from\s+the invocation checkout'
    }

    It 'isolates each semantic assessment in the specified model and context' {
        $script:Prompt | Should -Match 'one fresh subagent per eligible PR'
        $script:Prompt | Should -Match 'model: "gpt-6\.1-sol"'
        $script:Prompt | Should -Match 'reasoning_effort: "medium"'
        $script:Prompt | Should -Match 'context_tier: "default"'
        $script:Prompt | Should -Match 'Never assess source changes in the\s+orchestrator or silently substitute another model'
        $script:Prompt | Should -Match 'verify its returned completion status and digest using\s+native `results`'
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $assignment | Should -Match 'Do not invoke the\s+skill, delegate, publish remotely'
        $assignment | Should -Match 'Follow every inventory page'
        $assignment | Should -Match 'Do not return source text, discussion transcripts'
    }

    It 'recovers an ended incomplete worker on the same snapshot without resuming a synchronous task' {
        $script:Prompt | Should -Match 'A returned synchronous task has ended; do not use `write_agent` to resume it'
        $script:Prompt | Should -Match 'native `results` still reports `assessmentRequired` with a null\s+`assessmentDigest`'
        $script:Prompt | Should -Match 'at most one replacement worker per PR and snapshot'
        $script:Prompt | Should -Match 'same run path and snapshot digest'
        $script:Prompt | Should -Match 'including replacements'
        $script:Prompt | Should -Match 'Do not pass the previous worker''s semantic conclusions'
        $script:Prompt | Should -Match 'If the replacement also returns incomplete, report the PR as incomplete'
        $script:Prompt | Should -Match 'Do not use `refresh`, a new run, or a needs-review notice to recover\s+unfinished assessment work'
        $script:Prompt | Should -Match 'Staleness follows the separate recovery procedure'
        $script:Prompt | Should -Match 'unrelated tooling failures remain explicit failures'
    }

    It 'requires coverage repair rather than using complete as a probe or ending on its rejection' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $assignment | Should -Match 'Record each change judgment after reading its evidence'
        $assignment | Should -Match 'Before calling `complete`, verify that every inventory page has been visited'
        $assignment | Should -Match 'every change has a judgment, and the cumulative judgment is recorded'
        $assignment | Should -Match 'Do not use `complete` as a probe'
        $assignment | Should -Match 'If completion rejects missing coverage or reads, continue the missing work'
        $assignment | Should -Match 'A delivered tool result is not command success'
        $assignment | Should -Match 'nonzero native exit'
    }

    It 'uses progress-verified source handoffs without confusing them with failed workers' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $script:Prompt | Should -Match 'planned `checkpoint` is not a completed assessment'
        $script:Prompt | Should -Match '`assessmentProgress.judgedChangeCount` increased'
        $script:Prompt | Should -Match 'snapshot is unchanged'
        $script:Prompt | Should -Match 'no more source handoffs than the native\s+`assessmentProgress.changeCount`'
        $script:Prompt | Should -Match 'final cumulative worker is separate'
        $assignment | Should -Match 'at most ten new changes or\s+65,536 source characters'
        $assignment | Should -Match 'Do not record a cumulative judgment or call `complete` at a checkpoint'
        $assignment | Should -Match 'When a fresh worker starts with no unjudged changes'
        $assignment | Should -Match 'Do not infer the cumulative outcome\s+merely from individual dispositions'
        $script:Recipes | Should -Match 'next worker starts at `nextChangeOffset`'
    }

    It 'keeps output and context limits separate from semantic uncertainty' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $assignment | Should -Match 'Read one evidence response per tool call'
        $assignment | Should -Match 'Retry\s+that same reference and offset'
        $assignment | Should -Match 'Never bulk-mark unexamined inventory entries uncertain'
        $assignment | Should -Match 'not semantic uncertainty or a\s+completed refusal'
        $script:Recipes | Should -Match 'including the final newline'
        $script:Recipes | Should -Match 'JSON escaping and multibyte text can shorten'
        $script:Recipes | Should -Match 'requires all readable evidence for every disposition'
        $script:Recipes | Should -Match '`evidenceDelivery` diagnostic is a tooling failure'
    }

    It 'resumes persisted progress without treating retained reads as the replacement worker''s knowledge' {
        $script:Recipes | Should -Match 'Repeating `prepare` on the same snapshot preserves recorded reads and judgments'
        $script:Recipes | Should -Match 'Reread evidence needed for your own judgment'
        $script:Recipes | Should -Match 'Native read coverage does not establish that a replacement worker has seen the evidence'
        $script:Recipes | Should -Match 'retain existing judgments unless deliberately replacing them'
        $script:Recipes | Should -Match 'with their current digests'
    }

    It 'requires complete semantic discussion and shared historical excerpts' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $script:Recipes | Should -Match 'Discussion is a compact semantic document, not approval bookkeeping'
        $script:Recipes | Should -Match '`discussionDigest` binds the complete document'
        $script:Recipes | Should -Match 'Identical excerpt text does not merge comments'
        $assignment | Should -Match 'Discussion includes all description, review, and comment bodies'
        $assignment | Should -Match 'Read all unique excerpts'
        $assignment | Should -Match 'Historical\s+excerpts may describe code absent from both comparison endpoints'
        $script:Prompt | Should -Match 'compact presentation does not reduce coverage'
    }

    It 'requires exact patches and requested captured context without partial semantic handoffs' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $assignment | Should -Match '`CHANGE_ID:patch`'
        $assignment | Should -Match '`:old`'
        $assignment | Should -Match '`:new`'
        $script:Recipes | Should -Match '`sourceEvidenceDigest`'
        $help = & ss-ai autoreapprove source-context --help | Out-String
        $LASTEXITCODE | Should -Be 0
        foreach ($argument in @('change-id', 'side', 'start-line', 'line-count', 'limit', 'snapshot-digest')) {
            $help | Should -Match "\-\-$argument"
        }
    }

    It 'reports every in-range result with linked identity and service approval time' {
        $script:Prompt | Should -Match 'final report MUST include a Markdown table with one row for every in-range\s+candidate returned across all final `results` pages'
        $script:Prompt | Should -Match 'retained decisions,\s+already-valid approvals, no-prior-approval and ineligible outcomes, and failures'
        $script:Prompt | Should -Match 'table must have `PR`, `Decision`, and `Required detail` columns'
        $script:Prompt | Should -Match '\[PR number \+ title\]\(PR URL\)'
        $script:Prompt | Should -Match 'retained from `reviewNotice.createdAt`'
        $script:Prompt | Should -Match 'service `approvedAt` timestamp, even when issued in a previous invocation'
        $script:Prompt | Should -Match 'External approval valid'
    }

    It 'excludes draft PRs from work and reports' {
        $script:Prompt | Should -Match 'skill ignores draft PRs'
        $script:Prompt | Should -Match 'Do not list or count them in the report'
        $script:Recipes | Should -Match 'return no item when a PR has become a draft'
    }

    It 'retains refusal markers until a strictly later external approval' {
        $script:Prompt | Should -Match 'Only\s+an external approval by the authenticated account strictly after'
        $script:Prompt | Should -Match 'Never reassess an active marker'
        $script:Prompt | Should -Match 'Dry runs report the decision without posting a marker'
        $script:Recipes | Should -Match 'Repeating the original command reconciles the same intent without\s+posting again'
    }

    It 'recovers stale evidence with fresh assessments and a retained bounded budget' {
        $script:Prompt | Should -Match '`staleEvidence` and `staleDuringExecution` are transient evidence conditions'
        $script:Prompt | Should -Match 'two recovery attempts per PR after\s+the initial attempt'
        $script:Prompt | Should -Match 'call `refresh` to persist and report explicit\s+`retryExhausted`'
        $script:Prompt | Should -Match 'Never reuse old judgments or the old assessment digest'
        $script:Prompt | Should -Match 'Stop any previous worker'
        $script:Prompt | Should -Not -Match 'ends this PR''s attempt rather than chasing'
        $script:Recipes | Should -Match 'stale refresh request records `retryExhausted`'
        $script:Recipes | Should -Match 'An attempted or unknown effect blocks refresh, including accepted-but-stale'
        $script:Recipes | Should -Match 'original `publish` or\s+`notify` command'
    }
}

Describe 'Automatic reapproval provider fixtures' {
    It 'validates the GitHub contract projection and its explicit provenance' {
        $fixture = Join-Path $PSScriptRoot '..\packages\ss-ai-review\tests\fixtures\github-approval.json'
        $schema = Join-Path $PSScriptRoot '..\dev\ss-autoreapprove\github-approval-fixture.schema.json'
        Test-Json -LiteralPath $fixture -SchemaFile $schema | Should -BeTrue
    }

    It 'installs one canonical policy and native-only lifecycle recipes' {
        $root = Split-Path -Parent $PSScriptRoot
        $skill = Join-Path $root 'skills\ss-autoreapprove'
        Test-Path -LiteralPath (Join-Path $skill 'SKILL.md') | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $skill 'significance.md') | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $root 'dev\ss-autoreapprove\significance.md') | Should -BeFalse
        $recipes = Get-Content -LiteralPath (Join-Path $skill 'commands.md') -Raw
        $help = & ss-ai autoreapprove --help | Out-String
        $LASTEXITCODE | Should -Be 0
        foreach ($command in @('discover', 'results', 'inspect', 'refresh', 'prepare', 'assessment', 'read', 'source-context', 'judge', 'complete', 'publish', 'notify')) {
            $recipes | Should -Match "(?m)^ss-ai autoreapprove $command "
            $help | Should -Match "(?m)^\s+$command\s"
        }
        $recipes | Should -Not -Match 'pwsh .* -File|gh api|az devops|ConvertTo-Json'
    }

    It 'routes ADO publication and recovery through the guarded native transaction' {
        $skill = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-autoreapprove'
        $prompt = Get-Content -LiteralPath (Join-Path $skill 'SKILL.md') -Raw
        $recipes = Get-Content -LiteralPath (Join-Path $skill 'commands.md') -Raw
        $prompt | Should -Not -Match 'ADO publication.*not supported|ADO inspection and assessment are read-only'
        $recipes | Should -Not -Match 'ADO supports read-only|unsupported publication transaction'
        $recipes | Should -Match '(?m)^ss-ai autoreapprove publish .*--tenant "\{\{TENANT\}\}"'
        $recipes | Should -Match 'unique new structured vote on the assessed iteration'
        $prompt | Should -Match 'never issue a direct reviewer vote'
        $prompt | Should -Match 'never repeats an uncertain vote or audit'
        $help = & ss-ai autoreapprove publish --help | Out-String
        $LASTEXITCODE | Should -Be 0
        $help | Should -Match '\-\-tenant'
    }
}
