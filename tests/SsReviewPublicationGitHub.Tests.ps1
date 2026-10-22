BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:LibraryRoot = Join-Path $script:RepoRoot 'skills\ss-review\scripts\lib'

    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PublicationFailure.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubCliTransport.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')
    . (Join-Path $script:LibraryRoot 'WorkflowData.ps1')
    . (Join-Path $script:LibraryRoot 'FindingModel.ps1')
    . (Join-Path $script:LibraryRoot 'GitDiff.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestAnchor.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestPlatform.ps1')
    . (Join-Path $script:LibraryRoot 'ReviewPublication.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestPublication.ps1')

    # Characters are built from their code points so the fixtures never depend on how this file is
    # encoded on disk, and so the assertions compare code points rather than rendered glyphs.
    $script:EmDash = [string] [char]0x2014
    $script:LeftQuote = [string] [char]0x201C
    $script:RightQuote = [string] [char]0x201D
    $script:Robot = [char]::ConvertFromUtf32(0x1F916)

    # Every transport built here reads this instant instead of a clock, so nothing in this file
    # depends on when it runs.
    $script:FixedNowUtc = [DateTimeOffset]::new(2026, 8, 4, 12, 0, 0, [TimeSpan]::Zero)

    $script:SourceCommit = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
    $script:MergeBaseCommit = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'

    $script:Target = [ordered]@{
        platform      = 'gitHub'
        collectionUrl = $null
        organization  = 'octo'
        project       = $null
        repository    = 'widget'
        pullRequestId = 7
        webUrl        = 'https://github.com/octo/widget/pull/7'
    }

    function New-GhResponse {
        param(
            [string] $Body = '',
            [int] $ExitCode = 0,
            [string] $StandardError = ''
        )

        [ordered]@{
            exitCode            = $ExitCode
            standardOutputBytes = [Text.Encoding]::UTF8.GetBytes($Body)
            standardErrorText   = $StandardError
        }
    }

    # Registers a canned reply for every invocation whose command line and standard input contain the
    # key. Later registrations win, so a test overrides a default fixture by registering again.
    # Supplying several replies serves them in order and then repeats the last, which is what lets a
    # sequence of writes and a retried request be expressed without a bespoke fake per test.
    function Add-GhRoute {
        param(
            [Parameter(Mandatory)]
            [string] $Key,

            [Parameter(Mandatory)]
            [object[]] $Response
        )

        $script:GhRoutes.Add([ordered]@{
                key       = $Key
                responses = [System.Collections.Generic.List[object]]::new([object[]] $Response)
            })
    }

    function New-TestTransport {
        param(
            [int] $MaxAttempts = 5,
            [DateTimeOffset] $NowUtc = $script:FixedNowUtc
        )

        $routingInvoker = {
            param(
                [string[]] $ArgumentList,
                [string] $StandardInput
            )

            $script:GhCalls.Add([ordered]@{
                    arguments     = @($ArgumentList)
                    standardInput = [string] $StandardInput
                })

            $composite = ($ArgumentList -join ' ') + ' ' + [string] $StandardInput
            for ($index = $script:GhRoutes.Count - 1; $index -ge 0; $index--) {
                $route = $script:GhRoutes[$index]
                if (-not $composite.Contains([string] $route['key'])) { continue }
                $responses = $route['responses']
                if ($responses.Count -eq 0) {
                    throw "The fake GitHub CLI ran out of replies for '$($route['key'])'."
                }
                $response = $responses[0]
                if ($responses.Count -gt 1) { $responses.RemoveAt(0) }
                return $response
            }

            throw "The fake GitHub CLI has no reply for 'gh $($ArgumentList -join ' ')'."
        }

        $recordingWait = {
            param([int] $Milliseconds)

            $script:GhWaits.Add($Milliseconds)
        }

        # The closure is taken so the fixed instant survives the return of this function; a bare
        # block would look the name up in whatever scope the transport runs it from.
        $clock = { $NowUtc }.GetNewClosure()

        New-GitHubCliTransport `
            -InvokeGh $routingInvoker `
            -Wait $recordingWait `
            -GetUtcNow $clock `
            -MaxAttempts $MaxAttempts
    }

    function Reset-GhState {
        $script:GhRoutes = [System.Collections.Generic.List[object]]::new()
        $script:GhCalls = [System.Collections.Generic.List[object]]::new()
        $script:GhWaits = [System.Collections.Generic.List[int]]::new()
    }

    # The GraphQL documents each carry a fragment no other document carries, which is what lets the
    # routing table stand in for a service without parsing the query.
    $script:StartReviewKey = 'addPullRequestReview(input:'
    $script:LineThreadKey = 'subjectType:LINE'
    $script:RangeThreadKey = 'startLine:$startLine'
    $script:FileThreadKey = 'subjectType:FILE'
    $script:ReplyKey = 'addPullRequestReviewThreadReply'
    $script:SubmitKey = 'submitPullRequestReview'
    $script:DeleteKey = 'deletePullRequestReview'
    $script:UnresolveKey = 'unresolveReviewThread'
    $script:VerifyKey = 'node(id:$reviewId'
    $script:PendingReviewKey = 'states:[PENDING]'

    function Get-GhOperationSequence {
        $sequence = [System.Collections.Generic.List[string]]::new()
        foreach ($call in $script:GhCalls) {
            $text = [string] $call['standardInput']
            $name = switch -Regex ($text) {
                'addPullRequestReviewThreadReply' { 'reply'; break }
                'subjectType:FILE' { 'fileThread'; break }
                'subjectType:LINE' { 'lineThread'; break }
                'addPullRequestReview\(input:' { 'startReview'; break }
                'submitPullRequestReview' { 'submit'; break }
                'deletePullRequestReview' { 'delete'; break }
                'unresolveReviewThread' { 'unresolve'; break }
                'node\(id:\$reviewId' { 'verify'; break }
                'states:\[PENDING\]' { 'pendingReview'; break }
                default { 'other' }
            }
            $sequence.Add($name)
        }
        $sequence.ToArray()
    }

    function Get-GhCallVariable {
        param(
            [Parameter(Mandatory)]
            [string] $Operation
        )

        $found = [System.Collections.Generic.List[object]]::new()
        $index = 0
        foreach ($name in (Get-GhOperationSequence)) {
            if ($name -eq $Operation) {
                $payload = ConvertFrom-JsonVerbatim -Text ([string] $script:GhCalls[$index]['standardInput'])
                $found.Add((Get-JsonProperty -Node $payload -Name 'variables'))
            }
            $index++
        }
        , $found.ToArray()
    }

    function New-ThreadResponseJson {
        param(
            [string] $ThreadId = 'PRRT_new',
            [string] $CommentId = 'PRRC_new'
        )

        ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                data = [ordered]@{
                    addPullRequestReviewThread = [ordered]@{
                        thread = [ordered]@{
                            id       = $ThreadId
                            comments = [ordered]@{ nodes = @([ordered]@{ id = $CommentId; databaseId = 11 }) }
                        }
                    }
                }
            })
    }

    function New-VerificationJson {
        param(
            [string] $State = 'COMMENTED',
            [object[]] $Comment = @(),
            [string] $Body = 'the summary',
            [bool] $HasNextPage = $false,
            [string] $EndCursor = ''
        )

        ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                data = [ordered]@{
                    node = [ordered]@{
                        id       = 'PRR_1'
                        state    = $State
                        url      = 'https://github.com/octo/widget/pull/7#pullrequestreview-1'
                        body     = $Body
                        comments = [ordered]@{
                            pageInfo = [ordered]@{ hasNextPage = $HasNextPage; endCursor = $EndCursor }
                            nodes    = @($Comment)
                        }
                    }
                }
            })
    }

    function New-VerificationComment {
        param(
            [string] $Id = 'PRRC_new',
            [string] $State = 'SUBMITTED',
            [string] $Path = 'src/app.ps1',
            $Line = 3,
            $StartLine = $null,
            [string] $SubjectType = 'LINE',
            [string] $Body = 'rendered body'
        )

        [ordered]@{
            id          = $Id
            databaseId  = 11
            state       = $State
            path        = $Path
            line        = $Line
            startLine   = $StartLine
            subjectType = $SubjectType
            body        = $Body
        }
    }

    # Registers the replies a run that succeeds end to end needs. A test that is about one failing
    # write registers that one key again afterwards.
    function Register-PublicationRoute {
        param(
            [string] $SubmitState = 'COMMENTED',
            [object[]] $VerificationComment = @(),
            [string] $VerificationBody = 'the summary'
        )

        $pendingVerificationComment = @(
            foreach ($comment in $VerificationComment) {
                $copy = [ordered]@{}
                foreach ($key in $comment.Keys) { $copy[[string] $key] = $comment[$key] }
                $copy['state'] = 'PENDING'
                $copy
            }
        )

        Add-GhRoute -Key $script:PendingReviewKey -Response @(New-GhResponse -Body '{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[]}}}}}')
        Add-GhRoute -Key $script:StartReviewKey -Response @(New-GhResponse -Body '{"data":{"addPullRequestReview":{"pullRequestReview":{"id":"PRR_1","state":"PENDING"}}}}')
        Add-GhRoute -Key 'addPullRequestReviewThread(input:' -Response @(
            (New-GhResponse -Body (New-ThreadResponseJson -ThreadId 'PRRT_1' -CommentId 'PRRC_1')),
            (New-GhResponse -Body (New-ThreadResponseJson -ThreadId 'PRRT_2' -CommentId 'PRRC_2')),
            (New-GhResponse -Body (New-ThreadResponseJson -ThreadId 'PRRT_3' -CommentId 'PRRC_3'))
        )
        Add-GhRoute -Key $script:ReplyKey -Response @(New-GhResponse -Body '{"data":{"addPullRequestReviewThreadReply":{"comment":{"id":"PRRC_reply","databaseId":21}}}}')
        Add-GhRoute -Key $script:SubmitKey -Response @(New-GhResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        data = [ordered]@{
                            submitPullRequestReview = [ordered]@{
                                pullRequestReview = [ordered]@{
                                    id    = 'PRR_1'
                                    state = $SubmitState
                                    url   = 'https://github.com/octo/widget/pull/7#pullrequestreview-1'
                                    body  = 'summary'
                                }
                            }
                        }
                    })))
        Add-GhRoute -Key $script:VerifyKey -Response @(
            (New-GhResponse -Body (New-VerificationJson -State 'PENDING' -Comment $pendingVerificationComment -Body ''))
            (New-GhResponse -Body (New-VerificationJson -State $SubmitState -Comment $VerificationComment -Body $VerificationBody))
        )
        Add-GhRoute -Key $script:UnresolveKey -Response @(New-GhResponse -Body '{"data":{"unresolveReviewThread":{"thread":{"id":"PRRT_old","isResolved":false}}}}')
        Add-GhRoute -Key $script:DeleteKey -Response @(New-GhResponse -Body '{"data":{"deletePullRequestReview":{"pullRequestReview":{"id":"PRR_1"}}}}')
    }

    function New-TestState {
        param($PendingReviewId = $null)

        [ordered]@{
            platform          = 'gitHub'
            pullRequestNodeId = 'PR_node'
            url               = 'https://github.com/octo/widget/pull/7'
            status            = 'open'
            isActive          = $true
            isDraft           = $false
            sourceCommit      = $script:SourceCommit
            mergeBaseCommit   = $script:MergeBaseCommit
            identity          = 'runner'
            pendingReviewId   = $PendingReviewId
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
            [string] $Path = 'src/app.ps1',
            [int] $StartLine = 3,
            [int] $EndLine = 3,
            [string] $Side = 'right',
            [string] $ThreadId = '',
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
            commentId        = $null
            requiresReopen   = [bool] $RequiresReopen
            changeTrackingId = $null
        }
    }

    function New-ScratchDirectory {
        $path = Join-Path $TestDrive ([guid]::NewGuid().ToString('n'))
        New-Item -ItemType Directory -Path $path -Force | Out-Null
        $path
    }

    # Runs a whole publication against the routing table and hands back everything an assertion needs.
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

    $script:DiffText = @'
diff --git a/src/app.ps1 b/src/app.ps1
index 1111111..2222222 100644
--- a/src/app.ps1
+++ b/src/app.ps1
@@ -1,4 +1,5 @@
 line one
-line two
+line two changed
+line two added
 line three
 line four
'@

    # Parses recorded content without depending on a developer checkout.
    function New-TestDiff {
        @{
            mergeBaseCommit = $script:MergeBaseCommit
            sourceCommit = $script:SourceCommit
            files = ConvertFrom-GitDiffText -Text $script:DiffText
        }
    }

    function New-TestFinding {
        param(
            [string] $Id = 'F-1',
            [string] $Path = 'src/app.ps1',
            [int] $StartLine = 3
        )

        [ordered]@{
            schemaVersion     = 1
            id                = $Id
            sourceFindingIds  = @()
            origin            = 'reviewer'
            title             = "Title of $Id"
            description       = "Description of $Id"
            reasoning         = "Reasoning of $Id"
            consequence       = "Consequence of $Id"
            recommendedAction = "Recommended action of $Id"
            references        = @()
            impactedLocations = @()
            anchor            = [ordered]@{ kind = 'line'; path = $Path; startLine = $StartLine; endLine = $StartLine; side = 'right' }
        }
    }

    function New-TestApproved {
        param([object[]] $Finding)

        [ordered]@{
            schemaVersion           = 2
            target                  = $script:Target
            reviewedSourceCommit    = $script:SourceCommit
            reviewedMergeBaseCommit = $script:MergeBaseCommit
            reviewedContextHash     = Get-PullRequestEvidenceHash -Document (New-TestContext)
            findings                = @($Finding)
        }
    }

    function New-TestContext {
        [ordered]@{
            schemaVersion         = 3
            fetchedUtc            = '2026-08-04T11:00:00.0000000Z'
            platform              = 'gitHub'
            repositoryUrl         = 'https://github.com/octo/widget'
            pullRequestUrl        = 'https://github.com/octo/widget/pull/7'
            target                = $script:Target
            authenticatedIdentity = [ordered]@{ uniqueName = 'runner' }
            pullRequest           = [ordered]@{
                reviewedSourceCommit    = $script:SourceCommit
                reviewedMergeBaseCommit = $script:MergeBaseCommit
                isDraft                 = $false
                mergeState              = 'clean'
            }
            reviewers             = @()
            revisions             = @()
            threads               = @()
        }
    }

    function New-TestBodyMap {
        param([object[]] $Finding)

        $map = [ordered]@{}
        foreach ($item in $Finding) { $map[[string] $item['id']] = "body of $($item['id'])" }
        $map
    }

    # Registers the reads the live state is assembled from, together with a draft the identity already
    # holds, which is the state a discard is asked for from.
    function Register-PendingDraftStateRoute {
        Add-GhRoute -Key 'api user' -Response @(New-GhResponse -Body '{"login":"runner","id":701,"node_id":"U_runner","type":"User","html_url":"https://github.com/runner"}')
        Add-GhRoute -Key 'pulls/7 --method GET' -Response @(New-GhResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        node_id  = 'PR_node'
                        html_url = 'https://github.com/octo/widget/pull/7'
                        state    = 'open'
                        merged   = $false
                        draft    = $false
                        base     = [ordered]@{ sha = $script:MergeBaseCommit; repo = [ordered]@{ full_name = 'octo/widget' } }
                        head     = [ordered]@{ sha = $script:SourceCommit; repo = [ordered]@{ full_name = 'octo/widget'; owner = [ordered]@{ login = 'octo' } } }
                    })))
        Add-GhRoute -Key 'compare/' -Response @(New-GhResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        status            = 'ahead'
                        merge_base_commit = [ordered]@{ sha = $script:MergeBaseCommit }
                    })))
        Add-GhRoute -Key $script:PendingReviewKey -Response @(New-GhResponse -Body '{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[{"id":"PRR_existing","state":"PENDING"}]}}}}}')
        Add-GhRoute -Key $script:DeleteKey -Response @(New-GhResponse -Body '{"data":{"deletePullRequestReview":{"pullRequestReview":{"id":"PRR_existing"}}}}')
    }
}

Describe 'GitHub draft assembly' {
    BeforeEach { Reset-GhState }

    It 'adds a line thread, a file thread and a reply to one unsubmitted review' {
        Register-PublicationRoute -VerificationComment @(
            (New-VerificationComment -Id 'PRRC_1'),
            (New-VerificationComment -Id 'PRRC_2' -Path 'docs/readme.md' -SubjectType 'FILE' -Line $null),
            (New-VerificationComment -Id 'PRRC_reply')
        )

        $plan = @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')
            (New-PlanEntry -Operation 'newFileThread' -FindingId 'F-2' -Path 'docs/readme.md')
            (New-PlanEntry -Operation 'reply' -FindingId 'F-3' -ThreadId 'PRRT_old')
        )

        $run = Invoke-TestPublication -Plan $plan

        $run['failure'] | Should -BeNullOrEmpty
        Get-GhOperationSequence | Should -Be @('startReview', 'lineThread', 'fileThread', 'reply', 'verify', 'submit', 'verify')

        $reviewIds = @(
            (Get-GhCallVariable -Operation 'lineThread')[0]['reviewId']
            (Get-GhCallVariable -Operation 'fileThread')[0]['reviewId']
            (Get-GhCallVariable -Operation 'reply')[0]['reviewId']
            (Get-GhCallVariable -Operation 'submit')[0]['reviewId']
        )
        @($reviewIds | Sort-Object -Unique) | Should -Be @('PRR_1')
    }

    It 'publishes a review planned from an identity map with nothing in it' {
        # GitHub addresses a comment by path, so the file identity read answers with an empty map for
        # every file and the plan carries no identifier for any of them. That is a complete answer
        # rather than a missing one, and publication must proceed on it.
        $identity = Get-PullRequestFileIdentityForTarget `
            -Transport ([ordered]@{}) `
            -Target $script:Target `
            -State (New-TestState) `
            -Path @('src/app.ps1', 'docs/readme.md')

        $identity.Count | Should -Be 0

        Register-PublicationRoute -VerificationComment @(
            (New-VerificationComment -Id 'PRRC_1'),
            (New-VerificationComment -Id 'PRRC_2' -Path 'docs/readme.md' -SubjectType 'FILE' -Line $null)
        )

        $plan = @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Path 'src/app.ps1')
            (New-PlanEntry -Operation 'newFileThread' -FindingId 'F-2' -Path 'docs/readme.md')
        )
        @($plan | ForEach-Object { $_['changeTrackingId'] }) | Should -Be @($null, $null)

        $run = Invoke-TestPublication -Plan $plan

        $run['failure'] | Should -BeNullOrEmpty
        $run['persisted']['status'] | Should -Be 'succeeded'
        Get-GhOperationSequence | Should -Be @('startReview', 'lineThread', 'fileThread', 'verify', 'submit', 'verify')
    }

    It 'states the commit the review was produced against when it opens the draft' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1')

        Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') | Out-Null

        $variables = (Get-GhCallVariable -Operation 'startReview')[0]
        $variables['pullRequestId'] | Should -Be 'PR_node'
        $variables['commitOid'] | Should -Be $script:SourceCommit
    }

    It 'sends a left-side anchor as the line thread diff side' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1')

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Side 'left')

        $run['failure'] | Should -BeNullOrEmpty
        $variables = (Get-GhCallVariable -Operation 'lineThread')[0]
        $variables['side'] | Should -Be 'LEFT'
    }

    It 'sends a single-line anchor without a range and a multi-line anchor with one' {
        Register-PublicationRoute -VerificationComment @(
            (New-VerificationComment -Id 'PRRC_1'),
            (New-VerificationComment -Id 'PRRC_2' -Line 6)
        )

        $plan = @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -StartLine 3 -EndLine 3)
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-2' -StartLine 3 -EndLine 6)
        )

        Invoke-TestPublication -Plan $plan | Out-Null

        $calls = Get-GhCallVariable -Operation 'lineThread'
        $calls[0].Contains('startLine') | Should -BeFalse
        $calls[0]['line'] | Should -Be 3
        $calls[0]['side'] | Should -Be 'RIGHT'
        $calls[1]['startLine'] | Should -Be 3
        $calls[1]['line'] | Should -Be 6
    }

    It 'posts a downgraded anchor as a file thread carrying no position' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1' -SubjectType 'FILE' -Line $null)

        $plan = @(New-PlanEntry -Operation 'newFileThread' -FindingId 'F-1' -WasDowngraded)

        Invoke-TestPublication -Plan $plan | Out-Null

        Get-GhOperationSequence | Should -Be @('startReview', 'fileThread', 'verify', 'submit', 'verify')
        $variables = (Get-GhCallVariable -Operation 'fileThread')[0]
        $variables.Contains('line') | Should -BeFalse
        $variables.Contains('side') | Should -BeFalse
        $variables['path'] | Should -Be 'src/app.ps1'
    }
}

Describe 'GitHub review completion' {
    BeforeEach { Reset-GhState }

    It 'submits the review with the summary as its body' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1') -VerificationBody 'the review summary'

        $run = Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') `
            -SummaryBody 'the review summary'

        (Get-GhCallVariable -Operation 'submit')[0]['body'] | Should -Be 'the review summary'
        $run['result']['reviewUrl'] | Should -Be 'https://github.com/octo/widget/pull/7#pullrequestreview-1'
    }

    It 'submits the review as a comment when the merge is not approved' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1')

        Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') | Out-Null

        (Get-GhCallVariable -Operation 'submit')[0]['event'] | Should -Be 'COMMENT'
    }

    It 'submits the review as an approval when the merge is approved' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1') -SubmitState 'APPROVED'

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') -ApproveMerge

        (Get-GhCallVariable -Operation 'submit')[0]['event'] | Should -Be 'APPROVE'
        $run['result']['verification']['state'] | Should -Be 'APPROVED'
        $run['result']['verification']['isPublished'] | Should -BeTrue
    }

    It 'submits an approval carrying only the summary when the review raises no findings' {
        Register-PublicationRoute -SubmitState 'APPROVED' -VerificationBody 'the review summary'

        $run = Invoke-TestPublication -Plan @() -SummaryBody 'the review summary' -ApproveMerge

        $run['failure'] | Should -BeNullOrEmpty
        (Get-GhCallVariable -Operation 'submit')[0]['event'] | Should -Be 'APPROVE'
        (Get-GhCallVariable -Operation 'submit')[0]['body'] | Should -Be 'the review summary'
        $run['result']['verification']['isPublished'] | Should -BeTrue
        @($run['result']['verification']['comments']).Count | Should -Be 0
    }

    It 'verifies through the review node and reads back what the service published' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1' -Body 'published body')

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Body 'published body')

        (Get-GhCallVariable -Operation 'verify')[0]['reviewId'] | Should -Be 'PRR_1'
        $verification = $run['result']['verification']
        $verification['state'] | Should -Be 'COMMENTED'
        $verification['isPublished'] | Should -BeTrue
        @($verification['comments']).Count | Should -Be 1
        $verification['comments'][0]['isPublished'] | Should -BeTrue
        $verification['comments'][0]['body'] | Should -Be 'published body'
    }

    It 'reads every page of the review comments before it accepts the verification' {
        Register-PublicationRoute
        Add-GhRoute -Key $script:VerifyKey -Response @(
            (New-GhResponse -Body (New-VerificationJson `
                        -State 'PENDING' `
                        -Comment @(New-VerificationComment -Id 'PRRC_other' -State 'PENDING' -Path 'src/other.ps1') `
                        -Body '' `
                        -HasNextPage $true `
                        -EndCursor 'cursor-1'))
            (New-GhResponse -Body (New-VerificationJson `
                        -State 'PENDING' `
                        -Comment @(New-VerificationComment -Id 'PRRC_1' -State 'PENDING') `
                        -Body ''))
            (New-GhResponse -Body (New-VerificationJson `
                        -State 'COMMENTED' `
                        -Comment @(New-VerificationComment -Id 'PRRC_other' -Path 'src/other.ps1') `
                        -HasNextPage $true `
                        -EndCursor 'cursor-1'))
            (New-GhResponse -Body (New-VerificationJson `
                        -State 'COMMENTED' `
                        -Comment @(New-VerificationComment -Id 'PRRC_1')))
        )

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $run['failure'] | Should -BeNullOrEmpty
        @($run['result']['verification']['comments']).Count | Should -Be 2
        $verifyCalls = Get-GhCallVariable -Operation 'verify'
        $cursors = @(foreach ($call in $verifyCalls) { [string] (Get-JsonProperty -Node $call -Name 'cur') })
        ($cursors -join '|') | Should -BeExactly '|cursor-1||cursor-1'
        Get-GhOperationSequence | Should -Be @('startReview', 'lineThread', 'verify', 'verify', 'submit', 'verify', 'verify')
    }

    It 'refuses to submit when draft verification cannot find an accepted comment' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1')
        Add-GhRoute -Key $script:VerifyKey -Response @(New-GhResponse -Body (New-VerificationJson -State 'PENDING' -Comment @() -Body ''))

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $run['failure'] | Should -Not -BeNullOrEmpty
        $run['failure'].Exception.Message | Should -BeLike "*Finding 'F-1'*"
        Get-GhOperationSequence | Should -Be @('startReview', 'lineThread', 'verify', 'delete')
        Get-GhOperationSequence | Should -Not -Contain 'submit'
        $run['persisted']['preCompletionVerification'] | Should -Not -BeNullOrEmpty
        (Get-RecordedOperation -Record $run['persisted'] -Operation 'verifyFindings')['outcome'] | Should -Be 'failed'
        (Get-RecordedOperation -Record $run['persisted'] -Operation 'completeReview')['outcome'] | Should -Be 'pending'
    }

    It 'refuses a review the service still reports as unsubmitted' {
        Register-PublicationRoute
        Add-GhRoute -Key $script:VerifyKey -Response @(New-GhResponse -Body (New-VerificationJson -State 'PENDING' -Comment @(New-VerificationComment -Id 'PRRC_1' -State 'PENDING')))

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $run['failure'] | Should -Not -BeNullOrEmpty
        $run['failure'].Exception.Message | Should -BeLike "*reports state 'PENDING'*"
        $run['persisted']['status'] | Should -Be 'failed'
    }

    It 'counts only comments the service reports as visible' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1' -State 'PENDING')

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $run['failure'].Exception.Message | Should -BeLike '*published fewer comments than it was given*'
    }

    It 'reports a verification that could not confirm the review as a failure' {
        Register-PublicationRoute -VerificationComment @(
            (New-VerificationComment -Id 'PRRC_1' -State 'PENDING')
            (New-VerificationComment -Id 'PRRC_reply' -State 'PENDING')
        )

        $plan = @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')
            (New-PlanEntry -Operation 'reply' -FindingId 'F-2' -ThreadId 'PRRT_old' -RequiresReopen)
        )

        $run = Invoke-TestPublication -Plan $plan

        # A count the service will not confirm leaves nobody able to say what the pull request holds,
        # which is a different situation from a review that is published and merely untidy.
        $run['persisted']['status'] | Should -Be 'failed'
        $run['failure'] | Should -Not -BeNullOrEmpty
        Get-GhOperationSequence | Should -Not -Contain 'unresolve'
    }
}

Describe 'GitHub follow-up reopening' {
    BeforeEach { Reset-GhState }

    It 'sends the planned thread when it replies to an existing thread' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_reply')

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId 'PRRT_planned')

        $run['failure'] | Should -BeNullOrEmpty
        $variables = (Get-GhCallVariable -Operation 'reply')[0]
        $variables['threadId'] | Should -Be 'PRRT_planned'
    }

    It 'reopens a resolved thread strictly after the review became visible' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_reply')

        $plan = @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId 'PRRT_old' -RequiresReopen)

        $run = Invoke-TestPublication -Plan $plan

        $run['failure'] | Should -BeNullOrEmpty
        Get-GhOperationSequence | Should -Be @('startReview', 'reply', 'verify', 'submit', 'verify', 'unresolve')
        (Get-GhCallVariable -Operation 'unresolve')[0]['threadId'] | Should -Be 'PRRT_old'
    }

    It 'leaves a thread that was already open untouched' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_reply')

        $plan = @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId 'PRRT_open')

        Invoke-TestPublication -Plan $plan | Out-Null

        Get-GhOperationSequence | Should -Not -Contain 'unresolve'
    }

    It 'reopens nothing when the run never published' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_reply')
        Add-GhRoute -Key $script:SubmitKey -Response @(New-GhResponse -Body '{"errors":[{"message":"Review is empty."}]}')

        $plan = @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId 'PRRT_old' -RequiresReopen)

        $run = Invoke-TestPublication -Plan $plan

        $run['failure'] | Should -Not -BeNullOrEmpty
        Get-GhOperationSequence | Should -Not -Contain 'unresolve'
    }

    It 'reports a published review whose thread stayed resolved as published with a follow-up failure' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_reply')
        Add-GhRoute -Key $script:UnresolveKey -Response @(New-GhResponse -Body '{"errors":[{"message":"Thread is locked."}]}')

        $plan = @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId 'PRRT_old' -RequiresReopen)

        $run = Invoke-TestPublication -Plan $plan

        $run['failure'] | Should -BeNullOrEmpty
        $run['persisted']['status'] | Should -Be 'publishedWithFollowUpFailure'
        $run['persisted']['message'] | Should -BeLike "*PRRT_old*"
        $run['persisted']['message'] | Should -BeLike '*must not be published again*'

        $recorded = Get-RecordedOperation -Record $run['persisted'] -Operation 'reopenThread' -FindingId 'F-1'
        $recorded['outcome'] | Should -Be 'failed'
        $recorded['threadId'] | Should -Be 'PRRT_old'
        $recorded['message'] | Should -BeLike '*Thread is locked.*'
    }

    It 'reports the review as published even though a reopening failed' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_reply')
        Add-GhRoute -Key $script:UnresolveKey -Response @(New-GhResponse -Body '{"errors":[{"message":"Thread is locked."}]}')

        $plan = @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId 'PRRT_old' -RequiresReopen)

        $run = Invoke-TestPublication -Plan $plan
        $outcome = New-PullRequestPublicationOutcome -Result $run['result'] -ResultPath $run['resultPath']

        $outcome['publishedFindingCount'] | Should -Be 1
        $outcome['status'] | Should -Be 'publishedWithFollowUpFailure'
        $outcome['message'] | Should -BeLike '*PRRT_old*'
        (Get-RecordedOperation -Record $run['persisted'] -Operation 'completeReview')['outcome'] | Should -Be 'succeeded'
    }

    It 'attempts every reopening even when an earlier one fails' {
        Register-PublicationRoute -VerificationComment @(
            (New-VerificationComment -Id 'PRRC_reply')
            (New-VerificationComment -Id 'PRRC_reply2')
        )
        Add-GhRoute -Key $script:UnresolveKey -Response @(New-GhResponse -Body '{"errors":[{"message":"Thread is locked."}]}')

        $plan = @(
            (New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId 'PRRT_one' -RequiresReopen)
            (New-PlanEntry -Operation 'reply' -FindingId 'F-2' -ThreadId 'PRRT_two' -RequiresReopen)
        )

        $run = Invoke-TestPublication -Plan $plan

        @(Get-GhOperationSequence | Where-Object { $_ -eq 'unresolve' }).Count | Should -Be 2
        (Get-RecordedOperation -Record $run['persisted'] -Operation 'reopenThread' -FindingId 'F-2')['outcome'] | Should -Be 'failed'
        $run['persisted']['message'] | Should -BeLike '*PRRT_one*'
        $run['persisted']['message'] | Should -BeLike '*PRRT_two*'
    }

    It 'reports a review whose reopening succeeded as nothing but succeeded' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_reply')

        $plan = @(New-PlanEntry -Operation 'reply' -FindingId 'F-1' -ThreadId 'PRRT_old' -RequiresReopen)

        $run = Invoke-TestPublication -Plan $plan

        $run['persisted']['status'] | Should -Be 'succeeded'
        $run['persisted']['message'] | Should -BeNullOrEmpty
    }
}

Describe 'GitHub draft discard' {
    BeforeEach { Reset-GhState }

    It 'discards the draft when a write fails before the submit' {
        Register-PublicationRoute
        Add-GhRoute -Key 'addPullRequestReviewThread(input:' -Response @(New-GhResponse -Body '{"errors":[{"message":"Path is not part of the diff."}]}')

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        Get-GhOperationSequence | Should -Be @('startReview', 'lineThread', 'delete')
        (Get-GhCallVariable -Operation 'delete')[0]['reviewId'] | Should -Be 'PRR_1'
        $run['persisted']['draftDiscarded'] | Should -BeTrue
        { Assert-PullRequestPublicationRecord -OutputDirectory $run['outputDirectory'] -Target $script:Target } |
            Should -Not -Throw
    }

    It 'keeps the published review when the failure comes after the submit' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1')
        Add-GhRoute -Key $script:VerifyKey -Response @(
            (New-GhResponse -Body (New-VerificationJson -State 'PENDING' -Comment @(New-VerificationComment -Id 'PRRC_1' -State 'PENDING') -Body ''))
            (New-GhResponse -Body (New-VerificationJson -State 'PENDING' -Comment @(New-VerificationComment -Id 'PRRC_1' -State 'PENDING')))
        )

        Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') | Out-Null

        Get-GhOperationSequence | Should -Not -Contain 'delete'
    }

    It 'reports the original failure even when the discard also fails' {
        Register-PublicationRoute
        Add-GhRoute -Key 'addPullRequestReviewThread(input:' -Response @(New-GhResponse -Body '{"errors":[{"message":"Path is not part of the diff."}]}')
        Add-GhRoute -Key $script:DeleteKey -Response @(New-GhResponse -Body '{"errors":[{"message":"Review not found."}]}')

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $run['failure'].Exception.Message | Should -BeLike '*Path is not part of the diff*'
        $run['persisted']['draftDiscarded'] | Should -BeFalse
        $run['persisted']['draftDiscardMessage'] | Should -BeLike '*Review not found*'
    }
}

Describe 'GitHub pending review' {
    BeforeEach { Reset-GhState }

    It 'reports the draft the authenticated identity already holds' {
        Add-GhRoute -Key $script:PendingReviewKey -Response @(New-GhResponse -Body '{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[{"id":"PRR_existing","state":"PENDING"}]}}}}}')

        Get-GitHubPendingReviewId -Transport (New-TestTransport) -Target $script:Target -Login 'runner' |
            Should -Be 'PRR_existing'
    }

    It 'refuses to proceed past that draft unless the caller asked for it to go' {
        Add-GhRoute -Key $script:DeleteKey -Response @(New-GhResponse -Body '{"data":{"deletePullRequestReview":{"pullRequestReview":{"id":"PRR_existing"}}}}')
        $transport = New-TestTransport

        { Clear-PullRequestPendingReview `
                -Transport $transport `
                -Target $script:Target `
                -State (New-TestState -PendingReviewId 'PRR_existing') } |
            Should -Throw -ExpectedMessage '*-DiscardPendingReview*'

        Get-GhOperationSequence | Should -Be @()
    }

    It 'discards that draft when the caller asked for it to go' {
        Add-GhRoute -Key $script:DeleteKey -Response @(New-GhResponse -Body '{"data":{"deletePullRequestReview":{"pullRequestReview":{"id":"PRR_existing"}}}}')

        $state = Clear-PullRequestPendingReview `
            -Transport (New-TestTransport) `
            -Target $script:Target `
            -State (New-TestState -PendingReviewId 'PRR_existing') `
            -DiscardPendingReview

        Get-GhOperationSequence | Should -Be @('delete')
        (Get-GhCallVariable -Operation 'delete')[0]['reviewId'] | Should -Be 'PRR_existing'
        $state['pendingReviewId'] | Should -BeNullOrEmpty
    }
}

Describe 'GitHub write policy' {
    BeforeEach { Reset-GhState }

    It 'attempts a failed write exactly once, even when the failure looks transient' {
        Register-PublicationRoute
        Add-GhRoute -Key 'addPullRequestReviewThread(input:' -Response @(New-GhResponse -ExitCode 1 -StandardError 'HTTP 502 bad gateway')

        $run = Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') `
            -Transport (New-TestTransport -MaxAttempts 5)

        $run['failure'] | Should -Not -BeNullOrEmpty
        @(Get-GhOperationSequence | Where-Object { $_ -eq 'lineThread' }).Count | Should -Be 1
        $script:GhWaits.Count | Should -Be 0
    }

    It 'attempts a failed submit exactly once' {
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1')
        Add-GhRoute -Key $script:SubmitKey -Response @(New-GhResponse -ExitCode 1 -StandardError 'HTTP 502 bad gateway')

        Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') `
            -Transport (New-TestTransport -MaxAttempts 5) | Out-Null

        @(Get-GhOperationSequence | Where-Object { $_ -eq 'submit' }).Count | Should -Be 1
    }

    It 'attempts a failed draft open exactly once' {
        Register-PublicationRoute
        Add-GhRoute -Key $script:StartReviewKey -Response @(New-GhResponse -ExitCode 1 -StandardError 'HTTP 502 bad gateway')

        Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1') `
            -Transport (New-TestTransport -MaxAttempts 5) | Out-Null

        @(Get-GhOperationSequence | Where-Object { $_ -eq 'startReview' }).Count | Should -Be 1
    }
}

Describe 'GitHub publication record' {
    BeforeEach { Reset-GhState }

    It 'rewrites the record after every operation, so a run that dies is accounted for' {
        Register-PublicationRoute
        Add-GhRoute -Key 'addPullRequestReviewThread(input:' -Response @(
            (New-GhResponse -Body (New-ThreadResponseJson -ThreadId 'PRRT_1' -CommentId 'PRRC_1')),
            (New-GhResponse -Body '{"errors":[{"message":"Path is not part of the diff."}]}')
        )

        $plan = @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-2' -StartLine 4 -EndLine 4)
            (New-PlanEntry -Operation 'reply' -FindingId 'F-3' -ThreadId 'PRRT_old')
        )

        $run = Invoke-TestPublication -Plan $plan
        $record = $run['persisted']

        (Get-RecordedOperation -Record $record -Operation 'startReview')['outcome'] | Should -Be 'succeeded'
        $first = Get-RecordedOperation -Record $record -Operation 'newLineThread' -FindingId 'F-1'
        $first['outcome'] | Should -Be 'succeeded'
        $first['threadId'] | Should -Be 'PRRT_1'
        $first['commentId'] | Should -Be 'PRRC_1'
        (Get-RecordedOperation -Record $record -Operation 'newLineThread' -FindingId 'F-2')['outcome'] | Should -Be 'failed'
        (Get-RecordedOperation -Record $record -Operation 'reply' -FindingId 'F-3')['outcome'] | Should -Be 'pending'
        (Get-RecordedOperation -Record $record -Operation 'completeReview')['outcome'] | Should -Be 'pending'
        $record['status'] | Should -Be 'failed'
        $record['reviewId'] | Should -Be 'PRR_1'
    }

    It 'records a refusal the service reported as a definite failure' {
        Register-PublicationRoute
        Add-GhRoute -Key 'addPullRequestReviewThread(input:' -Response @(New-GhResponse -Body '{"errors":[{"message":"Path is not part of the diff."}]}')

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $entry = Get-RecordedOperation -Record $run['persisted'] -Operation 'newLineThread' -FindingId 'F-1'
        $entry['outcome'] | Should -Be 'failed'
        $entry['message'] | Should -BeLike '*Path is not part of the diff*'
    }

    It 'records a non-zero GraphQL response with service errors as a definite failure' {
        Register-PublicationRoute
        $response = New-GhResponse `
            -ExitCode 1 `
            -Body '{"errors":[{"message":"Path is not part of the diff."}]}' `
            -StandardError 'gh: Path is not part of the diff.'
        Add-GhRoute -Key 'addPullRequestReviewThread(input:' -Response @($response)

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $entry = Get-RecordedOperation -Record $run['persisted'] -Operation 'newLineThread' -FindingId 'F-1'
        $entry['outcome'] | Should -Be 'failed'
        $entry['message'] | Should -BeLike '*Path is not part of the diff*'
    }

    It 'records a lost response as ambiguous rather than as a failure' {
        Register-PublicationRoute
        Add-GhRoute -Key 'addPullRequestReviewThread(input:' -Response @(New-GhResponse -ExitCode 1 -StandardError 'the pipe has been ended')

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        $entry = Get-RecordedOperation -Record $run['persisted'] -Operation 'newLineThread' -FindingId 'F-1'
        $entry['outcome'] | Should -Be 'ambiguous'
        $entry['message'] | Should -BeLike '*the pipe has been ended*'
    }

    It 'records the ambiguity before the run stops, so a killed run still states it' {
        Register-PublicationRoute
        Add-GhRoute -Key 'addPullRequestReviewThread(input:' -Response @(New-GhResponse -ExitCode 1 -StandardError 'the pipe has been ended')

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')

        # The assertion reads the file rather than the in-memory record: the guarantee is about what
        # survives the process, not about what the run happened to be holding.
        $onDisk = ConvertFrom-JsonVerbatim -Text ([System.IO.File]::ReadAllText($run['resultPath'], [System.Text.Encoding]::UTF8))
        (Get-RecordedOperation -Record $onDisk -Operation 'newLineThread' -FindingId 'F-1')['outcome'] |
            Should -Be 'ambiguous'
    }

    It 'summarizes what reached the pull request rather than what was intended' {
        Register-PublicationRoute -VerificationComment @(
            (New-VerificationComment -Id 'PRRC_1'),
            (New-VerificationComment -Id 'PRRC_2' -SubjectType 'FILE' -Line $null)
        )

        $plan = @(
            (New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1')
            (New-PlanEntry -Operation 'newFileThread' -FindingId 'F-2' -WasDowngraded)
        )

        $run = Invoke-TestPublication -Plan $plan
        $outcome = New-PullRequestPublicationOutcome -Result $run['result'] -ResultPath $run['resultPath']

        $outcome['status'] | Should -Be 'succeeded'
        $outcome['publishedFindingCount'] | Should -Be 2
        $outcome['downgradedAnchorCount'] | Should -Be 1
        $outcome['failedOperationCount'] | Should -Be 0
        $outcome['ambiguousOperationCount'] | Should -Be 0
        $outcome['reviewUrl'] | Should -Be 'https://github.com/octo/widget/pull/7#pullrequestreview-1'
    }
}

Describe 'GitHub publication text fidelity' {
    BeforeEach { Reset-GhState }

    It 'carries every code point of a finding body and a summary through unchanged' {
        $body = "Rename $($script:LeftQuote)widget$($script:RightQuote) $($script:EmDash) it is a $($script:Robot) name."
        $summary = "Review complete $($script:EmDash) $($script:Robot)"

        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1' -Body $body) -VerificationBody $summary

        $run = Invoke-TestPublication `
            -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Body $body) `
            -SummaryBody $summary

        $run['failure'] | Should -BeNullOrEmpty

        $sent = (Get-GhCallVariable -Operation 'lineThread')[0]['body']
        ConvertTo-CodePointText -Text $sent | Should -Be (ConvertTo-CodePointText -Text $body)

        $sentSummary = (Get-GhCallVariable -Operation 'submit')[0]['body']
        ConvertTo-CodePointText -Text $sentSummary | Should -Be (ConvertTo-CodePointText -Text $summary)

        $readBack = $run['result']['verification']['comments'][0]['body']
        ConvertTo-CodePointText -Text $readBack | Should -Be (ConvertTo-CodePointText -Text $body)
    }

    It 'stores the record as UTF-8 without a byte order mark' {
        $body = "A $($script:Robot) finding"
        Register-PublicationRoute -VerificationComment @(New-VerificationComment -Id 'PRRC_1' -Body $body)

        $run = Invoke-TestPublication -Plan @(New-PlanEntry -Operation 'newLineThread' -FindingId 'F-1' -Body $body)

        $bytes = [System.IO.File]::ReadAllBytes($run['resultPath'])
        $bytes[0] | Should -Not -Be 0xEF
        $text = [System.Text.Encoding]::UTF8.GetString($bytes)
        $record = ConvertFrom-JsonVerbatim -Text $text
        ConvertTo-CodePointText -Text ([string] $record['verification']['comments'][0]['body']) |
            Should -Be (ConvertTo-CodePointText -Text $body)
    }
}

Describe 'GitHub publication state' {
    BeforeEach { Reset-GhState }

    It 'reads the live pull request, its merge base and the draft the identity holds' {
        Add-GhRoute -Key 'api user' -Response @(New-GhResponse -Body '{"login":"runner","id":701,"node_id":"U_runner","type":"User","html_url":"https://github.com/runner"}')
        Add-GhRoute -Key 'pulls/7 --method GET' -Response @(New-GhResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        node_id  = 'PR_node'
                        html_url = 'https://github.com/octo/widget/pull/7'
                        state    = 'open'
                        merged   = $false
                        draft    = $false
                        base     = [ordered]@{ sha = $script:MergeBaseCommit; repo = [ordered]@{ full_name = 'octo/widget' } }
                        head     = [ordered]@{ sha = $script:SourceCommit; repo = [ordered]@{ full_name = 'octo/widget'; owner = [ordered]@{ login = 'octo' } } }
                    })))
        Add-GhRoute -Key 'compare/' -Response @(New-GhResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        status            = 'ahead'
                        merge_base_commit = [ordered]@{ sha = $script:MergeBaseCommit }
                    })))
        Add-GhRoute -Key $script:PendingReviewKey -Response @(New-GhResponse -Body '{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[{"id":"PRR_existing","state":"PENDING"}]}}}}}')

        $state = Get-PullRequestPublicationStateForTarget -Transport (New-TestTransport) -Target $script:Target

        $state['pullRequestNodeId'] | Should -Be 'PR_node'
        $state['isActive'] | Should -BeTrue
        $state['sourceCommit'] | Should -Be $script:SourceCommit
        $state['mergeBaseCommit'] | Should -Be $script:MergeBaseCommit
        $state['identity'] | Should -Be 'runner'
        $state['pendingReviewId'] | Should -Be 'PRR_existing'
    }

    It 'reports a merged pull request as no longer accepting a review' {
        Add-GhRoute -Key 'api user' -Response @(New-GhResponse -Body '{"login":"runner","id":701,"node_id":"U_runner","type":"User","html_url":"https://github.com/runner"}')
        Add-GhRoute -Key 'pulls/7 --method GET' -Response @(New-GhResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        node_id  = 'PR_node'
                        html_url = 'https://github.com/octo/widget/pull/7'
                        state    = 'closed'
                        merged   = $true
                        draft    = $false
                        base     = [ordered]@{ sha = $script:MergeBaseCommit; repo = [ordered]@{ full_name = 'octo/widget' } }
                        head     = [ordered]@{ sha = $script:SourceCommit; repo = [ordered]@{ full_name = 'octo/widget'; owner = [ordered]@{ login = 'octo' } } }
                    })))
        Add-GhRoute -Key 'compare/' -Response @(New-GhResponse -Body '{"status":"identical","merge_base_commit":{"sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}')
        Add-GhRoute -Key $script:PendingReviewKey -Response @(New-GhResponse -Body '{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[]}}}}}')

        $state = Get-PullRequestPublicationStateForTarget -Transport (New-TestTransport) -Target $script:Target

        $state['isActive'] | Should -BeFalse
        $state['status'] | Should -Be 'closed'
    }
}

Describe 'GitHub publication plan ordering' {
    BeforeEach {
        Reset-GhState
        Mock Get-PullRequestContextForTarget { New-TestContext }
    }

    It 'refuses an anchor the reviewed diff cannot place without discarding the draft' {
        Register-PendingDraftStateRoute
        $findings = @(New-TestFinding -Id 'F-1' -Path 'src/absent.ps1')

        { New-PullRequestPublicationPlanForTarget `
                -Transport (New-TestTransport) `
                -Target $script:Target `
                -Approved (New-TestApproved -Finding $findings) `
                -Finding $findings `
                -Context (New-TestContext) `
                -Body (New-TestBodyMap -Finding $findings) `
                -Diff (New-TestDiff) `
                -DiscardPendingReview } |
            Should -Throw -ExpectedMessage "*Finding 'F-1' cannot be anchored*"

        Get-GhOperationSequence | Should -Not -Contain 'delete'
    }

    It 'refuses a finding that rendered to nothing without discarding the draft' {
        Register-PendingDraftStateRoute
        $findings = @(New-TestFinding -Id 'F-1')

        { New-PullRequestPublicationPlanForTarget `
                -Transport (New-TestTransport) `
                -Target $script:Target `
                -Approved (New-TestApproved -Finding $findings) `
                -Finding $findings `
                -Context (New-TestContext) `
                -Body ([ordered]@{ 'F-1' = '   ' }) `
                -Diff (New-TestDiff) `
                -DiscardPendingReview } |
            Should -Throw -ExpectedMessage "*'F-1' rendered to no text*"

        Get-GhOperationSequence | Should -Not -Contain 'delete'
    }

    It 'refuses a pull request that moved without discarding the draft' {
        Register-PendingDraftStateRoute
        $findings = @(New-TestFinding -Id 'F-1')
        $approved = New-TestApproved -Finding $findings
        $approved['reviewedSourceCommit'] = 'cccccccccccccccccccccccccccccccccccccccc'

        { New-PullRequestPublicationPlanForTarget `
                -Transport (New-TestTransport) `
                -Target $script:Target `
                -Approved $approved `
                -Finding $findings `
                -Context (New-TestContext) `
                -Body (New-TestBodyMap -Finding $findings) `
                -Diff (New-TestDiff) `
                -DiscardPendingReview } |
            Should -Throw

        Get-GhOperationSequence | Should -Not -Contain 'delete'
    }

    It 'discards the draft once the whole plan stands' {
        Register-PendingDraftStateRoute
        $findings = @(New-TestFinding -Id 'F-1')

        $session = New-PullRequestPublicationPlanForTarget `
            -Transport (New-TestTransport) `
            -Target $script:Target `
            -Approved (New-TestApproved -Finding $findings) `
            -Finding $findings `
            -Context (New-TestContext) `
            -Body (New-TestBodyMap -Finding $findings) `
            -Diff (New-TestDiff) `
            -DiscardPendingReview

        @($session['plan']).Count | Should -Be 1
        $session['plan'][0]['operation'] | Should -Be 'newLineThread'
        $session['state']['pendingReviewId'] | Should -BeNullOrEmpty

        # The discard is the only write this step performs, and it is the last thing it does.
        (Get-GhOperationSequence)[-1] | Should -Be 'delete'
    }

    It 'refuses to proceed past a draft the caller did not ask to discard' {
        Register-PendingDraftStateRoute
        $findings = @(New-TestFinding -Id 'F-1')

        { New-PullRequestPublicationPlanForTarget `
                -Transport (New-TestTransport) `
                -Target $script:Target `
                -Approved (New-TestApproved -Finding $findings) `
                -Finding $findings `
                -Context (New-TestContext) `
                -Body (New-TestBodyMap -Finding $findings) `
                -Diff (New-TestDiff) } |
            Should -Throw -ExpectedMessage '*-DiscardPendingReview*'

        Get-GhOperationSequence | Should -Not -Contain 'delete'
    }

    It 'refuses <Change> immediately before publication without discarding the draft' -ForEach @(
        @{ Change = 'a force-pushed source'; Mutate = { param($context) $context.pullRequest.reviewedSourceCommit = 'unrelated-history' } }
        @{ Change = 'a changed merge base'; Mutate = { param($context) $context.pullRequest.reviewedMergeBaseCommit = 'new-base' } }
        @{ Change = 'discussion arriving during the human wait'; Mutate = { param($context) $context.threads = @([ordered]@{ id = 'new-thread'; comments = @() }) } }
        @{ Change = 'changed PR intent'; Mutate = { param($context) $context.pullRequest['description'] = 'Changed requirements' } }
    ) {
        Register-PendingDraftStateRoute
        $script:LiveEvidence = New-TestContext
        & $Mutate $script:LiveEvidence
        Mock Get-PullRequestContextForTarget { $script:LiveEvidence }
        $findings = @(New-TestFinding -Id 'F-1')
        { New-PullRequestPublicationPlanForTarget -Transport (New-TestTransport) -Target $script:Target `
            -Approved (New-TestApproved -Finding $findings) -Finding $findings -Context (New-TestContext) `
            -Body (New-TestBodyMap -Finding $findings) -Diff (New-TestDiff) -DiscardPendingReview } |
            Should -Throw '*evidence changed immediately before publication*'
        Get-GhOperationSequence | Should -Not -Contain 'delete'
        Should -Invoke Get-PullRequestContextForTarget -Times 1 -Exactly
    }

    It 'refuses substituting a different unchecked capture even if commits match' {
        Register-PendingDraftStateRoute
        $context = New-TestContext
        $context.pullRequest['description'] = 'Unchecked discussion context'
        $findings = @(New-TestFinding -Id 'F-1')
        { New-PullRequestPublicationPlanForTarget -Transport (New-TestTransport) -Target $script:Target `
            -Approved (New-TestApproved -Finding $findings) -Finding $findings -Context $context `
            -Body (New-TestBodyMap -Finding $findings) -Diff (New-TestDiff) -DiscardPendingReview } |
            Should -Throw '*not bound to this checked capture*'
        Get-GhOperationSequence | Should -Not -Contain 'delete'
    }
}
