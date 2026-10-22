BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:ScriptRoot = Join-Path $script:RepoRoot 'skills\ss-review\scripts'
    $script:LibraryRoot = Join-Path $script:ScriptRoot 'lib'
    $script:AuthorshipScript = Join-Path $script:ScriptRoot 'Set-CommentAuthorship.ps1'

    # The libraries are loaded here only to build the captures the executable is run against, which
    # is the same composition the capture executable performs.
    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestContextQuery.ps1')

    # Built from code points rather than written literally, so that a test file whose own encoding was
    # damaged fails instead of comparing two identically damaged strings.
    $script:Robot = [char]::ConvertFromUtf32(0x1F916)
    $script:EmDash = [string][char]0x2014
    $script:LeftQuote = [string][char]0x201C
    $script:RightQuote = [string][char]0x201D
    $script:NonAsciiBody = "Consider the $script:Robot path $script:EmDash see $script:LeftQuote Registered I/O $script:RightQuote below."

    # The capture instant is fixed rather than read from the clock, so that two captures built from
    # the same content are identical byte for byte and the tests never depend on real time.
    $script:CaptureInstant = [DateTimeOffset]::new(2025, 11, 6, 12, 0, 0, [TimeSpan]::Zero)

    function New-TestIdentity {
        param(
            [string] $Id = '1',
            [string] $DisplayName = 'Test Person',
            [string] $UniqueName = 'person@example.com',
            [string] $Descriptor = 'aad.PERSON'
        )

        New-PullRequestIdentity -Platform 'azureDevOps' -Id $Id -DisplayName $DisplayName -UniqueName $UniqueName -Descriptor $Descriptor
    }

    function New-TestComment {
        param(
            [Parameter(Mandatory)] $Id,
            [Parameter(Mandatory)] [System.Collections.IDictionary] $Author,
            [AllowEmptyString()] [string] $Content = 'body',
            [string] $PublishedUtc = '2025-11-05T10:00:00Z'
        )

        $authorship = Get-PullRequestAuthorship -Identity $Author -Content $Content
        [ordered]@{
            id                 = $Id
            url                = "https://example.invalid/c/$Id"
            parentId           = $null
            author             = $Author
            content            = $Content
            publishedUtc       = (ConvertTo-CanonicalTimestamp -Value $PublishedUtc)
            lastEditedUtc      = $null
            isEdited           = $false
            authorship         = $authorship['authorship']
            authorshipEvidence = $authorship['evidence']
        }
    }

    function New-TestThread {
        param(
            [Parameter(Mandatory)] $Id,
            [Parameter(Mandatory)] [AllowEmptyCollection()] [object[]] $Comments,
            [string] $EventUtc = '2025-11-05T10:00:00Z'
        )

        [ordered]@{
            id               = $Id
            url              = "https://example.invalid/t/$Id"
            publishedUtc     = (ConvertTo-CanonicalTimestamp -Value $EventUtc)
            lastUpdatedUtc   = $null
            eventUtc         = (ConvertTo-CanonicalTimestamp -Value $EventUtc)
            rawStatus        = 'active'
            resolution       = 'active'
            kind             = 'text'
            isOutdated       = $false
            context          = $null
            anchorKey        = $null
            properties       = [ordered]@{}
            openingCommentId = $(if ($Comments.Count -gt 0) { $Comments[0]['id'] } else { $null })
            decision         = $null
            comments         = $Comments
        }
    }

    # Two comments the capture left indeterminate and one it proved machine-authored, which is the
    # state the classification pass is invoked against. Non-ASCII content must remain byte-faithful.
    function New-CaptureDocument {
        $person = New-TestIdentity -Id '1' -Descriptor 'aad.PERSON'
        $agent = New-TestIdentity -Id '2' -DisplayName 'Review Bot' -UniqueName 'bot@example.com' -Descriptor 'aadsp.BOT'

        $context = [ordered]@{
            target                = (Resolve-PullRequestTarget -Url 'https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/42')
            authenticatedIdentity = $person
            pullRequest           = [ordered]@{
                id                      = 42
                title                   = 'Test change'
                reviewedSourceCommit    = 'aaa111'
                reviewedMergeBaseCommit = 'bbb222'
            }
            reviewers             = @()
            revisions             = @()
            threads               = @(
                New-TestThread -Id '1' -Comments @(
                    (New-TestComment -Id '1' -Author $person -Content $script:NonAsciiBody)
                    (New-TestComment -Id '2' -Author $agent -PublishedUtc '2025-11-05T11:00:00Z')
                )
                New-TestThread -Id '2' -EventUtc '2025-11-06T10:00:00Z' -Comments @(
                    (New-TestComment -Id '3' -Author $person -PublishedUtc '2025-11-06T10:00:00Z')
                )
            )
        }

        New-PullRequestContextDocument -Context $context -NowUtc $script:CaptureInstant
    }

    function New-CaptureFile {
        param(
            [Parameter(Mandatory)] [string] $Path,

            # A capture the current helper does not read, written to prove the version is enforced
            # rather than merely recorded. Left unset, the document carries the version this helper
            # writes.
            [AllowNull()] $SchemaVersion = $null
        )

        $document = New-CaptureDocument
        if ($null -ne $SchemaVersion) {
            Set-JsonProperty -Node $document -Name 'schemaVersion' -Value $SchemaVersion
        }
        Write-PullRequestOutputFile -Path $Path -Content (ConvertTo-JsonText -Value $document)
        $Path
    }

    function ConvertTo-ClassificationText {
        param(
            [Parameter(Mandatory)] [AllowEmptyCollection()] [object[]] $Entry
        )

        ConvertTo-Json -InputObject ([object[]] $Entry) -Depth 32 -Compress
    }

    function New-Classification {
        param(
            [Parameter(Mandatory)] [string] $ThreadId,
            [Parameter(Mandatory)] [string] $CommentId,
            [Parameter(Mandatory)] [AllowEmptyString()] [string] $Kind
        )

        [ordered]@{ threadId = $ThreadId; commentId = $CommentId; kind = $Kind }
    }

    function Invoke-AuthorshipProcess {
        param(
            [Parameter(Mandatory)] [AllowEmptyCollection()] [string[]] $ArgumentList
        )

        $startInfo = [Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = (Get-Process -Id $PID).Path
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        # Both streams are decoded as UTF-8 rather than through the console code page the child would
        # otherwise inherit, since an inherited code page is how non-ASCII gets destroyed.
        $startInfo.StandardOutputEncoding = [System.Text.UTF8Encoding]::new($false)
        $startInfo.StandardErrorEncoding = [System.Text.UTF8Encoding]::new($false)

        # Non-interactive execution turns a missing mandatory parameter into an error rather than a
        # prompt on the inherited console, which would otherwise stall the test run.
        $capturePathIndex = [array]::IndexOf($ArgumentList, '-Path')
        if ($capturePathIndex -ge 0 -and '-SnapshotId' -notin $ArgumentList) {
            $raw = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($ArgumentList[$capturePathIndex + 1]))
            $ArgumentList += @('-SnapshotId', (Get-PullRequestContextSnapshotId -Document $raw))
        }
        $invocation = @('-NoProfile', '-NonInteractive', '-File', $script:AuthorshipScript) + $ArgumentList
        foreach ($argument in $invocation) {
            $startInfo.ArgumentList.Add($argument)
        }

        $process = [Diagnostics.Process]::Start($startInfo)
        try {
            # Both streams are drained concurrently. Reading one to its end while the other fills its
            # buffer deadlocks, and the deadlock would only appear once a message grew past the buffer.
            $standardOutput = $process.StandardOutput.ReadToEndAsync()
            $standardError = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()

            [pscustomobject]@{
                ExitCode       = $process.ExitCode
                StandardOutput = $standardOutput.GetAwaiter().GetResult()
                StandardError  = $standardError.GetAwaiter().GetResult()
            }
        } finally {
            # A Process holds operating system handles until it is disposed, and this helper runs
            # once per test, so leaving them to the finalizer would accumulate handles across the run.
            $process.Dispose()
        }
    }

    # PowerShell colours a terminating error and wraps it into a gutter-decorated block when it
    # renders to a redirected stream, so the message is compared with the colour codes stripped and
    # that layout collapsed.
    function Get-ErrorText {
        param(
            [Parameter(Mandatory)] [AllowEmptyString()] [string] $Text
        )

        $plain = $Text -replace "$([char]27)\[[0-9;]*m", ''
        ($plain -replace '[\r\n]+\s*\|\s*', ' ') -replace '\s+', ' '
    }

    # The single JSON object the executable writes to standard output, refusing anything else on the
    # stream. The calling agent parses this output, so a stray value would corrupt the result.
    function Get-SingleJsonResult {
        param(
            [Parameter(Mandatory)] [AllowEmptyString()] [string] $StandardOutput
        )

        $text = $StandardOutput.Trim()
        $lines = @($text -split "`r?`n")
        if ($lines.Count -ne 1) {
            throw "Standard output carries $($lines.Count) lines rather than one compressed JSON object."
        }
        ConvertFrom-JsonVerbatim -Text $lines[0]
    }

    function Get-CaptureComment {
        param(
            [Parameter(Mandatory)] [System.Collections.IDictionary] $Document,
            [Parameter(Mandatory)] [string] $ThreadId,
            [Parameter(Mandatory)] [string] $CommentId
        )

        # Assigned before use: these accessors comma-wrap their result so a single-element list
        # survives whole, and wrapping the call in @( ) would nest it one level deeper.
        $threads = Get-JsonArrayProperty -Node $Document -Name 'threads'
        foreach ($thread in $threads) {
            if ([string] (Get-JsonProperty -Node $thread -Name 'id') -cne $ThreadId) { continue }
            $comments = Get-JsonArrayProperty -Node $thread -Name 'comments'
            foreach ($comment in $comments) {
                if ([string] (Get-JsonProperty -Node $comment -Name 'id') -ceq $CommentId) { return $comment }
            }
        }
        throw "The capture contains no comment '$CommentId' in thread '$ThreadId'."
    }

    # Consumers read the immutable capture with its separately validated semantic overlay.
    function Read-CaptureFileDocument {
        param(
            [Parameter(Mandatory)] [string] $Path
        )

        Read-PullRequestContextDocument -Path $Path
    }

    function Get-FileFingerprint {
        param(
            [Parameter(Mandatory)] [string] $Path
        )

        [System.Convert]::ToBase64String([System.IO.File]::ReadAllBytes($Path))
    }
}

Describe 'Incremental semantic authorship' {
    BeforeEach {
        $caseId = [guid]::NewGuid().ToString('N')
        $script:BaselinePath = Join-Path $TestDrive "$caseId-baseline.json"
        $script:RefreshPath = Join-Path $TestDrive "$caseId-refresh.json"
        $script:Baseline = New-CaptureDocument
        Write-PullRequestCapture -Path $script:BaselinePath -Document $script:Baseline | Out-Null
        Save-CommentAuthorship -Path $script:BaselinePath `
            -SnapshotId (Get-PullRequestContextSnapshotId -Document $script:Baseline) `
            -Classification @(
                (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
                (New-Classification -ThreadId '2' -CommentId '3' -Kind 'agent')
            ) | Out-Null
        $script:Decisions = Read-CommentAuthorship -Path $script:BaselinePath -Document $script:Baseline
        $script:Current = New-CaptureDocument
    }

    It 'reuses all unchanged decisions without another semantic reading pass' {
        $script:Current['fetchedUtc'] = '2026-10-07T12:00:00Z'
        $script:Current['pullRequest']['reviewedSourceCommit'] = 'force-pushed-source'
        $before = Get-FileFingerprint -Path $script:BaselinePath
        $reused = Write-PullRequestCapture -Path $script:RefreshPath -Document $script:Current -PreviousDecisions $script:Decisions
        $reused | Should -Be 2
        $effective = Read-PullRequestContextDocument -Path $script:RefreshPath
        $summary = New-PullRequestCaptureResult -Document $effective -ContextPath $script:RefreshPath -ReusedClassificationCount $reused
        $summary.indeterminateCommentCount | Should -Be 0
        $summary.reusedClassificationCount | Should -Be 2
        $options = New-PullRequestQueryOptions -Query comments -Authorship indeterminate
        $page = Read-PullRequestContextQuery -Document $effective -Options $options | ConvertFrom-Json
        $page.items.Count | Should -Be 0
        $page.complete | Should -BeTrue
        Get-FileFingerprint -Path $script:BaselinePath | Should -BeExactly $before
        $raw = Read-PullRequestContextDocument -Path $script:RefreshPath -WithoutAuthorship
        $raw.threads[0].comments[0].authorship | Should -BeExactly 'indeterminate'
    }

    It 'ignores fetch time and semantic progress when comparing reviewed evidence' {
        $script:Current.fetchedUtc = '2026-10-07T12:00:00Z'
        Write-PullRequestCapture -Path $script:RefreshPath -Document $script:Current -PreviousDecisions $script:Decisions | Out-Null
        $effective = Read-PullRequestContextDocument -Path $script:RefreshPath
        (Get-PullRequestContextDelta -Previous $script:Baseline -Current $effective).hasChanges | Should -BeFalse
        Get-PullRequestEvidenceHash -Document $effective | Should -BeExactly (Get-PullRequestEvidenceHash -Document $script:Baseline)
    }

    It 'limits semantic reading to the added and edited comments in a large discussion' {
        # Representative discussion size from issue 126; work counts, not wall-clock timing, prove reuse.
        $baseline = New-CaptureDocument
        $baseline.threads = @($baseline.threads[0])
        $baseline.threads[0].comments = @(foreach ($id in 1..135) {
            New-TestComment -Id ([string] $id) -Author (New-TestIdentity) -Content "Human comment $id"
        })
        $baselinePath = Join-Path $TestDrive 'large-baseline.json'
        Write-PullRequestCapture -Path $baselinePath -Document $baseline | Out-Null
        Save-CommentAuthorship -Path $baselinePath -SnapshotId (Get-PullRequestContextSnapshotId -Document $baseline) `
            -Classification @(foreach ($comment in $baseline.threads[0].comments) {
                New-Classification -ThreadId '1' -CommentId $comment.id -Kind 'human'
            }) | Out-Null
        $decisions = Read-CommentAuthorship -Path $baselinePath -Document $baseline
        $current = ConvertFrom-JsonVerbatim -Text (ConvertTo-JsonText -Value $baseline)
        $current.threads[0].comments = @($current.threads[0].comments | Where-Object { $_.id -cne '1' })
        $current.threads[0].comments[0].content = 'Edited without changing the provider timestamp'
        foreach ($id in 136..146) { $current.threads[0].comments += New-TestComment -Id ([string] $id) -Author (New-TestIdentity) }
        $reused = Write-PullRequestCapture -Path $script:RefreshPath -Document $current -PreviousDecisions $decisions
        $reused | Should -Be 133
        $effective = Read-PullRequestContextDocument -Path $script:RefreshPath
        $options = New-PullRequestQueryOptions -Query comments -Authorship indeterminate -PageSize 5
        $cursor = ''; $ids = @()
        # The expected pages bound the test deterministically; a broken cursor cannot hang the suite.
        foreach ($pageNumber in 1..3) {
            $json = Read-PullRequestContextQuery -Document $effective -Options $options -Cursor $cursor
            [Text.Encoding]::UTF8.GetByteCount($json + "`n") | Should -BeLessOrEqual 16384
            $page = $json | ConvertFrom-Json
            $ids += $page.items.commentId
            $cursor = $page.nextCursor
            if ($pageNumber -lt 3) { $cursor | Should -Not -BeNullOrEmpty }
        }
        $page.complete | Should -BeTrue
        $ids.Count | Should -Be 12
        @($ids | Select-Object -Unique).Count | Should -Be 12
        $ids | Should -Contain '2'
        $ids | Should -Not -Contain '1'
        $ids | Should -Not -Contain '3'
        $delta = Get-PullRequestContextDelta -Previous $baseline -Current $effective
        $delta.commentsAdded.Count | Should -Be 11
        $delta.commentsEdited.Count | Should -Be 1
        $delta.commentsRemoved.Count | Should -Be 1
    }

    It 'reconsiders changed <Case> rather than reusing a comment ID' -ForEach @(
        @{ Case = 'body'; Change = { $script:Current.threads[0].comments[0].content += ' ' } }
        @{ Case = 'author ID'; Change = { $script:Current.threads[0].comments[0].author.id = 'other-account' } }
        @{ Case = 'display name'; Change = { $script:Current.threads[0].comments[0].author.displayName = 'Different attribution' } }
        @{ Case = 'account type'; Change = { $script:Current.threads[0].comments[0].author.accountType = 'Mannequin' } }
        @{ Case = 'self attribution'; Change = { $script:Current.threads[0].comments[0].isAuthoredBySelf = $false } }
        @{ Case = 'provider evidence'; Change = { $script:Current.threads[0].comments[0].authorshipEvidence = @('changed-proof') } }
    ) {
        & $Change
        Write-PullRequestCapture -Path $script:RefreshPath -Document $script:Current -PreviousDecisions $script:Decisions | Out-Null
        $effective = Read-PullRequestContextDocument -Path $script:RefreshPath
        $effective.threads[0].comments[0].authorship | Should -BeExactly 'indeterminate'
        (Get-PullRequestContextDelta -Previous $script:Baseline -Current $effective).hasChanges | Should -BeTrue
    }

    It 'never transfers decisions across <Identity> collisions' -ForEach @(
        @{ Identity = 'PR'; Field = 'pullRequestUrl'; Value = 'https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/43' }
        @{ Identity = 'repository'; Field = 'repositoryUrl'; Value = 'https://dev.azure.com/contoso/Core/_git/other' }
        @{ Identity = 'provider'; Field = 'platform'; Value = 'gitHub' }
    ) {
        $script:Current[$Field] = $Value
        Write-PullRequestCapture -Path $script:RefreshPath -Document $script:Current -PreviousDecisions $script:Decisions | Should -Be 0
    }

    It 'reconsiders every semantic decision when classification policy changes' {
        $oldPolicy = $script:CommentAuthorshipPolicyVersion
        try {
            $script:CommentAuthorshipPolicyVersion = 'next-policy'
            Write-PullRequestCapture -Path $script:RefreshPath -Document $script:Current -PreviousDecisions $script:Decisions | Should -Be 0
            $effective = Read-PullRequestContextDocument -Path $script:BaselinePath
            (New-PullRequestCaptureResult -Document $effective -ContextPath $script:BaselinePath).indeterminateCommentCount | Should -Be 2
            Save-CommentAuthorship -Path $script:BaselinePath -SnapshotId (Get-PullRequestContextSnapshotId -Document $effective) `
                -Classification @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'agent')) | Out-Null
            (Read-PullRequestContextDocument -Path $script:BaselinePath).threads[0].comments[0].authorship | Should -Be 'agent'
        } finally { $script:CommentAuthorshipPolicyVersion = $oldPolicy }
    }

    It 'gives current deterministic proofs priority over prior semantic decisions' -ForEach @('marker', 'service') {
        $comment = $script:Current.threads[0].comments[0]
        if ($_ -ceq 'marker') { $comment.content = '[Copilot speaking] Updated.' }
        else { $comment.author = New-TestIdentity -Descriptor 'aadsp.SERVICE' }
        $proof = Get-PullRequestAuthorship -Identity $comment.author -Content $comment.content
        $comment.authorship = $proof.authorship; $comment.authorshipEvidence = $proof.evidence
        Write-PullRequestCapture -Path $script:RefreshPath -Document $script:Current -PreviousDecisions $script:Decisions | Should -Be 1
        (Read-PullRequestContextDocument -Path $script:RefreshPath).threads[0].comments[0].authorship | Should -Be 'agent'
    }

    It 'classifies additions while retaining removed evidence only in the baseline' {
        $script:Current.threads = @($script:Current.threads[0])
        $script:Current.threads[0].comments += New-TestComment -Id 'added' -Author (New-TestIdentity)
        Write-PullRequestCapture -Path $script:RefreshPath -Document $script:Current -PreviousDecisions $script:Decisions | Should -Be 1
        $effective = Read-PullRequestContextDocument -Path $script:RefreshPath
        $delta = Get-PullRequestContextDelta -Previous (Read-PullRequestContextDocument -Path $script:BaselinePath) -Current $effective
        $delta.commentsAdded[0].commentId | Should -Be 'added'
        $delta.commentsRemoved[0].commentId | Should -Be '3'
        $summary = Save-CommentAuthorship -Path $script:RefreshPath -SnapshotId (Get-PullRequestContextSnapshotId -Document $effective) `
            -Classification @((New-Classification -ThreadId '1' -CommentId 'added' -Kind 'human'))
        $summary.applied | Should -Be 1
        $summary.isComplete | Should -BeTrue
        (Read-PullRequestContextDocument -Path $script:BaselinePath).threads[1].comments[0].authorship | Should -Be 'agent'
        $raw = Read-PullRequestContextDocument -Path $script:RefreshPath -WithoutAuthorship
        @((Read-CommentAuthorship -Path $script:RefreshPath -Document $raw).decisions.origin) | Should -Be @('reused', 'classified')
    }

    It 'invalidates old filtered cursors and stale decision batches after a write' {
        Write-PullRequestCapture -Path $script:RefreshPath -Document $script:Current | Out-Null
        $options = New-PullRequestQueryOptions -Query comments -Authorship indeterminate -PageSize 1
        $page = Read-PullRequestContextQuery -Document $script:Current -Options $options | ConvertFrom-Json
        $page.nextCursor | Should -Not -BeNullOrEmpty
        Save-CommentAuthorship -Path $script:RefreshPath -SnapshotId $page.snapshotId `
            -Classification @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')) | Out-Null
        $effective = Read-PullRequestContextDocument -Path $script:RefreshPath
        { Read-PullRequestContextQuery -Document $effective -Options $options -Cursor $page.nextCursor } | Should -Throw '*Stale*'
        { Save-CommentAuthorship -Path $script:RefreshPath -SnapshotId $page.snapshotId -Classification @() } | Should -Throw '*snapshot changed*'
        $next = Read-PullRequestContextQuery -Document $effective -Options $options | ConvertFrom-Json
        $next.items[0].commentId | Should -Be '3'
        $next.complete | Should -BeTrue
    }

    It 'rejects malformed sidecars, unknown schemas and capture replacement explicitly' {
        $sidecarPath = "$script:BaselinePath.authorship.json"
        $script:Decisions.schemaVersion = 99
        Write-PullRequestOutputFile -Path $sidecarPath -Content (ConvertTo-JsonText -Value $script:Decisions)
        { Read-PullRequestContextDocument -Path $script:BaselinePath } | Should -Throw
        $script:Decisions.schemaVersion = 1
        $script:Decisions.decisions[0].extra = 'not permitted'
        { Assert-CommentAuthorship -Decisions $script:Decisions } | Should -Throw
        $script:Decisions.decisions[0].Remove('extra')
        Write-PullRequestOutputFile -Path $sidecarPath -Content (ConvertTo-JsonText -Value $script:Decisions)
        { Write-PullRequestCapture -Path $script:BaselinePath -Document $script:Current } | Should -Throw '*already exists*'
        $script:Current.threads[0].comments[0].content = 'replaced snapshot'
        Write-PullRequestOutputFile -Path $script:BaselinePath -Content (ConvertTo-JsonText -Value $script:Current)
        { Read-PullRequestContextDocument -Path $script:BaselinePath } | Should -Throw '*do not belong*'
    }

    It 'rejects malformed decision drafts without changing stored decisions' -ForEach @(
        @{ Entry = @{ threadId = '1'; commentId = '1' } }
        @{ Entry = @{ threadId = '1'; commentId = '1'; kind = 'human'; unexpected = $true } }
        @{ Entry = @{ threadId = @('1'); commentId = '1'; kind = 'human' } }
        @{ Entry = @{ threadId = '1'; commentId = ''; kind = 'human' } }
    ) {
        $snapshot = Get-PullRequestContextSnapshotId -Document (Read-PullRequestContextDocument -Path $script:BaselinePath)
        $before = Get-FileFingerprint -Path "$script:BaselinePath.authorship.json"
        { Save-CommentAuthorship -Path $script:BaselinePath -SnapshotId $snapshot -Classification @($Entry) } | Should -Throw
        Get-FileFingerprint -Path "$script:BaselinePath.authorship.json" | Should -BeExactly $before
    }
}

Describe 'Set-CommentAuthorship' {
    BeforeAll {
        [System.IO.File]::Exists($script:AuthorshipScript) |
            Should -BeTrue -Because "the classification executable is expected at $script:AuthorshipScript"

        $script:WorkRoot = Join-Path ([System.IO.Path]::GetTempPath()) "ss-review-authorship-$([guid]::NewGuid())"
        New-Item -ItemType Directory -Path $script:WorkRoot | Out-Null
        $script:CaptureIndex = 0
    }

    AfterAll {
        if (Test-Path -LiteralPath $script:WorkRoot) {
            Remove-Item -LiteralPath $script:WorkRoot -Recurse -Force
        }
    }

    BeforeEach {
        # Every test owns its capture and semantic decisions.
        $script:CaptureIndex++
        $script:CapturePath = Join-Path $script:WorkRoot "capture-$script:CaptureIndex.json"
    }

    It 'records classification separately and exposes it through the capture reader' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'agent')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
        Test-Path -LiteralPath "$script:CapturePath.authorship.json" | Should -BeTrue
        $document = Read-CaptureFileDocument -Path $script:CapturePath
        (Get-JsonProperty -Node (Get-CaptureComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorship') | Should -BeExactly 'human'
        (Get-JsonProperty -Node (Get-CaptureComment -Document $document -ThreadId '2' -CommentId '3') -Name 'authorship') | Should -BeExactly 'agent'
        (Get-JsonProperty -Node (Get-CaptureComment -Document $document -ThreadId '1' -CommentId '2') -Name 'authorship') | Should -BeExactly 'agent'

        # The reader's judgement is recorded alongside the evidence the capture collected.
        $evidence = Get-JsonArrayProperty -Node (Get-CaptureComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorshipEvidence'
        $evidence -ccontains 'classification:reviewed' | Should -BeTrue
    }

    It 'writes exactly one compressed JSON object to standard output' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $classification = ConvertTo-ClassificationText -Entry @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)
        $result.StandardError | Should -BeNullOrEmpty
        $parsed = Get-SingleJsonResult -StandardOutput $result.StandardOutput

        @($parsed.Keys) | Should -BeExactly @('contextPath', 'applied', 'unchanged', 'indeterminate', 'isComplete')
        $parsed['contextPath'] | Should -BeExactly ([System.IO.Path]::GetFullPath($script:CapturePath))
        $parsed['applied'] | Should -Be 1
        $parsed['unchanged'] | Should -Be 0
        $parsed['indeterminate'] | Should -Be 1
        $parsed['isComplete'] | Should -BeFalse
    }

    It 'counts unsettled comments without dumping their identifiers' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $classification = ConvertTo-ClassificationText -Entry @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)
        $parsed = Get-SingleJsonResult -StandardOutput $result.StandardOutput
        $parsed['indeterminate'] | Should -Be 1
        $parsed.Contains('indeterminateComments') | Should -BeFalse
    }

    It 'reports a capture whose every comment is settled as complete' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '1' -CommentId '2' -Kind 'agent')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'human')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)
        $parsed = Get-SingleJsonResult -StandardOutput $result.StandardOutput
        $parsed['applied'] | Should -Be 2
        # Restating what the capture already proved is accepted and changes nothing.
        $parsed['unchanged'] | Should -Be 1
        $parsed['indeterminate'] | Should -Be 0
        $parsed['isComplete'] | Should -BeTrue
        $parsed.Contains('indeterminateComments') | Should -BeFalse
    }

    It 'preserves non-ASCII capture bytes through classification' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)

        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before

        $document = Read-CaptureFileDocument -Path $script:CapturePath
        $comment = Get-CaptureComment -Document $document -ThreadId '1' -CommentId '1'
        (Get-JsonProperty -Node $comment -Name 'authorship') | Should -BeExactly 'human'
        (Get-JsonProperty -Node $comment -Name 'content') | Should -BeExactly $script:NonAsciiBody

        # The immutable capture keeps its UTF-8 encoding and line-ending contract.
        $bytes = [System.IO.File]::ReadAllBytes($script:CapturePath)
        $bytes[0] | Should -Not -Be 0xEF
        $bytes | Should -Not -Contain 13
    }

    It 'accepts the classifications from a file' {
        # A capture with many comments produces a batch that exceeds what a command line accepts, so
        # the file form has to reach the same result as the inline form.
        $inlinePath = $script:CapturePath
        $filePath = Join-Path $script:WorkRoot "capture-$script:CaptureIndex-file.json"
        New-CaptureFile -Path $inlinePath | Out-Null
        New-CaptureFile -Path $filePath | Out-Null

        $entries = @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'agent')
        )
        $classification = ConvertTo-ClassificationText -Entry $entries
        $classificationPath = Join-Path $script:WorkRoot "classification-$script:CaptureIndex.json"
        [System.IO.File]::WriteAllText($classificationPath, $classification, [System.Text.UTF8Encoding]::new($false))

        $inlineResult = Invoke-AuthorshipProcess -ArgumentList @('-Path', $inlinePath, '-Classification', $classification)
        $fileResult = Invoke-AuthorshipProcess -ArgumentList @('-Path', $filePath, '-ClassificationPath', $classificationPath)

        $inlineResult.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $inlineResult.StandardError)
        $fileResult.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $fileResult.StandardError)

        $inlineParsed = Get-SingleJsonResult -StandardOutput $inlineResult.StandardOutput
        $fileParsed = Get-SingleJsonResult -StandardOutput $fileResult.StandardOutput

        # Anchored on what the batch actually does, so that the comparison between the two forms
        # cannot be satisfied by both of them doing nothing.
        $inlineParsed['applied'] | Should -Be 2
        $inlineParsed['isComplete'] | Should -BeTrue
        foreach ($name in @('applied', 'unchanged', 'indeterminate', 'isComplete')) {
            $fileParsed[$name] | Should -Be $inlineParsed[$name] -Because "the two forms report the same $name"
        }

        # The decision record holds no local path, so equivalent batches are byte-identical.
        Get-FileFingerprint -Path $filePath | Should -BeExactly (Get-FileFingerprint -Path $inlinePath)
        Get-FileFingerprint -Path "$filePath.authorship.json" | Should -BeExactly (Get-FileFingerprint -Path "$inlinePath.authorship.json")
        foreach ($path in @($inlinePath, $filePath)) {
            $comment = Get-CaptureComment -Document (Read-CaptureFileDocument -Path $path) -ThreadId '1' -CommentId '1'
            (Get-JsonProperty -Node $comment -Name 'authorship') | Should -BeExactly 'human'
        }
    }

    It 'refuses both classification forms at once' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $classification = ConvertTo-ClassificationText -Entry @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))
        $classificationPath = Join-Path $script:WorkRoot "classification-both-$script:CaptureIndex.json"
        [System.IO.File]::WriteAllText($classificationPath, $classification, [System.Text.UTF8Encoding]::new($false))

        $result = Invoke-AuthorshipProcess -ArgumentList @(
            '-Path', $script:CapturePath
            '-Classification', $classification
            '-ClassificationPath', $classificationPath
        )

        $result.ExitCode | Should -Not -Be 0
        (Get-ErrorText -Text $result.StandardError).Contains('not both', [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'refuses to run without any classification' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath)

        $result.ExitCode | Should -Not -Be 0
        (Get-ErrorText -Text $result.StandardError).Contains('-ClassificationPath', [StringComparison]::Ordinal) | Should -BeTrue
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
    }

    It 'refuses a classification payload that is not a list' {
        New-CaptureFile -Path $script:CapturePath | Out-Null

        $result = Invoke-AuthorshipProcess -ArgumentList @(
            '-Path', $script:CapturePath
            '-Classification', '{"threadId":"1","commentId":"1","kind":"human"}'
        )

        $result.ExitCode | Should -Not -Be 0
        (Get-ErrorText -Text $result.StandardError).Contains('JSON array', [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'accepts a capitalized authorship and stores it in lower case' {
        # The batch is written by a language model, which capitalizes a word as readily as not, and
        # refusing it would discard the whole pass over a single letter.
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'Human')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'AGENT')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)
        $document = Read-CaptureFileDocument -Path $script:CapturePath
        (Get-JsonProperty -Node (Get-CaptureComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorship') | Should -BeExactly 'human'
        (Get-JsonProperty -Node (Get-CaptureComment -Document $document -ThreadId '2' -CommentId '3') -Name 'authorship') | Should -BeExactly 'agent'
    }

    It 'leaves the capture untouched when a batch names an authorship outside the vocabulary' {
        # Leniency about case is not leniency about the word: an unrecognized authorship is a mistake
        # the caller has to hear about rather than a comment silently left unsettled.
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'Robot')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Not -Be 0
        $message = Get-ErrorText -Text $result.StandardError
        $message.Contains("authorship 'Robot'", [StringComparison]::Ordinal) | Should -BeTrue
        $message.Contains("Use 'agent' or 'human'", [StringComparison]::Ordinal) | Should -BeTrue
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
    }

    It 'leaves the capture untouched when a batch names a comment it does not contain' {
        # The document is rewritten only after every classification has been accepted, so a batch
        # whose later entry is wrong must not half-update the file.
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '1' -CommentId '98' -Kind 'human')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Not -Be 0
        (Get-ErrorText -Text $result.StandardError).Contains("no comment '98'", [StringComparison]::Ordinal) | Should -BeTrue
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
    }

    It 'leaves the capture untouched when a batch contradicts what the capture proved' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'human')
            (New-Classification -ThreadId '1' -CommentId '2' -Kind 'human')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Not -Be 0
        (Get-ErrorText -Text $result.StandardError).Contains('cannot be reclassified', [StringComparison]::Ordinal) | Should -BeTrue
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
    }

    It 'refuses a capture written to a schema version it does not read' {
        New-CaptureFile -Path $script:CapturePath -SchemaVersion 99 | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Not -Be 0
        $message = Get-ErrorText -Text $result.StandardError
        $message.Contains('version 99', [StringComparison]::Ordinal) | Should -BeTrue
        $message.Contains('Capture the pull request again', [StringComparison]::Ordinal) | Should -BeTrue
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
    }
}
