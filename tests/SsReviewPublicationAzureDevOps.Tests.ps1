BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:LibraryRoot = Join-Path $script:RepoRoot 'agents\.github\skills\ss-review\scripts\lib'

    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PublicationFailure.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'AdoRestTransport.ps1')
    . (Join-Path $script:LibraryRoot 'AdoPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')
    . (Join-Path $script:LibraryRoot 'WorkflowData.ps1')
    . (Join-Path $script:LibraryRoot 'FindingModel.ps1')
    . (Join-Path $script:LibraryRoot 'GitDiff.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestAnchor.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestPlatform.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestPublication.ps1')

    # The suite is entirely offline. Every request, every wait and every token acquisition is
    # injected, so nothing here depends on a service being reachable or on wall-clock time passing.

    # Characters are built from their code points so the fixtures never depend on how this file is
    # encoded on disk, and so the assertions compare code points rather than rendered glyphs.
    $script:EmDash = [string] [char]0x2014
    $script:LeftQuote = [string] [char]0x201C
    $script:RightQuote = [string] [char]0x201D
    $script:Robot = [char]::ConvertFromUtf32(0x1F916)

    # Every record that carries an instant is given this one, so a run at any moment produces the
    # same document.
    $script:FixedNowUtc = [DateTimeOffset]::new(2026, 8, 4, 12, 0, 0, [TimeSpan]::Zero)

    $script:SourceCommit = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
    $script:MergeBaseCommit = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'
    $script:IterationId = 4

    $script:Target = [ordered]@{
        platform      = 'azureDevOps'
        collectionUrl = 'https://example.visualstudio.com'
        organization  = 'example'
        project       = 'Example Project'
        repository    = 'example-repo'
        pullRequestId = 42
        webUrl        = 'https://example.visualstudio.com/Example%20Project/_git/example-repo/pullrequest/42'
    }

    function New-AdoResponse {
        param(
            [AllowEmptyString()]
            [string] $Body = '{}',

            [int] $StatusCode = 200,

            [hashtable] $Headers = @{}
        )

        [ordered]@{
            statusCode   = $StatusCode
            headers      = $Headers
            contentType  = 'application/json; charset=utf-8'
            contentBytes = [System.Text.Encoding]::UTF8.GetBytes($Body)
        }
    }

    # The in-memory stand-in for the pull request's discussion.
    #
    # The threads a test asserts against are the ones the run itself created, so the fake stores what
    # it is sent and serves it back rather than replaying fixtures a test had to keep in step with
    # the plan. That is what lets the verification tests read what publication actually wrote, and
    # what lets a failure test remove one comment and prove the check notices.
    function Reset-AdoState {
        $script:AdoCalls = [System.Collections.Generic.List[object]]::new()
        $script:AdoWaits = [System.Collections.Generic.List[int]]::new()
        $script:AdoFaults = [System.Collections.Generic.List[object]]::new()
        $script:AdoThreads = [System.Collections.Generic.List[object]]::new()
        $script:AdoReviewers = @()
        $script:AdoNextThreadId = 101
        $script:AdoPullRequestStatus = 'active'
        $script:AdoIterationChangePages = @(
            (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        changeEntries = @(
                            [ordered]@{ changeTrackingId = 3; changeType = 'edit'; item = [ordered]@{ path = '/crates/widget/src/lib.rs' } }
                            [ordered]@{ changeTrackingId = 7; changeType = 'edit'; item = [ordered]@{ path = '/docs/readme.md' } }
                        )
                    }))
        )
    }

    # Registers a canned answer for the requests whose "METHOD URL" matches the pattern.
    #
    # A fault takes precedence over the fake's own behaviour, so a test states the write it wants to
    # fail without restating the ones it wants to succeed. A count of zero means every matching
    # request from then on.
    function Add-AdoFault {
        param(
            [Parameter(Mandatory)]
            [string] $Pattern,

            [int] $StatusCode = 500,

            [string] $Body = '{"message":"the service refused"}',

            # Raised instead of answered, which is how a response that never arrived reaches the
            # caller: the request may or may not have been applied.
            [switch] $Lost,

            [int] $Skip = 0,

            [int] $Times = 0
        )

        $script:AdoFaults.Add([ordered]@{
                pattern    = $Pattern
                statusCode = $StatusCode
                body       = $Body
                lost       = [bool] $Lost
                skip       = $Skip
                remaining  = $Times
            })
    }

    function Find-AdoStoredThread {
        param([Parameter(Mandatory)][string] $ThreadId)

        foreach ($thread in $script:AdoThreads) {
            if ([string] $thread['id'] -ceq $ThreadId) { return $thread }
        }
        return $null
    }

    function New-AdoStoredThread {
        param([Parameter(Mandatory)][System.Collections.IDictionary] $Request)

        $comments = Get-JsonArrayProperty -Node $Request -Name 'comments'
        $comment = $comments[0]
        $thread = [ordered]@{
            id            = $script:AdoNextThreadId
            status        = [string] (Get-JsonProperty -Node $Request -Name 'status')
            threadContext = Get-JsonProperty -Node $Request -Name 'threadContext'
            comments      = [System.Collections.Generic.List[object]]::new()
        }
        $thread['comments'].Add([ordered]@{
                id              = 1
                parentCommentId = 0
                content         = [string] (Get-JsonProperty -Node $comment -Name 'content')
                commentType     = [string] (Get-JsonProperty -Node $comment -Name 'commentType')
            })

        $script:AdoNextThreadId++
        $script:AdoThreads.Add($thread)
        return $thread
    }

    function ConvertTo-AdoStoredThreadJson {
        param([Parameter(Mandatory)][System.Collections.IDictionary] $Thread)

        $payload = [ordered]@{
            id       = $Thread['id']
            status   = $Thread['status']
            comments = @($Thread['comments'])
        }
        if ($null -ne $Thread['threadContext']) { $payload['threadContext'] = $Thread['threadContext'] }
        ConvertTo-Json -Depth 12 -Compress -InputObject $payload
    }

    function Get-AdoFaultResponse {
        param([Parameter(Mandatory)][string] $Composite)

        foreach ($fault in $script:AdoFaults) {
            if ($Composite -notmatch [string] $fault['pattern']) { continue }
            if ([int] $fault['skip'] -gt 0) {
                $fault['skip'] = [int] $fault['skip'] - 1
                continue
            }
            if ([int] $fault['remaining'] -lt 0) { continue }
            if ([int] $fault['remaining'] -gt 0) {
                $fault['remaining'] = [int] $fault['remaining'] - 1
                if ([int] $fault['remaining'] -eq 0) { $fault['remaining'] = -1 }
            }
            return $fault
        }
        return $null
    }

    # Answers one request the way the service would, or the way the test asked for it to fail.
    function Get-AdoFakeResponse {
        param([Parameter(Mandatory)] $Request)

        $composite = "$($Request.Method) $($Request.Url)"

        $fault = Get-AdoFaultResponse -Composite $composite
        if ($null -ne $fault) {
            if ([bool] $fault['lost']) { throw 'the connection was closed before a response arrived' }
            return New-AdoResponse -StatusCode ([int] $fault['statusCode']) -Body ([string] $fault['body'])
        }

        $body = $null
        if ($null -ne $Request.Body) { $body = ConvertFrom-JsonVerbatim -Text ([string] $Request.Body) }

        switch -Regex ($composite) {
            '^POST .*/threads/(?<thread>\d+)/comments\?' {
                $thread = Find-AdoStoredThread -ThreadId $Matches['thread']
                if ($null -eq $thread) { throw "the fake holds no thread $($Matches['thread'])" }

                $comment = [ordered]@{
                    id              = $thread['comments'].Count + 1
                    parentCommentId = Get-JsonProperty -Node $body -Name 'parentCommentId'
                    content         = [string] (Get-JsonProperty -Node $body -Name 'content')
                    commentType     = [string] (Get-JsonProperty -Node $body -Name 'commentType')
                }
                $thread['comments'].Add($comment)
                return New-AdoResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject $comment)
            }

            '^POST .*/threads\?' {
                return New-AdoResponse -Body (ConvertTo-AdoStoredThreadJson -Thread (New-AdoStoredThread -Request $body))
            }

            '^PATCH .*/threads/(?<thread>\d+)\?' {
                $thread = Find-AdoStoredThread -ThreadId $Matches['thread']
                if ($null -eq $thread) { throw "the fake holds no thread $($Matches['thread'])" }

                $thread['status'] = [string] (Get-JsonProperty -Node $body -Name 'status')
                return New-AdoResponse -Body (ConvertTo-AdoStoredThreadJson -Thread $thread)
            }

            '^GET .*/threads\?' {
                $serialized = [System.Collections.Generic.List[object]]::new()
                foreach ($thread in $script:AdoThreads) {
                    $serialized.Add((ConvertFrom-JsonVerbatim -Text (ConvertTo-AdoStoredThreadJson -Thread $thread)))
                }
                return New-AdoResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                            count = $serialized.Count
                            value = @($serialized)
                        }))
            }

            '^GET .*/iterations/\d+/changes\?' {
                $index = @($script:AdoCalls | Where-Object { $_['url'] -match '/iterations/\d+/changes\?' }).Count - 1
                if ($index -ge $script:AdoIterationChangePages.Count) { throw "the fake holds no change page $($index + 1)" }
                return New-AdoResponse -Body $script:AdoIterationChangePages[$index]
            }

            '^GET .*/iterations\?' {
                return New-AdoResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                            count = 2
                            value = @(
                                [ordered]@{
                                    id              = 1
                                    createdDate     = '2026-08-01T09:00:00Z'
                                    sourceRefCommit = [ordered]@{ commitId = 'cccccccccccccccccccccccccccccccccccccccc' }
                                    targetRefCommit = [ordered]@{ commitId = 'dddddddddddddddddddddddddddddddddddddddd' }
                                    commonRefCommit = [ordered]@{ commitId = 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee' }
                                }
                                [ordered]@{
                                    id              = $script:IterationId
                                    createdDate     = '2026-08-03T09:00:00Z'
                                    sourceRefCommit = [ordered]@{ commitId = $script:SourceCommit }
                                    targetRefCommit = [ordered]@{ commitId = 'dddddddddddddddddddddddddddddddddddddddd' }
                                    commonRefCommit = [ordered]@{ commitId = $script:MergeBaseCommit }
                                }
                            )
                        }))
            }

            '^GET .*_apis/connectionData\?' {
                return New-AdoResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                            authenticatedUser = [ordered]@{
                                id                  = '11111111-1111-1111-1111-111111111111'
                                providerDisplayName = 'Review Runner'
                                subjectDescriptor   = 'aad.cnVubmVy'
                                properties          = [ordered]@{
                                    Account = [ordered]@{ '$type' = 'System.String'; '$value' = 'runner@example.com' }
                                }
                            }
                        }))
            }

            '^GET .*/pullRequests/\d+\?' {
                return New-AdoResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                            pullRequestId = $script:Target['pullRequestId']
                            title         = 'Add the environment crate'
                            status        = $script:AdoPullRequestStatus
                            isDraft       = $false
                            createdBy     = [ordered]@{ id = 'author'; displayName = 'Author'; uniqueName = 'author@example.com' }
                        }))
            }

            '^GET .*/reviewers\?' {
                return New-AdoResponse -Body (ConvertTo-Json -Depth 6 -Compress -InputObject ([ordered]@{
                            count = $script:AdoReviewers.Count
                            value = $script:AdoReviewers
                        }))
            }

            '^PUT .*/reviewers/[^/?]+\?' {
                return New-AdoResponse -Body (ConvertTo-Json -Depth 6 -Compress -InputObject ([ordered]@{
                            id   = [string] (Get-JsonProperty -Node $body -Name 'id')
                            vote = Get-JsonProperty -Node $body -Name 'vote'
                        }))
            }

            default { throw "the fake has no answer for '$composite'" }
        }
    }

    function New-TestTransport {
        param([int] $MaxAttempts = 5)

        # The recorded state lives at script scope rather than inside a closure, because a closure is
        # bound to its own module and would no longer resolve the helper functions this file defines.
        $send = {
            param(
                [string] $Method,
                [string] $Url,
                [hashtable] $Headers,
                $Body
            )

            $script:AdoCalls.Add([ordered]@{
                    method = $Method
                    url    = $Url
                    body   = [string] $Body
                })

            Get-AdoFakeResponse -Request ([pscustomobject]@{ Method = $Method; Url = $Url; Body = $Body })
        }

        $wait = {
            param([int] $Milliseconds)

            $script:AdoWaits.Add($Milliseconds)
        }

        New-AdoRestTransport `
            -CollectionUrl ([string] $script:Target['collectionUrl']) `
            -SendRequest $send `
            -GetAccessToken { 'token' } `
            -Wait $wait `
            -MaxAttempts $MaxAttempts
    }

    # Names each recorded request by what it does, which is what makes an ordering assertion legible.
    # The three kinds of thread creation share one route and are told apart by what they carry: a
    # line position, a file and nothing else, or no position at all.
    function Get-AdoOperationSequence {
        $sequence = [System.Collections.Generic.List[string]]::new()
        foreach ($call in $script:AdoCalls) {
            $composite = "$($call['method']) $($call['url'])"
            $body = [string] $call['body']

            $name = switch -Regex ($composite) {
                '^POST .*/threads/\d+/comments\?' { 'reply'; break }
                '^POST .*/threads\?' {
                    if ($body -match '"(right|left)FileStart"') { 'lineThread' }
                    elseif ($body -match '"threadContext"') { 'fileThread' }
                    else { 'summary' }
                    break
                }
                '^PATCH .*/threads/\d+\?' { 'threadStatus'; break }
                '^PUT .*/reviewers/[^/?]+\?' { 'castVote'; break }
                '^GET .*/reviewers\?' { 'readReviewers'; break }
                '^GET .*/threads\?' { 'readThreads'; break }
                '^GET .*/iterations/\d+/changes\?' { 'changes'; break }
                '^GET .*/iterations\?' { 'iterations'; break }
                '^GET .*_apis/connectionData\?' { 'connectionData'; break }
                '^GET .*/pullRequests/\d+\?' { 'pullRequest'; break }
                default { 'other' }
            }
            $sequence.Add($name)
        }
        $sequence.ToArray()
    }

    function Get-AdoCallBody {
        param([Parameter(Mandatory)][string] $Operation)

        $found = [System.Collections.Generic.List[object]]::new()
        $index = 0
        foreach ($name in (Get-AdoOperationSequence)) {
            if ($name -eq $Operation) {
                $found.Add((ConvertFrom-JsonVerbatim -Text ([string] $script:AdoCalls[$index]['body'])))
            }
            $index++
        }
        , $found.ToArray()
    }

    function Get-AdoCallUrl {
        param([Parameter(Mandatory)][string] $Operation)

        $found = [System.Collections.Generic.List[string]]::new()
        $index = 0
        foreach ($name in (Get-AdoOperationSequence)) {
            if ($name -eq $Operation) { $found.Add([string] $script:AdoCalls[$index]['url']) }
            $index++
        }
        , $found.ToArray()
    }

    function New-TestState {
        param($IterationId = $script:IterationId)

        [ordered]@{
            platform          = 'azureDevOps'
            pullRequestNodeId = $null
            url               = [string] $script:Target['webUrl']
            status            = 'active'
            isActive          = $true
            isDraft           = $false
            sourceCommit      = $script:SourceCommit
            mergeBaseCommit   = $script:MergeBaseCommit
            identity          = 'runner@example.com'
            pendingReviewId   = $null
            iterationId       = $IterationId
        }
    }

    function New-PlanEntry {
        param(
            [Parameter(Mandatory)]
            [ValidateSet('newLineThread', 'newFileThread', 'reply')]
            [string] $Operation,

            [Parameter(Mandatory)]
            [string] $FindingId,

            [string] $Body = 'rendered body',
            [string] $Path = 'crates/widget/src/lib.rs',
            [int] $StartLine = 10,
            [int] $EndLine = 10,
            [string] $Side = 'right',
            [string] $ThreadId = '',
            [string] $CommentId = '',
            $ChangeTrackingId = 3,
            [switch] $RequiresReopen,
            [switch] $WasDowngraded
        )

        $anchor = switch ($Operation) {
            'newLineThread' {
                [ordered]@{
                    kind          = 'line'
                    path          = $Path
                    startLine     = $StartLine
                    endLine       = $EndLine
                    side          = $Side
                    wasDowngraded = [bool] $WasDowngraded
                }
            }
            'newFileThread' { [ordered]@{ kind = 'file'; path = $Path; wasDowngraded = [bool] $WasDowngraded } }
            default { $null }
        }

        [ordered]@{
            operation        = $Operation
            findingId        = $FindingId
            body             = $Body
            anchor           = $anchor
            wasDowngraded    = [bool] $WasDowngraded
            threadId         = if ($ThreadId) { $ThreadId } else { $null }
            commentId        = if ($CommentId) { $CommentId } else { $null }
            requiresReopen   = [bool] $RequiresReopen
            changeTrackingId = if ($Operation -eq 'reply') { $null } else { $ChangeTrackingId }
        }
    }

    function New-ScratchDirectory {
        $path = Join-Path $TestDrive ([guid]::NewGuid().ToString('n'))
        New-Item -ItemType Directory -Path $path -Force | Out-Null
        $path
    }

    # Runs a whole publication against the fake and hands back everything an assertion needs.
    function Invoke-TestPublication {
        param(
            [Parameter(Mandatory)]
            [AllowEmptyCollection()]
            [object[]] $Plan,

            [string] $SummaryBody = 'the summary',

            [System.Collections.IDictionary] $Transport = $null,

            [switch] $ApproveMerge
        )

        if ($null -eq $Transport) { $Transport = New-TestTransport }

        $outputDirectory = New-ScratchDirectory
        $resultPath = Get-PullRequestPublicationResultPath -OutputDirectory $outputDirectory
        $result = New-PullRequestPublicationResult `
            -Target $script:Target `
            -State (New-TestState) `
            -Plan $Plan `
            -NowUtc $script:FixedNowUtc
        Write-PullRequestPublicationResult -Result $result -Path $resultPath

        $failure = $null
        try {
            Invoke-PullRequestPublication `
                -Transport $Transport `
                -Target $script:Target `
                -State (New-TestState) `
                -Plan $Plan `
                -SummaryBody $SummaryBody `
                -Result $result `
                -ResultPath $resultPath `
                -ApproveMerge:$ApproveMerge | Out-Null
        } catch {
            $failure = $_
        }

        [ordered]@{
            outputDirectory = $outputDirectory
            resultPath      = $resultPath
            result          = $result
            persisted       = ConvertFrom-JsonVerbatim -Text ([System.IO.File]::ReadAllText($resultPath, [System.Text.Encoding]::UTF8))
            failure         = $failure
        }
    }

    # Publishes an existing thread the run can reply into, the way the capture would have recorded
    # one, and reports the identifier a follow-up finding names.
    function Add-ExistingThread {
        param([string] $Status = 'fixed')

        $thread = [ordered]@{
            id            = $script:AdoNextThreadId
            status        = $Status
            threadContext = [ordered]@{
                filePath       = '/crates/widget/src/lib.rs'
                rightFileStart = [ordered]@{ line = 4; offset = 1 }
                rightFileEnd   = [ordered]@{ line = 4; offset = 2 }
            }
            comments      = [System.Collections.Generic.List[object]]::new()
        }
        $thread['comments'].Add([ordered]@{ id = 1; parentCommentId = 0; content = 'an earlier reviewer asked a question'; commentType = 'text' })

        $script:AdoNextThreadId++
        $script:AdoThreads.Add($thread)
        [string] $thread['id']
    }

    function Get-RecordedOperation {
        param(
            [Parameter(Mandatory)]
            $Record,

            [Parameter(Mandatory)]
            [string] $Operation,

            [string] $FindingId = ''
        )

        $operations = Get-JsonArrayProperty -Node $Record -Name 'operations'
        foreach ($entry in $operations) {
            if ([string] (Get-JsonProperty -Node $entry -Name 'operation') -cne $Operation) { continue }
            if ($FindingId -and [string] (Get-JsonProperty -Node $entry -Name 'findingId') -cne $FindingId) { continue }
            return $entry
        }

        throw "The record holds no '$Operation' operation for finding '$FindingId'."
    }

    # Renders the code points of a string as text, which is what makes a fidelity failure legible: the
    # assertion names the exact code point that changed rather than showing two strings that print
    # identically.
    function ConvertTo-CodePointText {
        param([AllowEmptyString()][string] $Text)

        (([System.Globalization.StringInfo]::GetTextElementEnumerator($Text) | ForEach-Object { $_ }) |
            ForEach-Object { ([int[]] [char[]] $_ | ForEach-Object { 'U+{0:X4}' -f $_ }) -join '+' }) -join ' '
    }
}

Describe 'Azure DevOps thread payloads' {
    BeforeEach { Reset-AdoState }

    It 'posts a line thread with the file, the region and the iteration it was judged against' {
        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Body 'the finding')

        $run['failure'] | Should -BeNullOrEmpty

        $sent = (Get-AdoCallBody -Operation 'lineThread')[0]
        ConvertTo-Json -Depth 12 -Compress -InputObject $sent |
            Should -Be (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        comments                 = @([ordered]@{ parentCommentId = 0; content = 'the finding'; commentType = 'text' })
                        status                   = 'active'
                        threadContext            = [ordered]@{
                            filePath       = '/crates/widget/src/lib.rs'
                            rightFileStart = [ordered]@{ line = 10; offset = 1 }
                            rightFileEnd   = [ordered]@{ line = 10; offset = 2 }
                        }
                        pullRequestThreadContext = [ordered]@{
                            changeTrackingId = 3
                            iterationContext = [ordered]@{ firstComparingIteration = 4; secondComparingIteration = 4 }
                        }
                    }))
    }

    It 'spans a multi-line anchor from where it begins to just inside where it ends' {
        Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -StartLine 10 -EndLine 14) | Out-Null

        $context = (Get-AdoCallBody -Operation 'lineThread')[0]['threadContext']
        $context['rightFileStart']['line'] | Should -Be 10
        $context['rightFileEnd']['line'] | Should -Be 14
        $context['rightFileStart']['offset'] | Should -Be 1
        $context['rightFileEnd']['offset'] | Should -Be 2
    }

    It 'positions an anchor on the pre-change side under the names that side uses' {
        Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Side 'left') | Out-Null

        $context = (Get-AdoCallBody -Operation 'lineThread')[0]['threadContext']
        $context.Contains('leftFileStart') | Should -BeTrue
        $context.Contains('rightFileStart') | Should -BeFalse
        $context['leftFileEnd']['line'] | Should -Be 10
    }

    It 'posts a downgraded anchor as a file thread carrying no position' {
        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newFileThread' -FindingId 'F-1' -Path 'docs/readme.md' -ChangeTrackingId 7 -WasDowngraded)

        $run['failure'] | Should -BeNullOrEmpty

        $sent = (Get-AdoCallBody -Operation 'fileThread')[0]
        ConvertTo-Json -Depth 12 -Compress -InputObject $sent |
            Should -Be (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        comments                 = @([ordered]@{ parentCommentId = 0; content = 'rendered body'; commentType = 'text' })
                        status                   = 'active'
                        threadContext            = [ordered]@{ filePath = '/docs/readme.md' }
                        pullRequestThreadContext = [ordered]@{
                            changeTrackingId = 7
                            iterationContext = [ordered]@{ firstComparingIteration = 4; secondComparingIteration = 4 }
                        }
                    }))
    }

    It 'posts the summary onto the pull request itself as a settled discussion, with no position of any kind' {
        Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') `
            -SummaryBody 'the review summary' | Out-Null

        $sent = (Get-AdoCallBody -Operation 'summary')[0]
        ConvertTo-Json -Depth 12 -Compress -InputObject $sent |
            Should -Be (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        comments = @([ordered]@{ parentCommentId = 0; content = 'the review summary'; commentType = 'text' })
                        status   = 'closed'
                    }))
    }

    It 'casts no vote when the merge is not approved' {
        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $run['failure'] | Should -BeNullOrEmpty
        Get-AdoOperationSequence | Should -Not -Contain 'castVote'
        Get-AdoOperationSequence | Should -Not -Contain 'readReviewers'
    }

    It 'casts the approving vote before the summary when the merge is approved' {
        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') -ApproveMerge

        $run['failure'] | Should -BeNullOrEmpty
        $sequence = Get-AdoOperationSequence
        $sequence | Should -Contain 'readReviewers'
        $sequence | Should -Contain 'castVote'
        [array]::IndexOf($sequence, 'readReviewers') | Should -BeLessThan ([array]::IndexOf($sequence, 'castVote'))
        # The vote is idempotent and the summary is not, so the vote is cast first (docs/design.md,
        # "Review publication").
        [array]::IndexOf($sequence, 'castVote') | Should -BeLessThan ([array]::IndexOf($sequence, 'summary'))

        $vote = (Get-AdoCallBody -Operation 'castVote')[0]
        $vote['id'] | Should -Be '11111111-1111-1111-1111-111111111111'
        $vote['vote'] | Should -Be 10
        (Get-AdoCallUrl -Operation 'castVote')[0] | Should -BeLike '*/pullRequests/42/reviewers/11111111-1111-1111-1111-111111111111?*'
    }

    It 'preserves an existing reviewer whose isRequired value is <Required>' -ForEach @(
        @{ Required = $true }
        @{ Required = $false }
        @{ Required = $null }
    ) {
        $reviewer = [ordered]@{ id = '11111111-1111-1111-1111-111111111111'; vote = -5 }
        if ($null -ne $Required) { $reviewer['isRequired'] = $Required }
        $script:AdoReviewers = @(
            [ordered]@{ id = 'another-reviewer'; vote = 5; isRequired = -not $Required }
            $reviewer
        )

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') -ApproveMerge

        $run['failure'] | Should -BeNullOrEmpty
        $votes = Get-AdoCallBody -Operation 'castVote'
        $votes.Count | Should -Be 1
        ConvertTo-Json -Depth 6 -Compress -InputObject $votes[0] |
            Should -Be (ConvertTo-Json -Depth 6 -Compress -InputObject ([ordered]@{
                        id         = $reviewer['id']
                        vote       = 10
                        isRequired = [bool] $Required
                    }))
        (Get-AdoCallUrl -Operation 'castVote')[0] | Should -BeLike "*/pullRequests/42/reviewers/$($reviewer['id'])?*"
    }

    It 'preserves required status when an existing reviewer matches by <Field>' -ForEach @(
        @{ Field = 'uniqueName'; Value = 'runner@example.com' }
        @{ Field = 'descriptor'; Value = 'aad.cnVubmVy' }
    ) {
        $reviewer = [ordered]@{ id = 'another-identifier-for-the-runner'; vote = 0; isRequired = $true }
        $reviewer[$Field] = $Value
        $script:AdoReviewers = @($reviewer)

        $run = Invoke-TestPublication -Plan @() -ApproveMerge

        $run['failure'] | Should -BeNullOrEmpty
        (Get-AdoCallBody -Operation 'castVote')[0]['isRequired'] | Should -BeTrue
    }

    It 'uses optional status when the approving account is not yet a reviewer' {
        $script:AdoReviewers = @([ordered]@{ id = 'another-reviewer'; vote = 5; isRequired = $true })

        $run = Invoke-TestPublication -Plan @() -ApproveMerge

        $run['failure'] | Should -BeNullOrEmpty
        $vote = (Get-AdoCallBody -Operation 'castVote')[0]
        $vote['id'] | Should -Be '11111111-1111-1111-1111-111111111111'
        $vote['vote'] | Should -Be 10
        $vote['isRequired'] | Should -BeFalse
    }

    It 'does not cast a vote or post the summary when reading reviewers fails with HTTP <StatusCode>' -ForEach @(
        @{ StatusCode = 403 }
        @{ StatusCode = 404 }
        @{ StatusCode = 500 }
    ) {
        Add-AdoFault -Pattern '^GET .*/reviewers\?' -StatusCode $StatusCode

        $run = Invoke-TestPublication -Plan @() -ApproveMerge -Transport (New-TestTransport -MaxAttempts 1)

        $run['failure'] | Should -Not -BeNullOrEmpty
        $run['failure'].Exception.Message | Should -BeLike "*HTTP $StatusCode*"
        Get-AdoOperationSequence | Should -Not -Contain 'castVote'
        Get-AdoOperationSequence | Should -Not -Contain 'summary'
    }

    It 'records an approving vote and a summary alone when the review raises no findings' {
        $run = Invoke-TestPublication -Plan @() -SummaryBody 'the review summary' -ApproveMerge

        $run['failure'] | Should -BeNullOrEmpty
        $sequence = Get-AdoOperationSequence
        $sequence | Should -Contain 'castVote'
        $sequence | Should -Contain 'summary'
        # A clean pull request posts no finding, so the vote and the summary are the only writes.
        $sequence | Should -Not -Contain 'lineThread'
        $sequence | Should -Not -Contain 'fileThread'
        [array]::IndexOf($sequence, 'castVote') | Should -BeLessThan ([array]::IndexOf($sequence, 'summary'))

        (Get-AdoCallBody -Operation 'castVote')[0]['vote'] | Should -Be 10
        (Get-AdoCallBody -Operation 'castVote')[0]['isRequired'] | Should -BeFalse
        (Get-AdoCallBody -Operation 'summary')[0]['comments'][0]['content'] | Should -Be 'the review summary'
    }

    It 'answers an existing thread by naming the comment that opened it' {
        $threadId = Add-ExistingThread

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -Body 'the answer' -ThreadId $threadId -CommentId '1')

        $run['failure'] | Should -BeNullOrEmpty

        $sent = (Get-AdoCallBody -Operation 'reply')[0]
        ConvertTo-Json -Depth 12 -Compress -InputObject $sent |
            Should -Be (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        parentCommentId = 1
                        content         = 'the answer'
                        commentType     = 'text'
                    }))
        (Get-AdoCallUrl -Operation 'reply')[0] | Should -BeLike "*/pullRequests/42/threads/$threadId/comments?*"
    }

    It 'adds the leading separator Azure DevOps states a file path with' {
        Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newFileThread' -FindingId 'F-1' -Path 'docs/readme.md' -ChangeTrackingId 7) | Out-Null

        (Get-AdoCallBody -Operation 'fileThread')[0]['threadContext']['filePath'] | Should -Be '/docs/readme.md'
    }

    It 'records what it published under the identifiers the service assigned' {
        $threadId = Add-ExistingThread -Status 'active'

        $run = Invoke-TestPublication -Plan @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')
            (New-PlanEntry -Operation 'newFileThread' -FindingId 'F-2' -Path 'docs/readme.md' -ChangeTrackingId 7)
            (New-PlanEntry -Operation 'reply' -FindingId 'F-3' -ThreadId $threadId -CommentId '1')
        )

        $first = Get-RecordedOperation -Record $run['persisted'] -Operation 'newLineThread' -FindingId 'F-1'
        $first['threadId'] | Should -Be '102'
        $first['commentId'] | Should -Be '1'

        $second = Get-RecordedOperation -Record $run['persisted'] -Operation 'newFileThread' -FindingId 'F-2'
        $second['threadId'] | Should -Be '103'
        $second['commentId'] | Should -Be '1'

        # A reply is recorded under the identifier the service gave the reply, not under the one the
        # request named as its parent. Both are small ordinals within the same thread, so recording
        # the wrong one would still address an existing comment.
        $third = Get-RecordedOperation -Record $run['persisted'] -Operation 'reply' -FindingId 'F-3'
        $third['threadId'] | Should -Be $threadId
        $third['commentId'] | Should -Be '2'
    }
}

Describe 'Azure DevOps change tracking identifiers' {
    BeforeEach { Reset-AdoState }

    It 'reports the identifier the newest iteration assigned to each anchored file' {
        $identity = Get-PullRequestFileIdentityForTarget `
            -Transport (New-TestTransport) `
            -Target $script:Target `
            -State (New-TestState) `
            -Path @('crates/widget/src/lib.rs', 'docs/readme.md')

        $identity['crates/widget/src/lib.rs'] | Should -Be 3
        $identity['docs/readme.md'] | Should -Be 7
        (Get-AdoCallUrl -Operation 'changes')[0] | Should -BeLike "*/iterations/$($script:IterationId)/changes?*"
    }

    It 'asks nothing when the review anchors nothing' {
        $identity = Get-PullRequestFileIdentityForTarget `
            -Transport (New-TestTransport) `
            -Target $script:Target `
            -State (New-TestState) `
            -Path @()

        $identity.Count | Should -Be 0
        Get-AdoOperationSequence | Should -Be @()
    }

    It 'follows the change listing across every page the service serves' {
        $script:AdoIterationChangePages = @(
            '{"changeEntries":[{"changeTrackingId":3,"item":{"path":"/crates/widget/src/lib.rs"}}],"nextSkip":1}'
            '{"changeEntries":[{"changeTrackingId":7,"item":{"path":"/docs/readme.md"}}]}'
        )

        $identity = Get-PullRequestFileIdentityForTarget `
            -Transport (New-TestTransport) `
            -Target $script:Target `
            -State (New-TestState) `
            -Path @('docs/readme.md')

        $identity['docs/readme.md'] | Should -Be 7
    }

    It 'refuses the review before it starts when the cumulative change listing does not contain an anchored file' {
        $transport = New-TestTransport

        { Get-PullRequestFileIdentityForTarget `
                -Transport $transport `
                -Target $script:Target `
                -State (New-TestState) `
                -Path @('docs/readme.md', 'src/untouched.rs') } |
            Should -Throw -ExpectedMessage "*src/untouched.rs*changed by the pull request as of iteration $($script:IterationId)*"

        # The refusal is worth nothing unless it is complete. Publication has not begun, so the pull
        # request must carry no comment and no thread must have changed status.
        Get-AdoOperationSequence | Should -Not -Contain 'lineThread'
        Get-AdoOperationSequence | Should -Not -Contain 'fileThread'
        Get-AdoOperationSequence | Should -Not -Contain 'reply'
        Get-AdoOperationSequence | Should -Not -Contain 'summary'
        Get-AdoOperationSequence | Should -Not -Contain 'threadStatus'
        @($script:AdoCalls | Where-Object { [string] $_['method'] -cne 'GET' }).Count | Should -Be 0
        $script:AdoThreads.Count | Should -Be 0
    }

    It 'names every file the cumulative change listing does not contain, so one pass settles them all' {
        { Get-PullRequestFileIdentityForTarget `
                -Transport (New-TestTransport) `
                -Target $script:Target `
                -State (New-TestState) `
                -Path @('src/untouched.rs', 'docs/readme.md', 'src/removed.rs') } |
            Should -Throw -ExpectedMessage '*src/untouched.rs, src/removed.rs*'
    }

    It 'refuses a plan entry carrying no identity, so a positionless thread is never posted' {
        # Unreachable through the executable, which reads the file identities first. The guard stands
        # because the alternative is a thread request naming no change to position itself against,
        # and a comment the service accepted on those terms is published and cannot be withdrawn.
        $transport = New-TestTransport
        $review = Start-PullRequestReviewForTarget -Transport $transport -Target $script:Target -State (New-TestState)

        $entry = New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Path 'src/untouched.rs'
        $entry['changeTrackingId'] = $null

        { Add-PullRequestReviewFindingForTarget `
                -Transport $transport `
                -Target $script:Target `
                -Review $review `
                -Operation $entry } |
            Should -Throw -ExpectedMessage "*no change tracking identifier for 'src/untouched.rs'*"

        @($script:AdoCalls | Where-Object { [string] $_['method'] -cne 'GET' }).Count | Should -Be 0
    }
}

Describe 'Azure DevOps immediate publication' {
    BeforeEach { Reset-AdoState }

    It 'writes each finding as it goes and posts the summary after all of them' {
        $threadId = Add-ExistingThread -Status 'active'

        $run = Invoke-TestPublication -Plan @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')
            (New-PlanEntry -Operation 'newFileThread' -FindingId 'F-2' -Path 'docs/readme.md' -ChangeTrackingId 7)
            (New-PlanEntry -Operation 'reply' -FindingId 'F-3' -ThreadId $threadId -CommentId '1')
        )

        $run['failure'] | Should -BeNullOrEmpty
        Get-AdoOperationSequence | Should -Be @('lineThread', 'fileThread', 'reply', 'readThreads', 'summary', 'readThreads')
    }

    It 'opens no review, because there is none to open' {
        $review = Start-PullRequestReviewForTarget `
            -Transport (New-TestTransport) `
            -Target $script:Target `
            -State (New-TestState)

        Get-AdoOperationSequence | Should -Be @()
        $review['reviewId'] | Should -Be "iteration/$($script:IterationId)"
        $review['iterationId'] | Should -Be $script:IterationId
    }

    It 'reports the summary thread as the place the review can be read' {
        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $run['result']['reviewUrl'] | Should -Be "$($script:Target['webUrl'])?discussionId=102"
    }

    It 'refuses to discard a draft the service never held' {
        { Remove-PullRequestPendingReviewForTarget `
                -Transport (New-TestTransport) `
                -Target $script:Target `
                -ReviewId 'iteration/4' } |
            Should -Throw -ExpectedMessage '*holds no draft review to discard*'
    }

    It 'leaves the preflight nothing to clear, so it asks the service for nothing' {
        $state = Clear-PullRequestPendingReview `
            -Transport (New-TestTransport) `
            -Target $script:Target `
            -State (New-TestState)

        $state['pendingReviewId'] | Should -BeNullOrEmpty
        Get-AdoOperationSequence | Should -Be @()
    }
}

Describe 'Azure DevOps verification' {
    BeforeEach { Reset-AdoState }

    It 'reads the threads back and reports what the service holds' {
        $run = Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Body 'the finding') `
            -SummaryBody 'the summary'

        $run['failure'] | Should -BeNullOrEmpty

        $verification = $run['result']['verification']
        $verification['isPublished'] | Should -BeTrue
        $verification['state'] | Should -Be 'published'
        $verification['body'] | Should -Be 'the summary'
        @($verification['comments']).Count | Should -Be 1
        $verification['comments'][0]['isPublished'] | Should -BeTrue
        $verification['comments'][0]['body'] | Should -Be 'the finding'
        $verification['comments'][0]['path'] | Should -Be 'crates/widget/src/lib.rs'
        $verification['comments'][0]['line'] | Should -Be 10
        $verification['comments'][0]['threadId'] | Should -Be '101'
        $verification['comments'][0]['subjectType'] | Should -Be 'LINE'
        $verification['comments'][0]['state'] | Should -Be 'published'
        $verification['comments'][0]['rawStatus'] | Should -Be 'active'
    }

    It 'refuses to post the summary when finding verification cannot find an accepted thread' {
        Add-AdoFault -Pattern '^GET .*/threads\?' -StatusCode 200 -Body '{"count":0,"value":[]}' -Times 1

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $run['failure'] | Should -Not -BeNullOrEmpty
        $run['failure'].Exception.Message | Should -BeLike "*Finding 'F-1'*"
        Get-AdoOperationSequence | Should -Be @('lineThread', 'readThreads')
        Get-AdoOperationSequence | Should -Not -Contain 'summary'
        $run['persisted']['preCompletionVerification'] | Should -Not -BeNullOrEmpty
        (Get-RecordedOperation -Record $run['persisted'] -Operation 'verifyFindings')['outcome'] | Should -Be 'failed'
        (Get-RecordedOperation -Record $run['persisted'] -Operation 'completeReview')['outcome'] | Should -Be 'pending'
    }

    It 'reports a multi-line comment against the last line of the region it covers' {
        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -StartLine 10 -EndLine 14)

        $run['failure'] | Should -BeNullOrEmpty
        $run['result']['verification']['comments'][0]['line'] | Should -Be 14
    }

    It 'reports a file comment under the subject that names it, and a reply under the thread it joined' {
        $threadId = Add-ExistingThread -Status 'active'

        $run = Invoke-TestPublication -Plan @(
            (New-PlanEntry -Operation 'newFileThread' -FindingId 'F-1' -Path 'docs/readme.md' -ChangeTrackingId 7)
            (New-PlanEntry -Operation 'reply' -FindingId 'F-2' -ThreadId $threadId -CommentId '1')
        )

        $run['failure'] | Should -BeNullOrEmpty

        $comments = $run['result']['verification']['comments']
        $comments[0]['subjectType'] | Should -Be 'FILE'
        $comments[0]['path'] | Should -Be 'docs/readme.md'
        $comments[0]['line'] | Should -BeNullOrEmpty
        $comments[1]['subjectType'] | Should -Be 'LINE'
    }

    It 'reports a comment on the pull request itself under the subject that names it' {
        $transport = New-TestTransport
        $review = Start-PullRequestReviewForTarget -Transport $transport -Target $script:Target -State (New-TestState)

        # An anchorless finding is written onto the pull request rather than onto a file. The plan
        # builder does not produce one today, and the adapter answers for it regardless so that a
        # finding can never be silently dropped for want of a position.
        $entry = New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1'
        $entry['anchor'] = $null
        $entry['changeTrackingId'] = $null

        Add-PullRequestReviewFindingForTarget -Transport $transport -Target $script:Target -Review $review -Operation $entry | Out-Null
        Complete-PullRequestReviewForTarget -Transport $transport -Target $script:Target -Review $review -SummaryBody 'the summary' | Out-Null
        $verification = Confirm-PullRequestReviewForTarget -Transport $transport -Target $script:Target -Review $review

        $verification['comments'][0]['subjectType'] | Should -Be 'PULL_REQUEST'
        $verification['comments'][0]['path'] | Should -Be ''
        $verification['comments'][0]['line'] | Should -BeNullOrEmpty
    }

    It 'refuses a review whose summary the service does not report' {
        # The summary is written last, so a pull request that does not carry it does not carry a
        # complete review either.
        Add-AdoFault -Pattern '^GET .*/threads\?' -StatusCode 200 -Body '{"count":0,"value":[]}' -Skip 1 -Times 1

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $run['failure'] | Should -Not -BeNullOrEmpty
        $run['failure'].Exception.Message | Should -BeLike "*reports state 'unpublished'*"
        $run['persisted']['status'] | Should -Be 'failed'
    }

    It 'refuses a review the service accepted and then published short' {
        # The pull request answers with the summary but not with the finding thread, which is the
        # case a summary counted among the findings would hide.
        Add-AdoFault -Pattern '^GET .*/threads\?' -StatusCode 200 -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                    count = 1
                    value = @(
                        [ordered]@{
                            id       = 102
                            status   = 'closed'
                            comments = @([ordered]@{ id = 1; parentCommentId = 0; content = 'the summary'; commentType = 'text' })
                        }
                    )
                })) -Skip 1 -Times 1

        $run = Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') `
            -SummaryBody 'the summary'

        $run['failure'] | Should -Not -BeNullOrEmpty
        $run['failure'].Exception.Message | Should -BeLike '*published fewer comments than it was given*'

        $verification = $run['result']['verification']
        $verification['isPublished'] | Should -BeTrue
        $verification['state'] | Should -Be 'published'
        @($verification['comments']).Count | Should -Be 1
        $verification['comments'][0]['isPublished'] | Should -BeFalse
        $verification['comments'][0]['state'] | Should -Be 'unpublished'
    }

    It 'counts a withdrawn comment as one the pull request no longer carries' {
        $transport = New-TestTransport
        $review = Start-PullRequestReviewForTarget -Transport $transport -Target $script:Target -State (New-TestState)
        Add-PullRequestReviewFindingForTarget -Transport $transport -Target $script:Target -Review $review -Operation (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') | Out-Null
        Complete-PullRequestReviewForTarget -Transport $transport -Target $script:Target -Review $review -SummaryBody 'the summary' | Out-Null

        $script:AdoThreads[0]['comments'][0]['isDeleted'] = $true
        $verification = Confirm-PullRequestReviewForTarget -Transport $transport -Target $script:Target -Review $review

        $verification['comments'][0]['isPublished'] | Should -BeFalse
    }
}

Describe 'Azure DevOps follow-up reopen' {
    BeforeEach { Reset-AdoState }

    It 'replies first and reopens the thread afterwards' {
        $threadId = Add-ExistingThread -Status 'fixed'

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId $threadId -CommentId '1' -RequiresReopen)

        $run['failure'] | Should -BeNullOrEmpty
        Get-AdoOperationSequence | Should -Be @('reply', 'readThreads', 'summary', 'readThreads', 'threadStatus')

        $comment = $run['result']['verification']['comments'][0]
        $comment['state'] | Should -Be 'published'
        $comment['rawStatus'] | Should -Be 'fixed'
        $comment['isPublished'] | Should -BeTrue
        (Get-AdoCallBody -Operation 'threadStatus')[0]['status'] | Should -Be 'active'
        (Get-AdoCallUrl -Operation 'threadStatus')[0] | Should -BeLike "*/threads/${threadId}?*"
        (Find-AdoStoredThread -ThreadId $threadId)['status'] | Should -Be 'active'
    }

    It 'leaves a thread that was already open untouched' {
        $threadId = Add-ExistingThread -Status 'active'

        Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId $threadId -CommentId '1') | Out-Null

        Get-AdoOperationSequence | Should -Not -Contain 'threadStatus'
    }

    It 'reopens nothing when the run never completed' {
        $threadId = Add-ExistingThread -Status 'fixed'
        Add-AdoFault -Pattern '^POST .*/threads\?' -StatusCode 400 -Body '{"message":"the thread was refused"}'

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId $threadId -CommentId '1' -RequiresReopen)

        $run['failure'] | Should -Not -BeNullOrEmpty
        Get-AdoOperationSequence | Should -Not -Contain 'threadStatus'
    }

    It 'reports a published review whose thread stayed resolved as published with a follow-up failure' {
        $threadId = Add-ExistingThread -Status 'fixed'
        Add-AdoFault -Pattern '^PATCH .*/threads/\d+\?' -StatusCode 400 -Body '{"message":"the thread is locked"}'

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId $threadId -CommentId '1' -RequiresReopen)

        $run['failure'] | Should -BeNullOrEmpty
        $run['persisted']['status'] | Should -Be 'publishedWithFollowUpFailure'
        $run['persisted']['message'] | Should -BeLike "*$threadId*"
        $run['persisted']['message'] | Should -BeLike '*must not be published again*'

        $recorded = Get-RecordedOperation -Record $run['persisted'] -Operation 'reopenThread' -FindingId 'F-1'
        $recorded['outcome'] | Should -Be 'failed'
        $recorded['threadId'] | Should -Be $threadId
        $recorded['message'] | Should -BeLike '*the thread is locked*'
        (Get-RecordedOperation -Record $run['persisted'] -Operation 'reply' -FindingId 'F-1')['outcome'] | Should -Be 'succeeded'
    }
}

Describe 'Azure DevOps write policy' {
    BeforeEach { Reset-AdoState }

    It 'attempts a failed thread exactly once, even when the failure looks transient' {
        Add-AdoFault -Pattern '^POST .*/threads\?' -StatusCode 503

        $run = Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') `
            -Transport (New-TestTransport -MaxAttempts 5)

        $run['failure'] | Should -Not -BeNullOrEmpty
        @(Get-AdoOperationSequence | Where-Object { $_ -eq 'lineThread' }).Count | Should -Be 1
        $script:AdoWaits.Count | Should -Be 0
    }

    It 'attempts a failed reply exactly once' {
        $threadId = Add-ExistingThread
        Add-AdoFault -Pattern '^POST .*/threads/\d+/comments\?' -StatusCode 503

        Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId $threadId -CommentId '1') `
            -Transport (New-TestTransport -MaxAttempts 5) | Out-Null

        @(Get-AdoOperationSequence | Where-Object { $_ -eq 'reply' }).Count | Should -Be 1
        $script:AdoWaits.Count | Should -Be 0
    }

    It 'attempts a failed summary exactly once' {
        Add-AdoFault -Pattern '^POST .*/threads\?' -StatusCode 503

        Invoke-TestPublication `
            -Plan @() `
            -Transport (New-TestTransport -MaxAttempts 5) | Out-Null

        @(Get-AdoOperationSequence | Where-Object { $_ -eq 'summary' }).Count | Should -Be 1
        $script:AdoWaits.Count | Should -Be 0
    }

    It 'attempts a failed reopen exactly once' {
        $threadId = Add-ExistingThread -Status 'fixed'
        Add-AdoFault -Pattern '^PATCH .*/threads/\d+\?' -StatusCode 503

        Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId $threadId -CommentId '1' -RequiresReopen) `
            -Transport (New-TestTransport -MaxAttempts 5) | Out-Null

        @(Get-AdoOperationSequence | Where-Object { $_ -eq 'threadStatus' }).Count | Should -Be 1
        $script:AdoWaits.Count | Should -Be 0
    }

    It 'does not re-send a write the service answered with a rejected token' {
        # A refreshed attempt would send the same body a second time, which on a service that
        # publishes as it writes is a duplicate comment nobody can withdraw.
        Add-AdoFault -Pattern '^POST .*/threads\?' -StatusCode 401 -Body '{"message":"token rejected"}'

        Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') `
            -Transport (New-TestTransport -MaxAttempts 5) | Out-Null

        @(Get-AdoOperationSequence | Where-Object { $_ -eq 'lineThread' }).Count | Should -Be 1
    }
}

Describe 'Azure DevOps publication record' {
    BeforeEach { Reset-AdoState }

    It 'rewrites the record after every operation, so a run that dies is accounted for' {
        $threadId = Add-ExistingThread
        Add-AdoFault -Pattern '^POST .*/threads\?' -StatusCode 400 -Body '{"message":"TF401398: the file is not part of the iteration"}' -Times 1

        $plan = @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')
            (New-PlanEntry -Operation 'newFileThread' -FindingId 'F-2' -Path 'docs/readme.md' -ChangeTrackingId 7)
            (New-PlanEntry -Operation 'reply' -FindingId 'F-3' -ThreadId $threadId -CommentId '1')
        )

        $run = Invoke-TestPublication -Plan $plan
        $record = $run['persisted']

        (Get-RecordedOperation -Record $record -Operation 'startReview')['outcome'] | Should -Be 'succeeded'
        (Get-RecordedOperation -Record $record -Operation 'newLineThread' -FindingId 'F-1')['outcome'] | Should -Be 'failed'
        (Get-RecordedOperation -Record $record -Operation 'newFileThread' -FindingId 'F-2')['outcome'] | Should -Be 'pending'
        (Get-RecordedOperation -Record $record -Operation 'reply' -FindingId 'F-3')['outcome'] | Should -Be 'pending'
        (Get-RecordedOperation -Record $record -Operation 'completeReview')['outcome'] | Should -Be 'pending'
        $record['status'] | Should -Be 'failed'
        $record['reviewId'] | Should -Be "iteration/$($script:IterationId)"
    }

    It 'records a refusal the service reported as a definite failure' {
        Add-AdoFault -Pattern '^POST .*/threads\?' -StatusCode 400 -Body '{"message":"TF401398: the file is not part of the iteration"}'

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $entry = Get-RecordedOperation -Record $run['persisted'] -Operation 'newLineThread' -FindingId 'F-1'
        $entry['outcome'] | Should -Be 'failed'
        $entry['message'] | Should -BeLike '*TF401398*'
    }

    It 'records a server-side write fault as ambiguous' {
        Add-AdoFault -Pattern '^POST .*/threads\?' -StatusCode 503 -Body '{"message":"the service did not finish the request"}'

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $entry = Get-RecordedOperation -Record $run['persisted'] -Operation 'newLineThread' -FindingId 'F-1'
        $entry['outcome'] | Should -Be 'ambiguous'
        $entry['message'] | Should -BeLike '*503*'
    }

    It 'records a lost response as ambiguous rather than as a failure' {
        Add-AdoFault -Pattern '^POST .*/threads\?' -Lost

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $entry = Get-RecordedOperation -Record $run['persisted'] -Operation 'newLineThread' -FindingId 'F-1'
        $entry['outcome'] | Should -Be 'ambiguous'
        $entry['message'] | Should -BeLike '*the connection was closed*'
    }

    It 'records the ambiguity before the run stops, so a killed run still states it' {
        Add-AdoFault -Pattern '^POST .*/threads\?' -Lost

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        # The assertion reads the file rather than the in-memory record: the guarantee is about what
        # survives the process, not about what the run happened to be holding.
        $onDisk = ConvertFrom-JsonVerbatim -Text ([System.IO.File]::ReadAllText($run['resultPath'], [System.Text.Encoding]::UTF8))
        (Get-RecordedOperation -Record $onDisk -Operation 'newLineThread' -FindingId 'F-1')['outcome'] |
            Should -Be 'ambiguous'
    }

    It 'states that the comments a failed run wrote remain on the pull request' {
        Add-AdoFault -Pattern '^POST .*/threads\?' -StatusCode 400 -Body '{"message":"the thread was refused"}' -Times 1

        $run = Invoke-TestPublication -Plan @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')
            (New-PlanEntry -Operation 'newFileThread' -FindingId 'F-2' -Path 'docs/readme.md' -ChangeTrackingId 7)
        )

        $run['persisted']['draftDiscarded'] | Should -BeFalse
        $run['persisted']['draftDiscardMessage'] | Should -BeLike '*holds no draft review to discard*'
    }

    It 'summarizes what reached the pull request rather than what was intended' {
        $run = Invoke-TestPublication -Plan @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')
            (New-PlanEntry -Operation 'newFileThread' -FindingId 'F-2' -Path 'docs/readme.md' -ChangeTrackingId 7 -WasDowngraded)
        )

        $outcome = New-PullRequestPublicationOutcome -Result $run['result'] -ResultPath $run['resultPath']

        $outcome['status'] | Should -Be 'succeeded'
        $outcome['platform'] | Should -Be 'azureDevOps'
        $outcome['publishedFindingCount'] | Should -Be 2
        $outcome['downgradedAnchorCount'] | Should -Be 1
        $outcome['failedOperationCount'] | Should -Be 0
        $outcome['ambiguousOperationCount'] | Should -Be 0
    }
}

Describe 'Azure DevOps publication text fidelity' {
    BeforeEach { Reset-AdoState }

    It 'carries every code point of a finding body and a summary through unchanged' {
        $body = "Rename $($script:LeftQuote)widget$($script:RightQuote) $($script:EmDash) it is a $($script:Robot) name."
        $summary = "Review complete $($script:EmDash) $($script:Robot)"

        $run = Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Body $body) `
            -SummaryBody $summary

        $run['failure'] | Should -BeNullOrEmpty

        $sent = (Get-AdoCallBody -Operation 'lineThread')[0]['comments'][0]['content']
        ConvertTo-CodePointText -Text $sent | Should -Be (ConvertTo-CodePointText -Text $body)

        $sentSummary = (Get-AdoCallBody -Operation 'summary')[0]['comments'][0]['content']
        ConvertTo-CodePointText -Text $sentSummary | Should -Be (ConvertTo-CodePointText -Text $summary)

        $readBack = $run['result']['verification']['comments'][0]['body']
        ConvertTo-CodePointText -Text $readBack | Should -Be (ConvertTo-CodePointText -Text $body)
    }

    It 'sends the body as UTF-8 bytes' {
        $body = "A $($script:Robot) finding"

        Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Body $body) | Out-Null

        # The transport hands the body to the sender as text and the sender encodes it, so the
        # assertion is that the text still holds the code points after the round trip through JSON.
        $sent = (Get-AdoCallBody -Operation 'lineThread')[0]['comments'][0]['content']
        [System.Text.Encoding]::UTF8.GetBytes($sent) | Should -Be ([System.Text.Encoding]::UTF8.GetBytes($body))
    }

    It 'stores the record as UTF-8 without a byte order mark' {
        $body = "A $($script:Robot) finding"

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Body $body)

        $bytes = [System.IO.File]::ReadAllBytes($run['resultPath'])
        $bytes[0] | Should -Not -Be 0xEF
        $record = ConvertFrom-JsonVerbatim -Text ([System.Text.Encoding]::UTF8.GetString($bytes))
        ConvertTo-CodePointText -Text ([string] $record['verification']['comments'][0]['body']) |
            Should -Be (ConvertTo-CodePointText -Text $body)
    }
}

Describe 'Azure DevOps publication state' {
    BeforeEach { Reset-AdoState }

    It 'reports the newest iteration, its commits and the account about to comment' {
        $state = Get-PullRequestPublicationStateForTarget -Transport (New-TestTransport) -Target $script:Target

        $state['platform'] | Should -Be 'azureDevOps'
        $state['isActive'] | Should -BeTrue
        $state['status'] | Should -Be 'active'
        $state['sourceCommit'] | Should -Be $script:SourceCommit
        $state['mergeBaseCommit'] | Should -Be $script:MergeBaseCommit
        $state['iterationId'] | Should -Be $script:IterationId
        $state['identity'] | Should -Be 'runner@example.com'
        $state['url'] | Should -Be ([string] $script:Target['webUrl'])
    }

    It 'reports no draft review, because the service holds none' {
        $state = Get-PullRequestPublicationStateForTarget -Transport (New-TestTransport) -Target $script:Target

        $state.Contains('pendingReviewId') | Should -BeTrue
        $state['pendingReviewId'] | Should -BeNullOrEmpty
    }

    It 'reports a completed pull request as no longer accepting a review' {
        $script:AdoPullRequestStatus = 'completed'

        $state = Get-PullRequestPublicationStateForTarget -Transport (New-TestTransport) -Target $script:Target

        $state['isActive'] | Should -BeFalse
        $state['status'] | Should -Be 'completed'
    }

    It 'refuses a pull request that reports no iteration at all' {
        Add-AdoFault -Pattern '^GET .*/iterations\?' -StatusCode 200 -Body '{"count":0,"value":[]}'

        { Get-PullRequestPublicationStateForTarget -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw -ExpectedMessage '*no iteration*'
    }
}
