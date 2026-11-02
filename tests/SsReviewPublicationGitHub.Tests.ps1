#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

Describe 'Native GitHub follow-up publication' {
    foreach ($scenario in @(
            @{ Scenario = 'first reopen fails and second succeeds'; Mode = 'normal'; FailSecond = $false }
            @{ Scenario = 'both reopens fail'; Mode = 'normal'; FailSecond = $true }
            @{ Scenario = 'successful reopens retain a succeeded review'; Mode = 'successReopen'; FailSecond = $false }
            @{ Scenario = 'open follow-up threads are not reopened'; Mode = 'openFollowups'; FailSecond = $false }
            @{ Scenario = 'out-of-range anchor is published and recorded at file level'; Mode = 'downgrade'; FailSecond = $false }
            @{ Scenario = 'single-line anchor is sent without a range'; Mode = 'singleLine'; FailSecond = $false }
            @{ Scenario = 'submission failure never reopens threads'; Mode = 'submitFailure'; FailSecond = $false }
            @{ Scenario = 'missing draft findings prevent submission'; Mode = 'missingDraft'; FailSecond = $false }
            @{ Scenario = 'draft discard failure preserves the original readback failure'; Mode = 'missingDraftDiscardFailure'; FailSecond = $false }
            @{ Scenario = 'missing completed findings prevent reopening'; Mode = 'missingCompleted'; FailSecond = $false }
            @{ Scenario = 'pending completed review prevents reopening'; Mode = 'pendingCompleted'; FailSecond = $false }
            @{ Scenario = 'explicit approval publishes a summary without findings'; Mode = 'emptyApproval'; FailSecond = $false }
            @{ Scenario = 'Unicode finding survives publication and journal readback'; Mode = 'unicode'; FailSecond = $false }
            @{ Scenario = 'closed pull request without authorization'; Mode = 'closed'; FailSecond = $false }
            @{ Scenario = 'closed pull request with explicit authorization'; Mode = 'closedAllowed'; FailSecond = $false }
            @{ Scenario = 'closed pull request with a changed source'; Mode = 'closedSourceChanged'; FailSecond = $false }
            @{ Scenario = 'source changed before publication'; Mode = 'sourceChanged'; FailSecond = $false }
            @{ Scenario = 'merge base changed on an open pull request'; Mode = 'baseChanged'; FailSecond = $false }
            @{ Scenario = 'closed-publication option does not waive open merge-base freshness'; Mode = 'baseChangedAllowed'; FailSecond = $false }
            @{ Scenario = 'discussion evidence changed before publication'; Mode = 'evidenceChanged'; FailSecond = $false }
            @{ Scenario = 'existing draft without discard authority'; Mode = 'pending'; FailSecond = $false }
            @{ Scenario = 'existing draft with discard authority'; Mode = 'pendingAllowed'; FailSecond = $false }
            @{ Scenario = 'changed evidence never discards an existing draft'; Mode = 'evidenceChangedPending'; FailSecond = $false }
            @{ Scenario = 'changed source never discards an existing draft'; Mode = 'sourceChangedPending'; FailSecond = $false }
            @{ Scenario = 'changed merge base never discards an existing draft'; Mode = 'baseChangedPending'; FailSecond = $false }
            @{ Scenario = 'new discussion never discards an existing draft'; Mode = 'newThreadPending'; FailSecond = $false }
            @{ Scenario = 'unplaceable anchor never discards an existing draft'; Mode = 'badAnchorPending'; FailSecond = $false }
            @{ Scenario = 'malformed finding never discards an existing draft'; Mode = 'badFindingPending'; FailSecond = $false }
        )) {
        It "enforces the native publication scenario $($scenario.Scenario)" -Skip:$IsWindows -ForEach @($scenario) {
            $root = Join-Path $PSScriptRoot ('.publication-followups-' + [guid]::NewGuid().ToString('N'))
            [void] [IO.Directory]::CreateDirectory($root)
            try {
                $ssAi = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
                $pwsh = (Get-Process -Id $PID).Path
                $source = Join-Path $PSScriptRoot 'fixtures/ss-review-native-capture/github.json'
                $fixture = ConvertFrom-Json -AsHashtable -DateKind String -InputObject ([IO.File]::ReadAllText($source))
                $nodes = $fixture.responses.reviewThreads.data.repository.pullRequest.reviewThreads.nodes
                $nodes[0].isResolved = $Mode -ne 'openFollowups'
                $second = ConvertFrom-Json -AsHashtable -DateKind String -InputObject (
                    ConvertTo-Json -InputObject $nodes[0] -Depth 100 -Compress
                )
                $second.id = 'PRRT_2'
                $second.comments.nodes[0].id = 'PRRC_2'
                $second.comments.nodes[0].databaseId = 900002
                $second.comments.nodes[0].url = 'https://github.com/octo/widget/pull/7#discussion_PRRC_2'
                $fixture.responses.reviewThreads.data.repository.pullRequest.reviewThreads.nodes = @($nodes[0], $second)
                $fixturePath = Join-Path $root 'provider.json'
                $statePath = Join-Path $root 'provider-state.json'
                [IO.File]::WriteAllText($fixturePath, (ConvertTo-Json -InputObject $fixture -Depth 100 -Compress))
                [IO.File]::WriteAllText($statePath, '{"replies":[],"summary":null,"verdict":"COMMENTED","events":[],"snapshots":[]}')

                # The launcher never falls through to a real provider command.
                $provider = @'
    #Requires -Version 7.6
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $PSNativeCommandUseErrorActionPreference = $true
    $fixture = ConvertFrom-Json -AsHashtable -DateKind String -InputObject ([IO.File]::ReadAllText($env:SS_FIXTURE))
    $state = ConvertFrom-Json -AsHashtable -DateKind String -InputObject ([IO.File]::ReadAllText($env:SS_STATE))
    $priorEvents = $state.events.Count
    $route = $args[1]
    $responses = $fixture.responses
    if ($route -eq 'graphql') {
        $request = ConvertFrom-Json -AsHashtable -InputObject ([Console]::In.ReadToEnd())
        $query = $request.query
        $variables = $request.variables
        if ($query.Contains('states:[PENDING]')) {
            $pending = if ($env:SS_PENDING -eq 'yes') { @(@{ id = 'existing'; state = 'PENDING' }) } else { @() }
            $response = @{ data = @{ repository = @{ pullRequest = @{ reviews = @{ nodes = @($pending) } } } } }
        } elseif ($query.Contains('addPullRequestReviewThreadReply(')) {
            $id = 'reply-' + ($state.replies.Count + 1)
            $state.replies += @{
                thread = $variables.threadId; id = $id; body = $variables.body
                path = 'src/café.rs'; line = 5; startLine = 4; subject = 'LINE'
            }
            $state.events += 'reply:' + $variables.threadId
            $response = @{ data = @{ addPullRequestReviewThreadReply = @{ comment = @{ id = $id } } } }
        } elseif ($query.Contains('addPullRequestReviewThread(')) {
            if ($env:SS_MODE -eq 'singleLine') {
                if ($variables.Contains('startLine') -or $variables.line -ne 1 -or $variables.side -ne 'RIGHT') {
                    throw 'Expected a single-line request without a range'
                }
                $subject = 'LINE'
                $line = 1
            } else {
                if ($variables.Contains('line') -or $variables.Contains('side')) {
                    throw 'Expected a positionless file request'
                }
                $subject = 'FILE'
                $line = $null
            }
            $state.replies += @{
                thread = 'new-thread'; id = 'new-comment'; body = $variables.body
                path = $variables.path; line = $line; startLine = $null; subject = $subject
            }
            $state.events += 'file:' + $variables.path
            $response = @{ data = @{ addPullRequestReviewThread = @{ thread = @{
                id = 'new-thread'; comments = @{ nodes = @(@{ id = 'new-comment' }) }
            } } } }
        } elseif ($query.Contains('addPullRequestReview(')) {
            $state.events += 'start'
            $response = @{ data = @{ addPullRequestReview = @{ pullRequestReview = @{ id = 'review'; state = 'PENDING' } } } }
        } elseif ($query.Contains('submitPullRequestReview(')) {
            $state.events += 'submit'
            if ($env:SS_MODE -eq 'submitFailure') {
                $response = @{ errors = @(@{ message = 'fixture submission failure' }) }
            } else {
                $state.summary = $variables.body
                $state.verdict = if ($variables.event -eq 'APPROVE') { 'APPROVED' } else { 'COMMENTED' }
                $response = @{ data = @{ submitPullRequestReview = @{ pullRequestReview = @{
                    id = 'review'; state = $state.verdict; url = 'https://github.com/octo/widget/pull/7#review'
                } } } }
            }
        } elseif ($query.Contains('deletePullRequestReview(')) {
            $state.events += 'discard:' + $variables.reviewId
            $response = if ($env:SS_MODE -eq 'missingDraftDiscardFailure') {
                @{ errors = @(@{ message = 'fixture discard failure' }) }
            } else {
                @{ data = @{ deletePullRequestReview = @{ pullRequestReview = @{ id = $variables.reviewId } } } }
            }
        } elseif ($query.Contains('unresolveReviewThread(')) {
            $state.events += 'reopen:' + $variables.threadId
            if (($variables.threadId -eq 'PRRT_1' -and $env:SS_MODE -ne 'successReopen') -or $env:SS_FAIL_SECOND -eq 'yes') {
                $response = @{ errors = @(@{ message = 'fixture refusal' }) }
            } else {
                $response = @{ data = @{ unresolveReviewThread = @{ thread = @{ id = $variables.threadId; isResolved = $false } } } }
            }
        } elseif ($query.Contains('node(id:$reviewId)')) {
            $submitted = $null -ne $state.summary
            $comments = @($state.replies | ForEach-Object {
                @{
                    id = $_.id; state = $(if ($submitted) { 'SUBMITTED' } else { 'PENDING' })
                    path = $_.path; line = $_.line; startLine = $_.startLine; subjectType = $_.subject; body = $_.body
                }
            })
            if (($env:SS_MODE -in 'missingDraft', 'missingDraftDiscardFailure' -and -not $submitted) -or
                ($env:SS_MODE -eq 'missingCompleted' -and $submitted)) {
                $comments = @()
            }
            $response = @{ data = @{ node = @{
                id = 'review'; state = $(if ($submitted -and $env:SS_MODE -ne 'pendingCompleted') { $state.verdict } else { 'PENDING' })
                url = 'https://github.com/octo/widget/pull/7#review'
                body = $(if ($submitted) { $state.summary } else { '' })
                comments = @{ pageInfo = @{ hasNextPage = $false; endCursor = $null }; nodes = $comments }
            } } }
        } elseif ($query.Contains('reviewThreads(') -and $state.replies.Count -gt 0) {
            $threads = @($state.replies | ForEach-Object {
                @{
                    id = $_.thread; diffSide = 'RIGHT'; subjectType = $_.subject
                    comments = @{ pageInfo = @{ hasNextPage = $false; endCursor = $null }; nodes = @(@{ id = $_.id }) }
                }
            })
            $response = @{ data = @{ repository = @{ pullRequest = @{ reviewThreads = @{
                pageInfo = @{ hasNextPage = $false; endCursor = $null }; nodes = $threads
            } } } } }
        } elseif ($query.Contains('reviewThreads(')) {
            $response = $responses.reviewThreads
        } elseif ($query.Contains('timelineItems(')) {
            $response = $responses.timeline
        } elseif ($query.Contains('reviews(')) {
            $response = $responses.reviewEdits
        } elseif ($query.Contains('comments(')) {
            $response = $responses.issueEdits
        } else {
            throw "Unexpected GraphQL request: $query"
        }
    } else {
        switch -Wildcard ($route) {
            'user' { $response = $responses.user }
            'repos/octo/widget/pulls/7' {
                $response = $responses.pullRequest
                $response.html_url = 'https://github.com/octo/widget/pull/7'
            }
            'repos/octo/widget/pulls/7/reviews?per_page=100' { $response = ,@($responses.reviews) }
            'repos/octo/widget/issues/7/comments?per_page=100' { $response = ,@($responses.issueComments) }
            'repos/octo/widget/pulls/7/requested_reviewers?per_page=100' { $response = ,@($responses.requestedReviewers) }
            'repos/octo/widget/compare/*' { $response = $responses.compare }
            default { throw "Unexpected provider route: $route" }
        }
    }
    $journalPath = Join-Path $env:SS_OUTPUT 'publication-result.json'
    # Sample durable intent inside the provider before the write response completes the operation.
    if ($state.events.Count -gt $priorEvents -and [IO.File]::Exists($journalPath)) {
        $journal = ConvertFrom-Json -AsHashtable -InputObject ([IO.File]::ReadAllText($journalPath))
        $state.snapshots += @{
            event = $state.events[-1]; inFlight = $journal.inFlight; operations = $journal.operations
        }
    }
    [IO.File]::WriteAllText($env:SS_STATE, (ConvertTo-Json -InputObject $state -Depth 100 -Compress))
    [Console]::Out.Write((ConvertTo-Json -InputObject $response -Depth 100 -Compress))
'@
                $providerPath = Join-Path $root 'provider.ps1'
                [IO.File]::WriteAllText($providerPath, $provider)
                $ghPath = Join-Path $root 'gh'
                [IO.File]::WriteAllText($ghPath, @'
#!/bin/sh
    exec "$SS_PWSH" -NoProfile -NonInteractive -File "$SS_PROVIDER" "$@"
'@
                )
                $gitPath = Join-Path $root 'git'
                [IO.File]::WriteAllText($gitPath, @'
#!/bin/sh
    case "$*" in
      *cat-file*) printf 'commit\n' ;;
      *diff*) printf 'diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n context\n' ;;
      *) printf '.git\n' ;;
    esac
'@
                )
                [IO.File]::SetUnixFileMode($ghPath, [IO.UnixFileMode] 'UserRead, UserWrite, UserExecute')
                [IO.File]::SetUnixFileMode($gitPath, [IO.UnixFileMode] 'UserRead, UserWrite, UserExecute')

                function Invoke-FakePublicationCommand {
                    param([string[]] $Arguments)
                    $start = [Diagnostics.ProcessStartInfo]::new($ssAi)
                    $start.UseShellExecute = $false
                    $start.RedirectStandardOutput = $true
                    $start.RedirectStandardError = $true
                    $start.Environment['PATH'] = $root
                    $start.Environment['SS_PWSH'] = $pwsh
                    $start.Environment['SS_PROVIDER'] = $providerPath
                    $start.Environment['SS_FIXTURE'] = $fixturePath
                    $start.Environment['SS_STATE'] = $statePath
                    $start.Environment['SS_OUTPUT'] = Join-Path $root 'output'
                    $start.Environment['SS_MODE'] = $Mode
                    $start.Environment['SS_FAIL_SECOND'] = if ($FailSecond) { 'yes' } else { 'no' }
                    $start.Environment['SS_PENDING'] = if ($Mode -like '*Pending' -or $Mode -in 'pending', 'pendingAllowed') { 'yes' } else { 'no' }
                    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
                    $process = [Diagnostics.Process]::Start($start)
                    try {
                        $stdout = $process.StandardOutput.ReadToEndAsync()
                        $stderr = $process.StandardError.ReadToEndAsync()
                        $process.WaitForExit()
                        return @{
                            ExitCode = $process.ExitCode
                            Output = $stdout.GetAwaiter().GetResult()
                            Error = $stderr.GetAwaiter().GetResult()
                        }
                    } finally {
                        $process.Dispose()
                    }
                }

                $captureDir = Join-Path $root 'capture'
                $created = Invoke-FakePublicationCommand @(
                    'context', 'create', '--url', 'https://github.com/octo/widget/pull/7',
                    '--output-directory', $captureDir, '--now-utc', '2026-08-04T12:00:00Z'
                )
                $created.ExitCode | Should -Be 0 -Because $created.Error
                $contextPath = (ConvertFrom-Json -AsHashtable -InputObject $created.Output).contextPath
                $captured = ConvertFrom-Json -AsHashtable -DateKind String -InputObject ([IO.File]::ReadAllText($contextPath))
                @($captured.threads.id) | Should -Be @('PRRT_1', 'PRRT_2')
                $resolution = if ($Mode -eq 'openFollowups') { 'active' } else { 'resolved' }
                @($captured.threads.resolution) | Should -Be @($resolution, $resolution)

                $findings = foreach ($number in 1, 2) {
                    [ordered]@{
                        schemaVersion = 1; id = "F-$number"; sourceFindingIds = @("F-$number")
                        origin = 'reviewer'; title = "Finding $number"; description = 'Problem'
                        reasoning = 'Reason'; consequence = 'Impact'; recommendedAction = 'Fix'
                        anchor = @{ kind = 'file'; path = 'src/lib.rs' }
                        followUp = @{ threadId = "PRRT_$number"; commentId = "PRRC_$number" }
                    }
                }
                $unicodeText = "Quoted $([char]0x2018)widget$([char]0x2019) $([char]0x2014) $([char]::ConvertFromUtf32(0x1F916))"
                if ($Mode -eq 'unicode') {
                    $findings[0].description = $unicodeText
                }
                $artifactPath = Join-Path $root 'findings.json'
                $decisionsPath = Join-Path $root 'decisions.json'
                $approvedPath = Join-Path $root 'approved.json'
                $repositoryPath = Join-Path $root 'repository'
                [void] [IO.Directory]::CreateDirectory($repositoryPath)
                [IO.File]::WriteAllText($artifactPath, (ConvertTo-Json -Depth 100 -Compress -InputObject @{
                    schemaVersion = 1; kind = 'consolidated'; findings = @($findings); removed = @()
                }))
                [IO.File]::WriteAllText($decisionsPath, (ConvertTo-Json -Depth 100 -Compress -InputObject @{
                    schemaVersion = 1; decisions = @(
                        @{ findingId = 'F-1'; decision = 'approved'; recordedUtc = '2026-08-04T12:00:00Z' }
                        @{ findingId = 'F-2'; decision = 'approved'; recordedUtc = '2026-08-04T12:00:00Z' }
                    )
                }))
                $built = Invoke-FakePublicationCommand @(
                    'workflow', 'approved', 'build', '--path', $artifactPath, '--decisions-path', $decisionsPath,
                    '--output-path', $approvedPath, '--context-path', $contextPath, '--repository-path', $repositoryPath
                )
                $built.ExitCode | Should -Be 0 -Because $built.Error
                switch ($Mode) {
                    { $_ -in 'closed', 'closedAllowed' } { $fixture.responses.pullRequest.state = 'closed' }
                    'sourceChanged' { $fixture.responses.pullRequest.head.sha = 'b' * 40 }
                    'sourceChangedPending' { $fixture.responses.pullRequest.head.sha = 'b' * 40 }
                    'closedSourceChanged' {
                        $fixture.responses.pullRequest.state = 'closed'
                        $fixture.responses.pullRequest.head.sha = 'b' * 40
                    }
                    'baseChanged' { $fixture.responses.compare.merge_base_commit.sha = 'd' * 40 }
                    'baseChangedAllowed' { $fixture.responses.compare.merge_base_commit.sha = 'd' * 40 }
                    'baseChangedPending' { $fixture.responses.compare.merge_base_commit.sha = 'd' * 40 }
                    'evidenceChanged' { $fixture.responses.pullRequest.title = 'Changed title' }
                    'evidenceChangedPending' { $fixture.responses.pullRequest.title = 'Changed title' }
                    'newThreadPending' {
                        $newThread = ConvertFrom-Json -AsHashtable -DateKind String -InputObject (
                            ConvertTo-Json -InputObject $second -Depth 100 -Compress
                        )
                        $newThread.id = 'PRRT_3'
                        $newThread.comments.nodes[0].id = 'PRRC_3'
                        $newThread.comments.nodes[0].databaseId = 900003
                        $fixture.responses.reviewThreads.data.repository.pullRequest.reviewThreads.nodes += $newThread
                    }
                }
                [IO.File]::WriteAllText($fixturePath, (ConvertTo-Json -InputObject $fixture -Depth 100 -Compress))
                if ($Mode -in 'badAnchorPending', 'badFindingPending', 'downgrade', 'singleLine', 'emptyApproval') {
                    $approved = ConvertFrom-Json -AsHashtable -InputObject ([IO.File]::ReadAllText($approvedPath))
                    if ($Mode -eq 'badAnchorPending') {
                        [void] $approved.findings[0].Remove('followUp')
                        $approved.findings[0].anchor.path = 'not-in-diff.rs'
                    } elseif ($Mode -eq 'badFindingPending') {
                        $approved.findings[0].title = ''
                    } elseif ($Mode -eq 'emptyApproval') {
                        $approved.findings = @()
                    } else {
                        [void] $approved.findings[0].Remove('followUp')
                        $lineNumber = if ($Mode -eq 'singleLine') { 1 } else { 99 }
                        $approved.findings[0].anchor = @{
                            kind = 'line'; path = 'src/lib.rs'; startLine = $lineNumber; endLine = $lineNumber; side = 'right'
                        }
                    }
                    [IO.File]::WriteAllText($approvedPath, (ConvertTo-Json -InputObject $approved -Depth 100 -Compress))
                }
                $diagnosticsPath = Join-Path $root 'diagnostics.json'
                [IO.File]::WriteAllText($diagnosticsPath, '{}')
                $outputPath = Join-Path $root 'output'
                $publishArguments = @(
                    'review', 'publish-final-review', '--url', 'https://github.com/octo/widget/pull/7',
                    '--approved-findings-path', $approvedPath, '--context-path', $contextPath,
                    '--diagnostics-path', $diagnosticsPath, '--repository-path', $repositoryPath,
                    '--output-directory', $outputPath, '--now-utc', '2026-08-04T12:00:00Z'
                )
                if ($Mode -in 'closedAllowed', 'closedSourceChanged', 'baseChangedAllowed') {
                    $publishArguments += '--publish-to-closed-pull-request'
                }
                if ($Mode -in 'pendingAllowed', 'evidenceChangedPending', 'sourceChangedPending',
                    'baseChangedPending', 'newThreadPending', 'badAnchorPending', 'badFindingPending') {
                    $publishArguments += '--discard-pending-review'
                }
                if ($Mode -eq 'emptyApproval') {
                    $mergePath = Join-Path $root 'merge.json'
                    [IO.File]::WriteAllText($mergePath, (ConvertTo-Json -Compress -InputObject @{
                        schemaVersion = 1; pullRequestUrl = 'https://github.com/octo/widget/pull/7'
                        approveMerge = $true; approvedSourceCommit = $captured.pullRequest.reviewedSourceCommit
                    }))
                    $publishArguments += @('--merge-approval-path', $mergePath)
                }
                $published = Invoke-FakePublicationCommand $publishArguments
                if ($Mode -eq 'emptyApproval') {
                    $published.ExitCode | Should -Be 0 -Because $published.Error
                    $outcome = ConvertFrom-Json -AsHashtable -InputObject $published.Output
                    $outcome.status | Should -Be 'succeeded'
                    $outcome.publishedFindingCount | Should -Be 0
                    $state = ConvertFrom-Json -AsHashtable -InputObject ([IO.File]::ReadAllText($statePath))
                    @($state.events) | Should -Be @('start', 'submit')
                    $state.verdict | Should -Be 'APPROVED'
                    $state.summary | Should -Not -BeNullOrEmpty
                    return
                }
                if ($Mode -in 'successReopen', 'openFollowups') {
                    $published.ExitCode | Should -Be 0 -Because $published.Error
                    $outcome = ConvertFrom-Json -AsHashtable -InputObject $published.Output
                    $outcome.status | Should -Be 'succeeded'
                    $outcome.publishedFindingCount | Should -Be 2
                    $state = ConvertFrom-Json -AsHashtable -InputObject ([IO.File]::ReadAllText($statePath))
                    $expected = @('start', 'reply:PRRT_1', 'reply:PRRT_2', 'submit')
                    if ($Mode -eq 'successReopen') { $expected += @('reopen:PRRT_1', 'reopen:PRRT_2') }
                    @($state.events) | Should -Be $expected
                    return
                }
                if ($Mode -in 'downgrade', 'singleLine') {
                    $published.ExitCode | Should -Be 0 -Because $published.Error
                    $outcome = ConvertFrom-Json -AsHashtable -InputObject $published.Output
                    $outcome.status | Should -Be 'succeeded'
                    $outcome.downgradedAnchorCount | Should -Be $(if ($Mode -eq 'downgrade') { 1 } else { 0 })
                    $journal = ConvertFrom-Json -AsHashtable -InputObject (
                        [IO.File]::ReadAllText((Join-Path $outputPath 'publication-result.json'))
                    )
                    $entry = @($journal.operations | Where-Object findingId -eq 'F-1')[0]
                    if ($Mode -eq 'downgrade') {
                        $entry.operation | Should -Be 'newFileThread'
                        $entry.wasDowngraded | Should -BeTrue
                        $entry.anchor.downgradeReason | Should -Match 'line 99'
                    } else {
                        $entry.operation | Should -Be 'newLineThread'
                        $entry.wasDowngraded | Should -BeFalse
                        $entry.anchor.endLine | Should -Be 1
                    }
                    return
                }
                $published.ExitCode | Should -Not -Be 0
                if ($Mode -eq 'submitFailure') {
                    $published.Output | Should -BeNullOrEmpty
                    $state = ConvertFrom-Json -AsHashtable -InputObject ([IO.File]::ReadAllText($statePath))
                    @($state.events) | Should -Be @('start', 'reply:PRRT_1', 'reply:PRRT_2', 'submit')
                    $journal = ConvertFrom-Json -AsHashtable -InputObject (
                        [IO.File]::ReadAllText((Join-Path $outputPath 'publication-result.json'))
                    )
                    @($journal.operations | Where-Object operation -eq 'completeReview')[0].outcome | Should -Be 'ambiguous'
                    return
                }
                if ($Mode -in 'missingDraft', 'missingDraftDiscardFailure', 'missingCompleted', 'pendingCompleted') {
                    $published.Output | Should -BeNullOrEmpty
                    $state = ConvertFrom-Json -AsHashtable -InputObject ([IO.File]::ReadAllText($statePath))
                    $expected = @('start', 'reply:PRRT_1', 'reply:PRRT_2')
                    if ($Mode -in 'missingDraft', 'missingDraftDiscardFailure') {
                        $expected += 'discard:review'
                    } else {
                        $expected += 'submit'
                    }
                    @($state.events) | Should -Be $expected
                    $journal = ConvertFrom-Json -AsHashtable -InputObject (
                        [IO.File]::ReadAllText((Join-Path $outputPath 'publication-result.json'))
                    )
                    $journal.status | Should -Be 'failed'
                    if ($Mode -eq 'missingDraftDiscardFailure') {
                        $journal.draftDiscarded | Should -BeFalse
                        $journal.draftDiscardMessage | Should -Match 'ambiguousWrite'
                        $journal.message | Should -Not -Be $journal.draftDiscardMessage
                    }
                    return
                }
                if ($Mode -eq 'unicode') {
                    $state = ConvertFrom-Json -AsHashtable -InputObject ([IO.File]::ReadAllText($statePath))
                    $journalPath = Join-Path $outputPath 'publication-result.json'
                    $bytes = [IO.File]::ReadAllBytes($journalPath)
                    $bytes[0] | Should -Not -Be 0xEF
                    $journal = ConvertFrom-Json -AsHashtable -InputObject ([Text.Encoding]::UTF8.GetString($bytes))
                    $state.replies[0].body | Should -BeLike "*$unicodeText*"
                    $journal.verification.comments[0].body | Should -BeExactly $state.replies[0].body
                    $journal.verification.body | Should -BeExactly $state.summary
                    return
                }
                if ($Mode -notin 'normal', 'closedAllowed', 'pendingAllowed') {
                    $published.Output | Should -BeNullOrEmpty
                    $message = switch ($Mode) {
                        'closed' { 'closed.*authorization' }
                        'sourceChanged' { 'revisions changed' }
                        'sourceChangedPending' { 'revisions changed' }
                        'closedSourceChanged' { 'revisions changed' }
                        'baseChanged' { 'revisions changed' }
                        'baseChangedAllowed' { 'revisions changed' }
                        'baseChangedPending' { 'revisions changed' }
                        'evidenceChanged' { 'evidence changed' }
                        'evidenceChangedPending' { 'evidence changed' }
                        'newThreadPending' { 'evidence changed' }
                        'badAnchorPending' { 'anchor|finding' }
                        'badFindingPending' { 'title' }
                        'pending' { 'draft.*authorization' }
                    }
                    $published.Error | Should -Match $message
                    $state = ConvertFrom-Json -AsHashtable -InputObject ([IO.File]::ReadAllText($statePath))
                    @($state.events).Count | Should -Be 0
                    [IO.File]::Exists((Join-Path $outputPath 'publication-result.json')) | Should -BeFalse
                    return
                }
                $outcome = ConvertFrom-Json -AsHashtable -InputObject $published.Output
                $outcome.status | Should -Be 'publishedWithFollowUpFailure'
                $journal = ConvertFrom-Json -AsHashtable -InputObject (
                    [IO.File]::ReadAllText((Join-Path $outputPath 'publication-result.json'))
                )
                $reopen = @($journal.operations | Where-Object operation -eq 'reopenThread')
                @($reopen.threadId) | Should -Be @('PRRT_1', 'PRRT_2')
                @($reopen.outcome) | Should -Be @('ambiguous', $(if ($FailSecond) { 'ambiguous' } else { 'succeeded' }))
                $outcome.message | Should -Match 'PRRT_1'
                if ($FailSecond) { $outcome.message | Should -Match 'PRRT_2' }
                $state = ConvertFrom-Json -AsHashtable -InputObject ([IO.File]::ReadAllText($statePath))
                $expectedEvents = @('start', 'reply:PRRT_1', 'reply:PRRT_2', 'submit', 'reopen:PRRT_1', 'reopen:PRRT_2')
                if ($Mode -eq 'pendingAllowed') { $expectedEvents = @('discard:existing') + $expectedEvents }
                @($state.events) | Should -Be $expectedEvents
                @($state.snapshots).Count | Should -Be 6
                # Each provider-side sample must already record the in-flight write as ambiguous.
                foreach ($snapshot in $state.snapshots) {
                    $snapshot.inFlight | Should -Not -BeNullOrEmpty
                    $snapshot.operations[$snapshot.inFlight.index].outcome | Should -Be 'ambiguous'
                }
                $outcome.publishedFindingCount | Should -Be 2
                $beforeReplay = [IO.File]::ReadAllText($statePath)
                $journalPath = Join-Path $outputPath 'publication-result.json'
                $journalBeforeReplay = [IO.File]::ReadAllText($journalPath)
                $replay = Invoke-FakePublicationCommand $publishArguments
                $replay.ExitCode | Should -Not -Be 0
                $replay.Output | Should -BeNullOrEmpty
                [IO.File]::ReadAllText($statePath) | Should -BeExactly $beforeReplay
                [IO.File]::ReadAllText($journalPath) | Should -BeExactly $journalBeforeReplay
            } finally {
                if ([IO.Directory]::Exists($root)) { [IO.Directory]::Delete($root, $true) }
            }
        }
    }
}
