BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:LibraryRoot = Join-Path $script:RepoRoot 'skills\ss-review\scripts\lib'
    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PublicationFailure.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    # The adapter derives no browser URL of its own; it calls the target library, exactly as the
    # composition roots load both libraries side by side.
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubCliTransport.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')

    # Characters are built from their code points so the fixtures never depend on how this file is
    # encoded on disk, and so the assertions compare code points rather than rendered glyphs.
    $script:EmDash = [string] [char]0x2014
    $script:LeftQuote = [string] [char]0x201C
    $script:RightQuote = [string] [char]0x201D
    $script:Robot = [char]::ConvertFromUtf32(0x1F916)

    # Every transport built here reads this instant instead of a clock, so a wait the transport
    # derives from an absolute rate limit reset is a fixed arithmetic result the assertions can state
    # exactly.
    $script:FixedNowUtc = [DateTimeOffset]::new(2026, 8, 4, 12, 0, 0, [TimeSpan]::Zero)

    $script:Target = [ordered]@{
        platform      = 'gitHub'
        collectionUrl = $null
        organization  = 'octo'
        project       = $null
        repository    = 'widget'
        pullRequestId = 7
        webUrl        = 'https://github.com/octo/widget/pull/7'
    }

    function ConvertTo-FixtureJson {
        param(
            [Parameter(Mandatory)]
            $Value
        )

        ConvertTo-Json -InputObject $Value -Depth 24 -Compress
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

    # Registers a canned reply for every invocation whose command line and standard input contain
    # the key. Later registrations win, so a test overrides a default fixture by registering again.
    # Supplying several replies serves them in order and then repeats the last, which is what lets a
    # paginated read and a retried request be expressed without a bespoke fake per test.
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

            [DateTimeOffset] $NowUtc = $script:FixedNowUtc,

            # Replaces the routing invoker for a test whose subject is a reply the routing table
            # cannot express, such as one that is not a dictionary at all.
            [scriptblock] $Invoker = $null,

            # Replaces the recording wait block for a test whose subject is what the transport does
            # with what the wait block leaves behind.
            [scriptblock] $WaitBlock = $null
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
            # Match the transport's configurable transient-budget ceiling, beyond these finite
            # fixtures, so a broken unbounded retry cannot hang the offline suite.
            if ($script:GhCalls.Count -gt 100) { throw 'The fake GitHub CLI exceeded its request budget.' }

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

        if ($null -eq $Invoker) { $Invoker = $routingInvoker }
        if ($null -eq $WaitBlock) { $WaitBlock = $recordingWait }

        # The closure is taken so the fixed instant survives the return of this function; a bare
        # block would look the name up in whatever scope the transport runs it from.
        $clock = { $NowUtc }.GetNewClosure()

        New-GitHubCliTransport `
            -InvokeGh $Invoker `
            -Wait $WaitBlock `
            -GetUtcNow $clock `
            -MaxAttempts $MaxAttempts
    }

    function New-AccountPayload {
        param(
            [string] $Login = 'reviewer',
            [long] $Id = 501,
            [string] $NodeId = 'U_kgDOAAAB',
            [string] $Type = 'User',
            [string] $HtmlUrl = 'https://github.com/reviewer'
        )

        [ordered]@{
            login     = $Login
            id        = $Id
            node_id   = $NodeId
            type      = $Type
            html_url  = $HtmlUrl
        }
    }

    function New-ActorPayload {
        param(
            [string] $Login = 'reviewer',
            [long] $DatabaseId = 501,
            [string] $NodeId = 'U_kgDOAAAB',
            [string] $TypeName = 'User',
            [string] $Url = 'https://github.com/reviewer'
        )

        [ordered]@{
            login      = $Login
            __typename = $TypeName
            id         = $NodeId
            databaseId = $DatabaseId
            url        = $Url
        }
    }

    function New-ThreadCommentPayload {
        param(
            [string] $Id = 'PRRC_1',
            [string] $Body = 'Please rename this.',
            $Author = $null,
            [string] $CreatedAt = '2026-08-01T10:00:00Z',
            $LastEditedAt = $null,
            $ReplyTo = $null,
            $MinimizedReason = $null,
            [string] $State = 'SUBMITTED'
        )

        if ($null -eq $Author) { $Author = New-ActorPayload }

        [ordered]@{
            id                = $Id
            databaseId        = 900001
            url               = "https://github.com/octo/widget/pull/7#discussion_$Id"
            body              = $Body
            createdAt         = $CreatedAt
            lastEditedAt      = $LastEditedAt
            replyTo           = $ReplyTo
            state             = $State
            author            = $Author
            authorAssociation = 'MEMBER'
            minimizedReason   = $MinimizedReason
        }
    }

    function New-ThreadPayload {
        param(
            [string] $Id = 'PRRT_1',
            [bool] $IsResolved = $false,
            [bool] $IsOutdated = $false,
            [string] $Path = 'src/widget.ps1',
            $Line = 42,
            $OriginalLine = 40,
            $StartLine = $null,
            $OriginalStartLine = $null,
            [string] $DiffSide = 'RIGHT',
            [object[]] $Comment = @(),
            [bool] $CommentsHaveNextPage = $false,
            $CommentsEndCursor = $null
        )

        if ($Comment.Count -eq 0) { $Comment = @(New-ThreadCommentPayload) }

        [ordered]@{
            id                = $Id
            isResolved        = $IsResolved
            isOutdated        = $IsOutdated
            isCollapsed       = $IsResolved
            path              = $Path
            line              = $Line
            originalLine      = $OriginalLine
            startLine         = $StartLine
            originalStartLine = $OriginalStartLine
            diffSide          = $DiffSide
            startDiffSide     = $DiffSide
            subjectType       = 'LINE'
            resolvedBy        = if ($IsResolved) { [ordered]@{ login = 'reviewer' } } else { $null }
            comments          = [ordered]@{
                pageInfo = [ordered]@{ hasNextPage = $CommentsHaveNextPage; endCursor = $CommentsEndCursor }
                nodes    = @($Comment)
            }
        }
    }

    function New-ThreadPageJson {
        param(
            [object[]] $Thread = @(),
            [bool] $HasNextPage = $false,
            $EndCursor = $null
        )

        ConvertTo-FixtureJson -Value ([ordered]@{
                data = [ordered]@{
                    repository = [ordered]@{
                        pullRequest = [ordered]@{
                            reviewThreads = [ordered]@{
                                pageInfo = [ordered]@{ hasNextPage = $HasNextPage; endCursor = $EndCursor }
                                nodes    = @($Thread)
                            }
                        }
                    }
                }
            })
    }

    function New-ThreadCommentPageJson {
        param(
            [object[]] $Comment = @(),
            [bool] $HasNextPage = $false,
            $EndCursor = $null
        )

        ConvertTo-FixtureJson -Value ([ordered]@{
                data = [ordered]@{
                    node = [ordered]@{
                        comments = [ordered]@{
                            pageInfo = [ordered]@{ hasNextPage = $HasNextPage; endCursor = $EndCursor }
                            nodes    = @($Comment)
                        }
                    }
                }
            })
    }

    function New-IssueCommentEditPageJson {
        param(
            [object[]] $Node = @(),
            [bool] $HasNextPage = $false,
            $EndCursor = $null
        )

        ConvertTo-FixtureJson -Value ([ordered]@{
                data = [ordered]@{
                    repository = [ordered]@{
                        pullRequest = [ordered]@{
                            comments = [ordered]@{
                                pageInfo = [ordered]@{ hasNextPage = $HasNextPage; endCursor = $EndCursor }
                                nodes    = @($Node)
                            }
                        }
                    }
                }
            })
    }

    # The reviews connection carries the edit time of each review summary. It is queried separately
    # from the comments connection because each connection is walked with its own cursor, so the
    # routing key names the connection rather than the node selection the two queries share.
    function New-ReviewEditPageJson {
        param(
            [object[]] $Node = @(),
            [bool] $HasNextPage = $false,
            $EndCursor = $null
        )

        ConvertTo-FixtureJson -Value ([ordered]@{
                data = [ordered]@{
                    repository = [ordered]@{
                        pullRequest = [ordered]@{
                            reviews = [ordered]@{
                                pageInfo = [ordered]@{ hasNextPage = $HasNextPage; endCursor = $EndCursor }
                                nodes    = @($Node)
                            }
                        }
                    }
                }
            })
    }

    function New-TimelinePageJson {
        param(
            [object[]] $Node = @(),
            [bool] $HasNextPage = $false,
            $EndCursor = $null
        )

        ConvertTo-FixtureJson -Value ([ordered]@{
                data = [ordered]@{
                    repository = [ordered]@{
                        pullRequest = [ordered]@{
                            timelineItems = [ordered]@{
                                pageInfo = [ordered]@{ hasNextPage = $HasNextPage; endCursor = $EndCursor }
                                nodes    = @($Node)
                            }
                        }
                    }
                }
            })
    }

    function New-ReviewPayload {
        param(
            # Review identifiers exceed the range of a 32-bit integer on the live service, so the
            # fixture carries them at the width GitHub actually uses.
            [long] $Id = 1001,
            [string] $NodeId = 'PRR_1',
            [string] $State = 'APPROVED',
            [string] $Body = '',
            $User = $null,
            [string] $SubmittedAt = '2026-08-02T09:00:00Z'
        )

        if ($null -eq $User) { $User = New-AccountPayload }

        [ordered]@{
            id                = $Id
            node_id           = $NodeId
            user              = $User
            body              = $Body
            state             = $State
            html_url          = "https://github.com/octo/widget/pull/7#pullrequestreview-$Id"
            author_association = 'MEMBER'
            submitted_at      = $SubmittedAt
            commit_id         = 'cafe1234'
        }
    }

    function New-IssueCommentPayload {
        param(
            [long] $Id = 2001,
            [string] $NodeId = 'IC_1',
            [string] $Body = 'Looks reasonable to me.',
            $User = $null,
            [string] $CreatedAt = '2026-08-02T11:00:00Z',
            [string] $UpdatedAt = '2026-08-02T11:00:01Z'
        )

        if ($null -eq $User) { $User = New-AccountPayload }

        [ordered]@{
            url                = "https://api.github.com/repos/octo/widget/issues/comments/$Id"
            html_url           = "https://github.com/octo/widget/pull/7#issuecomment-$Id"
            id                 = $Id
            node_id            = $NodeId
            user               = $User
            created_at         = $CreatedAt
            updated_at         = $UpdatedAt
            body               = $Body
            author_association = 'MEMBER'
            minimized          = $false
        }
    }

    function New-PullRequestPayload {
        param(
            $User = $null,
            [string] $Title = 'Add the widget',
            [string] $Body = 'This changes the widget.',
            [string] $State = 'open',
            [bool] $Merged = $false,
            [bool] $Draft = $false,
            [string] $HeadRepository = 'octo/widget',
            [string] $HeadOwner = 'octo'
        )

        if ($null -eq $User) { $User = New-AccountPayload -Login 'author' -Id 601 -NodeId 'U_author' -HtmlUrl 'https://github.com/author' }

        [ordered]@{
            number           = 7
            node_id          = 'PR_7'
            title            = $Title
            body             = $Body
            state            = $State
            merged           = $Merged
            mergeable        = $true
            merge_commit_sha = 'mergecommit000000000000000000000000000'
            draft            = $Draft
            user             = $User
            created_at       = '2026-08-01T08:00:00Z'
            mergeable_state  = 'clean'
            head             = [ordered]@{
                ref   = 'feature/widget'
                label = "${HeadOwner}:feature/widget"
                sha   = 'headsha0000000000000000000000000000000'
                repo  = [ordered]@{
                    full_name = $HeadRepository
                    owner     = [ordered]@{ login = $HeadOwner }
                }
            }
            base             = [ordered]@{
                ref   = 'main'
                label = 'octo:main'
                sha   = 'basesha0000000000000000000000000000000'
                repo  = [ordered]@{
                    full_name = 'octo/widget'
                    owner     = [ordered]@{ login = 'octo' }
                }
            }
        }
    }

    # Registers a complete, coherent fixture for one pull request. Every read the adapter performs
    # has a reply, so a test only states the part it is about. A connection is stated either as the
    # nodes of its single page or, when the test is about the walk itself, as the pages themselves.
    function Register-PullRequestFixture {
        param(
            $PullRequest = $null,
            [object[]] $Review = @(),
            [object[]] $IssueComment = @(),
            $RequestedReviewers = $null,
            [string[]] $ThreadPageJson = @(),
            [string[]] $ThreadCommentPageJson = @(),
            [object[]] $IssueCommentEditNode = @(),
            [string[]] $IssueCommentEditPageJson = @(),
            [object[]] $ReviewEditNode = @(),
            [string[]] $ReviewEditPageJson = @(),
            [object[]] $TimelineNode = @(),
            [string[]] $TimelinePageJson = @(),
            $AuthenticatedAccount = $null
        )

        if ($null -eq $PullRequest) { $PullRequest = New-PullRequestPayload }
        if ($null -eq $RequestedReviewers) { $RequestedReviewers = [ordered]@{ users = @(); teams = @() } }
        if ($null -eq $AuthenticatedAccount) { $AuthenticatedAccount = New-AccountPayload -Login 'runner' -Id 701 -NodeId 'U_runner' -HtmlUrl 'https://github.com/runner' }
        if ($ThreadPageJson.Count -eq 0) { $ThreadPageJson = @(New-ThreadPageJson -Thread @()) }
        if ($ThreadCommentPageJson.Count -eq 0) { $ThreadCommentPageJson = @(New-ThreadCommentPageJson -Comment @()) }
        if ($IssueCommentEditPageJson.Count -eq 0) { $IssueCommentEditPageJson = @(New-IssueCommentEditPageJson -Node $IssueCommentEditNode) }
        if ($ReviewEditPageJson.Count -eq 0) {
            # Ordinary fixtures have no inline review comments. Explicit metadata overrides this
            # default, including deliberately incomplete responses used to test validation.
            $metadata = @(
                $ReviewEditNode
                foreach ($reviewItem in $Review) {
                    if ($reviewItem['node_id'] -cnotin @($ReviewEditNode | ForEach-Object { $_['id'] })) {
                        [ordered]@{ id = $reviewItem['node_id']; lastEditedAt = $null; comments = @{ totalCount = 0 } }
                    }
                }
            )
            $ReviewEditPageJson = @(New-ReviewEditPageJson -Node $metadata)
        }
        if ($TimelinePageJson.Count -eq 0) { $TimelinePageJson = @(New-TimelinePageJson -Node $TimelineNode) }

        Add-GhRoute -Key 'api user' -Response @(New-GhResponse -Body (ConvertTo-FixtureJson -Value $AuthenticatedAccount))
        Add-GhRoute -Key 'pulls/7 --method GET' -Response @(New-GhResponse -Body (ConvertTo-FixtureJson -Value $PullRequest))
        Add-GhRoute -Key 'pulls/7/reviews' -Response @(New-GhResponse -Body (ConvertTo-FixtureJson -Value @(, @($Review))))
        Add-GhRoute -Key 'issues/7/comments' -Response @(New-GhResponse -Body (ConvertTo-FixtureJson -Value @(, @($IssueComment))))
        Add-GhRoute -Key 'requested_reviewers --method GET' -Response @(New-GhResponse -Body (ConvertTo-FixtureJson -Value $RequestedReviewers))
        Add-GhRoute -Key 'compare/' -Response @(New-GhResponse -Body (ConvertTo-FixtureJson -Value ([ordered]@{
                        status            = 'ahead'
                        merge_base_commit = [ordered]@{ sha = 'mergebase00000000000000000000000000000' }
                    })))
        Add-GhRoute -Key 'reviewThreads' -Response ($ThreadPageJson | ForEach-Object { New-GhResponse -Body $_ })
        Add-GhRoute -Key 'PullRequestReviewThread' -Response ($ThreadCommentPageJson | ForEach-Object { New-GhResponse -Body $_ })
        Add-GhRoute -Key 'nodes{ id lastEditedAt }' -Response ($IssueCommentEditPageJson | ForEach-Object { New-GhResponse -Body $_ })
        Add-GhRoute -Key 'reviews(first:' -Response ($ReviewEditPageJson | ForEach-Object { New-GhResponse -Body $_ })
        Add-GhRoute -Key 'timelineItems' -Response ($TimelinePageJson | ForEach-Object { New-GhResponse -Body $_ })
    }

    function Get-CanonicalThread {
        param(
            [Parameter(Mandatory)]
            $Context,

            [Parameter(Mandatory)]
            [string] $Id
        )

        $match = @($Context['threads'] | Where-Object { $_['id'] -eq $Id })
        if ($match.Count -ne 1) {
            throw "Expected exactly one thread with id '$Id' but found $($match.Count)."
        }
        $match[0]
    }

    # Renders the code points of a string as text, which is what makes a fidelity failure legible:
    # the assertion names the exact code point that changed rather than showing two strings that
    # print identically.
    function Get-CodePoint {
        param(
            [Parameter(Mandatory)]
            [AllowEmptyString()]
            [string] $Text
        )

        $points = [System.Collections.Generic.List[int]]::new()
        $enumerator = $Text.EnumerateRunes()
        while ($enumerator.MoveNext()) {
            $points.Add($enumerator.Current.Value)
        }
        return ($points -join ',')
    }

    function Get-PostCall {
        @($script:GhCalls | Where-Object { $_['arguments'] -contains 'POST' })
    }

    # Every test starts from an empty routing table, call log and wait log, so one test's fixtures
    # and assertions can never reach another.
    function Reset-GhState {
        $script:GhRoutes = [System.Collections.Generic.List[object]]::new()
        $script:GhCalls = [System.Collections.Generic.List[object]]::new()
        $script:GhWaits = [System.Collections.Generic.List[int]]::new()
    }
}

Describe 'GitHub CLI transport' {
    BeforeEach {
        Reset-GhState
    }
    It 'decodes standard output as UTF-8 without altering any code point' {
        $body = "Rename this $script:EmDash the $($script:LeftQuote)widget$($script:RightQuote) name is taken $script:Robot"
        $payload = ConvertTo-FixtureJson -Value ([ordered]@{ body = $body })
        Add-GhRoute -Key 'api probe' -Response @(New-GhResponse -Body $payload)

        # The fixture has to travel as multi-byte UTF-8, otherwise the decode under test is never
        # exercised.
        [Text.Encoding]::UTF8.GetBytes($payload).Length | Should -BeGreaterThan $payload.Length

        $transport = New-TestTransport
        $result = Invoke-GitHubRestRequest -Transport $transport -Path 'probe'

        (Get-CodePoint -Text $result['body']) | Should -Be (Get-CodePoint -Text $body)
        $result['body'] | Should -Be $body
    }

    It 'keeps a body that is itself a timestamp as the exact string it arrived as' {
        $body = '2026-08-04T10:33:54Z'
        Add-GhRoute -Key 'api probe' -Response @(New-GhResponse -Body (ConvertTo-FixtureJson -Value ([ordered]@{ body = $body })))

        $transport = New-TestTransport
        $result = Invoke-GitHubRestRequest -Transport $transport -Path 'probe'

        $result['body'] | Should -BeOfType ([string])
        $result['body'] | Should -Be $body
    }

    It 'fails when the CLI exits non-zero' {
        Add-GhRoute -Key 'api probe' -Response @(New-GhResponse `
                -ExitCode 1 `
                -Body '{"message":"Not Found"}' `
                -StandardError 'gh: Not Found (HTTP 404)')

        $transport = New-TestTransport

        { Invoke-GitHubRestRequest -Transport $transport -Path 'probe' } |
            Should -Throw '*Not Found (HTTP 404)*'
        $script:GhCalls.Count | Should -Be 1
    }

    It 'keeps credential material out of the failure it reports' {
        Add-GhRoute -Key 'api probe' -Response @(New-GhResponse `
                -ExitCode 1 `
                -StandardError 'gh: request failed with Authorization: bearer gho_abcdefghijklmnopqrstuvwxyz012345')

        $transport = New-TestTransport

        $failure = $null
        try {
            Invoke-GitHubRestRequest -Transport $transport -Path 'probe'
        } catch {
            $failure = $_
        }

        $failure | Should -Not -BeNullOrEmpty
        $failure.Exception.Message | Should -Not -BeLike '*gho_*'
        $failure.Exception.Message | Should -BeLike '*<redacted>*'
    }

    It 'fails on a GraphQL errors array even when the CLI succeeded' {
        $payload = ConvertTo-FixtureJson -Value ([ordered]@{
                data   = [ordered]@{ repository = $null }
                errors = @([ordered]@{ type = 'NOT_FOUND'; message = 'Could not resolve to a Repository.' })
            })
        Add-GhRoute -Key 'graphql' -Response @(New-GhResponse -Body $payload)

        $transport = New-TestTransport

        { Invoke-GitHubGraphQlQuery -Transport $transport -Query 'query{viewer{login}}' } |
            Should -Throw '*Could not resolve to a Repository*'
    }

    It 'retries a GraphQL rate limit and waits exactly as long as the service asked' {
        $limited = ConvertTo-FixtureJson -Value ([ordered]@{
                errors = @([ordered]@{ message = 'API rate limit exceeded for this resource.' })
            })
        Add-GhRoute -Key 'graphql' -Response @(
            (New-GhResponse -ExitCode 1 -Body $limited -StandardError 'gh: API rate limit exceeded (HTTP 403) retry-after: 30')
            (New-GhResponse -Body '{"data":{"viewer":{"login":"octocat"}}}')
        )

        $transport = New-TestTransport
        $result = Invoke-GitHubGraphQlQuery -Transport $transport -Query 'query{viewer{login}}'

        (Get-JsonProperty -Node (Get-JsonProperty -Node $result -Name 'data') -Name 'viewer')['login'] |
            Should -Be 'octocat'
        $script:GhCalls.Count | Should -Be 2
        @($script:GhWaits) | Should -Be @(30000)
    }

    It 'marks a non-zero GraphQL errors response without data as a refusal when retrying is suppressed' {
        $payload = ConvertTo-FixtureJson -Value ([ordered]@{
                errors = @([ordered]@{ message = "Field 'missing' does not exist on type 'Query'." })
            })
        Add-GhRoute -Key 'graphql' -Response @(New-GhResponse `
                -ExitCode 1 `
                -Body $payload `
                -StandardError "gh: Field 'missing' does not exist on type 'Query'.")

        $transport = New-TestTransport
        $failure = $null
        try {
            Invoke-GitHubGraphQlQuery -Transport $transport -Query 'query{missing}' -NoRetry | Out-Null
        } catch {
            $failure = $_
        }

        $failure | Should -Not -BeNullOrEmpty
        (Test-PullRequestServiceRefusalException -Exception $failure.Exception) | Should -BeTrue
        $script:GhCalls.Count | Should -Be 1
        @($script:GhWaits).Count | Should -Be 0
    }

    It 'keeps a non-zero GraphQL errors response with data ambiguous when retrying is suppressed' {
        $payload = ConvertTo-FixtureJson -Value ([ordered]@{
                data   = [ordered]@{
                    addPullRequestReviewComment = [ordered]@{
                        comment = [ordered]@{ id = 'PRRC_created' }
                    }
                }
                errors = @([ordered]@{ message = 'A sibling resolver failed.' })
            })
        Add-GhRoute -Key 'graphql' -Response @(New-GhResponse `
                -ExitCode 1 `
                -Body $payload `
                -StandardError 'gh: A sibling resolver failed.')

        $transport = New-TestTransport
        $failure = $null
        try {
            Invoke-GitHubGraphQlQuery -Transport $transport -Query 'mutation{addPullRequestReviewComment(input:{}){comment{id}}}' -NoRetry | Out-Null
        } catch {
            $failure = $_
        }

        $failure | Should -Not -BeNullOrEmpty
        (Test-PullRequestServiceRefusalException -Exception $failure.Exception) | Should -BeFalse
        $script:GhCalls.Count | Should -Be 1
        @($script:GhWaits).Count | Should -Be 0
    }

    It 'keeps a non-zero GraphQL errors response with null data ambiguous when retrying is suppressed' {
        $payload = ConvertTo-FixtureJson -Value ([ordered]@{
                errors = @([ordered]@{ message = 'The mutation resolver failed after execution began.' })
                data   = $null
            })
        $payload.Contains('"data":null') | Should -BeTrue
        Add-GhRoute -Key 'graphql' -Response @(New-GhResponse `
                -ExitCode 1 `
                -Body $payload `
                -StandardError 'gh: The mutation resolver failed after execution began.')

        $transport = New-TestTransport
        $failure = $null
        try {
            Invoke-GitHubGraphQlQuery -Transport $transport -Query 'mutation{addPullRequestReviewComment(input:{}){comment{id}}}' -NoRetry | Out-Null
        } catch {
            $failure = $_
        }

        $failure | Should -Not -BeNullOrEmpty
        (Test-PullRequestServiceRefusalException -Exception $failure.Exception) | Should -BeFalse
        $script:GhCalls.Count | Should -Be 1
        @($script:GhWaits).Count | Should -Be 0
    }

    It 'keeps a zero-exit GraphQL errors response with partial data ambiguous' {
        $payload = ConvertTo-FixtureJson -Value ([ordered]@{
                data   = [ordered]@{
                    addPullRequestReviewComment = [ordered]@{
                        comment = [ordered]@{ id = 'PRRC_created' }
                    }
                }
                errors = @([ordered]@{ message = 'A sibling resolver failed.' })
            })
        Add-GhRoute -Key 'graphql' -Response @(New-GhResponse -Body $payload)

        $transport = New-TestTransport
        $failure = $null
        try {
            Invoke-GitHubGraphQlQuery -Transport $transport -Query 'mutation{addPullRequestReviewComment(input:{}){comment{id}}}' | Out-Null
        } catch {
            $failure = $_
        }

        $failure | Should -Not -BeNullOrEmpty
        (Test-PullRequestServiceRefusalException -Exception $failure.Exception) | Should -BeFalse
        $script:GhCalls.Count | Should -Be 1
        @($script:GhWaits).Count | Should -Be 0
    }

    It 'keeps malformed and empty non-zero GraphQL responses ambiguous when retrying is suppressed' {
        foreach ($body in @('', '{')) {
            Reset-GhState
            Add-GhRoute -Key 'graphql' -Response @(New-GhResponse `
                    -ExitCode 1 `
                    -Body $body `
                    -StandardError 'gh: request failed before a usable response was available.')

            $transport = New-TestTransport
            $failure = $null
            try {
                Invoke-GitHubGraphQlQuery -Transport $transport -Query 'mutation{addPullRequestReviewComment(input:{}){comment{id}}}' -NoRetry | Out-Null
            } catch {
                $failure = $_
            }

            $failure | Should -Not -BeNullOrEmpty
            (Test-PullRequestServiceRefusalException -Exception $failure.Exception) | Should -BeFalse
            $script:GhCalls.Count | Should -Be 1
            @($script:GhWaits).Count | Should -Be 0
        }
    }

    It 'flattens every page of a paginated read' {
        $pages = @(
            @([ordered]@{ id = 1 }, [ordered]@{ id = 2 })
            @([ordered]@{ id = 3 }, [ordered]@{ id = 4 })
            @([ordered]@{ id = 5 })
        )
        Add-GhRoute -Key 'api items' -Response @(New-GhResponse -Body (ConvertTo-FixtureJson -Value $pages))

        $transport = New-TestTransport
        $result = Invoke-GitHubRestRequest -Transport $transport -Path 'items' -Paginate

        $result.Count | Should -Be 5
        @($result | ForEach-Object { $_['id'] }) | Should -Be @(1, 2, 3, 4, 5)

        $arguments = $script:GhCalls[0]['arguments']
        $arguments | Should -Contain '--paginate'
        $arguments | Should -Contain '--slurp'
        ($arguments -join ' ') | Should -BeLike '*per_page=100*'
    }

    It 'retries a rate-limited request and waits exactly as long as the service asked' {
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError 'gh: API rate limit exceeded (HTTP 429) retry-after: 30')
            (New-GhResponse -Body '{"ok":true}')
        )

        $transport = New-TestTransport
        $result = Invoke-GitHubRestRequest -Transport $transport -Path 'probe'

        $result['ok'] | Should -BeTrue
        $script:GhCalls.Count | Should -Be 2
        @($script:GhWaits) | Should -Be @(30000)
    }

    It 'waits until an absolute rate limit reset the service reports' {
        # A reset shortly ahead of the transport's clock, inside the ceiling on a single wait, so
        # this measures the reset arithmetic rather than the clamp that bounds it.
        $reset = $script:FixedNowUtc.AddSeconds(45).ToUnixTimeSeconds()
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError "gh: API rate limit exceeded (HTTP 403) x-ratelimit-reset: $reset")
            (New-GhResponse -Body '{"ok":true}')
        )

        Invoke-GitHubRestRequest -Transport (New-TestTransport) -Path 'probe' | Out-Null

        @($script:GhWaits) | Should -Be @(45000)
    }

    It 'honours a distant rate limit reset without shortening it to the backoff cap' {
        $reset = $script:FixedNowUtc.AddHours(1).ToUnixTimeSeconds()
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError "gh: API rate limit exceeded (HTTP 403) x-ratelimit-reset: $reset")
            (New-GhResponse -Body '{"ok":true}')
        )

        Invoke-GitHubRestRequest -Transport (New-TestTransport) -Path 'probe' | Out-Null

        @($script:GhWaits) | Should -Be @(3600000)
    }

    It 'uses backoff when the reported rate limit reset has already passed' {
        $reset = $script:FixedNowUtc.AddMinutes(-5).ToUnixTimeSeconds()
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError "gh: API rate limit exceeded (HTTP 403) x-ratelimit-reset: $reset")
            (New-GhResponse -Body '{"ok":true}')
        )

        Invoke-GitHubRestRequest -Transport (New-TestTransport) -Path 'probe' | Out-Null

        @($script:GhWaits) | Should -Be @(60000)
        $script:GhCalls.Count | Should -Be 2
    }

    It 'retries HTTP <Status> write refusals beyond the transient budget with capped exponential backoff' -ForEach @(
        @{ Status = 403 }
        @{ Status = 429 }
    ) {
        # The refusal sequence outlasts even the default transient budget and reaches the delay cap.
        $responses = @(foreach ($index in 1..8) {
            New-GhResponse -ExitCode 1 -Body '{"message":"Rate Limit Exceeded"}' `
                -StandardError "gh: Rate Limit Exceeded (HTTP $Status)"
        }) + @(New-GhResponse -Body "HTTP/2.0 201 Created`r`nContent-Type: application/json`r`n`r`n{`"id`":123}")
        Add-GhRoute -Key 'api gists' -Response $responses
        $body = '{"files":{"overview.md":{"content":"Frozen report"}}}'
        $result = Invoke-GitHubRestRequest -Transport (New-TestTransport -MaxAttempts 1) `
            -Path 'gists' -Method POST -Body $body -NoRetry

        $result['id'] | Should -Be 123
        $script:GhCalls.Count | Should -Be 9
        @($script:GhWaits) | Should -Be @(60000, 120000, 120000, 120000, 120000, 120000, 120000, 120000)
        foreach ($call in $script:GhCalls) {
            $call['standardInput'] | Should -BeExactly $body
            $call['arguments'] | Should -Contain '--include'
            $call['arguments'] | Should -Contain 'POST'
        }
    }

    It 'reads Retry-After from actual CLI response headers and honours it beyond the backoff cap' {
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError 'gh: Rate Limit Exceeded (HTTP 403)' `
                -Body "HTTP/2.0 403 Forbidden`nRetry-After: 600`n`n{`"message`":`"Rate Limit Exceeded`"}")
            (New-GhResponse -Body "HTTP/2.0 200 OK`n`n{`"ok`":true}")
        )

        $result = Invoke-GitHubRestRequest -Transport (New-TestTransport) -Path 'probe' -NoRetry

        $result['ok'] | Should -BeTrue
        @($script:GhWaits) | Should -Be @(600000)
    }

    It 'uses backoff for a secondary limit while the primary quota still has requests' {
        $reset = $script:FixedNowUtc.AddHours(1).ToUnixTimeSeconds()
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError 'gh: secondary rate limit (HTTP 403)' `
                -Body "HTTP/2.0 403 Forbidden`r`nX-RateLimit-Remaining: 99`r`nX-RateLimit-Reset: $reset`r`n`r`n{}")
            (New-GhResponse -Body '{"ok":true}')
        )

        Invoke-GitHubRestRequest -Transport (New-TestTransport) -Path 'probe' -NoRetry | Out-Null

        @($script:GhWaits) | Should -Be @(60000)
    }

    It 'retries a GraphQL rate-limit refusal with null data and CLI exit code <ExitCode>' -ForEach @(
        @{ ExitCode = 0 }
        @{ ExitCode = 1 }
    ) {
        $body = '{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}'
        Add-GhRoute -Key 'graphql' -Response @(
            (New-GhResponse -ExitCode $ExitCode -Body $body)
            (New-GhResponse -Body '{"data":{"createThing":{"id":"created"}}}')
        )

        $result = Invoke-GitHubGraphQlQuery -Transport (New-TestTransport -MaxAttempts 1) `
            -Query 'mutation{createThing{id}}' -NoRetry

        $result['data']['createThing']['id'] | Should -BeExactly 'created'
        $script:GhCalls.Count | Should -Be 2
        @($script:GhWaits) | Should -Be @(60000)
        $script:GhCalls[0]['standardInput'] | Should -BeExactly $script:GhCalls[1]['standardInput']
    }

    It 'does not retry GraphQL rate-limit errors with partial mutation data or mixed errors' -ForEach @(
        @{ Body = '{"data":{"createThing":{"id":"created"}},"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}' }
        @{ Body = '{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"},{"message":"Mutation failed during execution"}]}' }
    ) {
        Add-GhRoute -Key 'graphql' -Response @(New-GhResponse -ExitCode 1 -Body $Body `
                -StandardError 'gh: API rate limit exceeded (HTTP 403)')

        $failure = { Invoke-GitHubGraphQlQuery -Transport (New-TestTransport) -Query 'mutation{createThing{id}}' -NoRetry } |
            Should -Throw -PassThru

        Get-PublicationFailureOutcome -Exception $failure.Exception | Should -BeExactly 'ambiguous'
        $script:GhCalls.Count | Should -Be 1
        $script:GhWaits.Count | Should -Be 0
    }

    It 'records an interrupted throttling wait as a definite refusal' {
        Add-GhRoute -Key 'api probe' -Response @(New-GhResponse -ExitCode 1 -StandardError 'gh: Rate Limit Exceeded (HTTP 403)')
        $wait = { param([int] $Milliseconds) throw [OperationCanceledException]::new('Wait interrupted') }

        $failure = { Invoke-GitHubRestRequest -Transport (New-TestTransport -WaitBlock $wait) -Path 'probe' -NoRetry } |
            Should -Throw -PassThru

        Get-PublicationFailureOutcome -Exception $failure.Exception | Should -BeExactly 'failed'
        $script:GhCalls.Count | Should -Be 1
    }

    It 'does not retry a lost write response following a definite rate-limit refusal' {
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError 'gh: Rate Limit Exceeded (HTTP 403)')
            (New-GhResponse -ExitCode 1 -StandardError 'gh: unexpected EOF')
            (New-GhResponse -Body '{"ok":true}')
        )

        $failure = { Invoke-GitHubRestRequest -Transport (New-TestTransport) -Path 'probe' -Method POST -Body '{}' -NoRetry } |
            Should -Throw -PassThru

        Get-PublicationFailureOutcome -Exception $failure.Exception | Should -BeExactly 'ambiguous'
        $script:GhCalls.Count | Should -Be 2
        @($script:GhWaits) | Should -Be @(60000)
    }

    It 'does not retry an ordinary forbidden response as throttling' {
        Add-GhRoute -Key 'api probe' -Response @(New-GhResponse -ExitCode 1 `
                -Body "HTTP/2.0 403 Forbidden`r`nX-RateLimit-Limit: 5000`r`nX-RateLimit-Remaining: 99`r`n`r`n{`"message`":`"Resource not accessible`"}" `
                -StandardError 'gh: Resource not accessible (HTTP 403)')
        $wait = { param([int] $Milliseconds) throw 'A permission refusal was incorrectly retried.' }

        { Invoke-GitHubRestRequest -Transport (New-TestTransport -WaitBlock $wait) -Path 'probe' -NoRetry } |
            Should -Throw '*Resource not accessible*'

        $script:GhCalls.Count | Should -Be 1
        $script:GhWaits.Count | Should -Be 0
    }

    It 'does not spend the transient retry budget on rate-limit refusals' {
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError 'gh: Rate Limit Exceeded (HTTP 403)')
            (New-GhResponse -ExitCode 1 -StandardError 'gh: Bad Gateway (HTTP 502)')
            (New-GhResponse -Body '{"ok":true}')
        )

        $result = Invoke-GitHubRestRequest -Transport (New-TestTransport -MaxAttempts 2) -Path 'probe'

        $result['ok'] | Should -BeTrue
        @($script:GhWaits) | Should -Be @(60000, 1000)
    }

    It 'saturates backoff safely for a long-running refusal sequence' {
        Get-GitHubRetryDelayMilliseconds -Diagnostics '' -Attempt ([int]::MaxValue) `
            -NowUtc $script:FixedNowUtc -RateLimit | Should -Be 120000
    }

    It 'keeps whatever the wait block leaves behind out of the result' {
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError 'gh: Bad Gateway (HTTP 502)')
            (New-GhResponse -Body '{"ok":true}')
        )

        # A wait implementation that emits would otherwise prepend its output to the reply, turning
        # the materialized document into an array whose properties cannot be read.
        $emittingWait = { param([int] $Milliseconds) $Milliseconds }

        $result = Invoke-GitHubRestRequest -Transport (New-TestTransport -WaitBlock $emittingWait) -Path 'probe'

        $result | Should -BeOfType [System.Collections.IDictionary]
        $result['ok'] | Should -BeTrue
    }

    It 'fails rather than reporting one document as a single page' {
        # Without --slurp the CLI splices array pages together, so a reply that is not a list of
        # pages means the walk never happened and the read would silently report one page as all.
        Add-GhRoute -Key 'api items' -Response @(New-GhResponse -Body (ConvertTo-FixtureJson -Value ([ordered]@{ id = 1 })))

        { Invoke-GitHubRestRequest -Transport (New-TestTransport) -Path 'items' -Paginate } |
            Should -Throw '*returned a single document where pages were expected*'
    }

    It 'fails when the invoker answers with something other than a dictionary' {
        $invoker = { param([string[]] $ArgumentList, [string] $StandardInput) 'gh output' }

        { Invoke-GitHubRestRequest -Transport (New-TestTransport -Invoker $invoker) -Path 'probe' } |
            Should -Throw '*must return a dictionary*'
    }

    It 'fails when the invoker omits a value the transport reads' {
        $invoker = {
            param([string[]] $ArgumentList, [string] $StandardInput)

            [ordered]@{ exitCode = 0; standardOutputBytes = [Text.Encoding]::UTF8.GetBytes('{"ok":true}') }
        }

        { Invoke-GitHubRestRequest -Transport (New-TestTransport -Invoker $invoker) -Path 'probe' } |
            Should -Throw "*no 'standardErrorText' value*"
    }

    It 'backs off exponentially and gives up after the configured attempts' {
        Add-GhRoute -Key 'api probe' -Response @(New-GhResponse -ExitCode 1 -StandardError 'gh: Bad Gateway (HTTP 502)')

        $transport = New-TestTransport -MaxAttempts 4

        { Invoke-GitHubRestRequest -Transport $transport -Path 'probe' } | Should -Throw '*after 4 attempt*'
        $script:GhCalls.Count | Should -Be 4
        @($script:GhWaits) | Should -Be @(1000, 2000, 4000)
    }

    It 'does not retry a failure that repeating cannot fix' {
        Add-GhRoute -Key 'api probe' -Response @(New-GhResponse -ExitCode 1 -StandardError 'gh: Validation Failed (HTTP 422)')

        $transport = New-TestTransport

        { Invoke-GitHubRestRequest -Transport $transport -Path 'probe' } | Should -Throw '*Validation Failed*'
        $script:GhCalls.Count | Should -Be 1
        @($script:GhWaits).Count | Should -Be 0
    }

    It 'attempts a request once when retrying is suppressed, even though the failure is retryable' {
        # A second attempt would succeed here, so a single call proves the suppression rather than
        # an exhausted attempt budget.
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError 'gh: Bad Gateway (HTTP 502)')
            (New-GhResponse -Body '{"ok":true}')
        )

        $transport = New-TestTransport

        { Invoke-GitHubRestRequest -Transport $transport -Path 'probe' -NoRetry } |
            Should -Throw '*after 1 attempt*'
        $script:GhCalls.Count | Should -Be 1
        @($script:GhWaits).Count | Should -Be 0
    }

    It 'retries the same failure when retrying is not suppressed' {
        Add-GhRoute -Key 'api probe' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError 'gh: Bad Gateway (HTTP 502)')
            (New-GhResponse -Body '{"ok":true}')
        )

        $transport = New-TestTransport
        $result = Invoke-GitHubRestRequest -Transport $transport -Path 'probe'

        $result['ok'] | Should -BeTrue
        $script:GhCalls.Count | Should -Be 2
    }

    It 'attempts a GraphQL request once when retrying is suppressed' {
        Add-GhRoute -Key 'graphql' -Response @(
            (New-GhResponse -ExitCode 1 -StandardError 'gh: Service Unavailable (HTTP 503)')
            (New-GhResponse -Body '{"data":{"viewer":{"login":"octocat"}}}')
        )

        $transport = New-TestTransport

        { Invoke-GitHubGraphQlQuery -Transport $transport -Query 'query{viewer{login}}' -NoRetry } |
            Should -Throw '*after 1 attempt*'
        $script:GhCalls.Count | Should -Be 1
        @($script:GhWaits).Count | Should -Be 0
    }
}

Describe 'GitHub pull request capture' {
    BeforeEach {
        Reset-GhState
    }

    It 'follows the review thread cursor to the end of the connection' {
        Register-PullRequestFixture -ThreadPageJson @(
            (New-ThreadPageJson `
                    -Thread @(New-ThreadPayload -Id 'PRRT_1' -Comment @(New-ThreadCommentPayload -Id 'PRRC_1')) `
                    -HasNextPage $true `
                    -EndCursor 'thread-cursor-1')
            (New-ThreadPageJson -Thread @(New-ThreadPayload -Id 'PRRT_2' -Comment @(New-ThreadCommentPayload -Id 'PRRC_2')))
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        # Exactly once each: a fetcher that dropped the cursor would request the first page again and
        # report its threads twice over.
        @($context['threads'] | Where-Object { $_['id'] -eq 'PRRT_1' }).Count | Should -Be 1
        @($context['threads'] | Where-Object { $_['id'] -eq 'PRRT_2' }).Count | Should -Be 1

        $threadQueries = @($script:GhCalls | Where-Object { $_['standardInput'].Contains('reviewThreads(') })
        $threadQueries.Count | Should -Be 2
        $threadQueries[0]['standardInput'].Contains('thread-cursor-1') | Should -BeFalse
        $threadQueries[1]['standardInput'].Contains('thread-cursor-1') | Should -BeTrue
    }

    It 'follows the cursor of the connection that supplies comment edit times' {
        Register-PullRequestFixture `
            -IssueComment @(
            (New-IssueCommentPayload -Id 2001 -NodeId 'IC_1')
            (New-IssueCommentPayload -Id 2002 -NodeId 'IC_2')
        ) `
            -IssueCommentEditPageJson @(
            (New-IssueCommentEditPageJson `
                    -Node @([ordered]@{ id = 'IC_1'; lastEditedAt = '2026-08-03T09:15:00Z' }) `
                    -HasNextPage $true `
                    -EndCursor 'edit-cursor-1')
            (New-IssueCommentEditPageJson -Node @([ordered]@{ id = 'IC_2'; lastEditedAt = '2026-08-04T09:15:00Z' }))
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        # The second page is the only source of the second comment's edit time, so a walk that
        # stopped or restarted would report that comment as never edited.
        (Get-CanonicalThread -Context $context -Id 'IC_1')['comments'][0]['lastEditedUtc'] |
            Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-03T09:15:00Z')
        (Get-CanonicalThread -Context $context -Id 'IC_2')['comments'][0]['lastEditedUtc'] |
            Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-04T09:15:00Z')

        $editQueries = @($script:GhCalls | Where-Object { $_['standardInput'].Contains('comments(first:$size,after:$cur)') -and $_['standardInput'].Contains('nodes{ id lastEditedAt }') })
        $editQueries.Count | Should -Be 2
        $editQueries[1]['standardInput'].Contains('edit-cursor-1') | Should -BeTrue
    }

    It 'follows the cursor of the connection that supplies review edit times' {
        Register-PullRequestFixture `
            -Review @(
            (New-ReviewPayload -Id 1001 -NodeId 'PRR_1' -State 'COMMENTED' -Body 'First summary.')
            (New-ReviewPayload -Id 1002 -NodeId 'PRR_2' -State 'APPROVED' -Body '')
        ) `
            -ReviewEditPageJson @(
            (New-ReviewEditPageJson `
                    -Node @([ordered]@{ id = 'PRR_1'; lastEditedAt = '2026-08-03T09:15:00Z' }) `
                    -HasNextPage $true `
                    -EndCursor 'review-edit-cursor-1')
            (New-ReviewEditPageJson -Node @([ordered]@{
                        id = 'PRR_2'; lastEditedAt = '2026-08-04T09:15:00Z'; comments = @{ totalCount = 0 }
                    }))
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        (Get-CanonicalThread -Context $context -Id 'PRR_1')['comments'][0]['lastEditedUtc'] |
            Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-03T09:15:00Z')
        (Get-CanonicalThread -Context $context -Id 'PRR_2')['comments'][0]['lastEditedUtc'] |
            Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-04T09:15:00Z')
        (Get-CanonicalThread -Context $context -Id 'PRR_2')['comments'][0]['authorship'] | Should -BeExactly 'human'

        $editQueries = @($script:GhCalls | Where-Object { $_['standardInput'].Contains('reviews(first:$size,after:$cur)') })
        $editQueries.Count | Should -Be 2
        $editQueries[1]['standardInput'].Contains('review-edit-cursor-1') | Should -BeTrue
    }

    It 'follows the cursor of the connection that supplies force pushes' {
        Register-PullRequestFixture -TimelinePageJson @(
            (New-TimelinePageJson -Node @(
                [ordered]@{
                    __typename   = 'HeadRefForcePushedEvent'
                    id           = 'HRFPE_1'
                    createdAt    = '2026-08-03T07:00:00Z'
                    actor        = New-ActorPayload -Login 'author' -DatabaseId 601 -NodeId 'U_author' -Url 'https://github.com/author'
                    beforeCommit = [ordered]@{ oid = 'before01' }
                    afterCommit  = [ordered]@{ oid = 'after001' }
                }
            ) -HasNextPage $true -EndCursor 'timeline-cursor-1')
            (New-TimelinePageJson -Node @(
                [ordered]@{
                    __typename   = 'HeadRefForcePushedEvent'
                    id           = 'HRFPE_2'
                    createdAt    = '2026-08-03T08:00:00Z'
                    actor        = New-ActorPayload -Login 'author' -DatabaseId 601 -NodeId 'U_author' -Url 'https://github.com/author'
                    beforeCommit = [ordered]@{ oid = 'before02' }
                    afterCommit  = [ordered]@{ oid = 'after002' }
                }
            ))
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        (Get-CanonicalThread -Context $context -Id 'HRFPE_1')['properties']['afterCommit'] | Should -Be 'after001'
        (Get-CanonicalThread -Context $context -Id 'HRFPE_2')['properties']['afterCommit'] | Should -Be 'after002'

        $timelineQueries = @($script:GhCalls | Where-Object { $_['standardInput'].Contains('timelineItems(') })
        $timelineQueries.Count | Should -Be 2
        $timelineQueries[1]['standardInput'].Contains('timeline-cursor-1') | Should -BeTrue
    }

    It 'fails when the response omits the connection the query asked for' {
        $missing = ConvertTo-FixtureJson -Value ([ordered]@{
                data = [ordered]@{ repository = [ordered]@{ pullRequest = [ordered]@{} } }
            })
        Register-PullRequestFixture -ThreadPageJson @($missing)

        { Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw "*missing 'data.repository.pullRequest.reviewThreads'*"
    }

    It 'fails when a connection page carries no nodes field' {
        $malformed = ConvertTo-FixtureJson -Value ([ordered]@{
                data = [ordered]@{
                    repository = [ordered]@{
                        pullRequest = [ordered]@{
                            reviewThreads = [ordered]@{
                                pageInfo = [ordered]@{ hasNextPage = $false; endCursor = $null }
                            }
                        }
                    }
                }
            })
        Register-PullRequestFixture -ThreadPageJson @($malformed)

        { Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw '*carrying no nodes field*'
    }

    It 'fails when a page fetcher hands back no page at all' {
        { Get-GitHubGraphQlConnectionNode -FetchPage { $null } -Description 'probe' } |
            Should -Throw '*returned no page*'
    }

    It 'fails when a page reports its own existence without saying whether another follows' {
        $page = [ordered]@{ nodes = @(); pageInfo = [ordered]@{ endCursor = 'cursor-1' } }
        $fetchPage = { $page }.GetNewClosure()

        { Get-GitHubGraphQlConnectionNode -FetchPage $fetchPage -Description 'probe' } |
            Should -Throw '*carrying no hasNextPage field*'
    }

    It 'stops a connection that never stops reporting another page' {
        # The service answers every request with another page and a usable cursor, which without a
        # bound would keep the capture fetching for as long as the service keeps answering.
        $page = [ordered]@{
            nodes    = @()
            pageInfo = [ordered]@{ hasNextPage = $true; endCursor = 'cursor-1' }
        }
        $fetchPage = { $page }.GetNewClosure()

        { Get-GitHubGraphQlConnectionNode -FetchPage $fetchPage -Description 'probe' } |
            Should -Throw '*exceeded 1000 pages*'
    }

    It 'follows the comment cursor of a thread that holds more comments than one page' {
        $firstComment = New-ThreadCommentPayload -Id 'PRRC_1' -Body 'First observation.' -CreatedAt '2026-08-01T10:00:00Z'
        $secondComment = New-ThreadCommentPayload -Id 'PRRC_2' -Body 'Second observation.' -CreatedAt '2026-08-01T10:05:00Z'
        $thirdComment = New-ThreadCommentPayload -Id 'PRRC_3' -Body 'Third observation.' -CreatedAt '2026-08-01T10:10:00Z'

        Register-PullRequestFixture `
            -ThreadPageJson @(New-ThreadPageJson -Thread @(
                    New-ThreadPayload -Id 'PRRT_1' -Comment @($firstComment) -CommentsHaveNextPage $true -CommentsEndCursor 'cursor-1'
                )) `
            -ThreadCommentPageJson @(
            (New-ThreadCommentPageJson -Comment @($secondComment) -HasNextPage $true -EndCursor 'cursor-2')
            (New-ThreadCommentPageJson -Comment @($thirdComment))
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $thread = Get-CanonicalThread -Context $context -Id 'PRRT_1'

        @($thread['comments'] | ForEach-Object { $_['id'] }) | Should -Be @('PRRC_1', 'PRRC_2', 'PRRC_3')
        $thread['publishedUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-01T10:00:00Z')
        $thread['lastUpdatedUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-01T10:10:00Z')

        # The cursors have to reach the service, otherwise a walk that ignored them would still
        # assemble the right comments from this fixture and hide the defect.
        $commentQueries = @($script:GhCalls | Where-Object { $_['standardInput'] -like '*node(id:*' })
        $commentQueries.Count | Should -Be 2
        $commentQueries[0]['standardInput'] | Should -BeLike '*cursor-1*'
        $commentQueries[1]['standardInput'] | Should -BeLike '*cursor-2*'
    }

    It 'names the comment that replies to nothing as the thread opener' {
        # GitHub distinguishes the root of a review thread from the replies beneath it by replyTo
        # alone, so the opener is named structurally rather than taken from whichever comment
        # presentation order places first. The root is served last and shares its instant with the
        # reply, so an opener taken from ordering would name the reply.
        $root = New-ThreadCommentPayload -Id 'PRRC_root' -Body 'The root.' -CreatedAt '2026-08-01T10:00:00Z'
        $reply = New-ThreadCommentPayload -Id 'PRRC_reply' -Body 'The reply.' -CreatedAt '2026-08-01T10:00:00Z' -ReplyTo ([ordered]@{ id = 'PRRC_root' })

        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_1' -Comment @($reply, $root)
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $thread = Get-CanonicalThread -Context $context -Id 'PRRT_1'

        $thread['openingCommentId'] | Should -BeExactly 'PRRC_root'
    }

    It 'fails rather than truncating when a cursor cannot be followed' {
        Register-PullRequestFixture `
            -ThreadPageJson @(New-ThreadPageJson -Thread @(New-ThreadPayload -Id 'PRRT_1') -HasNextPage $true -EndCursor $null)

        { Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw '*supplies no cursor*'
    }

    It 'fails rather than truncating when a thread reports more comments without a cursor' {
        Register-PullRequestFixture `
            -ThreadPageJson @(New-ThreadPageJson -Thread @(
                    New-ThreadPayload -Id 'PRRT_1' -CommentsHaveNextPage $true -CommentsEndCursor $null
                ))

        { Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw '*comments of thread PRRT_1*'
    }

    It 'fails when a connection page omits the information needed to walk it' {
        $malformed = ConvertTo-FixtureJson -Value ([ordered]@{
                data = [ordered]@{
                    repository = [ordered]@{
                        pullRequest = [ordered]@{
                            reviewThreads = [ordered]@{ nodes = @() }
                        }
                    }
                }
            })
        Register-PullRequestFixture -ThreadPageJson @($malformed)

        { Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw '*pageInfo*'
    }

    It 'classifies an account typed as a bot as a machine' {
        Register-PullRequestFixture -Review @(
            New-ReviewPayload -Id 1001 -NodeId 'PRR_bot' -State 'COMMENTED' -Body 'Automated findings follow.' -User (
                New-AccountPayload -Login 'copilot-pull-request-reviewer[bot]' -Id 1 -NodeId 'BOT_example' -Type 'Bot' -HtmlUrl 'https://github.com/apps/copilot-pull-request-reviewer'
            )
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $thread = Get-CanonicalThread -Context $context -Id 'PRR_bot'

        $thread['comments'][0]['author']['kind'] | Should -Be 'service'
        $thread['comments'][0]['author']['evidence'] | Should -Contain 'accountType:Bot'
        $thread['comments'][0]['authorship'] | Should -BeExactly 'agent'
    }

    It 'classifies an account carrying a bot node identifier as a machine' {
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_bot' -Comment @(
                    New-ThreadCommentPayload -Id 'PRRC_bot' -Author (
                        New-ActorPayload -Login 'helper' -TypeName 'Mannequin' -NodeId 'BOT_example' -Url 'https://github.com/helper'
                    )
                )
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $author = (Get-CanonicalThread -Context $context -Id 'PRRT_bot')['comments'][0]['author']

        $author['kind'] | Should -Be 'service'
        $author['evidence'] | Should -Contain 'nodeId:BOT_'
    }

    It 'classifies an account published as an app as a machine' {
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_app' -Comment @(
                    New-ThreadCommentPayload -Id 'PRRC_app' -Author (
                        New-ActorPayload -Login 'inspector' -TypeName 'Mannequin' -NodeId 'MDQ6VXNlcjE=' -Url 'https://github.com/apps/inspector'
                    )
                )
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $author = (Get-CanonicalThread -Context $context -Id 'PRRT_app')['comments'][0]['author']

        $author['kind'] | Should -Be 'service'
        $author['evidence'] | Should -Contain 'url:apps'
    }

    It 'never classifies from a login suffix alone' {
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_suffix' -Comment @(
                    New-ThreadCommentPayload -Id 'PRRC_suffix' -Author (
                        New-ActorPayload -Login 'pretender[bot]' -TypeName 'User' -NodeId 'U_kgDOsuffix' -Url 'https://github.com/pretender'
                    )
                )
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $author = (Get-CanonicalThread -Context $context -Id 'PRRT_suffix')['comments'][0]['author']

        $author['kind'] | Should -Be 'person'
        $author['evidence'] | Should -Be @('accountType:User')
        $author['uniqueName'] | Should -Be 'pretender[bot]'
    }

    It 'treats a comment carrying the agent marker as machine-authored under a personal account' {
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_marker' -Comment @(
                    New-ThreadCommentPayload -Id 'PRRC_marker' -Body '[Copilot speaking] The retry bound is off by one.'
                )
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $comment = (Get-CanonicalThread -Context $context -Id 'PRRT_marker')['comments'][0]

        $comment['author']['kind'] | Should -Be 'person'
        $comment['authorship'] | Should -BeExactly 'agent'
        $comment['authorshipEvidence'] | Should -Contain 'content:agentMarker'
    }

    It 'leaves a comment that only mentions the agent marker for the classification pass' {
        # The marker is what an agent puts at the start of what it writes, so a body that carries it
        # anywhere else is a person quoting or discussing an agent. Mechanical capture states only
        # what it can prove, and only an indeterminate comment reaches the classification pass, so a
        # mechanical verdict here would never be reconsidered
        # (docs/design.md, "Authorship classification").
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_quote' -Comment @(
                    New-ThreadCommentPayload -Id 'PRRC_quote' -Body "I disagree with this:`n`n> [Copilot speaking] The retry bound is off by one."
                )
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $comment = (Get-CanonicalThread -Context $context -Id 'PRRT_quote')['comments'][0]

        $comment['authorship'] | Should -BeExactly 'indeterminate'
        $comment['authorshipEvidence'] | Should -Not -Contain 'content:agentMarker'
    }

    It 'reads the agent marker through the whitespace a body may begin with' {
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_padded' -Comment @(
                    New-ThreadCommentPayload -Id 'PRRC_padded' -Body "`r`n  [Copilot speaking] The retry bound is off by one."
                )
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $comment = (Get-CanonicalThread -Context $context -Id 'PRRT_padded')['comments'][0]

        $comment['authorship'] | Should -BeExactly 'agent'
        $comment['authorshipEvidence'] | Should -Contain 'content:agentMarker'
    }

    It 'leaves a comment posted from a personal account unclassified' {
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_person' -Comment @(
                    New-ThreadCommentPayload -Id 'PRRC_person' -Body 'The retry bound is off by one.'
                )
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $comment = (Get-CanonicalThread -Context $context -Id 'PRRT_person')['comments'][0]

        # The account proves who the comment was posted as, not who composed it: the same account
        # posts on its owner's behalf and on an agent's. Capture therefore states only what it can
        # prove, and a later classification pass decides the rest.
        $comment['authorship'] | Should -BeExactly 'indeterminate'
        # Identity kind and comment authorship answer different questions, and an unclassified
        # comment from a personal account is exactly where confusing them would show.
        $comment['author']['kind'] | Should -BeExactly 'person'
    }

    It 'captures an approval that carries no text' {
        Register-PullRequestFixture -Review @(
            New-ReviewPayload -Id 1001 -NodeId 'PRR_approved' -State 'APPROVED' -Body '' -SubmittedAt '2026-08-04T10:38:05Z' -User (
                New-AccountPayload -Login 'reviewer' -Id 1 -NodeId 'U_reviewer' -HtmlUrl 'https://github.com/reviewer'
            )
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $thread = Get-CanonicalThread -Context $context -Id 'PRR_approved'

        # A review is captured for the prose it carries. Its verdict is not recorded, so the thread
        # carries no decision, and its kind reports that it is a review summary rather than a
        # conversation a reply can be posted into.
        $thread['kind'] | Should -BeExactly 'reviewSummary'
        $thread['rawStatus'] | Should -BeNullOrEmpty
        $thread['decision'] | Should -BeNullOrEmpty
        $thread['properties']['state'] | Should -BeExactly 'APPROVED'
        $thread['comments'][0]['author']['uniqueName'] | Should -Be 'reviewer'
        $thread['publishedUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-04T10:38:05Z')
        $thread['comments'][0]['content'] | Should -Be ''
        $thread['comments'][0]['authorship'] | Should -BeExactly 'human'
        $thread['comments'][0]['authorshipEvidence'] | Should -Contain 'review:voteOnly'
    }

    It 'classifies an empty <State> review as human without semantic decisions' -ForEach @(
        @{ State = 'APPROVED'; Body = $null }
        @{ State = 'CHANGES_REQUESTED'; Body = '' }
        @{ State = 'DISMISSED'; Body = " `r`n`t" }
    ) {
        $review = New-ReviewPayload -State $State
        $review['body'] = $Body
        Register-PullRequestFixture -Review @($review)

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $document = New-PullRequestContextDocument -Context $context -NowUtc $script:FixedNowUtc
        $path = Join-Path $TestDrive "$State.json"
        Write-PullRequestCapture -Path $path -Document $document | Out-Null
        $captured = Read-PullRequestContextDocument -Path $path
        $summary = New-PullRequestCaptureResult -Document $captured -ContextPath $path

        $summary['indeterminateCommentCount'] | Should -Be 0
        $comment = (Get-CanonicalThread -Context $captured -Id 'PRR_1')['comments'][0]
        $comment['authorship'] | Should -BeExactly 'human'
        $comment['authorshipEvidence'] | Should -Contain 'review:voteOnly'
        $query = @($script:GhCalls | Where-Object { $_['standardInput'].Contains('reviews(first:$size,after:$cur)') })
        $query.Count | Should -Be 1
        $query[0]['standardInput'] | Should -Match 'comments\s*\{\s*totalCount\s*\}'
    }

    It 'keeps known bot attribution for an empty vote' {
        Register-PullRequestFixture -Review @(
            New-ReviewPayload -User (New-AccountPayload -Type 'Bot')
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $comment = (Get-CanonicalThread -Context $context -Id 'PRR_1')['comments'][0]

        $comment['authorship'] | Should -BeExactly 'agent'
        $comment['authorshipEvidence'] | Should -Not -Contain 'review:voteOnly'
    }

    It 'leaves a review with <Description> for semantic classification' -ForEach @(
        @{ Description = 'body text'; Body = 'Looks good.'; Count = 0 }
        @{ Description = 'inline feedback'; Body = ''; Count = 1 }
    ) {
        Register-PullRequestFixture -Review @(New-ReviewPayload -Body $Body) `
            -ReviewEditNode @(@{ id = 'PRR_1'; lastEditedAt = $null; comments = @{ totalCount = $Count } })

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $comment = (Get-CanonicalThread -Context $context -Id 'PRR_1')['comments'][0]

        $comment['authorship'] | Should -BeExactly 'indeterminate'
        $comment['authorshipEvidence'] | Should -Not -Contain 'review:voteOnly'
    }

    It 'rejects a vote-only classification with an invalid comment count <Count>' -ForEach @(
        @{ Count = $null }
        @{ Count = -1 }
        @{ Count = '0' }
    ) {
        Register-PullRequestFixture -Review @(New-ReviewPayload) `
            -ReviewEditNode @(@{ id = 'PRR_1'; lastEditedAt = $null; comments = @{ totalCount = $Count } })

        { Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw '*missing a valid comment count*'
    }

    It 'discards a commenting review that carries no text of its own' {
        Register-PullRequestFixture -Review @(
            New-ReviewPayload -Id 1001 -NodeId 'PRR_container' -State 'COMMENTED' -Body ''
            New-ReviewPayload -Id 1002 -NodeId 'PRR_spoken' -State 'COMMENTED' -Body 'One more thought.'
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        @($context['threads'] | ForEach-Object { $_['id'] }) | Should -Be @('PRR_spoken')
    }

    It 'leaves an unsubmitted review out of the capture' {
        # GitHub returns a pending review only to the account that owns it, which is the account
        # this run authenticates as. Capturing it would let a draft left behind by an interrupted
        # run be read back by a later run as feedback that had already been given, suppressing the
        # finding it was about to raise.
        Register-PullRequestFixture -Review @(
            New-ReviewPayload -Id 1001 -NodeId 'PRR_pending' -State 'PENDING' -Body 'Draft I have not sent.' -SubmittedAt $null `
                -User (New-AccountPayload -Login 'drafter' -Id 777 -NodeId 'U_drafter' -HtmlUrl 'https://github.com/drafter')
            New-ReviewPayload -Id 1002 -NodeId 'PRR_submitted' -State 'COMMENTED' -Body 'One more thought.'
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        @($context['threads'] | ForEach-Object { $_['id'] }) | Should -Be @('PRR_submitted')
        # The roster is filtered by the same rule as the threads. An account holding nothing but a
        # draft has reviewed nothing, so reporting it as a reviewer would leak the draft's existence
        # by another route.
        @($context['reviewers'] | ForEach-Object { $_['identity']['uniqueName'] }) | Should -Not -Contain 'drafter'
    }

    It 'leaves a draft comment out of a thread that also holds submitted comments' {
        # A line or file comment belonging to an unsubmitted review reaches the thread connection
        # for the account that owns the draft, which is the account this run authenticates as.
        # Publication assembles its whole review as a draft (docs/design.md, "Review publication"), so
        # an abandoned run and a person composing feedback in the web interface both produce this.
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_mixed' -Comment @(
                    New-ThreadCommentPayload -Id 'PRRC_sent' -Body 'Published remark.'
                    New-ThreadCommentPayload -Id 'PRRC_draft' -Body 'Unsent reply.' -ReplyTo ([ordered]@{ id = 'PRRC_sent' }) -State 'PENDING'
                )
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $thread = Get-CanonicalThread -Context $context -Id 'PRRT_mixed'

        @($thread['comments'] | ForEach-Object { $_['id'] }) | Should -Be @('PRRC_sent')
    }

    It 'leaves out a thread whose comments are all drafts' {
        # Keeping the thread with an empty comment list would force every consumer to guard against
        # a thread nobody has said anything in, and would report a file as already discussed when
        # the discussion is unsent.
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_sent' -Comment @(New-ThreadCommentPayload -Id 'PRRC_a' -Body 'Published remark.')
                New-ThreadPayload -Id 'PRRT_draft' -Comment @(New-ThreadCommentPayload -Id 'PRRC_b' -Body 'Unsent remark.' -State 'PENDING')
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        @($context['threads'] | Where-Object { $_['kind'] -eq 'text' -and $null -ne $_['context'] } | ForEach-Object { $_['id'] }) |
            Should -Be @('PRRT_sent')
    }

    It 'reports a review as edited when the service records an edit time' {        # A submitted review implements the Comment interface, so its edit time is available exactly
        # as an issue comment's is. Without it a rewritten review summary would compare equal to its
        # original and the freshness check would report no change.
        Register-PullRequestFixture `
            -Review @(New-ReviewPayload -Id 1001 -NodeId 'PRR_1' -State 'COMMENTED' -Body 'Rewritten summary.') `
            -ReviewEditNode @([ordered]@{ id = 'PRR_1'; lastEditedAt = '2026-08-05T09:15:00Z' })

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $thread = Get-CanonicalThread -Context $context -Id 'PRR_1'

        $thread['comments'][0]['isEdited'] | Should -BeTrue
        $thread['comments'][0]['lastEditedUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-05T09:15:00Z')
        $thread['lastUpdatedUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-05T09:15:00Z')
    }

    It 'reports a review as unedited when the service records no edit time' {
        Register-PullRequestFixture `
            -Review @(New-ReviewPayload -Id 1001 -NodeId 'PRR_1' -State 'COMMENTED' -Body 'Original summary.' -SubmittedAt '2026-08-02T09:00:00Z') `
            -ReviewEditNode @([ordered]@{ id = 'PRR_1'; lastEditedAt = $null })

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $thread = Get-CanonicalThread -Context $context -Id 'PRR_1'

        $thread['comments'][0]['isEdited'] | Should -BeFalse
        $thread['comments'][0]['lastEditedUtc'] | Should -BeNullOrEmpty
        $thread['lastUpdatedUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-02T09:00:00Z')
    }

    It 'retains both the current and the original anchor of an outdated thread' {
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload `
                    -Id 'PRRT_outdated' `
                    -IsResolved $true `
                    -IsOutdated $true `
                    -Path 'pkg/widget.go' `
                    -Line $null `
                    -OriginalLine 1221 `
                    -StartLine $null `
                    -OriginalStartLine 1219
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $thread = Get-CanonicalThread -Context $context -Id 'PRRT_outdated'

        $thread['isOutdated'] | Should -BeTrue
        $thread['resolution'] | Should -Be 'resolved'
        $thread['rawStatus'] | Should -Be 'resolved'
        $thread['anchorKey'] | Should -Be 'PRRT_outdated'
        $thread['context']['filePath'] | Should -Be 'pkg/widget.go'
        $thread['context']['endLine'] | Should -BeNullOrEmpty
        $thread['context']['startLine'] | Should -BeNullOrEmpty
        $thread['context']['originalEndLine'] | Should -Be 1221
        $thread['context']['originalStartLine'] | Should -Be 1219
    }

    It 'reports an unresolved thread as active' {
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_open' -IsResolved $false
            ))

        $thread = Get-CanonicalThread `
            -Context (Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target) `
            -Id 'PRRT_open'

        $thread['resolution'] | Should -Be 'active'
        $thread['rawStatus'] | Should -Be 'unresolved'
    }

    It 'reports a comment as unedited when the service records no edit' {
        Register-PullRequestFixture `
            -IssueComment @(New-IssueCommentPayload -Id 2001 -NodeId 'IC_1' -CreatedAt '2026-08-02T11:00:00Z' -UpdatedAt '2026-08-02T11:00:01Z') `
            -IssueCommentEditNode @([ordered]@{ id = 'IC_1'; lastEditedAt = $null })

        $thread = Get-CanonicalThread `
            -Context (Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target) `
            -Id 'IC_1'

        $thread['comments'][0]['isEdited'] | Should -BeFalse
        $thread['comments'][0]['lastEditedUtc'] | Should -BeNullOrEmpty
        $thread['comments'][0]['publishedUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-02T11:00:00Z')
    }

    It 'reports a comment as edited when the service records an edit time' {
        Register-PullRequestFixture `
            -IssueComment @(New-IssueCommentPayload -Id 2001 -NodeId 'IC_1') `
            -IssueCommentEditNode @([ordered]@{ id = 'IC_1'; lastEditedAt = '2026-08-03T09:15:00Z' })

        $thread = Get-CanonicalThread `
            -Context (Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target) `
            -Id 'IC_1'

        $thread['comments'][0]['isEdited'] | Should -BeTrue
        $thread['comments'][0]['lastEditedUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-03T09:15:00Z')
    }

    It 'reports a non-threaded comment as having no resolution state' {
        Register-PullRequestFixture -IssueComment @(New-IssueCommentPayload -Id 2001 -NodeId 'IC_1' -Body 'A general remark.')

        $thread = Get-CanonicalThread `
            -Context (Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target) `
            -Id 'IC_1'

        $thread['kind'] | Should -Be 'text'
        $thread['resolution'] | Should -Be 'indeterminate'
        $thread['rawStatus'] | Should -BeNullOrEmpty
        $thread['comments'][0]['content'] | Should -Be 'A general remark.'
    }

    It 'captures a force push as a reference update carrying both commits' {
        Register-PullRequestFixture -TimelineNode @(
            [ordered]@{
                __typename   = 'HeadRefForcePushedEvent'
                id           = 'HRFPE_1'
                createdAt    = '2026-08-03T07:00:00Z'
                actor        = New-ActorPayload -Login 'author' -DatabaseId 601 -NodeId 'U_author' -Url 'https://github.com/author'
                beforeCommit = [ordered]@{ oid = 'before00' }
                afterCommit  = [ordered]@{ oid = 'after000' }
            }
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $thread = Get-CanonicalThread -Context $context -Id 'HRFPE_1'

        $thread['kind'] | Should -Be 'refUpdate'
        $thread['properties']['beforeCommit'] | Should -Be 'before00'
        $thread['properties']['afterCommit'] | Should -Be 'after000'
        $thread['decision']['by']['uniqueName'] | Should -Be 'author'
        $thread['decision']['utc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-03T07:00:00Z')
        $thread['comments'] | Should -BeNullOrEmpty
        $context['revisions'] | Should -BeNullOrEmpty
    }

    It 'captures every discussion source exactly once' {
        Register-PullRequestFixture `
            -Review @(New-ReviewPayload -Id 1001 -NodeId 'PRR_1' -State 'APPROVED' -SubmittedAt '2026-08-02T09:00:00Z') `
            -IssueComment @(New-IssueCommentPayload -Id 2001 -NodeId 'IC_1' -CreatedAt '2026-08-03T09:00:00Z') `
            -ThreadPageJson @(New-ThreadPageJson -Thread @(
                    New-ThreadPayload -Id 'PRRT_1' -Comment @(New-ThreadCommentPayload -CreatedAt '2026-08-01T10:00:00Z')
                ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        # Membership rather than order: the shared model sorts the discussion chronologically once
        # every adapter has returned, so the adapter states only which events exist and when.
        $threads = @($context['threads'])
        (@($threads | ForEach-Object { $_['id'] }) | Sort-Object) -join ',' | Should -Be 'IC_1,PRR_1,PRRT_1'
        @($threads | Where-Object { $_['id'] -eq 'PRRT_1' })[0]['eventUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-01T10:00:00Z')
        @($threads | Where-Object { $_['id'] -eq 'PRR_1' })[0]['eventUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-02T09:00:00Z')
        @($threads | Where-Object { $_['id'] -eq 'IC_1' })[0]['eventUtc'] | Should -Be (ConvertTo-CanonicalTimestamp -Value '2026-08-03T09:00:00Z')
    }

    It 'reports the reviewed commits and the pull request record' {
        Register-PullRequestFixture

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        $context['target'] | Should -Be $script:Target
        $context['authenticatedIdentity']['uniqueName'] | Should -Be 'runner'
        $context['pullRequest']['id'] | Should -Be 7
        $context['pullRequest']['title'] | Should -Be 'Add the widget'
        $context['pullRequest']['sourceRef'] | Should -Be 'feature/widget'
        $context['pullRequest']['targetRef'] | Should -Be 'main'
        $context['pullRequest']['reviewedSourceCommit'] | Should -Be 'headsha0000000000000000000000000000000'
        $context['pullRequest']['reviewedMergeBaseCommit'] | Should -Be 'mergebase00000000000000000000000000000'
        $context['pullRequest']['mergeState']['status'] | Should -Be 'clean'
        $context['pullRequest']['mergeState'] | Should -BeOfType [System.Collections.IDictionary]
        $context['pullRequest']['author']['uniqueName'] | Should -Be 'author'
    }

    It 'records no lifecycle signal for an open pull request' {
        Register-PullRequestFixture -PullRequest (New-PullRequestPayload -State 'open')

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        # Approval standing and pull request lifecycle are the concern of whoever merges, so the
        # capture records neither. What remains of the merge picture is the platform-specific
        # conflict bookkeeping.
        $context['pullRequest'].Contains('state') | Should -BeFalse
        $context['pullRequest'].Contains('platformState') | Should -BeFalse
        $context['pullRequest']['mergeState'].Contains('isMerged') | Should -BeFalse
        @($context['pullRequest']['mergeState'].Keys) | Should -Be @('status', 'isMergeable', 'mergeCommit')
    }

    It 'reads a merged pull request the same way as an open one' {
        # The service reports the merge in the payload the adapter reads, so its absence from the
        # capture is the exclusion working rather than the fixture being silent.
        Register-PullRequestFixture -PullRequest (New-PullRequestPayload -State 'closed' -Merged $true)

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        $context['pullRequest']['mergeState'].Contains('isMerged') | Should -BeFalse
        @($context['pullRequest']['mergeState'].Keys) | Should -Be @('status', 'isMergeable', 'mergeCommit')
    }

    It 'reports the diff side in the canonical lower case' {
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_right' -DiffSide 'RIGHT'
                New-ThreadPayload -Id 'PRRT_left' -DiffSide 'LEFT' -Comment @(New-ThreadCommentPayload -Id 'PRRC_left')
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        # The comparison is ordinal: Should -Be is case-insensitive and would pass on the service's
        # own upper case, which is the single distinction this test exists to make.
        (Get-CanonicalThread -Context $context -Id 'PRRT_right')['context']['side'] | Should -BeExactly 'right'
        (Get-CanonicalThread -Context $context -Id 'PRRT_left')['context']['side'] | Should -BeExactly 'left'
        (Get-CanonicalThread -Context $context -Id 'PRRT_left')['properties']['diffSide'] | Should -BeExactly 'LEFT'
    }

    It 'captures the text of a review whatever state the service reports' {
        Register-PullRequestFixture -Review @(
            New-ReviewPayload -Id 1001 -NodeId 'PRR_approved' -State 'APPROVED' -Body 'Fine.'
            New-ReviewPayload -Id 1002 -NodeId 'PRR_rejected' -State 'CHANGES_REQUESTED' -Body 'Please revise.'
            New-ReviewPayload -Id 1003 -NodeId 'PRR_commented' -State 'COMMENTED' -Body 'An aside.'
            New-ReviewPayload -Id 1004 -NodeId 'PRR_dismissed' -State 'DISMISSED' -Body 'No longer relevant.'
            New-ReviewPayload -Id 1005 -NodeId 'PRR_future' -State 'SOMETHING_NEW' -Body 'A state this skill has not seen.'
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        (Get-CanonicalThread -Context $context -Id 'PRR_approved')['comments'][0]['content'] | Should -Be 'Fine.'
        (Get-CanonicalThread -Context $context -Id 'PRR_rejected')['comments'][0]['content'] | Should -Be 'Please revise.'
        (Get-CanonicalThread -Context $context -Id 'PRR_commented')['comments'][0]['content'] | Should -Be 'An aside.'
        (Get-CanonicalThread -Context $context -Id 'PRR_dismissed')['comments'][0]['content'] | Should -Be 'No longer relevant.'
        # A state this skill has not seen still yields the prose the reviewer wrote, because nothing
        # in the capture depends on recognizing the state.
        (Get-CanonicalThread -Context $context -Id 'PRR_future')['comments'][0]['content'] | Should -Be 'A state this skill has not seen.'

        # The service's own state is kept in the platform-specific property bag, where it makes no
        # claim about meaning.
        (Get-CanonicalThread -Context $context -Id 'PRR_rejected')['properties']['state'] | Should -BeExactly 'CHANGES_REQUESTED'
        foreach ($id in @('PRR_approved', 'PRR_rejected', 'PRR_commented', 'PRR_dismissed', 'PRR_future')) {
            (Get-CanonicalThread -Context $context -Id $id)['kind'] | Should -BeExactly 'reviewSummary'
        }
    }

    It 'separates a review summary from the threads a reply can be posted into' {
        # A review summary is identified by a review node, and the service accepts no reply posted
        # into it. A consumer choosing where to answer therefore has to tell it apart from a review
        # thread or a general comment, and the canonical kind is what lets it do so without reading
        # platform-specific fields.
        Register-PullRequestFixture `
            -Review @(New-ReviewPayload -Id 1001 -NodeId 'PRR_summary' -State 'COMMENTED' -Body 'A summary of my review.') `
            -ThreadPageJson @(New-ThreadPageJson -Thread @(New-ThreadPayload -Id 'PRRT_1')) `
            -IssueComment @(New-IssueCommentPayload -Id 2001 -NodeId 'IC_1')

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        (Get-CanonicalThread -Context $context -Id 'PRR_summary')['kind'] | Should -BeExactly 'reviewSummary'
        (Get-CanonicalThread -Context $context -Id 'PRRT_1')['kind'] | Should -BeExactly 'text'
        (Get-CanonicalThread -Context $context -Id 'IC_1')['kind'] | Should -BeExactly 'text'
    }

    It 'addresses a fork head by its owner and names the source branch unambiguously' {
        Register-PullRequestFixture -PullRequest (New-PullRequestPayload -HeadRepository 'contributor/widget' -HeadOwner 'contributor')

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        $context['pullRequest']['sourceRef'] | Should -Be 'contributor:feature/widget'
        $compareCall = @($script:GhCalls | Where-Object { ($_['arguments'] -join ' ') -like '*compare/*' })[0]
        ($compareCall['arguments'] -join ' ') | Should -BeLike '*compare/basesha*...contributor:headsha*'
    }

    It 'falls back to the unqualified comparison when the fork address is gone' {
        Register-PullRequestFixture -PullRequest (New-PullRequestPayload -HeadRepository 'contributor/widget' -HeadOwner 'contributor')
        # The fork address answers 404 once the fork is deleted, while the commits stay reachable
        # through the upstream repository, so the unqualified address still resolves.
        Add-GhRoute -Key 'compare/basesha0000000000000000000000000000000...contributor:' -Response @(
            New-GhResponse -ExitCode 1 -Body '{"message":"Not Found"}' -StandardError 'gh: Not Found (HTTP 404)'
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        $context['pullRequest']['reviewedMergeBaseCommit'] | Should -Be 'mergebase00000000000000000000000000000'
        $compareCalls = @($script:GhCalls | Where-Object { ($_['arguments'] -join ' ') -like '*compare/*' })
        $compareCalls.Count | Should -Be 2
        ($compareCalls[1]['arguments'] -join ' ') | Should -BeLike '*compare/basesha*...headsha*'
    }

    It 'reports no merge base when no comparison resolves' {
        Register-PullRequestFixture -PullRequest (New-PullRequestPayload -HeadRepository 'contributor/widget' -HeadOwner 'contributor')
        Add-GhRoute -Key 'compare/' -Response @(
            New-GhResponse -ExitCode 1 -Body '{"message":"Not Found"}' -StandardError 'gh: Not Found (HTTP 404)'
        )

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target

        $context['pullRequest']['reviewedMergeBaseCommit'] | Should -BeNullOrEmpty
        @($script:GhCalls | Where-Object { ($_['arguments'] -join ' ') -like '*compare/*' }).Count | Should -Be 2
    }

    It 'surfaces a comparison failure that is not an unresolvable address' {
        Register-PullRequestFixture
        # A permission failure says nothing about the address being wrong, so trying another form
        # would report an absent merge base for a pull request that has one.
        Add-GhRoute -Key 'compare/' -Response @(
            New-GhResponse -ExitCode 1 -Body '{"message":"Must have admin rights to Repository."}' -StandardError 'gh: Must have admin rights to Repository. (HTTP 403)'
        )

        { Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw -ExpectedMessage '*HTTP 403*'
    }

    It 'lists every reviewer who reviewed and every reviewer still requested, once each' {
        Register-PullRequestFixture `
            -Review @(
            New-ReviewPayload -Id 1001 -NodeId 'PRR_1' -State 'CHANGES_REQUESTED' -Body 'Please revise.' -SubmittedAt '2026-08-02T09:00:00Z'
            New-ReviewPayload -Id 1002 -NodeId 'PRR_2' -State 'APPROVED' -Body 'Better.' -SubmittedAt '2026-08-02T12:00:00Z'
        ) `
            -RequestedReviewers ([ordered]@{
                users = @(New-AccountPayload -Login 'pending' -Id 801 -NodeId 'U_pending' -HtmlUrl 'https://github.com/pending')
                teams = @([ordered]@{ id = 55; node_id = 'T_1'; name = 'Widget Owners'; slug = 'widget-owners'; html_url = 'https://github.com/orgs/octo/teams/widget-owners' })
            })

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $reviewers = @($context['reviewers'])

        # The same account submitted two reviews, so a listing that did not de-duplicate would name
        # it twice.
        $reviewers.Count | Should -Be 3
        $reviewers[0]['identity']['uniqueName'] | Should -Be 'reviewer'
        $reviewers[0]['isTeam'] | Should -BeFalse
        $reviewers[1]['identity']['uniqueName'] | Should -Be 'pending'
        $reviewers[2]['isTeam'] | Should -BeTrue
        $reviewers[2]['identity']['uniqueName'] | Should -Be 'widget-owners'

        foreach ($reviewer in $reviewers) {
            (@($reviewer.Keys) -join ', ') | Should -BeExactly 'identity, isRequired, isTeam'
        }
    }

    It 'names a reviewer once when they both reviewed and were asked to look again' {
        # GitHub removes a reviewer from the requested list when they submit a review, so a reviewer
        # who appears in both has been asked to look again.
        Register-PullRequestFixture `
            -Review @(
            New-ReviewPayload -Id 1001 -NodeId 'PRR_1' -State 'APPROVED' -Body 'Fine by me.' -SubmittedAt '2026-08-02T09:00:00Z' -User (
                New-AccountPayload -Login 'reviewer' -Id 501 -NodeId 'U_reviewer' -HtmlUrl 'https://github.com/reviewer'
            )
        ) `
            -RequestedReviewers ([ordered]@{
                users = @(New-AccountPayload -Login 'reviewer' -Id 501 -NodeId 'U_reviewer' -HtmlUrl 'https://github.com/reviewer')
                teams = @()
            })

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $reviewers = @($context['reviewers'])

        $reviewers.Count | Should -Be 1
        $reviewers[0]['identity']['uniqueName'] | Should -Be 'reviewer'
    }

    It 'carries content through the whole capture without altering a code point' {
        $body = "Consider $($script:LeftQuote)widget$($script:RightQuote) $script:EmDash it reads better $script:Robot"
        Register-PullRequestFixture -ThreadPageJson @(New-ThreadPageJson -Thread @(
                New-ThreadPayload -Id 'PRRT_text' -Comment @(New-ThreadCommentPayload -Id 'PRRC_text' -Body $body)
            ))

        $context = Get-GitHubPullRequestContext -Transport (New-TestTransport) -Target $script:Target
        $content = (Get-CanonicalThread -Context $context -Id 'PRRT_text')['comments'][0]['content']

        (Get-CodePoint -Text $content) | Should -Be (Get-CodePoint -Text $body)
    }
}

Describe 'GitHub identity and target helpers' {
    BeforeEach {
        Reset-GhState
    }

    It 'derives the browser URL through the shared target library' {
        $bare = [ordered]@{
            platform      = 'gitHub'
            organization  = 'octo'
            repository    = 'widget'
            pullRequestId = 7
        }

        $resolved = Resolve-GitHubPullRequestTarget -Target $bare

        # The adapter publishes the address the rest of the skill resolves for the same coordinates,
        # and the two assertions together pin the forms to each other: a derivation reintroduced in
        # the adapter breaks the first, and a change to the shared derivation breaks the second. A
        # link that opens nothing is the only symptom a drifting URL produces.
        $resolved['webUrl'] | Should -BeExactly (Get-PullRequestWebUrl -Target $resolved)
        $resolved['webUrl'] | Should -BeExactly 'https://github.com/octo/widget/pull/7'
    }

    It 'reports the authenticated account by its stable identifier' {
        Add-GhRoute -Key 'api user' -Response @(New-GhResponse -Body (ConvertTo-FixtureJson -Value (
                    New-AccountPayload -Login 'runner' -Id 701 -NodeId 'U_runner' -HtmlUrl 'https://github.com/runner'
                )))

        $identity = Get-GitHubAuthenticatedIdentity -Transport (New-TestTransport)

        $identity['id'] | Should -Be '701'
        $identity['uniqueName'] | Should -Be 'runner'
        $identity['kind'] | Should -Be 'person'
    }
}
