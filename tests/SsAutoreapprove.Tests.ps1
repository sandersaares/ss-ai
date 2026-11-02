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
        $script:Prompt | Should -Match 'verify its completion and digest using native `results`'
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $assignment | Should -Match 'Do not invoke any skill, delegate,\s+publish'
        $assignment | Should -Match 'Follow every inventory page'
        $assignment | Should -Match 'Do not return source text,\s+discussion transcripts'
    }

    It 'provides a bounded worker-only command guide with complete context pages' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $workerPath = Join-Path $skill 'assessment-commands.md'
        $worker = Get-Content -LiteralPath $workerPath -Raw
        $assignment | Should -Match 'assessment-commands\.md'
        ([Text.Encoding]::UTF8.GetByteCount($worker)) | Should -BeLessThan 16384
        $worker | Should -Match 'source-context[^\r\n]*--limit 16384'
        $script:Recipes | Should -Match 'source-context[^\r\n]*--limit 16384'
        $worker | Should -Not -Match 'autoreapprove (discover|inspect|publish|notify)'
        $script:Recipes | Should -Match 'Omit `--after` for the first page'
    }

    It 'reserves workers across recovery without overlapping or resetting budgets' {
        $script:Prompt | Should -Match 'Reserve a worker with `start-worker`\s+before dispatch'
        $script:Prompt | Should -Match 'at most three workers\s+for one PR across planned handoffs, recovery, and stale snapshots'
        $script:Prompt | Should -Match 'including replacements, obtain a new native worker reservation'
        $script:Prompt | Should -Match 'Only one worker may be active for a PR'
        $script:Prompt | Should -Match 'Do not pass semantic conclusions'
        $script:Prompt | Should -Match 'Do not create a new run, refresh, or notify to\s+recover unfinished work'
    }

    It 'finishes each candidate before starting another inspection budget' {
        $script:Prompt | Should -Match 'process one candidate at a time'
        $script:Prompt | Should -Match 'continue through stages 3 and 4, including any required recovery, before\s+inspecting the next candidate'
        $script:Prompt | Should -Match 'Resume any previously inspected candidate\s+using its existing budget'
        $script:Prompt | Should -Match 'not to a separate pass over\s+all inspected candidates'
    }

    It 'requires coverage repair instead of treating completion as a probe' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $assignment | Should -Match 'Record one final judgment per changed-path record, after reading its policy'
        $assignment | Should -Match 'Complete only after every inventory page'
        $assignment | Should -Match 'every path has a judgment'
        $assignment | Should -Match 'Do not use `complete` as a probe'
        $assignment | Should -Match 'If it rejects missing coverage, repair the\s+missing work within the remaining budget'
        $assignment | Should -Match 'A delivered tool result is not\s+command success'
        $assignment | Should -Match 'nonzero native exits'
    }

    It 'uses direct batches and reserves handoffs for real context pressure' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $script:Prompt | Should -Match 'genuine context pressure'
        $script:Prompt | Should -Match 'Do not require unit\s+notes'
        $assignment | Should -Match 'Do not write a note per unit'
        $assignment | Should -Match 'independently reason about the combined\s+change'
        $assignment | Should -Match 'do not derive it mechanically from individual labels'
        $assignment | Should -Match 'Judge coherent groups as their evidence becomes available'
        $assignment | Should -Match 'Record checks for concrete unresolved concerns or interactions'
        $assignment | Should -Match 'independently addressable\s+unit references'
        $assignment | Should -Match 'unit-read-plan --reference discussion'
        $assignment | Should -Match 'Do not fetch the unit manifest again for each\s+window'
        $script:Recipes | Should -Match 'including escaped text and metadata'
        $assignment | Should -Match 'Issue independent batches together\s+as separate calls in one model turn'
        $assignment | Should -Match 'Retain the returned change IDs and their evidence references'
        $assignment | Should -Match 'Follow the returned `nextOffset` rather than advancing by a fixed page size'
        $assignment | Should -Match 'Leave time within the assessment budget for this reasoning and completion'
        $script:Recipes | Should -Match 'commits the entire batch atomically'
    }

    It 'recovers local read-token changes without replacing the worker' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $assignment | Should -Match 'Start with `assessment`\s+using `--compact --limit 100`'
        $assignment | Should -Match 'new `readToken` returned by `source-context`'
        $assignment | Should -Match 'oldLineCount'
        $assignment | Should -Match 'Do not\s+request lines outside the captured side'
        $assignment | Should -Match 'obtain the new token from `assessment`\s+and retry the undelivered references at the same offsets in this worker'
        $assignment | Should -Match 'Do not\s+restart the worker or refresh the snapshot'
        $script:Recipes | Should -Match 'This is not snapshot staleness'
    }

    It 'keeps output and context limits separate from semantic uncertainty' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $assignment | Should -Match 'retry a smaller batch from the same\s+offsets'
        $assignment | Should -Match 'Keep separately bounded responses separate in the host tool output'
        $assignment | Should -Match 'Never fabricate judgments for unexamined paths'
        $assignment | Should -Match 'not semantic uncertainty or a reason\s+to notify'
        $script:Recipes | Should -Match 'including the final newline'
        $script:Recipes | Should -Match 'JSON escaping and multibyte text can shorten'
        $script:Recipes | Should -Match 'requires all readable evidence for every disposition'
        $script:Recipes | Should -Match '`evidenceDelivery` diagnostic is a tooling failure'
    }

    It 'reacquires evidence required by replacement workers' {
        $script:Recipes | Should -Match 'Repeating `prepare` preserves reads and judgments'
        $script:Recipes | Should -Match 'reacquires the evidence it needs'
        $script:Recipes | Should -Match 'earlier receipts do not\s+establish that worker''s knowledge'
        $script:Recipes | Should -Match 'Retain judgments unless deliberately\s+replacing them with their current digests'
    }

    It 'requires complete semantic discussion and shared historical excerpts' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $script:Recipes | Should -Match 'Discussion is a compact semantic document, not approval bookkeeping'
        $script:Recipes | Should -Match '`discussionDigest` binds the complete document'
        $script:Recipes | Should -Match 'Identical excerpt text does not merge comments'
        $assignment | Should -Match 'Read all discussion bodies and shared historical excerpts'
        $assignment | Should -Match 'Resolved,\s+outdated, repeated, and agent-authored discussion is not automatically\s+irrelevant'
        $assignment | Should -Match 'receipt\s+proves delivery, not comprehension'
    }

    It 'requires exact patches and context without unit-note aggregation' {
        $assignment = Get-Content -LiteralPath (Join-Path $skill 'assessment.md') -Raw
        $assignment | Should -Match '`:old`'
        $assignment | Should -Match '`:new`'
        $assignment | Should -Match 'Do not write a note per unit'
        $assignment | Should -Match 'Every registered range is\s+required evidence'
        $script:Recipes | Should -Match 'direct\s+assessments need no unit notes'
        $script:Recipes | Should -Match 'Replacements invalidate dependent records'
        $script:Recipes | Should -Match '`sourceEvidenceDigest`'
        $help = & ss-ai autoreapprove source-context --help | Out-String
        $LASTEXITCODE | Should -Be 0
        foreach ($argument in @('change-id', 'side', 'start-line', 'line-count', 'limit', 'snapshot-digest')) {
            $help | Should -Match "\-\-$argument"
        }
    }

    It 'reports every in-range result without inventing approval origin' {
        $script:Prompt | Should -Match 'final report MUST include a Markdown table with one row for every in-range\s+candidate returned across all final `results` pages'
        $script:Prompt | Should -Match 'retained decisions,\s+already-valid approvals, no-prior-approval and ineligible outcomes, and failures'
        $script:Prompt | Should -Match 'table must have `PR`, `Decision`, and `Required detail` columns'
        $script:Prompt | Should -Match '\[PR number \+ title\]\(PR URL\)'
        $script:Prompt | Should -Match 'retained from `reviewNotice.createdAt`'
        $script:Prompt | Should -Match 'Approval already valid'
        $script:Prompt | Should -Match 'do not infer who or what submitted it'
        $script:Prompt | Should -Match 'required documents and requested\s+context ranges delivered versus total'
        $script:Prompt | Should -Match 'Transport fragments are not separate required judgments'
    }

    It 'excludes draft PRs from work and reports' {
        $script:Prompt | Should -Match 'skill ignores draft PRs'
        $script:Prompt | Should -Match 'Do not list or count them in the report'
        $script:Recipes | Should -Match 'return no item when a PR has become a draft'
    }

    It 'retains refusal markers until a strictly later account approval' {
        $script:Prompt | Should -Match 'an approval by the authenticated account strictly after'
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
        $script:Recipes | Should -Match 'do not automatically\s+resend it in the same attempt'
        $script:Recipes | Should -Not -Match 'accepted-but-stale'
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
        foreach ($command in @('discover', 'results', 'inspect', 'refresh', 'prepare', 'start-worker', 'assessment', 'read', 'read-batch', 'source-context', 'units', 'note', 'checks', 'resolve', 'checkpoint', 'judge', 'judge-batch', 'complete', 'publish', 'notify')) {
            $recipes | Should -Match "(?m)^ss-ai autoreapprove $command "
            $help | Should -Match "(?m)^\s+$command\s"
        }
        $recipes | Should -Not -Match 'pwsh .* -File|gh api|az devops|ConvertTo-Json'
    }

    It 'routes ADO publication through the native transaction without post-vote calls' {
        $skill = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-autoreapprove'
        $prompt = Get-Content -LiteralPath (Join-Path $skill 'SKILL.md') -Raw
        $recipes = Get-Content -LiteralPath (Join-Path $skill 'commands.md') -Raw
        $prompt | Should -Not -Match 'ADO publication.*not supported|ADO inspection and assessment are read-only'
        $recipes | Should -Not -Match 'ADO supports read-only|unsupported publication transaction'
        $recipes | Should -Match '(?m)^ss-ai autoreapprove publish .*--tenant "\{\{TENANT\}\}"'
        $recipes | Should -Match 'without later provider calls'
        $prompt | Should -Match 'never issue a direct reviewer vote'
        $prompt | Should -Match 'do not resend it\s+during that attempt'
        $help = & ss-ai autoreapprove publish --help | Out-String
        $LASTEXITCODE | Should -Be 0
        $help | Should -Match '\-\-tenant'
    }
}
