BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:LibraryRoot = Join-Path $script:RepoRoot 'skills\ss-review\scripts\lib'

    # Libraries never dot-source one another, so every dependency is loaded here in dependency order:
    # value handling and failure signaling first, then identity and target coordinates, each
    # transport with the adapter that uses it, and the shared model last because it assembles what
    # the adapters return.
    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PublicationFailure.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'AdoRestTransport.ps1')
    . (Join-Path $script:LibraryRoot 'AdoPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubCliTransport.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')

    # The canonical vocabularies both adapters emit (docs/design.md, "Captured pull request context").
    #
    # Each list is stated once and both platforms are checked against that one list. Stating a list
    # per platform is the failure this file exists to prevent: two lists can drift apart, and a
    # consumer selecting on the value of a field then matches on one platform and silently sees
    # nothing on the other.
    $script:ThreadKindVocabulary = @('text', 'reviewSummary', 'refUpdate', 'reviewersUpdated', 'policy', 'system')
    $script:DecisionTypeVocabulary = @('refUpdate')
    $script:ContextSideVocabulary = @('right', 'left')
    $script:ThreadResolutionVocabulary = @('resolved', 'active', 'indeterminate')
    # Identity kind and authorship are separate vocabularies answering different questions: what the
    # account is, and who composed the comment. A service identity produces agent authorship, while
    # an identity that belongs to a person settles nothing about authorship, so a value of one
    # vocabulary must never be read as an answer in the other.
    $script:IdentityKindVocabulary = @('person', 'service', 'indeterminate')
    $script:AuthorshipVocabulary = @('agent', 'human', 'indeterminate')

    # Lists a node's keys in the order it carries them.
    #
    # Comma-wrapped and assigned by the caller, because a command that emits a single array emits it
    # as one object and an unwrapped return would be unrolled onto the pipeline.
    function Get-NodeKeyList {
        param(
            [Parameter(Mandatory)]
            $Node
        )

        # Coercion rather than a type test: anything that has been through the pipeline arrives
        # wrapped in a PSObject, whose key enumeration is not the dictionary's own.
        $dictionary = $Node -as [System.Collections.IDictionary]
        if ($null -eq $dictionary) {
            $typeName = if ($null -eq $Node) { 'null' } else { $Node.GetType().FullName }
            throw "Expected a dictionary to read keys from but found $typeName."
        }

        $keys = [System.Collections.Generic.List[string]]::new()
        foreach ($key in $dictionary.Keys) { $keys.Add([string] $key) }
        return , $keys.ToArray()
    }

    # Renders a node's key list as one comparable string.
    #
    # A single string is what makes a divergence legible: the failure names both key lists in full
    # rather than reporting that two collections differ somewhere.
    function Get-KeySignature {
        param(
            [Parameter(Mandatory)]
            $Node
        )

        $keys = Get-NodeKeyList -Node $Node
        $keys -join ', '
    }

    # Reduces a set of nodes to the distinct key signatures they carry.
    #
    # A platform that reports more than one signature for the same kind of node has already diverged
    # from itself, so this is checked before the two platforms are compared with each other.
    function Get-DistinctKeySignature {
        param(
            [Parameter(Mandatory)]
            [AllowEmptyCollection()]
            [object[]] $Node
        )

        $signatures = [System.Collections.Generic.List[string]]::new()
        foreach ($item in $Node) {
            $signature = Get-KeySignature -Node $item
            if (-not $signatures.Contains($signature)) { $signatures.Add($signature) }
        }
        return , $signatures.ToArray()
    }

    function Get-DocumentThread {
        param(
            [Parameter(Mandatory)]
            $Document
        )

        $threads = Get-JsonArrayProperty -Node $Document -Name 'threads'
        return , $threads
    }

    function Get-DocumentComment {
        param(
            [Parameter(Mandatory)]
            $Document
        )

        $result = [System.Collections.Generic.List[object]]::new()
        $threads = Get-JsonArrayProperty -Node $Document -Name 'threads'
        foreach ($thread in $threads) {
            $comments = Get-JsonArrayProperty -Node $thread -Name 'comments'
            foreach ($comment in $comments) { $result.Add($comment) }
        }
        return , $result.ToArray()
    }

    function Get-DocumentReviewer {
        param(
            [Parameter(Mandatory)]
            $Document
        )

        $reviewers = Get-JsonArrayProperty -Node $Document -Name 'reviewers'
        return , $reviewers
    }

    function Get-DocumentThreadContext {
        param(
            [Parameter(Mandatory)]
            $Document
        )

        $result = [System.Collections.Generic.List[object]]::new()
        $threads = Get-JsonArrayProperty -Node $Document -Name 'threads'
        foreach ($thread in $threads) {
            $context = Get-JsonProperty -Node $thread -Name 'context'
            if ($null -ne $context) { $result.Add($context) }
        }
        return , $result.ToArray()
    }

    function Get-DocumentDecision {
        param(
            [Parameter(Mandatory)]
            $Document,

            [string] $Type
        )

        $result = [System.Collections.Generic.List[object]]::new()
        $threads = Get-JsonArrayProperty -Node $Document -Name 'threads'
        foreach ($thread in $threads) {
            $decision = Get-JsonProperty -Node $thread -Name 'decision'
            if ($null -eq $decision) { continue }
            if ($Type -and (Get-JsonProperty -Node $decision -Name 'type') -ne $Type) { continue }
            $result.Add($decision)
        }
        return , $result.ToArray()
    }

    # --- Azure DevOps fixture -------------------------------------------------------------------

    $script:AdoConnectionDataJson = @'
{
  "authenticatedUser": {
    "id": "runner-id",
    "descriptor": "Microsoft.IdentityModel.Claims.ClaimsIdentity;tenant\\runner@example.com",
    "subjectDescriptor": "aad.cnVubmVy",
    "providerDisplayName": "Review Runner",
    "properties": { "Account": { "$type": "System.String", "$value": "runner@example.com" } }
  }
}
'@

    $script:AdoPullRequestJson = @'
{
  "pullRequestId": 7,
  "status": "active",
  "createdBy": {
    "id": "author-id",
    "displayName": "Widget Author",
    "uniqueName": "author@example.com",
    "descriptor": "aad.YXV0aG9y"
  },
  "creationDate": "2026-08-01T08:00:00.000Z",
  "title": "Add the widget",
  "description": "This changes the widget.",
  "sourceRefName": "refs/heads/feature/widget",
  "targetRefName": "refs/heads/main",
  "mergeStatus": "succeeded",
  "isDraft": false,
  "mergeId": "44444444-4444-4444-4444-444444444444",
  "lastMergeSourceCommit": { "commitId": "headsha" },
  "lastMergeTargetCommit": { "commitId": "targetbranchhead" },
  "lastMergeCommit": { "commitId": "mergecommit" }
}
'@

    $script:AdoIterationsJson = @'
{
  "count": 1,
  "value": [
    {
      "id": 1,
      "createdDate": "2026-08-01T08:00:00.000Z",
      "reason": "push",
      "sourceRefCommit": { "commitId": "headsha" },
      "targetRefCommit": { "commitId": "targetbranchhead" },
      "commonRefCommit": { "commitId": "mergebase" }
    }
  ]
}
'@

    $script:AdoReviewersJson = @'
{
  "count": 1,
  "value": [
    {
      "id": "reviewer-id",
      "displayName": "Review Reviewer",
      "uniqueName": "reviewer@example.com",
      "vote": 10,
      "isRequired": true,
      "isContainer": false
    }
  ]
}
'@

    # These threads are the Azure DevOps half of the equivalence: an ordinary text thread
    # anchored to a file, a VoteUpdate thread that the capture excludes, and a RefUpdate thread. The
    # GitHub fixture below presents the closest counterpart the other service offers for each.
    $script:AdoThreadsJson = @'
{
  "count": 3,
  "value": [
    {
      "id": 101,
      "publishedDate": "2026-08-01T10:00:00.000Z",
      "lastUpdatedDate": "2026-08-01T10:00:00.000Z",
      "status": "active",
      "threadContext": {
        "filePath": "/src/widget.ps1",
        "rightFileStart": { "line": 42, "offset": 1 },
        "rightFileEnd": { "line": 42, "offset": 9 }
      },
      "pullRequestThreadContext": {
        "iterationContext": { "firstComparingIteration": 1, "secondComparingIteration": 1 },
        "changeTrackingId": 14
      },
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": {
            "id": "reviewer-id",
            "displayName": "Review Reviewer",
            "uniqueName": "reviewer@example.com",
            "descriptor": "aad.cmV2aWV3ZXI"
          },
          "content": "Please rename this.",
          "publishedDate": "2026-08-01T10:00:00.000Z",
          "lastUpdatedDate": "2026-08-01T10:00:00.000Z",
          "lastContentUpdatedDate": "2026-08-01T10:00:00.000Z",
          "commentType": "text"
        }
      ]
    },
    {
      "id": 102,
      "publishedDate": "2026-08-02T09:00:00.000Z",
      "lastUpdatedDate": "2026-08-02T09:00:00.000Z",
      "identities": {
        "1": {
          "id": "reviewer-id",
          "displayName": "Review Reviewer",
          "uniqueName": "reviewer@example.com",
          "descriptor": "aad.cmV2aWV3ZXI"
        }
      },
      "properties": {
        "CodeReviewThreadType": { "$type": "System.String", "$value": "VoteUpdate" },
        "CodeReviewVoteResult": { "$type": "System.String", "$value": "10" },
        "CodeReviewVotedByIdentity": { "$type": "System.String", "$value": "1" }
      },
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": {
            "id": "notifier-id",
            "displayName": "Microsoft.VisualStudio.Services.TFS",
            "descriptor": "s2s.bm90aWZpZXI"
          },
          "content": "Review Reviewer approved",
          "publishedDate": "2026-08-02T09:00:00.000Z",
          "commentType": "system"
        }
      ]
    },
    {
      "id": 103,
      "publishedDate": "2026-08-02T12:00:00.000Z",
      "lastUpdatedDate": "2026-08-02T12:00:00.000Z",
      "properties": {
        "CodeReviewThreadType": { "$type": "System.String", "$value": "RefUpdate" }
      },
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": {
            "id": "notifier-id",
            "displayName": "Microsoft.VisualStudio.Services.TFS",
            "descriptor": "s2s.bm90aWZpZXI"
          },
          "content": "Widget Author pushed new changes",
          "publishedDate": "2026-08-02T12:00:00.000Z",
          "commentType": "system"
        }
      ]
    }
  ]
}
'@

    $script:AdoTarget = [ordered]@{
        platform      = 'azureDevOps'
        collectionUrl = 'https://example.visualstudio.com'
        organization  = 'example'
        project       = 'Example Project'
        repository    = 'widget'
        pullRequestId = 7
        webUrl        = $null
    }

    function New-AdoFixtureResponse {
        param(
            [Parameter(Mandatory)]
            [string] $Text
        )

        [ordered]@{
            statusCode   = 200
            headers      = @{}
            contentType  = 'application/json; charset=utf-8'
            contentBytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
        }
    }

    # Builds an Azure DevOps transport whose HTTP call, token acquisition and wait are all injected,
    # so the capture touches no network and never sleeps.
    function New-FakeAdoTransport {
        $send = {
            param(
                [string] $Method,
                [string] $Url,
                [hashtable] $Headers,
                $Body
            )

            if ($Url -match '/_apis/connectionData\?') { return New-AdoFixtureResponse -Text $script:AdoConnectionDataJson }
            if ($Url -match '/threads\?') { return New-AdoFixtureResponse -Text $script:AdoThreadsJson }
            if ($Url -match '/iterations\?') { return New-AdoFixtureResponse -Text $script:AdoIterationsJson }
            if ($Url -match '/reviewers\?') { return New-AdoFixtureResponse -Text $script:AdoReviewersJson }
            if ($Url -match '/pullRequests/\d+\?') { return New-AdoFixtureResponse -Text $script:AdoPullRequestJson }
            throw "The fake Azure DevOps transport has no reply for $Url."
        }

        $getToken = { 'fake-bearer-material' }

        # Nothing in these fixtures fails, so the transport never retries and the wait is never
        # invoked. It is injected regardless, so that a fixture that did provoke a retry would still
        # complete instantly rather than sleep.
        $wait = {
            param([int] $Milliseconds)
        }

        New-AdoRestTransport `
            -CollectionUrl $script:AdoTarget['collectionUrl'] `
            -SendRequest $send `
            -GetAccessToken $getToken `
            -Wait $wait
    }

    # --- GitHub fixture -------------------------------------------------------------------------

    $script:GitHubTarget = [ordered]@{
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
            [string] $Body = ''
        )

        [ordered]@{
            exitCode            = 0
            standardOutputBytes = [Text.Encoding]::UTF8.GetBytes($Body)
            standardErrorText   = ''
        }
    }

    # Registers a canned reply for every invocation whose command line and standard input contain
    # the key. Later registrations win, which is what lets the review thread query and the thread
    # comment query be told apart by the fragment unique to each.
    function Add-GhRoute {
        param(
            [Parameter(Mandatory)]
            [string] $Key,

            [Parameter(Mandatory)]
            [string] $Body
        )

        $script:GhRoutes.Add([ordered]@{ key = $Key; body = $Body })
    }

    function New-FakeGitHubTransport {
        $invokeGh = {
            param(
                [string[]] $ArgumentList,
                [string] $StandardInput
            )

            $composite = ($ArgumentList -join ' ') + ' ' + [string] $StandardInput
            for ($index = $script:GhRoutes.Count - 1; $index -ge 0; $index--) {
                $route = $script:GhRoutes[$index]
                if ($composite.Contains([string] $route['key'])) {
                    return New-GhResponse -Body ([string] $route['body'])
                }
            }

            throw "The fake GitHub CLI has no reply for 'gh $($ArgumentList -join ' ')'."
        }

        # Injected for the same reason as on the other transport: no fixture provokes a retry, and a
        # future one that did must not spend real time waiting.
        $wait = {
            param([int] $Milliseconds)
        }

        New-GitHubCliTransport -InvokeGh $invokeGh -Wait $wait
    }

    # Registers a complete, coherent set of replies for the GitHub half of the equivalence: a review
    # thread anchored to a file, a submitted review carrying an approval, a non-threaded comment and
    # a force push. Every read the adapter performs has a reply, so a missing route would fail the
    # capture outright rather than yield a quietly shorter document.
    function Register-GitHubFixture {
        $script:GhRoutes = [System.Collections.Generic.List[object]]::new()

        $reviewerAccount = [ordered]@{
            login    = 'reviewer'
            id       = 501
            node_id  = 'U_reviewer'
            type     = 'User'
            html_url = 'https://github.com/reviewer'
        }
        $authorAccount = [ordered]@{
            login    = 'author'
            id       = 601
            node_id  = 'U_author'
            type     = 'User'
            html_url = 'https://github.com/author'
        }
        $runnerAccount = [ordered]@{
            login    = 'runner'
            id       = 701
            node_id  = 'U_runner'
            type     = 'User'
            html_url = 'https://github.com/runner'
        }
        $reviewerActor = [ordered]@{
            login      = 'reviewer'
            __typename = 'User'
            id         = 'U_reviewer'
            databaseId = 501
            url        = 'https://github.com/reviewer'
        }
        $authorActor = [ordered]@{
            login      = 'author'
            __typename = 'User'
            id         = 'U_author'
            databaseId = 601
            url        = 'https://github.com/author'
        }

        $pullRequest = [ordered]@{
            number           = 7
            node_id          = 'PR_7'
            title            = 'Add the widget'
            body             = 'This changes the widget.'
            state            = 'open'
            merged           = $false
            mergeable        = $true
            merge_commit_sha = 'mergecommit'
            draft            = $false
            user             = $authorAccount
            created_at       = '2026-08-01T08:00:00Z'
            mergeable_state  = 'clean'
            head             = [ordered]@{
                ref   = 'feature/widget'
                label = 'octo:feature/widget'
                sha   = 'headsha'
                repo  = [ordered]@{ full_name = 'octo/widget'; owner = [ordered]@{ login = 'octo' } }
            }
            base             = [ordered]@{
                ref   = 'main'
                label = 'octo:main'
                sha   = 'basesha'
                repo  = [ordered]@{ full_name = 'octo/widget'; owner = [ordered]@{ login = 'octo' } }
            }
        }

        $review = [ordered]@{
            id                 = 1001
            node_id            = 'PRR_1'
            user               = $reviewerAccount
            body               = 'Looks correct.'
            state              = 'APPROVED'
            html_url           = 'https://github.com/octo/widget/pull/7#pullrequestreview-1001'
            author_association = 'MEMBER'
            submitted_at       = '2026-08-02T09:00:00Z'
            commit_id          = 'headsha'
        }

        $issueComment = [ordered]@{
            url                = 'https://api.github.com/repos/octo/widget/issues/comments/2001'
            html_url           = 'https://github.com/octo/widget/pull/7#issuecomment-2001'
            id                 = 2001
            node_id            = 'IC_1'
            user               = $reviewerAccount
            created_at         = '2026-08-02T11:00:00Z'
            updated_at         = '2026-08-02T11:00:00Z'
            body               = 'Looks reasonable to me.'
            author_association = 'MEMBER'
            minimized          = $false
        }

        $threadComment = [ordered]@{
            id                = 'PRRC_1'
            databaseId        = 900001
            url               = 'https://github.com/octo/widget/pull/7#discussion_PRRC_1'
            body              = 'Please rename this.'
            createdAt         = '2026-08-01T10:00:00Z'
            lastEditedAt      = $null
            replyTo           = $null
            author            = $reviewerActor
            authorAssociation = 'MEMBER'
            minimizedReason   = $null
        }

        $thread = [ordered]@{
            id                = 'PRRT_1'
            isResolved        = $false
            isOutdated        = $false
            isCollapsed       = $false
            path              = 'src/widget.ps1'
            line              = 42
            originalLine      = 42
            startLine         = $null
            originalStartLine = $null
            diffSide          = 'RIGHT'
            startDiffSide     = 'RIGHT'
            subjectType       = 'LINE'
            resolvedBy        = $null
            comments          = [ordered]@{
                pageInfo = [ordered]@{ hasNextPage = $false; endCursor = $null }
                nodes    = @($threadComment)
            }
        }

        $forcePush = [ordered]@{
            __typename   = 'HeadRefForcePushedEvent'
            id           = 'HRFPE_1'
            createdAt    = '2026-08-02T12:00:00Z'
            actor        = $authorActor
            beforeCommit = [ordered]@{ oid = 'oldheadsha' }
            afterCommit  = [ordered]@{ oid = 'headsha' }
        }

        $threadPage = [ordered]@{
            data = [ordered]@{
                repository = [ordered]@{
                    pullRequest = [ordered]@{
                        reviewThreads = [ordered]@{
                            pageInfo = [ordered]@{ hasNextPage = $false; endCursor = $null }
                            nodes    = @($thread)
                        }
                    }
                }
            }
        }

        $issueCommentEditPage = [ordered]@{
            data = [ordered]@{
                repository = [ordered]@{
                    pullRequest = [ordered]@{
                        comments = [ordered]@{
                            pageInfo = [ordered]@{ hasNextPage = $false; endCursor = $null }
                            nodes    = @([ordered]@{ id = 'IC_1'; lastEditedAt = $null })
                        }
                    }
                }
            }
        }

        $reviewEditPage = [ordered]@{
            data = [ordered]@{
                repository = [ordered]@{
                    pullRequest = [ordered]@{
                        reviews = [ordered]@{
                            pageInfo = [ordered]@{ hasNextPage = $false; endCursor = $null }
                            nodes    = @([ordered]@{ id = 'PRR_1'; lastEditedAt = $null })
                        }
                    }
                }
            }
        }

        $timelinePage = [ordered]@{
            data = [ordered]@{
                repository = [ordered]@{
                    pullRequest = [ordered]@{
                        timelineItems = [ordered]@{
                            pageInfo = [ordered]@{ hasNextPage = $false; endCursor = $null }
                            nodes    = @($forcePush)
                        }
                    }
                }
            }
        }

        # A paginated read is served as an array of pages, which is the shape the CLI produces with
        # --slurp; the inner comma keeps a single page from collapsing into its own element.
        Add-GhRoute -Key 'api user' -Body (ConvertTo-FixtureJson -Value $runnerAccount)
        Add-GhRoute -Key 'pulls/7 --method GET' -Body (ConvertTo-FixtureJson -Value $pullRequest)
        Add-GhRoute -Key 'pulls/7/reviews' -Body (ConvertTo-FixtureJson -Value @(, @($review)))
        Add-GhRoute -Key 'issues/7/comments' -Body (ConvertTo-FixtureJson -Value @(, @($issueComment)))
        Add-GhRoute -Key 'requested_reviewers --method GET' -Body (ConvertTo-FixtureJson -Value ([ordered]@{ users = @(); teams = @() }))
        Add-GhRoute -Key 'compare/' -Body (ConvertTo-FixtureJson -Value ([ordered]@{
                    status            = 'ahead'
                    merge_base_commit = [ordered]@{ sha = 'mergebase' }
                }))
        Add-GhRoute -Key 'reviewThreads' -Body (ConvertTo-FixtureJson -Value $threadPage)
        Add-GhRoute -Key 'nodes{ id lastEditedAt }' -Body (ConvertTo-FixtureJson -Value $issueCommentEditPage)
        # Registered after the shared node selection because the two edit-time queries select the
        # same fields and are told apart only by the connection they walk; later registrations win.
        Add-GhRoute -Key 'reviews(first:' -Body (ConvertTo-FixtureJson -Value $reviewEditPage)
        Add-GhRoute -Key 'timelineItems' -Body (ConvertTo-FixtureJson -Value $timelinePage)
    }

    # --- The two documents under comparison -----------------------------------------------------

    # A fixed instant rather than the wall clock, so the two documents differ only where the two
    # services differ and a re-run compares the same bytes.
    $script:FixedNowUtc = [DateTimeOffset]::new(2026, 8, 4, 12, 0, 0, [TimeSpan]::Zero)

    Register-GitHubFixture

    $script:AdoDocument = New-PullRequestContextDocument `
        -Context (Get-AdoPullRequestContext -Transport (New-FakeAdoTransport) -Target $script:AdoTarget) `
        -NowUtc $script:FixedNowUtc

    $script:GitHubDocument = New-PullRequestContextDocument `
        -Context (Get-GitHubPullRequestContext -Transport (New-FakeGitHubTransport) -Target $script:GitHubTarget) `
        -NowUtc $script:FixedNowUtc

    $script:Documents = @(
        [ordered]@{ platform = 'azureDevOps'; document = $script:AdoDocument }
        [ordered]@{ platform = 'gitHub'; document = $script:GitHubDocument }
    )
}

# The two platform adapters exist to make one document shape out of two services, and every stage
# after the capture reads that shape without asking which service produced it. Each adapter's own
# test suite checks it against its own fixtures, which cannot detect a field added, renamed or
# retyped on one platform alone. That divergence has no visible symptom: a consumer selecting on the
# field keeps working on one platform and silently matches nothing on the other.
#
# These tests therefore compare one platform against the other rather than either against a fixture.
Describe 'ss-review canonical pull request shape' {
    Context 'fixture coverage' {
        # The comparisons below are only as strong as what the fixtures contain: a key set compared
        # across two empty collections is equal for the wrong reason.
        It 'captures a text thread, a reference update thread and a reviewer on both platforms' {
            foreach ($entry in $script:Documents) {
                $threads = Get-DocumentThread -Document $entry['document']
                $kinds = @(foreach ($thread in $threads) { [string] (Get-JsonProperty -Node $thread -Name 'kind') })
                ($kinds -ccontains 'text') | Should -BeTrue -Because $entry['platform']
                ($kinds -ccontains 'refUpdate') | Should -BeTrue -Because $entry['platform']

                $comments = Get-DocumentComment -Document $entry['document']
                $comments.Count | Should -BeGreaterThan 0 -Because $entry['platform']

                $reviewers = Get-DocumentReviewer -Document $entry['document']
                $reviewers.Count | Should -BeGreaterThan 0 -Because $entry['platform']

                $contexts = Get-DocumentThreadContext -Document $entry['document']
                $contexts.Count | Should -BeGreaterThan 0 -Because $entry['platform']
            }
        }

        # Only GitHub records a decision at all: the reference update is the one event the capture
        # attaches one to, and Azure DevOps reports that as a thread carrying no decision.
        It 'captures a reference update decision on GitHub' {
            $decisions = Get-DocumentDecision -Document $script:GitHubDocument -Type 'refUpdate'
            $decisions.Count | Should -BeGreaterThan 0
        }
    }

    Context 'key sets' {
        It 'presents the same document keys on both platforms' {
            (Get-KeySignature -Node $script:AdoDocument) |
                Should -BeExactly (Get-KeySignature -Node $script:GitHubDocument)
        }

        It 'presents the same target keys on both platforms' {
            # A GitHub target has no project and no collection URL, but it still carries those keys
            # holding null, so a consumer reads one target shape whichever platform produced it.
            $ado = Get-JsonProperty -Node $script:AdoDocument -Name 'target'
            $gitHub = Get-JsonProperty -Node $script:GitHubDocument -Name 'target'

            (Get-KeySignature -Node $ado) | Should -BeExactly (Get-KeySignature -Node $gitHub)
        }

        It 'presents the same pull request keys on both platforms' {
            $ado = Get-JsonProperty -Node $script:AdoDocument -Name 'pullRequest'
            $gitHub = Get-JsonProperty -Node $script:GitHubDocument -Name 'pullRequest'

            (Get-KeySignature -Node $ado) | Should -BeExactly (Get-KeySignature -Node $gitHub)
        }

        It 'presents the same authenticated identity keys as a comment author' {
            # The reviewer's own identity is compared against comment authors, so it has to be the
            # same record shape rather than a reduced one.
            $comments = Get-DocumentComment -Document $script:AdoDocument
            $author = Get-JsonProperty -Node $comments[0] -Name 'author'
            $self = Get-JsonProperty -Node $script:AdoDocument -Name 'authenticatedIdentity'

            (Get-KeySignature -Node $self) | Should -BeExactly (Get-KeySignature -Node $author)
        }

        It 'presents the same thread keys on both platforms' {
            $ado = Get-DistinctKeySignature -Node (Get-DocumentThread -Document $script:AdoDocument)
            $gitHub = Get-DistinctKeySignature -Node (Get-DocumentThread -Document $script:GitHubDocument)

            $ado.Count | Should -Be 1 -Because 'every Azure DevOps thread carries one key set'
            $gitHub.Count | Should -Be 1 -Because 'every GitHub thread carries one key set, whichever source it came from'
            $ado[0] | Should -BeExactly $gitHub[0]
        }

        It 'presents the same comment keys on both platforms' {
            $ado = Get-DistinctKeySignature -Node (Get-DocumentComment -Document $script:AdoDocument)
            $gitHub = Get-DistinctKeySignature -Node (Get-DocumentComment -Document $script:GitHubDocument)

            $ado.Count | Should -Be 1
            $gitHub.Count | Should -Be 1
            $ado[0] | Should -BeExactly $gitHub[0]
        }

        It 'presents the same reviewer keys on both platforms' {
            $ado = Get-DistinctKeySignature -Node (Get-DocumentReviewer -Document $script:AdoDocument)
            $gitHub = Get-DistinctKeySignature -Node (Get-DocumentReviewer -Document $script:GitHubDocument)

            $ado.Count | Should -Be 1
            $gitHub.Count | Should -Be 1
            $ado[0] | Should -BeExactly $gitHub[0]
        }

        It 'presents the same thread context keys on both platforms' {
            $ado = Get-DistinctKeySignature -Node (Get-DocumentThreadContext -Document $script:AdoDocument)
            $gitHub = Get-DistinctKeySignature -Node (Get-DocumentThreadContext -Document $script:GitHubDocument)

            $ado.Count | Should -Be 1
            $gitHub.Count | Should -Be 1
            $ado[0] | Should -BeExactly $gitHub[0]
        }

        It 'presents the same author keys on both platforms' {
            # The comment collections are assigned before they are iterated: a command that emits a
            # single array emits it as one object, so piping the call would hand the whole array to
            # the first iteration as a single item.
            $adoComments = Get-DocumentComment -Document $script:AdoDocument
            $gitHubComments = Get-DocumentComment -Document $script:GitHubDocument

            $adoAuthors = @(foreach ($comment in $adoComments) { Get-JsonProperty -Node $comment -Name 'author' })
            $gitHubAuthors = @(foreach ($comment in $gitHubComments) { Get-JsonProperty -Node $comment -Name 'author' })

            $ado = Get-DistinctKeySignature -Node $adoAuthors
            $gitHub = Get-DistinctKeySignature -Node $gitHubAuthors

            $ado.Count | Should -Be 1
            $gitHub.Count | Should -Be 1
            $ado[0] | Should -BeExactly $gitHub[0]
        }

        It 'presents one decision key set wherever a decision is recorded' {
            $adoDecisions = Get-DocumentDecision -Document $script:AdoDocument
            $gitHubDecisions = Get-DocumentDecision -Document $script:GitHubDocument
            $all = @($adoDecisions) + @($gitHubDecisions)
            $signatures = Get-DistinctKeySignature -Node $all

            $signatures.Count | Should -Be 1
        }
    }

    Context 'shared vocabulary' {
        It 'reports every thread kind from the shared vocabulary' {
            foreach ($entry in $script:Documents) {
                $threads = Get-DocumentThread -Document $entry['document']
                foreach ($thread in $threads) {
                    $kind = [string] (Get-JsonProperty -Node $thread -Name 'kind')
                    ($script:ThreadKindVocabulary -ccontains $kind) |
                        Should -BeTrue -Because "$($entry['platform']) reported thread kind '$kind'"
                }
            }
        }

        It 'reports every thread resolution from the shared vocabulary' {
            foreach ($entry in $script:Documents) {
                $threads = Get-DocumentThread -Document $entry['document']
                foreach ($thread in $threads) {
                    $resolution = [string] (Get-JsonProperty -Node $thread -Name 'resolution')
                    ($script:ThreadResolutionVocabulary -ccontains $resolution) |
                        Should -BeTrue -Because "$($entry['platform']) reported thread resolution '$resolution'"
                }
            }
        }

        It 'reports every decision type from the shared vocabulary' {
            foreach ($entry in $script:Documents) {
                $decisions = Get-DocumentDecision -Document $entry['document']
                foreach ($decision in $decisions) {
                    $type = [string] (Get-JsonProperty -Node $decision -Name 'type')
                    ($script:DecisionTypeVocabulary -ccontains $type) |
                        Should -BeTrue -Because "$($entry['platform']) reported decision type '$type'"
                }
            }
        }

        It 'reports every identity kind from the shared vocabulary' {
            foreach ($entry in $script:Documents) {
                $comments = Get-DocumentComment -Document $entry['document']
                $identities = @(foreach ($comment in $comments) { Get-JsonProperty -Node $comment -Name 'author' })
                $identities += Get-JsonProperty -Node $entry['document'] -Name 'authenticatedIdentity'
                foreach ($identity in $identities) {
                    $kind = [string] (Get-JsonProperty -Node $identity -Name 'kind')
                    ($script:IdentityKindVocabulary -ccontains $kind) |
                        Should -BeTrue -Because "$($entry['platform']) reported identity kind '$kind'"
                }
            }
        }

        # Lowercase on both platforms: GitHub reports the side in upper case and the adapter is
        # required to fold it, because a consumer filtering on a diff side matches one casing only.
        # The membership test is therefore case-sensitive, as every test in this context is.
        It 'reports every thread context side from the shared vocabulary' {
            foreach ($entry in $script:Documents) {
                $contexts = Get-DocumentThreadContext -Document $entry['document']
                foreach ($context in $contexts) {
                    $side = [string] (Get-JsonProperty -Node $context -Name 'side')
                    ($script:ContextSideVocabulary -ccontains $side) |
                        Should -BeTrue -Because "$($entry['platform']) reported context side '$side'"
                }
            }
        }

        It 'reports every comment authorship from the shared vocabulary' {
            foreach ($entry in $script:Documents) {
                $comments = Get-DocumentComment -Document $entry['document']
                foreach ($comment in $comments) {
                    $authorship = [string] (Get-JsonProperty -Node $comment -Name 'authorship')
                    ($script:AuthorshipVocabulary -ccontains $authorship) |
                        Should -BeTrue -Because "$($entry['platform']) reported comment authorship '$authorship'"
                }
            }
        }
    }

    Context 'platform-specific values' {
        # Merge bookkeeping is the one part of the pull request record that is genuinely
        # platform-specific, so its contents differ. Its type must not: a consumer that reads a
        # member of it would fail on a platform reporting a bare string.
        It 'reports merge state as a dictionary on both platforms' {
            foreach ($entry in $script:Documents) {
                $pullRequest = Get-JsonProperty -Node $entry['document'] -Name 'pullRequest'
                $mergeState = Get-JsonProperty -Node $pullRequest -Name 'mergeState'

                ($null -ne ($mergeState -as [System.Collections.IDictionary])) |
                    Should -BeTrue -Because "$($entry['platform']) reports mergeState as $(if ($null -eq $mergeState) { 'null' } else { $mergeState.GetType().Name })"
                (Get-NodeKeyList -Node $mergeState).Count | Should -BeGreaterThan 0 -Because $entry['platform']
            }
        }

        It 'carries exactly the merge bookkeeping each platform is declared to report' {
            # The contents of the bag are raw and therefore differ, which means the cross-platform
            # key comparison cannot police them. They are pinned per platform instead, so that a
            # field added on one platform alone has to be declared here rather than slipping into
            # the capture unnoticed.
            $expected = [ordered]@{
                azureDevOps = @('status', 'mergeId', 'lastMergeSourceCommit', 'lastMergeTargetCommit', 'lastMergeCommit')
                gitHub      = @('status', 'isMergeable', 'mergeCommit')
            }

            foreach ($entry in $script:Documents) {
                $pullRequest = Get-JsonProperty -Node $entry['document'] -Name 'pullRequest'
                $keys = Get-NodeKeyList -Node (Get-JsonProperty -Node $pullRequest -Name 'mergeState')

                ($keys -join ',') | Should -BeExactly (($expected[$entry['platform']]) -join ',') -Because $entry['platform']
            }
        }
    }

    Context 'excluded from the capture on both platforms' {
        # Approval standing is not what the capture is for, so neither the reviewer's position nor
        # the pull request's own status is recorded, and Azure DevOps vote threads - which hold
        # nothing else - are left out altogether.
        It 'records no pull request state' {
            foreach ($entry in $script:Documents) {
                $pullRequest = Get-JsonProperty -Node $entry['document'] -Name 'pullRequest'
                $keys = Get-NodeKeyList -Node $pullRequest

                ($keys -ccontains 'state') | Should -BeFalse -Because $entry['platform']
                ($keys -ccontains 'platformState') | Should -BeFalse -Because $entry['platform']
            }
        }

        It 'records no lifecycle signal inside merge bookkeeping' {
            # Whether the pull request has merged, or been abandoned, says nothing a reviewer
            # composing comments can act on, and one platform reporting it while the other does not
            # would leave a consumer reading a field that exists on one service alone. The check
            # reaches inside mergeState, because the raw bag is where such a field would appear.
            $lifecycleKeys = @('isMerged', 'merged', 'isClosed', 'closed', 'isAbandoned', 'isActive', 'state')

            foreach ($entry in $script:Documents) {
                $pullRequest = Get-JsonProperty -Node $entry['document'] -Name 'pullRequest'
                $keys = Get-NodeKeyList -Node (Get-JsonProperty -Node $pullRequest -Name 'mergeState')

                foreach ($lifecycleKey in $lifecycleKeys) {
                    ($keys -ccontains $lifecycleKey) |
                        Should -BeFalse -Because "$($entry['platform']) mergeState names '$lifecycleKey'"
                }
            }
        }

        It 'records no reviewer vote' {
            foreach ($entry in $script:Documents) {
                $reviewers = Get-DocumentReviewer -Document $entry['document']
                foreach ($reviewer in $reviewers) {
                    $keys = Get-NodeKeyList -Node $reviewer
                    ($keys -ccontains 'vote') | Should -BeFalse -Because $entry['platform']
                    ($keys -ccontains 'platformVote') | Should -BeFalse -Because $entry['platform']
                    ($keys -ccontains 'voteLabel') | Should -BeFalse -Because $entry['platform']
                }
            }
        }

        It 'records no vote decision' {
            foreach ($entry in $script:Documents) {
                $decisions = Get-DocumentDecision -Document $entry['document'] -Type 'vote'
                $decisions.Count | Should -Be 0 -Because $entry['platform']
            }
        }

        It 'leaves an Azure DevOps vote thread out of the capture' {
            # The fixture serves a VoteUpdate thread, so its absence here is the exclusion working
            # rather than the service having sent nothing.
            $threads = Get-DocumentThread -Document $script:AdoDocument
            $ids = @(foreach ($thread in $threads) { [string] (Get-JsonProperty -Node $thread -Name 'id') })

            $script:AdoThreadsJson.Contains('VoteUpdate') | Should -BeTrue
            ($ids -ccontains '102') | Should -BeFalse
        }
    }

    Context 'thread attribution' {
        # The opener is named on the thread rather than inferred from presentation order, because two
        # comments can share a timestamp and a comment can carry none at all.
        It 'names an opening comment that the thread contains, on both platforms' {
            foreach ($entry in $script:Documents) {
                $threads = Get-DocumentThread -Document $entry['document']
                foreach ($thread in $threads) {
                    # The key is asserted present separately from its value: indexing a dictionary
                    # for an absent key yields null without throwing, so a value assertion alone
                    # would also pass if the field were never emitted.
                    $thread.Contains('openingCommentId') |
                        Should -BeTrue -Because "$($entry['platform']) thread $(Get-JsonProperty -Node $thread -Name 'id') must name its opener"
                    $openingCommentId = [string] (Get-JsonProperty -Node $thread -Name 'openingCommentId')
                    $comments = Get-JsonArrayProperty -Node $thread -Name 'comments'
                    if ($comments.Count -eq 0) {
                        $thread['openingCommentId'] |
                            Should -BeNullOrEmpty -Because "$($entry['platform']) thread $(Get-JsonProperty -Node $thread -Name 'id') carries no comment"
                        continue
                    }

                    $ids = @(foreach ($comment in $comments) { [string] (Get-JsonProperty -Node $comment -Name 'id') })
                    ($ids -ccontains $openingCommentId) |
                        Should -BeTrue -Because "$($entry['platform']) thread $(Get-JsonProperty -Node $thread -Name 'id') names opener '$openingCommentId'"
                }
            }
        }
    }
}
