#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:SkillRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review'

    function Invoke-WorkCommand {
        param([string[]] $ArgumentList)
        $executable = Get-Command ss-ai -CommandType Application | Select-Object -First 1
        $start = [Diagnostics.ProcessStartInfo]::new($executable.Source)
        foreach ($argument in (@('workflow') + $ArgumentList)) {
            $start.ArgumentList.Add($argument)
        }
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $process = [Diagnostics.Process]::Start($start)
        try {
            $output = $process.StandardOutput.ReadToEndAsync()
            $errors = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{
                exitCode = $process.ExitCode
                output = $output.GetAwaiter().GetResult()
                errors = $errors.GetAwaiter().GetResult()
            }
        } finally {
            $process.Dispose()
        }
    }

    function Invoke-WorkJson {
        param([string[]] $ArgumentList)
        $result = Invoke-WorkCommand -ArgumentList $ArgumentList
        $result.exitCode | Should -Be 0 -Because $result.errors
        $result.errors | Should -BeNullOrEmpty
        $result.output | ConvertFrom-Json
    }

    function Write-WorkJson {
        param([string] $Path, $Value)
        [IO.File]::WriteAllText($Path, (ConvertTo-Json -InputObject $Value -Depth 30))
    }
}

Describe 'Native review work CLI' {
    BeforeEach {
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid())
        $script:Areas = Join-Path $script:Root 'focus-areas'
        [void] [IO.Directory]::CreateDirectory($script:Areas)
        [IO.File]::WriteAllText((Join-Path $script:Areas 'area.md'), 'Review correctness.')
        $script:Files = @(
            @{ path = Join-Path $script:Root 'new [file].rs'; sizeBytes = 1; change = 'new' }
            @{ path = Join-Path $script:Root 'deleted.rs'; sizeBytes = 3; change = 'deleted' }
        )
        $script:Groups = @(
            @{ groupId = 'group-001'; files = @($script:Files[0]) }
            @{ groupId = 'group-002'; files = @($script:Files[1]) }
        )
        $script:Inventory = Join-Path $script:Root 'inventory.json'
        $script:GroupedInventory = Join-Path $script:Root 'groups.json'
        Write-WorkJson -Path $script:Inventory -Value $script:Files
        Write-WorkJson -Path $script:GroupedInventory -Value $script:Groups
        $script:Work = Join-Path $script:Root 'work'
        $script:Findings = Join-Path $script:Root 'findings'
    }

    It 'requires complete preparation binding without falling back after failure' {
        $arguments = @(
            'create-review-work', '--inventory-path', $script:Inventory,
            '--file-groups-path', $script:GroupedInventory,
            '--focus-area-directory', $script:Areas, '--output-directory', $script:Work
        )
        $partial = Invoke-WorkCommand -ArgumentList ($arguments + @('--session-state', $script:Root))
        $partial.exitCode | Should -Not -Be 0
        $partial.output | Should -BeNullOrEmpty
        $partial.errors | Should -Match 'preparation'
        Test-Path -LiteralPath $script:Work | Should -BeFalse

        $unready = Invoke-WorkCommand -ArgumentList (
            $arguments + @('--session-state', $script:Root, '--preparation-identity', 'not-ready')
        )
        $unready.exitCode | Should -Not -Be 0
        $unready.output | Should -BeNullOrEmpty
        Test-Path -LiteralPath $script:Work | Should -BeFalse
        (Invoke-WorkJson -ArgumentList $arguments).reviewerCount | Should -Be 1
    }

    It 'reports an omitted deleted path without creating review work' {
        Write-WorkJson -Path $script:GroupedInventory -Value @($script:Groups[0])
        $result = Invoke-WorkCommand -ArgumentList @(
            'create-review-work', '--inventory-path', $script:Inventory,
            '--file-groups-path', $script:GroupedInventory,
            '--focus-area-directory', $script:Areas, '--output-directory', $script:Work
        )
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.errors | Should -Match 'deleted\.rs'
        Test-Path -LiteralPath $script:Work | Should -BeFalse
    }

    It 'publishes inputs, journals coverage and findings, and validates the finished work' {
        $inventory = Join-Path $script:Root 'published-inventory.json'
        $groups = Join-Path $script:Root 'published-groups.json'
        $input = Invoke-WorkJson -ArgumentList @(
            'set-review-input', '--kind', 'inventory', '--input-path', $script:Inventory, '--path', $inventory
        )
        $input.count | Should -Be $script:Files.Count
        $input = Invoke-WorkJson -ArgumentList @(
            'set-review-input', '--kind', 'groups', '--input-path', $script:GroupedInventory,
            '--path', $groups, '--inventory-path', $inventory
        )
        $input.count | Should -Be $script:Groups.Count
        $work = Invoke-WorkJson -ArgumentList @(
            'create-review-work', '--inventory-path', $inventory, '--file-groups-path', $groups,
            '--focus-area-directory', $script:Areas, '--output-directory', $script:Work
        )
        $work.fileCount | Should -Be $script:Files.Count
        $work.groupCount | Should -Be $script:Groups.Count
        $manifest = Get-Content -LiteralPath $work.manifestPath -Raw | ConvertFrom-Json
        $claim = Invoke-WorkJson -ArgumentList @('next-review-item', '--queue-path', $work.focusAreaQueuePath)
        $worker = $claim.item
        $worker.reviewerId | Should -BeExactly $manifest.reviewers[0].reviewerId
        (Invoke-WorkJson -ArgumentList @('next-review-item', '--queue-path', $work.focusAreaQueuePath)).exhausted | Should -BeTrue

        $queue = $worker.fileGroupQueuePath
        $queueState = Invoke-WorkJson -ArgumentList @(
            'initialize-review-queue', '--inventory-path', $work.inventoryPath,
            '--file-groups-path', $work.fileGroupsPath, '--queue-path', $queue
        )
        $queueState.groupCount | Should -Be $script:Groups.Count
        $queueState.fileCount | Should -Be $script:Files.Count
        $incomplete = Invoke-WorkJson -ArgumentList @(
            'validate-review-work', '--artifact-directory', $script:Findings, '--manifest-path', $work.manifestPath
        )
        $incomplete.isValid | Should -BeFalse
        @($incomplete.missingReviewers) | Should -Be @($worker.reviewerId)
        $created = Invoke-WorkJson -ArgumentList @(
            'initialize-finding', '--output-directory', $script:Findings,
            '--reviewer-id', $worker.reviewerId, '--focus-area', $worker.focusArea, '--queue-path', $queue
        )
        $artifact = $created.artifactPath
        $premature = Invoke-WorkCommand -ArgumentList @('complete-finding', '--artifact-path', $artifact, '--queue-path', $queue)
        $premature.exitCode | Should -Not -Be 0
        $premature.output | Should -BeNullOrEmpty
        (Get-Content -LiteralPath $artifact -Raw | ConvertFrom-Json).status | Should -Be 'open'

        $finding = Join-Path $script:Root 'finding.json'
        Write-WorkJson -Path $finding -Value @{
            title = 'Preserve data'; description = 'A concrete problem.'; reasoning = 'Evidence supports this.'
            consequence = 'Data can be lost.'; recommendedAction = 'Retain data.'
            anchor = @{ kind = 'file'; path = 'new [file].rs' }
        }
        $findingId = (Invoke-WorkJson -ArgumentList @(
            'add-finding', '--artifact-path', $artifact, '--finding-path', $finding, '--origin', 'reviewer'
        )).findingId
        foreach ($group in $script:Groups) {
            $claim = Invoke-WorkJson -ArgumentList @('next-review-item', '--queue-path', $queue)
            $claim.exhausted | Should -BeFalse
            $claim.item.groupId | Should -BeExactly $group.groupId
            @($claim.item.files.path) | Should -Be @($group.files.path)
            $batch = @{
                schemaVersion = 1; snapshot = $claim.snapshot; groupId = $claim.item.groupId
                assessments = @($claim.item.files | ForEach-Object {
                    @{ groupId = $claim.item.groupId; path = $_.path; disposition = 'reviewed'; scope = ''; reason = '' }
                })
                findingIds = @(if ($group.groupId -eq 'group-001') { $findingId })
            }
            $batchPath = Join-Path $script:Root "$($group.groupId).json"
            Write-WorkJson -Path $batchPath -Value $batch
            $completed = Invoke-WorkJson -ArgumentList @(
                'complete-review-batch', '--queue-path', $queue, '--inventory-path', $work.inventoryPath,
                '--file-groups-path', $work.fileGroupsPath, '--artifact-path', $artifact, '--batch-path', $batchPath
            )
            $completed.groupId | Should -BeExactly $group.groupId
        }
        (Invoke-WorkJson -ArgumentList @('next-review-item', '--queue-path', $queue)).exhausted | Should -BeTrue
        (Invoke-WorkJson -ArgumentList @('complete-finding', '--artifact-path', $artifact, '--queue-path', $queue)).status | Should -BeExactly 'complete'
        $validation = Invoke-WorkJson -ArgumentList @(
            'validate-review-work', '--artifact-directory', $script:Findings, '--manifest-path', $work.manifestPath
        )
        $validation.isValid | Should -BeTrue
        $finished = Get-Content -LiteralPath $artifact -Raw | ConvertFrom-Json
        $finished.findings[0].id | Should -BeExactly $findingId
        (Get-Content -LiteralPath $queue -Raw | ConvertFrom-Json).completedGroupIds.Count | Should -Be $script:Groups.Count
    }
}

Describe 'Focus-area worker instructions' {
    It 'treats guideline departures as eligible and explains consequences with proportionate remedies' {
        $review = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'review.md') -Raw
        $quality = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'qualitymaxx.md') -Raw
        foreach ($prompt in @($review, $quality)) {
            $prompt | Should -Match 'verified, unjustified departure from an applicable guideline is sufficient grounds'
            $prompt | Should -Match 'implementation effort, revalidation effort, and recurring maintenance burden'
            $prompt | Should -Match 'Do not invent consequences or exaggerate their likelihood'
            $prompt | Should -Not -Match 'offers no concrete benefit|against its demonstrated benefit'
        }
        $review | Should -Match '\| `consequence` \| yes \|.*reader who does not share our guidelines'
        $review | Should -Match 'quantified harm or a demonstrated runtime defect is not required'
        $review | Should -Match 'find a smaller one; omit the finding only when no proportionate, actionable recommendation remains'
        $quality | Should -Match 'Repair an incomplete explanation'
        $quality | Should -Match 'the departure is justified'
        $quality | Should -Match 'after considering simpler alternatives'
        [regex]::Matches($quality, 'Remove a finding only when').Count | Should -Be 1
        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $synthesis = [regex]::Match($skill, '(?s)# Phase 5: synthesis(.*?)## Pre-audit refresh').Groups[1].Value
        $synthesis | Should -Match 'same eligibility and proportionate-remedy rules as an individual finding'
        $synthesis | Should -Match "without independently proving the guideline's value"
    }

    It 'requires rolling concurrency, default context, passive compaction notes, and granular resume' {
        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $phase = [regex]::Match($skill, '(?s)# Phase 2: review(.*?)# Phase 3:').Groups[1].Value
        $phase | Should -Match 'rolling pool of up to 16 concurrent workers'
        $phase | Should -Match 'context_tier: "default"`, including retries'
        $phase | Should -Match 'Do not claim work before a slot is available'
        $phase | Should -Match 'nonblocking/background mode'
        $phase | Should -Match 'before awaiting any worker result'
        $phase | Should -Match 'completed groups are skipped'
        $phase | Should -Not -Match 'matrix|context_tier: "long_context"'
        $phase | Should -Match 'one findings artifact per focus-area worker, not per group or attempt'
        $phase | Should -Match 'all.*finding|every emitted finding'
        $phase | Should -Not -Match 'COMPACTION_ACTION|RETRY_FLAG|resetting both|retry its full pass'
        $skill | Should -Match 'Do not set optional subagent token budgets'
        $skill | Should -Match 'prepare-file-groups.md'
        $review = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'review.md') -Raw
        $review | Should -Match 'do not try to force it, wait for it, or require it'
        $review | Should -Match 'Discard the contents of files already read rather than summarizing'
        $review | Should -Match 'includes focus-area guidance'
        $review | Should -Match "re-read the review instructions and focus-area file through the assignment's prepared bounded reader"
        $review | Should -Match 'Do not recreate your queue or artifact on compaction'
        $review | Should -Match 'record-assessment-batch.md'
        $review | Should -Match 'do not launch a process per file'
        $review | Should -Match '\{\{RESUME_FLAG\}\}'
        $grouping = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'prepare-file-groups.md') -Raw
        $grouping | Should -Match '20 files or 100 000 bytes'
        $grouping | Should -Match 'Include every file exactly once'
    }

    It 'accepts diff-first review and schema-aware queries without mandatory whole-file reads' {
        $review = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'review.md') -Raw
        $review | Should -Match 'start with the diff and analyze the changed content in every non-excluded file'
        $review | Should -Match 'surrounding ranges, related files, or the whole file'
        $review | Should -Match 'full-file reads are optional'
        $review | Should -Match 'For a review without a diff, analyze the requested content'
        $review | Should -Match 'known schema, such as JSON'
        $review | Should -Match 'prefer read-only schema-aware queries'
        $review | Should -Match 'truncates evidence needed for the analysis, retrieve the missing evidence'
        $review | Should -Match 'reassess it using the same diff-first, targeted-read, and query approach'
        $review | Should -Match 'Targeted reads or queries are not incomplete coverage'
        $review | Should -Match 'Every PR context read must follow `pwsh-calls/query-pr-context.md`'

        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $skill | Should -Match 'full-file reads are optional'
        $skill | Should -Match 'Diffs, targeted reads, and structured-data queries can satisfy coverage'
        $skill | Should -Match 'same targeted-inspection policy'
        foreach ($relativePath in @(
            'review.md', 'SKILL.md', 'docs\design.md', 'docs\implementation.md', 'workflow-data.md',
            'data-guides\review-coverage.md',
            'pwsh-calls\get-next-review-file-group.md',
            'pwsh-calls\complete-review-file-group.md',
            'pwsh-calls\complete-finding-artifact.md'
        )) {
            $content = Get-Content -LiteralPath (Join-Path $script:SkillRoot $relativePath) -Raw
            $requiresFullRead = $content -match '\b(read (every non-excluded file|the file|it) in full|fully read|full reading|full-file\s+analysis|full-read)\b'
            $requiresFullRead | Should -BeFalse -Because "$relativePath must allow targeted inspection"
        }
    }

    It 'requires explicit scope evidence for every skipped file without bypassing group completion' {
        $review = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'review.md') -Raw
        $review | Should -Match 'only when every guideline section clearly excludes it'
        $review | Should -Match 'If any section might apply, analyze the requested scope'
        $review | Should -Match 'uncertainty requires investigation, not a skip'
        $review | Should -Match 'record-file-assessment.md'
        $review | Should -Match 'Exclusions require the path, controlling scope, and concrete reason'
        $review | Should -Match 'Do not remove files from the inventory or omit queue groups'
        $review | Should -Match 'Assess deleted files using pre-change paths, content, and removal diffs'
        $review | Should -Match 'Assess both entries of a rename independently'
        $review | Should -Not -Match 'Do not skip files or groups based on predicted relevance'

        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $skill | Should -Match 'Verify exclusions against all sections in mixed-scope focus-area files'
        $skill | Should -Match 'Normal queue exhaustion does not complete this explicit repair assignment'
        $review | Should -Match 'On resume, read the checkpoint and existing journal'
        $review | Should -Match 'Earlier assessments survive queue initialization'
        $completion = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\complete-review-file-group.md'
        ) -Raw
        $completion | Should -Match 'journaled explicit scope exclusion'
        $review | Should -Match 'groups with no findings or only excluded files'
        $completion | Should -Match 'legacy per-file recovery path'
        $completion | Should -Match 'can satisfy coverage without a whole-file read'
        $claim = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\get-next-review-file-group.md'
        ) -Raw
        $claim | Should -Match 'analyze the requested scope in every non-excluded file'
        $artifact = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\complete-finding-artifact.md'
        ) -Raw
        $artifact | Should -Match 'journaled explicit scope exclusion'
        $artifact | Should -Match 'Complete any targeted coverage repair'
        $artifact | Should -Match 'can satisfy coverage without a whole-file read'
    }

    It 'carries original scope and provenance through generation and verifies scopes before storage' {
        $preparation = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'prepare-focus-areas.md') -Raw
        $preparation | Should -Match 'Derive scope from the materialized source files'
        $preparation | Should -Match 'complete `applyTo` glob list'
        $preparation | Should -Match 'inherited by every section in that file'
        $preparation | Should -Match 'in any enclosing section'
        $preparation | Should -Match 'original scope declarations and frontmatter values verbatim'
        $preparation | Should -Match 'Alternatives within an `applyTo` list remain alternatives'
        $preparation | Should -Match 'Expanded guidance inherits the scope of its referring instruction'
        $preparation | Should -Match 'assign only one writer to each expanded target'
        $preparation | Should -Match 'including later-discovered referrers'
        $preparation | Should -Match 'Do not remove review criteria for authored agent workflows'
        $preparation | Should -Match 'complete alternative referring contexts'
        $preparation | Should -Match 'retaining all source attribution and distinct applicability contexts as alternatives'
        $preparation | Should -Match 'Scope verification happens before storage in Stage 3'
        $preparation | Should -Match 'Structural validation cannot establish these semantic properties'
    }
}

Describe 'Audit presentation instructions' {
    BeforeAll {
        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $script:AuditPresentation = [regex]::Match($skill, '(?s)# Phase 7: audit(.*?)# Phase 8:').Groups[1].Value
        $script:InSessionAudit = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'audit-in-session.md') -Raw
    }

    It 'pairs the audited capture with the review clone when building the approved set' {
        $script:AuditPresentation | Should -Match 'pwsh-calls/build-approved-findings.md'
        $recipe = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\build-approved-findings.md'
        ) -Raw
        $command = [regex]::Match($recipe, '(?s)```text\s+(.*?)```').Groups[1].Value
        $command | Should -Match 'ss-ai workflow approved build'
        $command | Should -Match '--context-path "\{\{CONTEXT_PATH\}\}"'
        $command | Should -Match '--repository-path "\{\{REPOSITORY_PATH\}\}"'
        $recipe | Should -Match '\| `REPOSITORY_PATH` \| Local review clone holding the commits named by the audited capture\. \|'
        $recipe | Should -Match 'pre-audit refresh or a return to audit after revalidation'
        $recipe | Should -Match 'With no pull request, omit both parameters'
        $recipe | Should -Match 'Handle any excluded anchors as phase 8 describes'
    }

    It 'repeats the audited PR identity at every audit question in either presentation mode' {
        $script:AuditPresentation | Should -Match '`id`, `title`, and `pullRequestUrl` for presentation'
        $script:AuditPresentation | Should -Match 'repeat the audited snapshot''s PR number \(`id`\) and full `title` as a named hyperlink to `pullRequestUrl`'
        $script:AuditPresentation | Should -Match 'in ordinary session text immediately before every audit question, regardless of the publication destination'
        $script:AuditPresentation | Should -Match 'Also repeat the PR number and title in the question itself'
        $script:AuditPresentation | Should -Match 'document and in-session audits, including resumed or amended presentations and the final merge question'
        $script:AuditPresentation | Should -Match 'Omit it when the review has no associated pull request'
        $script:InSessionAudit | Should -Match "Apply phase 7's PR reminder in both session text and the question UI for every per-finding and final merge question"
    }

    It 'offers an opt-in walkthrough after the applicable document approval choices' {
        $script:AuditPresentation | Should -Match 'Offer the combined document by default'
        $script:AuditPresentation | Should -Match 'Omit the merge approval choice when.*isAuthoredBySelf.*or the destination is not the pull request'
        $script:AuditPresentation | Should -Match 'selecting it approves nothing'
        $script:AuditPresentation | Should -Match 'Load that guide only when the user chooses the walkthrough or resumes one'
        $script:InSessionAudit | Should -Match 'user selects `Review findings in-session` in phase 7'
        $script:AuditPresentation | Should -Not -Match 'Do not present the findings one by one|do not ask a separate question per finding'
    }

    It 'defines one literal document-audit question and ordered choice list for every presentation' {
        $question = [regex]::Match(
            $script:AuditPresentation, '(?s)```text\r?\n(.*?)\r?\n```'
        ).Groups[1].Value -replace "`r`n", "`n"
        $question | Should -Be (
            'PR #{{PR_ID}}: {{PR_TITLE}}' + "`n`n" +
            'How would you like to proceed with the linked audit? Provide any per-finding feedback using finding IDs.'
        )
        $choices = [regex]::Matches(
            $script:AuditPresentation, '(?m)^[1-3]\. `([^`]+)`\r?$'
        ) | ForEach-Object { $_.Groups[1].Value }
        ($choices -join '|') | Should -Be 'Approve findings|Approve findings and approve merge|Review findings in-session'
        $script:AuditPresentation | Should -Match 'Replace `\{\{PR_ID\}\}` and `\{\{PR_TITLE\}\}` with the audited snapshot''s `id` and full `title`'
        $script:AuditPresentation | Should -Match 'Omit the PR reminder line and its following blank line when the review has no associated pull request'
        $script:AuditPresentation | Should -Match 'preserve the question wording, choice labels, and relative order exactly, including on resumed or amended document presentations'
        $script:AuditPresentation | Should -Match 'Do not paraphrase, reorder, add recommendation suffixes, or add explicit choices for feedback or Other'
        $script:AuditPresentation | Should -Match 'use the tool''s free-form response for feedback'
    }

    It 'uses pending identifiers and existing records rather than a second audit queue' {
        $script:InSessionAudit | Should -Match 'get-next-auto-approval-finding.md` before each iteration and on resumption'
        $script:InSessionAudit | Should -Match 'Use only its `auditFindingIds`, in their returned order'
        $script:InSessionAudit | Should -Match 'describe policy classification, not human-audit completion'
        $script:InSessionAudit | Should -Match 'settled and policy-approved findings are not presented again'
        $script:InSessionAudit | Should -Match 'Do not rerun classification or create a separate queue'
        $script:AuditPresentation | Should -Match 'continue with `audit-in-session.md` instead of repeating the document-choice question'
        $script:InSessionAudit | Should -Match "an interruption preserves the existing presentation's progress instead of resetting it"
    }

    It 'shows the complete rendered finding in session text before its own approval question' {
        $script:InSessionAudit | Should -Match 'Show `content-overview.md` in ordinary session text once before the findings'
        $script:InSessionAudit | Should -Match 'findings` entries \(`findingId` and `path`\)'
        $script:InSessionAudit | Should -Match 'complete any truncated read before presentation'
        $script:InSessionAudit | Should -Match '(?s)complete rendered Markdown verbatim in an ordinary assistant message.*Invoke the user-question tool for that finding alone'
        $script:InSessionAudit | Should -Match 'Do not summarize, truncate, paraphrase, or substitute a file link'
        $script:InSessionAudit | Should -Match 'asking "Approve finding <id>\?" with `Approve` as the explicit choice'
        $script:InSessionAudit | Should -Match "tool's built-in Other/free-form response"
    }

    It 'persists only settled findings and keeps feedback or unanswered questions from advancing' {
        $script:InSessionAudit | Should -Match 'only the displayed finding, never other findings or the merge'
        $script:InSessionAudit | Should -Match 'record-finding-decision.md` before selecting another finding'
        $script:InSessionAudit | Should -Match 'preserve verbatim feedback with `pwsh-calls/record-audit-feedback.md`'
        $script:InSessionAudit | Should -Match 'a clarification or amendment leaves the finding undecided'
        $script:InSessionAudit | Should -Match 'ask about the same finding again after clarification or amendment'
        $script:InSessionAudit | Should -Match 'An unanswered question pauses the workflow without a new verdict or advancement'
        $script:AuditPresentation | Should -Match 'an in-session `Approve` settles only the displayed finding'
        $script:AuditPresentation | Should -Match 'In in-session mode, redisplay the current finding'
    }

    It 'keeps a resumable final merge gate separate from per-finding approvals and publication' {
        $script:InSessionAudit | Should -Match 'Mark that question pending before asking about findings'
        $script:InSessionAudit | Should -Match 'destination is the pull request and `isAuthoredBySelf` is false'
        $script:InSessionAudit | Should -Match 'ask a separate final question offering "approve the pull request merge" and "do not approve the pull request merge"'
        $script:InSessionAudit | Should -Match 'even if the user rejected every finding'
        $script:InSessionAudit | Should -Match 'ask it if still pending even when `auditFindingIds` is empty'
        $script:InSessionAudit | Should -Match 'Do not offer merge approval for self-authored pull requests or other destinations'
        $script:InSessionAudit | Should -Match 'Do not write merge state for an unanswered question'
        $script:InSessionAudit | Should -Match 'only after every finding and any applicable final merge question are settled'
        $script:InSessionAudit | Should -Match 'normal phase 8 freshness check before publication'
        $script:AuditPresentation | Should -Match 'unless an in-session audit still awaits its final merge decision'
        $script:AuditPresentation | Should -Match 'In-session per-finding approvals do not settle the audit or write merge state'
    }

    It 'preserves Windows paths and keeps them out of the question UI' {
        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $phase = [regex]::Match($skill, '(?s)# Phase 7: audit(.*?)# Phase 8:').Groups[1].Value
        $phase | Should -Match 'absolute `htmlPath` as inline code'
        $phase | Should -Match 'absolute `auditDocumentPath` as inline code'
        $phase | Should -Match 'separator before `\.copilot`'
        $phase | Should -Match 'do not repeat its URI or absolute path in the question UI'

        $markdownCall = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\render-findings.md'
        ) -Raw
        $markdownCall | Should -Match 'auditDocumentPath` separately as an inline-code fallback'

        $htmlCall = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\render-report-html.md'
        ) -Raw
        $htmlCall | Should -Match 'absolute HTML path separately as inline code'

        $design = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'docs\design.md') -Raw
        $design | Should -Match 'links as inline code'
        $design | Should -Match 'rather than\s+repeating local paths'

        $implementation = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'docs\implementation.md'
        ) -Raw
        $implementation | Should -Match 'absolute paths separately as inline code'
        $implementation | Should -Match 'rather than repeating their URIs or local paths'
    }
}

Describe 'Merge approval refresh instructions' {
    BeforeAll {
        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $script:AuditPhase = [regex]::Match($skill, '(?s)# Phase 7: audit(.*?)# Phase 8:').Groups[1].Value
        $script:FreshnessPhase = [regex]::Match($skill, '(?s)# Phase 8: freshness check(.*?)# Phase 9:').Groups[1].Value
    }

    Describe 'Incremental freshness ordering' {
        It 'retires stale findings before policy or human presentation and retains the final gate' {
            $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
            $preAudit = [regex]::Match($skill, '(?s)## Pre-audit refresh(.*?)# Phase 6:').Groups[1].Value
            $preAudit | Should -Match 'before phase 6 policy classification or phase 7 human audit'
            $preAudit | Should -Match 'every current finding, not just approved findings'
            $preAudit | Should -Match 'retire fixed or invalidated findings before presentation'
            $preAudit | Should -Match 'including force-pushes, without assuming ancestry'
            $preAudit | Should -Match 'New and revised findings proceed through phase 6 normally'
            $preAudit | Should -Match 'Measure agent execution separately from the human response interval'
            $final = [regex]::Match($skill, '(?s)# Phase 8: freshness check(.*?)# Phase 9:').Groups[1].Value
            $final | Should -Match 'including after every human wait or destination decision'
            $final | Should -Match 'last checked capture, not always the initial capture'
            $final | Should -Match 'If `hasChanges` is false and no authorship decisions need reconsideration'
            $final | Should -Match 'skip repeated semantic revalidation'
            $final | Should -Match 'never substitution of an unchecked capture or bypass'
            $final | Should -Match 'strict narrowing.*retains the existing approval'
            $final | Should -Match 'mark each materially revised finding.*as `needs-audit`'
            $skill | Should -Match 'Preserve the audited path and rendered text while waiting'
        }
    }

    It 'asks for an initial merge decision without repeating it for an empty returned audit' {
        $script:AuditPhase | Should -Match 'this run has no recorded merge decision, ask whether'
        $script:AuditPhase | Should -Match 'freshness revalidation requires no new audit, retain that decision without asking again'
        $script:AuditPhase | Should -Not -Match 'still ask whether to also approve'
    }

    It 'carries approval after movement only when no findings need audit' {
        $script:FreshnessPhase | Should -Match 'After recording revalidation decisions, execute `pwsh-calls/get-next-auto-approval-finding.md` to refresh `auditFindingIds`'
        $script:FreshnessPhase | Should -Match 'If `auditFindingIds` is empty, retain the prior merge decision and do not return to phase 7 or ask for merge approval again'
        $script:FreshnessPhase | Should -Match 'This also applies when revalidation drops every finding'
        $script:FreshnessPhase | Should -Match 'When the source commit moved, use the carry-forward command'
        $script:FreshnessPhase | Should -Match 'When the source commit is unchanged, leave the merge-decision record unchanged'

        $recordCall = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\record-merge-approval.md'
        ) -Raw
        $recordCall | Should -Match 'carry an existing approval without asking the user again'
        $recordCall | Should -Match 'preserve the absence of\s+approval rather than asking again solely because the PR moved'
    }

    It 'requires a fresh merge decision whenever refresh produces findings needing audit' {
        $script:FreshnessPhase | Should -Match 'If that list is nonempty, return to phase 7 and require a fresh merge decision alongside the new audit, even if only discussion changed'
        $script:FreshnessPhase | Should -Match 'Do not carry an earlier merge approval in that case'
        $script:AuditPhase | Should -Match 'if the user settles that audit without explicitly approving the merge, record `DoNotApprove`'
        $script:AuditPhase | Should -Match 'Clarification or an unanswered question does not settle the audit and writes no merge state'
    }
}
