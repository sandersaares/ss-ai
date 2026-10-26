BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $script:SkillsRoot = Join-Path $PSScriptRoot '..\skills'
    $script:ReviewRoot = Join-Path $script:SkillsRoot 'ss-review'
    $script:Recovery = Get-Content -LiteralPath (Join-Path $script:ReviewRoot 'error-recovery.md') -Raw
    $script:Review = Get-Content -LiteralPath (Join-Path $script:ReviewRoot 'SKILL.md') -Raw
    $script:Proactor = Get-Content -LiteralPath (Join-Path $script:SkillsRoot 'ss-review-proactor\SKILL.md') -Raw
    $script:Query = Get-Content -LiteralPath (Join-Path $script:ReviewRoot 'pwsh-calls\query-pr-context.md') -Raw
}

Describe 'Review workflow recovery instructions' {
    It 'shares one recovery policy between both orchestrators and bounded workers' {
        $script:Review | Should -Match '\[error-recovery.md\]\(error-recovery.md\)'
        $script:Review | Should -Match 'Pass this policy to bounded workers'
        $script:Proactor | Should -Match '\.\./ss-review/error-recovery\.md'
        foreach ($path in @(
            'workflow-data.md', 'review.md', 'prepare-references.md',
            'prepare-content-overview.md', 'qualitymaxx.md', 'auto-approve.md',
            'pwsh-calls/query-pr-context.md'
        )) {
            Get-Content -LiteralPath (Join-Path $script:ReviewRoot $path) -Raw |
                Should -Match 'error-recovery\.md' -Because "$path must retain the recovery contract"
        }
    }

    It 'corrects local invocation mistakes and bounds safe transient retries before escalation' {
        $script:Recovery | Should -Match 'Correct orchestration mistakes and retry safe transient\s+failures without asking the user'
        $script:Recovery | Should -Match 'wrong input path, omitted argument, shell quoting error'
        $script:Recovery | Should -Match 'at most three\s+total attempts'
        $script:Recovery | Should -Match 'Do not reset the budget by\s+replacing a worker'
        $script:Recovery | Should -Match 'retry only after a concrete\s+correction'
        $script:Recovery | Should -Match 'honoring service retry guidance'
        $script:Recovery | Should -Match 'If no safe recovery remains, report what was attempted'
        $script:Recovery | Should -Match 'HTTP 403 permission denial'
        $script:Recovery | Should -Match 'Stop the affected workflow and report'
    }

    It 'repairs the capture versus request path mistake without abandoning preparation' {
        $script:Query | Should -Match '`-Path` must name the existing capture'
        $script:Query | Should -Match '`-RequestPath` names the separate query request output'
        $script:Query | Should -Match 'correct it to the assigned capture path'
        $script:Query | Should -Match 'verify existence without reading the capture into context'
        $script:Query | Should -Match 'rerun request creation with the intended request output path'
        $script:Query | Should -Match 'Do not ask the user, rerun proactor selection, or cancel unrelated preparation workers'
        $script:Recovery | Should -Match 'Keep unrelated preparation and\s+review workers running'
        $script:Recovery | Should -Match 'primary\s+orchestrator applies this same policy before escalating'
    }

    It 'keeps bounded evidence access and saved-query replay and restart rules intact' {
        $script:Query | Should -Match 'Never open it with a file-view tool'
        $script:Query | Should -Match 'Never bypass this recipe with an alternate provider fetch'
        $script:Query | Should -Match 'Never fall back to a raw file read'
        $script:Query | Should -Match 'Do not use `-Next` if uncertain whether the prior output was received: replay first'
        $script:Query | Should -Match '`-Restart` to bind its current contents and read the first page'
        $script:Query | Should -Match 'Different filters or a different capture path require a new request'
    }

    It 'requires positive pre-write evidence and preserves reconciliation for ambiguous external writes' {
        $script:Recovery | Should -Match 'missing receipt does not prove\s+that nothing reached the service'
        $script:Recovery | Should -Match 'no\s+remote write was attempted and the command''s lifecycle permits re-entry'
        $script:Recovery | Should -Match 'Never blindly replay a possibly successful claim'
        $script:Recovery | Should -Match 'reviewer registration, overview publication, review submission, or tracker'
        $script:Recovery | Should -Match 'do not delete, move\s+aside, or replace them to enable a retry'
        $script:Review | Should -Match 'After an attempted or ambiguous remote write, never run this command a second time against the same pull request'
        $script:Review | Should -Match 'After an attempted or ambiguous remote write, never run this command a second time to recover from a failure without human reconciliation'
        $script:Review | Should -Match 'absence of a result file alone is not proof'
        $script:Proactor | Should -Match 'Do not rerun selection, erase the journal, or fabricate\s+confirmation'
        $script:Proactor | Should -Match 'Ask the real user to reconcile the journal and PR discussion'
        $recipe = Get-Content -LiteralPath (Join-Path $script:SkillsRoot 'ss-review-proactor\tool-calls\select-pull-request.md') -Raw
        $recipe | Should -Match 'journal lifecycle permits re-entry'
        $recipe | Should -Match 'still requires human reconciliation'
    }

    It 'retains real human gates and best-effort outcomes without granting recovery authority' {
        $script:Review | Should -Match 'A question directed to the user is a hard suspension point'
        $script:Proactor | Should -Match 'Every question to the user is a hard suspension point'
        $script:Recovery | Should -Match 'Recovery cannot supply\s+audit decisions, merge approval, installation consent, or publication\s+adjudication'
        $script:Recovery | Should -Match 'Preserve explicit best-effort outcomes'
        $script:Review | Should -Match 'If it reports a 404 error, ignore the error and continue'
        $script:Review | Should -Match 'Never replay ambiguous writes'
    }

    It 'removes unconditional stop-on-error wording from coupled workflow instructions' {
        # These forms stop the entire workflow before it can diagnose a recoverable invocation.
        $blanketStop = '(?i)(?:On any (?:command )?error|Any command failure is a stop|Any nonzero exit or error\s+requires human input|(?:If (?:it|either|the command) reports (?:an|any other) error|On (?:a query|an?|query) error),? (?:report [^.]* and stop|stop and report)|Report query errors and stop|stop and report query errors)'
        foreach ($skill in @('ss-review', 'ss-review-proactor')) {
            foreach ($file in Get-ChildItem -LiteralPath (Join-Path $script:SkillsRoot $skill) -Filter '*.md' -Recurse) {
                Get-Content -LiteralPath $file.FullName -Raw |
                    Should -Not -Match $blanketStop -Because "$($file.FullName) must allow diagnosis and safe recovery"
            }
        }
    }
}
