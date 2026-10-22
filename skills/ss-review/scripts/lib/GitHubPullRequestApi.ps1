#Requires -Version 7.6

# GitHub adapter for pull request capture and reviewer registration
# (docs/design.md, "Captured pull request context"; "Reviewer registration";
# docs/implementation.md, "Verbatim fidelity").
#
# Role in the overall design: this library performs every GitHub-specific read and write the skill
# needs, and presents the result in the platform-neutral canonical shape the review stages consume.
# Everything above it works from that shape alone and never sees a GitHub payload, so a second
# platform adapter can be added without touching the review stages.
#
# The capture is deterministic and complete rather than summarized. Comment bodies are carried
# verbatim, every pagination cursor is followed to exhaustion, and a cursor that cannot be followed
# fails the capture instead of yielding a shorter discussion than the service holds.
#
# Two protocols are used deliberately. REST supplies the pull request record, the reviews, the issue
# comments and the merge base. GraphQL supplies everything REST cannot express: thread resolution
# state, which appears in no REST review comment field, authoritative edit times, and force pushes
# with explicit before and after commits.
#
# This library calls Get-JsonProperty, Get-JsonArrayProperty, Set-JsonProperty, Test-JsonProperty and
# ConvertTo-CanonicalTimestamp from JsonValue.ps1, New-PullRequestIdentity, Get-PullRequestAuthorship
# and Test-PullRequestIdentityMayMatch from PullRequestIdentity.ps1, Get-PullRequestWebUrl from
# PullRequestTarget.ps1, and Invoke-GitHubRestRequest and Invoke-GitHubGraphQlQuery from
# GitHubCliTransport.ps1. Libraries never dot-source one another; the executable composition root
# dot-sources every library it needs before calling into this one.
#
# List-valued payload properties are read through Get-JsonArrayProperty into a variable before being
# iterated, because a command emitting a single array emits it as one object, and writes to a
# materialized node go through Set-JsonProperty, because a name-keyed write through a reference that
# is not typed as IDictionary binds to the integer indexer of OrderedDictionary instead.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# GraphQL connection page size, applied at both levels of a nested request. The service caps a
# connection at 100 nodes per page and bounds the total node count of a query, which a nested request
# multiplies: threads times comments. This size keeps the product of the two levels far below that
# bound while still fetching all but the largest reviews in a single round trip.
$script:GitHubGraphQlPageSize = 50

# Bound on how many pages one connection may contribute, so a service that keeps reporting another
# page cannot spin forever. At the page size above, the node count it admits from a single connection
# is far beyond what any pull request discussion approaches.
$script:GitHubGraphQlMaximumPages = 1000

# The state GitHub reports for a review, or for a comment within one, that its author has not
# submitted: the service returns it only to the account that owns it, so it is that account's own
# unsent draft rather than feedback anyone else can see. It is excluded from the capture, because
# reading a draft back as existing feedback would let a later run treat a point as already raised
# when nobody has raised it.
#
# The same value names both states because the two enumerations agree on it, and both need
# excluding: a draft contributes a summary through its review and line and file threads through its
# comments, and the two arrive by different routes. GitHub publication uses this same
# mechanism (docs/design.md, "Review publication"), so a run that abandons a draft, or a person
# composing one in the web interface, is an ordinary occurrence rather than a remote possibility.
$script:GitHubPendingReviewState = 'PENDING'

$script:GitHubReviewThreadQuery = @'
query($owner:String!,$repo:String!,$num:Int!,$cur:String,$size:Int!){
  repository(owner:$owner,name:$repo){ pullRequest(number:$num){
    reviewThreads(first:$size,after:$cur){
      pageInfo{hasNextPage endCursor}
      nodes{ id isResolved isOutdated isCollapsed path line originalLine startLine originalStartLine
             diffSide startDiffSide subjectType resolvedBy{login}
             comments(first:$size){ pageInfo{hasNextPage endCursor}
               nodes{ id databaseId url body createdAt lastEditedAt replyTo{id} state
                      author{login __typename
                             ... on User{id databaseId url}
                             ... on Bot{id databaseId url}
                             ... on Organization{id databaseId url}
                             ... on Mannequin{id databaseId url}}
                      authorAssociation minimizedReason } } } } } } }
'@

$script:GitHubReviewThreadCommentQuery = @'
query($threadId:ID!,$cur:String,$size:Int!){
  node(id:$threadId){ ... on PullRequestReviewThread {
    comments(first:$size,after:$cur){ pageInfo{hasNextPage endCursor}
      nodes{ id databaseId url body createdAt lastEditedAt replyTo{id} state
             author{login __typename
                    ... on User{id databaseId url}
                    ... on Bot{id databaseId url}
                    ... on Organization{id databaseId url}
                    ... on Mannequin{id databaseId url}}
             authorAssociation minimizedReason } } } } }
'@

$script:GitHubIssueCommentEditQuery = @'
query($owner:String!,$repo:String!,$num:Int!,$cur:String,$size:Int!){
  repository(owner:$owner,name:$repo){ pullRequest(number:$num){
    comments(first:$size,after:$cur){ pageInfo{hasNextPage endCursor}
      nodes{ id lastEditedAt } } } } }
'@

# A submitted review implements the Comment interface, so its edit time is available exactly as an
# issue comment's is. The REST review record carries no edit time at all, so without this a rewritten
# review summary would compare equal to its original and the freshness check that gates publication
# would report no change (docs/design.md, "Freshness check").
#
# The reviews connection is queried separately from the comments connection rather than being added
# to it, because a connection is walked with its own cursor and two connections advanced by one
# cursor variable would page each other's results.
# The comment count distinguishes vote-only reviews from reviews carrying inline feedback without
# inferring review membership from unrelated threads (docs/design.md, "Authorship classification").
$script:GitHubReviewEditQuery = @'
query($owner:String!,$repo:String!,$num:Int!,$cur:String,$size:Int!){
  repository(owner:$owner,name:$repo){ pullRequest(number:$num){
    reviews(first:$size,after:$cur){ pageInfo{hasNextPage endCursor}
      nodes{ id lastEditedAt comments{totalCount} } } } } }
'@

$script:GitHubForcePushQuery = @'
query($owner:String!,$repo:String!,$num:Int!,$cur:String,$size:Int!){
  repository(owner:$owner,name:$repo){ pullRequest(number:$num){
    timelineItems(first:$size,after:$cur,itemTypes:[HEAD_REF_FORCE_PUSHED_EVENT]){
      pageInfo{hasNextPage endCursor}
      nodes{ __typename ... on HeadRefForcePushedEvent {
        id createdAt
        actor{login __typename
              ... on User{id databaseId url}
              ... on Bot{id databaseId url}
              ... on Organization{id databaseId url}
              ... on Mannequin{id databaseId url}}
        beforeCommit{oid} afterCommit{oid} } } } } } }
'@

# --- Publication ---------------------------------------------------------------------------------

# The state a PullRequestReviewComment reports once the review carrying it is visible. The review
# itself is verified as published by absence of the pending state, because its submitted state
# depends on the event used to submit it.
$script:GitHubPublishedReviewCommentState = 'SUBMITTED'

# Bound on the drafts one identity's reviews connection may report. GitHub permits a single
# unsubmitted review per identity, so anything this reads beyond the first contradicts the service's
# own rule; the bound exists to keep a contradicting answer finite rather than to page through
# drafts that cannot exist.
$script:GitHubPendingReviewPageSize = 10

# Page size for the comments the verification reads back from a review. The service caps a
# connection page at 100 nodes, and this request nests no second connection, so it asks for the
# largest page the service will answer with. The connection is walked to its end, so this bounds a
# round trip rather than the number of findings a review may carry.
$script:GitHubReviewCommentPageSize = 100

$script:GitHubPendingReviewQuery = @'
query($owner:String!,$repo:String!,$num:Int!,$login:String!,$size:Int!){
  repository(owner:$owner,name:$repo){ pullRequest(number:$num){
    reviews(first:$size,states:[PENDING],author:$login){ nodes{ id state } } } } }
'@

# A review created without an event is a draft: it exists, it accepts threads, and nobody but its
# author can see any of it until the submit. The commit is stated so every comment in the review is
# anchored to the revision the review was produced against.
$script:GitHubStartReviewMutation = @'
mutation($pullRequestId:ID!,$commitOid:GitObjectID!){
  addPullRequestReview(input:{pullRequestId:$pullRequestId,commitOID:$commitOid}){
    pullRequestReview{ id state } } }
'@

$script:GitHubAddLineThreadMutation = @'
mutation($reviewId:ID!,$path:String!,$line:Int!,$side:DiffSide!,$body:String!){
  addPullRequestReviewThread(input:{pullRequestReviewId:$reviewId,path:$path,line:$line,side:$side,subjectType:LINE,body:$body}){
    thread{ id comments(first:1){ nodes{ id databaseId } } } } }
'@

# A range thread states both ends and is a separate document rather than the single-line one with a
# null start, because a start line equal to the end is not a range the service accepts.
$script:GitHubAddLineRangeThreadMutation = @'
mutation($reviewId:ID!,$path:String!,$startLine:Int!,$line:Int!,$side:DiffSide!,$body:String!){
  addPullRequestReviewThread(input:{pullRequestReviewId:$reviewId,path:$path,startLine:$startLine,line:$line,startSide:$side,side:$side,subjectType:LINE,body:$body}){
    thread{ id comments(first:1){ nodes{ id databaseId } } } } }
'@

# A file thread is the same mutation with no position at all. There is no separate endpoint for one,
# which is why an anchor downgraded to the file needs no separate code path either.
$script:GitHubAddFileThreadMutation = @'
mutation($reviewId:ID!,$path:String!,$body:String!){
  addPullRequestReviewThread(input:{pullRequestReviewId:$reviewId,path:$path,subjectType:FILE,body:$body}){
    thread{ id comments(first:1){ nodes{ id databaseId } } } } }
'@

$script:GitHubAddThreadReplyMutation = @'
mutation($reviewId:ID!,$threadId:ID!,$body:String!){
  addPullRequestReviewThreadReply(input:{pullRequestReviewId:$reviewId,pullRequestReviewThreadId:$threadId,body:$body}){
    comment{ id databaseId } } }
'@

# The submit publishes every thread the draft holds and the summary at once, and the review's own
# body is the summary. The event is chosen at submission: a review that only reports findings is
# submitted as a comment, and a review the user approved the merge on during the audit is submitted
# as an approval (docs/design.md, "Review publication"). A request for changes is never submitted.
$script:GitHubSubmitReviewMutation = @'
mutation($reviewId:ID!,$body:String!,$event:PullRequestReviewEvent!){
  submitPullRequestReview(input:{pullRequestReviewId:$reviewId,event:$event,body:$body}){
    pullRequestReview{ id state url body } } }
'@

# The submission events this skill uses, both values of GitHub's PullRequestReviewEvent enum. COMMENT
# reports the findings without a verdict; APPROVE reports them and also records the reviewer's
# approval of the merge. Ref: docs/design.md, "Review publication".
$script:GitHubCommentReviewEvent = 'COMMENT'
$script:GitHubApproveReviewEvent = 'APPROVE'

$script:GitHubDeleteReviewMutation = @'
mutation($reviewId:ID!){
  deletePullRequestReview(input:{pullRequestReviewId:$reviewId}){ pullRequestReview{ id } } }
'@

# Verification reads the review itself rather than the pull request's threads, because the question
# is what this review holds, and a pull request thread listing answers what the pull request holds.
# The review's own comments answer it for both verification points: a draft's comments are readable
# by the identity that authored them and report PENDING until the submit, so the same read serves the
# check before the review becomes visible and the check after.
#
# The comment node carries the anchor directly. GitHub exposes the diff side on the thread rather
# than on the comment, so a review-scoped read cannot report it and the side check is inert on this
# platform; the anchor is still pinned by path, subject type and line range.
$script:GitHubReviewVerificationQuery = @'
query($reviewId:ID!,$cur:String,$size:Int!){
  node(id:$reviewId){ ... on PullRequestReview {
    id state url body
    comments(first:$size,after:$cur){ pageInfo{hasNextPage endCursor}
      nodes{ id databaseId state path line startLine subjectType body } } } } }
'@

$script:GitHubUnresolveThreadMutation = @'
mutation($threadId:ID!){
  unresolveReviewThread(input:{threadId:$threadId}){ thread{ id isResolved } } }
'@

# Reads a nested value out of a GraphQL response, failing when any step is absent.
#
# A missing step means the response did not have the shape the query asked for, and continuing would
# capture an empty discussion that looks indistinguishable from a pull request with no comments.
function Get-GitHubGraphQlNode {
    param(
        [Parameter(Mandatory)]
        $Response,

        [Parameter(Mandatory)]
        [string[]] $Path
    )

    $current = $Response
    foreach ($name in $Path) {
        if (-not (Test-JsonProperty -Node $current -Name $name)) {
            throw "The GraphQL response is missing '$($Path -join '.')'."
        }
        $current = $current[$name]
    }
    return $current
}

# Walks one GraphQL connection to exhaustion and returns every node it holds.
#
# The page fetcher is supplied by the caller because the cursor lives at a different place in every
# query. A connection that claims another page but hands back no cursor is a failure rather than a
# stopping condition: silently accepting it would truncate the capture, which is the single outcome
# this design exists to prevent.
function Get-GitHubGraphQlConnectionNode {
    param(
        [Parameter(Mandatory)]
        [scriptblock] $FetchPage,

        [Parameter(Mandatory)]
        [string] $Description,

        $FirstPage
    )

    $nodes = [System.Collections.Generic.List[object]]::new()
    $page = $FirstPage
    $pageCount = 0

    while ($true) {
        if ($null -eq $page) {
            $page = & $FetchPage $null
        }
        if ($null -eq $page) {
            throw "The GraphQL connection for $Description returned no page."
        }

        $pageCount++
        if ($pageCount -gt $script:GitHubGraphQlMaximumPages) {
            throw "The GraphQL connection for $Description exceeded $script:GitHubGraphQlMaximumPages pages."
        }

        # Every query here selects both nodes and pageInfo. A page missing either is a malformed
        # response rather than an empty connection, and reading it as empty or as the last page is
        # precisely the silent truncation this walk exists to prevent.
        if (-not (Test-JsonProperty -Node $page -Name 'nodes')) {
            throw "The GraphQL connection for $Description returned a page carrying no nodes field."
        }
        if (-not (Test-JsonProperty -Node $page -Name 'pageInfo')) {
            throw "The GraphQL connection for $Description returned a page carrying no pageInfo field."
        }

        $pageNodes = Get-JsonArrayProperty -Node $page -Name 'nodes'
        foreach ($node in $pageNodes) {
            $nodes.Add($node)
        }

        $pageInfo = Get-JsonProperty -Node $page -Name 'pageInfo'
        if (-not (Test-JsonProperty -Node $pageInfo -Name 'hasNextPage')) {
            throw "The GraphQL connection for $Description returned page information carrying no hasNextPage field."
        }
        $hasNextPage = [bool] (Get-JsonProperty -Node $pageInfo -Name 'hasNextPage' -Default $false)
        if (-not $hasNextPage) {
            break
        }

        $cursor = Get-JsonProperty -Node $pageInfo -Name 'endCursor'
        if ([string]::IsNullOrEmpty([string] $cursor)) {
            throw "The GraphQL connection for $Description reports a further page but supplies no cursor, so the capture would be incomplete."
        }

        $page = & $FetchPage $cursor
    }

    return , $nodes.ToArray()
}

# Builds a canonical identity from a REST account object.
function New-GitHubIdentityFromRestAccount {
    param(
        $Account
    )

    if ($null -eq $Account) {
        return New-PullRequestIdentity -Platform 'gitHub'
    }

    $login = [string] (Get-JsonProperty -Node $Account -Name 'login')
    $id = Get-JsonProperty -Node $Account -Name 'id'

    New-PullRequestIdentity `
        -Platform 'gitHub' `
        -Id ([string] $id) `
        -DisplayName $login `
        -UniqueName $login `
        -Descriptor ([string] (Get-JsonProperty -Node $Account -Name 'node_id')) `
        -AccountType ([string] (Get-JsonProperty -Node $Account -Name 'type')) `
        -Url ([string] (Get-JsonProperty -Node $Account -Name 'html_url'))
}

# Builds a canonical identity from a GraphQL actor.
#
# The numeric databaseId is carried as the identity because it is the same value REST reports, which
# is what lets a GraphQL thread author and a REST review author be recognized as one account. The
# node identifier travels as the descriptor, so an account GitHub types only through its node prefix
# is still classifiable.
function New-GitHubIdentityFromGraphQlActor {
    param(
        $Actor
    )

    if ($null -eq $Actor) {
        return New-PullRequestIdentity -Platform 'gitHub'
    }

    $login = [string] (Get-JsonProperty -Node $Actor -Name 'login')
    $databaseId = Get-JsonProperty -Node $Actor -Name 'databaseId'

    New-PullRequestIdentity `
        -Platform 'gitHub' `
        -Id ([string] $databaseId) `
        -DisplayName $login `
        -UniqueName $login `
        -Descriptor ([string] (Get-JsonProperty -Node $Actor -Name 'id')) `
        -AccountType ([string] (Get-JsonProperty -Node $Actor -Name '__typename')) `
        -Url ([string] (Get-JsonProperty -Node $Actor -Name 'url'))
}

# Builds a canonical identity for a requested team.
#
# Teams are reviewers but not accounts, so they carry the slug as their unique name and an explicit
# Team account type, which is what keeps them out of the account comparisons used for self detection.
function New-GitHubIdentityFromTeam {
    param(
        $Team
    )

    $slug = [string] (Get-JsonProperty -Node $Team -Name 'slug')
    $name = [string] (Get-JsonProperty -Node $Team -Name 'name')
    if ([string]::IsNullOrWhiteSpace($name)) { $name = $slug }

    New-PullRequestIdentity `
        -Platform 'gitHub' `
        -Id ([string] (Get-JsonProperty -Node $Team -Name 'id')) `
        -DisplayName $name `
        -UniqueName $slug `
        -Descriptor ([string] (Get-JsonProperty -Node $Team -Name 'node_id')) `
        -AccountType 'Team' `
        -Url ([string] (Get-JsonProperty -Node $Team -Name 'html_url'))
}

# Converts one GraphQL review comment into a canonical comment.
#
# The edit time comes from lastEditedAt and from nowhere else. REST reports an updated_at that moves
# for reasons unrelated to editing - a comment that was never edited can show a created and updated
# time a second apart - so treating it as evidence of an edit would mark most of a discussion edited.
function ConvertTo-GitHubCanonicalComment {
    param(
        [Parameter(Mandatory)]
        $Comment
    )

    $identity = New-GitHubIdentityFromGraphQlActor -Actor (Get-JsonProperty -Node $Comment -Name 'author')
    $content = [string] (Get-JsonProperty -Node $Comment -Name 'body' -Default '')
    $authorship = Get-PullRequestAuthorship -Identity $identity -Content $content
    $lastEditedAt = Get-JsonProperty -Node $Comment -Name 'lastEditedAt'
    $replyTo = Get-JsonProperty -Node $Comment -Name 'replyTo'

    [ordered]@{
        id                 = Get-JsonProperty -Node $Comment -Name 'id'
        url                = Get-JsonProperty -Node $Comment -Name 'url'
        parentId           = Get-JsonProperty -Node $replyTo -Name 'id'
        author             = $identity
        content            = $content
        publishedUtc       = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $Comment -Name 'createdAt'))
        lastEditedUtc      = ConvertTo-CanonicalTimestamp -Value ([string] $lastEditedAt)
        isEdited           = $null -ne $lastEditedAt
        authorship         = $authorship['authorship']
        authorshipEvidence = @($authorship['evidence'])
    }
}

# Converts one GraphQL review thread and its comments into a canonical thread.
#
# An outdated thread reports a null line with the original line still set, and both are retained:
# the current line is where the thread sits in the pull request now, and the original line is the
# only way to place it in the diff it was written against.
function ConvertTo-GitHubReviewThread {
    param(
        [Parameter(Mandatory)]
        $Node,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Comment
    )

    $comments = @(foreach ($item in $Comment) { ConvertTo-GitHubCanonicalComment -Comment $item })

    $publishedUtc = if ($comments.Count -gt 0) { $comments[0]['publishedUtc'] } else { $null }
    $lastUpdatedUtc = $publishedUtc
    # The loop variable is deliberately not named $comment: a parameter's type constraint follows its
    # variable, so reusing the $Comment name would coerce each dictionary into a one-element array.
    foreach ($canonicalComment in $comments) {
        foreach ($candidate in @($canonicalComment['publishedUtc'], $canonicalComment['lastEditedUtc'])) {
            if ($candidate -and (-not $lastUpdatedUtc -or [string]::CompareOrdinal($candidate, $lastUpdatedUtc) -gt 0)) {
                $lastUpdatedUtc = $candidate
            }
        }
    }

    $isResolved = [bool] (Get-JsonProperty -Node $Node -Name 'isResolved' -Default $false)
    $filePath = Get-JsonProperty -Node $Node -Name 'path'
    $line = Get-JsonProperty -Node $Node -Name 'line'
    $startLine = Get-JsonProperty -Node $Node -Name 'startLine'
    $originalLine = Get-JsonProperty -Node $Node -Name 'originalLine'
    $originalStartLine = Get-JsonProperty -Node $Node -Name 'originalStartLine'

    $context = $null
    if ($filePath) {
        # The diff side is lowercased to the canonical vocabulary the other platform adapter also
        # emits; the service's own casing is kept in the thread properties.
        $diffSide = [string] (Get-JsonProperty -Node $Node -Name 'diffSide')
        $context = [ordered]@{
            filePath          = $filePath
            side              = if ($diffSide) { $diffSide.ToLowerInvariant() } else { $null }
            # A single-line thread reports only the end line, so the start falls back to it rather
            # than being recorded as absent.
            startLine         = if ($null -ne $startLine) { $startLine } else { $line }
            endLine           = $line
            originalStartLine = if ($null -ne $originalStartLine) { $originalStartLine } else { $originalLine }
            originalEndLine   = $originalLine
        }
    }

    $minimized = @(
        foreach ($item in $Comment) {
            if (Test-JsonProperty -Node $item -Name 'minimizedReason') {
                [ordered]@{
                    id     = Get-JsonProperty -Node $item -Name 'id'
                    reason = Get-JsonProperty -Node $item -Name 'minimizedReason'
                }
            }
        }
    )

    # The opener is the comment that replies to nothing, which is how GitHub distinguishes the root
    # of a review thread from the replies beneath it. Naming it here lets the shared model attribute
    # the thread structurally rather than from presentation order
    # (docs/implementation.md, "Canonical document"), which would otherwise hand the thread to a
    # reply whenever the reply sorts ahead of the root. A thread whose root is absent falls back to
    # the first comment the service returned, because the connection preserves posting order.
    $openingCommentId = $null
    foreach ($canonicalComment in $comments) {
        if ($null -eq $canonicalComment['parentId']) {
            $openingCommentId = $canonicalComment['id']
            break
        }
    }
    if ($null -eq $openingCommentId -and $comments.Count -gt 0) { $openingCommentId = $comments[0]['id'] }

    [ordered]@{
        id               = Get-JsonProperty -Node $Node -Name 'id'
        url              = if ($comments.Count -gt 0) { $comments[0]['url'] } else { $null }
        publishedUtc     = $publishedUtc
        lastUpdatedUtc   = $lastUpdatedUtc
        eventUtc         = $publishedUtc
        # GitHub models thread resolution as a boolean rather than as a state name, so the raw status
        # is that boolean rendered as the two values it can take.
        rawStatus        = if ($isResolved) { 'resolved' } else { 'unresolved' }
        resolution       = if ($isResolved) { 'resolved' } else { 'active' }
        kind             = 'text'
        isOutdated       = [bool] (Get-JsonProperty -Node $Node -Name 'isOutdated' -Default $false)
        context          = $context
        anchorKey        = Get-JsonProperty -Node $Node -Name 'id'
        properties       = [ordered]@{
            isCollapsed       = [bool] (Get-JsonProperty -Node $Node -Name 'isCollapsed' -Default $false)
            subjectType       = Get-JsonProperty -Node $Node -Name 'subjectType'
            diffSide          = Get-JsonProperty -Node $Node -Name 'diffSide'
            startDiffSide     = Get-JsonProperty -Node $Node -Name 'startDiffSide'
            resolvedBy        = Get-JsonProperty -Node (Get-JsonProperty -Node $Node -Name 'resolvedBy') -Name 'login'
            minimizedComments = @($minimized)
        }
        openingCommentId = $openingCommentId
        decision         = $null
        comments         = @($comments)
    }
}

# Retrieves every review thread, following the thread cursor and each thread's own comment cursor.
#
# Thread resolution state is obtainable in no other way: a REST review comment carries no resolved,
# minimized or thread identifier field, so GraphQL is the only source for the state that decides
# whether a review finding still stands.
function Get-GitHubReviewThread {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $fetchThreadPage = {
        param($Cursor)

        $response = Invoke-GitHubGraphQlQuery -Transport $Transport -Query $script:GitHubReviewThreadQuery -Variables @{
            owner = $Target['organization']
            repo  = $Target['repository']
            num   = $Target['pullRequestId']
            cur   = $Cursor
            size  = $script:GitHubGraphQlPageSize
        }
        Get-GitHubGraphQlNode -Response $response -Path @('data', 'repository', 'pullRequest', 'reviewThreads')
    }

    $threadNodes = Get-GitHubGraphQlConnectionNode -FetchPage $fetchThreadPage -Description 'review threads'

    $threads = foreach ($node in $threadNodes) {
        $threadId = Get-JsonProperty -Node $node -Name 'id'
        $firstPage = Get-JsonProperty -Node $node -Name 'comments'

        $fetchCommentPage = {
            param($Cursor)

            $response = Invoke-GitHubGraphQlQuery -Transport $Transport -Query $script:GitHubReviewThreadCommentQuery -Variables @{
                threadId = $threadId
                cur      = $Cursor
                size     = $script:GitHubGraphQlPageSize
            }
            Get-GitHubGraphQlNode -Response $response -Path @('data', 'node', 'comments')
        }

        $comments = Get-GitHubGraphQlConnectionNode `
            -FetchPage $fetchCommentPage `
            -Description "comments of thread $threadId" `
            -FirstPage $firstPage

        # A draft comment is dropped here rather than at the point the thread is converted, so that
        # a thread consisting only of drafts disappears entirely instead of being captured with an
        # empty comment list, which every consumer would then have to guard against.
        $submittedComments = @(
            foreach ($comment in $comments) {
                if ([string] (Get-JsonProperty -Node $comment -Name 'state') -eq $script:GitHubPendingReviewState) { continue }
                $comment
            }
        )
        if ($submittedComments.Count -eq 0) { continue }

        ConvertTo-GitHubReviewThread -Node $node -Comment $submittedComments
    }

    return , @($threads)
}

# Retrieves supplemental comment metadata keyed by node identifier.
#
# REST lacks authoritative edit times and review comment counts. Both non-threaded comments and
# submitted reviews use this reader so pagination and case-sensitive identity joins stay consistent.
function Get-GitHubCommentMetadata {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [string] $Query,

        # Name of the connection the query selects under the pull request, which is where the walk
        # reads its pages from.
        [Parameter(Mandatory)]
        [string] $ConnectionName,

        [Parameter(Mandatory)]
        [string] $Description
    )

    $fetchPage = {
        param($Cursor)

        $response = Invoke-GitHubGraphQlQuery -Transport $Transport -Query $Query -Variables @{
            owner = $Target['organization']
            repo  = $Target['repository']
            num   = $Target['pullRequestId']
            cur   = $Cursor
            size  = $script:GitHubGraphQlPageSize
        }
        Get-GitHubGraphQlNode -Response $response -Path @('data', 'repository', 'pullRequest', $ConnectionName)
    }

    # Ordinal keys, because a GitHub node identifier is a case-sensitive opaque string and the
    # default comparer of a PowerShell dictionary ignores case, which would let the edit time of one
    # record be reported against another.
    $metadata = [System.Collections.Specialized.OrderedDictionary]::new([System.StringComparer]::Ordinal)
    # The connection is assigned before it is iterated: a command that emits a single array emits it
    # as one object, so iterating the call directly would hand over the whole array at once.
    $nodes = Get-GitHubGraphQlConnectionNode -FetchPage $fetchPage -Description $Description
    foreach ($node in $nodes) {
        $id = [string] (Get-JsonProperty -Node $node -Name 'id')
        if ($id) {
            Set-JsonProperty -Node $metadata -Name $id -Value $node
        }
    }
    return $metadata
}

# Converts one submitted review into a canonical thread carrying its summary text.
#
# The prose a reviewer writes when submitting a review is feedback like any other comment, and it
# appears nowhere else in the capture: the review comments belong to their own threads, and the
# summary belongs to the review. The verdict the review was submitted with is not recorded, because
# approval standing is the concern of whoever merges the pull request rather than of a reviewer
# composing comments (docs/design.md, "Deterministic discussion capture").
function ConvertTo-GitHubReviewSummaryThread {
    param(
        [Parameter(Mandatory)]
        $Review,

        [AllowNull()]
        $LastEditedAt,

        [AllowNull()]
        $CommentCount
    )

    $identity = New-GitHubIdentityFromRestAccount -Account (Get-JsonProperty -Node $Review -Name 'user')
    $content = [string] (Get-JsonProperty -Node $Review -Name 'body' -Default '')
    $authorship = Get-PullRequestAuthorship -Identity $identity -Content $content

    # Vote-only reviews have no text to classify. Apply the human default only when the service
    # confirms there are no inline comments, and never override proven agent attribution.
    # Ref: docs/design.md, "Authorship classification".
    $state = [string] (Get-JsonProperty -Node $Review -Name 'state')
    if ($authorship['authorship'] -ceq 'indeterminate' -and
        $state -in @('APPROVED', 'CHANGES_REQUESTED', 'DISMISSED') -and
        [string]::IsNullOrWhiteSpace($content)) {
        if (($CommentCount -isnot [int] -and $CommentCount -isnot [long]) -or $CommentCount -lt 0) {
            throw "GitHub review '$((Get-JsonProperty -Node $Review -Name 'node_id'))' is missing a valid comment count."
        }
        if ($CommentCount -eq 0) {
            $authorship['authorship'] = 'human'
            $authorship['evidence'] = @($authorship['evidence']) + 'review:voteOnly'
        }
    }

    $submittedUtc = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $Review -Name 'submitted_at'))
    $lastEditedUtc = ConvertTo-CanonicalTimestamp -Value ([string] $LastEditedAt)
    $nodeId = Get-JsonProperty -Node $Review -Name 'node_id'
    $url = Get-JsonProperty -Node $Review -Name 'html_url'

    $comment = [ordered]@{
        id                 = $nodeId
        url                = $url
        parentId           = $null
        author             = $identity
        content            = $content
        publishedUtc       = $submittedUtc
        lastEditedUtc      = $lastEditedUtc
        isEdited           = $null -ne $LastEditedAt
        authorship         = $authorship['authorship']
        authorshipEvidence = @($authorship['evidence'])
    }

    $lastUpdatedUtc = if ($lastEditedUtc) { $lastEditedUtc } else { $submittedUtc }

    [ordered]@{
        id               = $nodeId
        url              = $url
        publishedUtc     = $submittedUtc
        lastUpdatedUtc   = $lastUpdatedUtc
        eventUtc         = $submittedUtc
        # GitHub records no resolution state for a review summary, so both the raw status and the
        # canonical resolution report the absence rather than inventing an unresolved finding.
        rawStatus        = $null
        resolution       = 'indeterminate'
        # A review summary is prose a reviewer wrote, but it is not a conversation. Its identifier
        # names a review rather than a comment thread, and the service accepts no reply posted into
        # it, so a consumer deciding where to answer must be able to tell it apart from a thread it
        # can reply to without inspecting platform-specific fields. The canonical kind states that
        # directly.
        kind             = 'reviewSummary'
        isOutdated       = $false
        context          = $null
        anchorKey        = $nodeId
        properties       = [ordered]@{
            reviewId          = Get-JsonProperty -Node $Review -Name 'id'
            state             = Get-JsonProperty -Node $Review -Name 'state'
            authorAssociation = Get-JsonProperty -Node $Review -Name 'author_association'
            commitId          = Get-JsonProperty -Node $Review -Name 'commit_id'
        }
        openingCommentId = $nodeId
        decision         = $null
        comments         = @($comment)
    }
}

# Converts one non-threaded comment into a canonical thread.
#
# GitHub records no resolution state for these comments, so the resolution is indeterminate rather
# than active: reporting them as active would invent an unresolved finding the platform never had.
function ConvertTo-GitHubIssueCommentThread {
    param(
        [Parameter(Mandatory)]
        $Comment,

        [AllowNull()]
        $LastEditedAt
    )

    $identity = New-GitHubIdentityFromRestAccount -Account (Get-JsonProperty -Node $Comment -Name 'user')
    $content = [string] (Get-JsonProperty -Node $Comment -Name 'body' -Default '')
    $authorship = Get-PullRequestAuthorship -Identity $identity -Content $content
    $nodeId = Get-JsonProperty -Node $Comment -Name 'node_id'
    $url = Get-JsonProperty -Node $Comment -Name 'html_url'
    $publishedUtc = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $Comment -Name 'created_at'))
    $lastEditedUtc = ConvertTo-CanonicalTimestamp -Value ([string] $LastEditedAt)

    $canonicalComment = [ordered]@{
        id                 = $nodeId
        url                = $url
        parentId           = $null
        author             = $identity
        content            = $content
        publishedUtc       = $publishedUtc
        lastEditedUtc      = $lastEditedUtc
        isEdited           = $null -ne $LastEditedAt
        authorship         = $authorship['authorship']
        authorshipEvidence = @($authorship['evidence'])
    }

    $lastUpdatedUtc = if ($lastEditedUtc) { $lastEditedUtc } else { $publishedUtc }

    [ordered]@{
        id               = $nodeId
        url              = $url
        publishedUtc     = $publishedUtc
        lastUpdatedUtc   = $lastUpdatedUtc
        eventUtc         = $publishedUtc
        rawStatus        = $null
        resolution       = 'indeterminate'
        kind             = 'text'
        isOutdated       = $false
        context          = $null
        anchorKey        = $nodeId
        properties       = [ordered]@{
            commentId         = Get-JsonProperty -Node $Comment -Name 'id'
            authorAssociation = Get-JsonProperty -Node $Comment -Name 'author_association'
            isMinimized       = [bool] (Get-JsonProperty -Node $Comment -Name 'minimized' -Default $false)
        }
        openingCommentId = $nodeId
        decision         = $null
        comments         = @($canonicalComment)
    }
}

# Retrieves force pushes as canonical threads.
#
# The GraphQL timeline event states the commit before and after the push outright, while the REST
# timeline leaves which commit is which to inference. A force push replaces the reviewed content, so
# recording it exactly is what lets a later review tell a rewritten branch from a new commit.
function Get-GitHubForcePushThread {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $fetchPage = {
        param($Cursor)

        $response = Invoke-GitHubGraphQlQuery -Transport $Transport -Query $script:GitHubForcePushQuery -Variables @{
            owner = $Target['organization']
            repo  = $Target['repository']
            num   = $Target['pullRequestId']
            cur   = $Cursor
            size  = $script:GitHubGraphQlPageSize
        }
        Get-GitHubGraphQlNode -Response $response -Path @('data', 'repository', 'pullRequest', 'timelineItems')
    }

    # The connection is assigned before it is iterated: a command that emits a single array emits it
    # as one object, so iterating the call directly would hand over the whole array at once.
    $eventNodes = Get-GitHubGraphQlConnectionNode -FetchPage $fetchPage -Description 'force push events'
    $threads = foreach ($node in $eventNodes) {
        $identity = New-GitHubIdentityFromGraphQlActor -Actor (Get-JsonProperty -Node $node -Name 'actor')
        $authorship = Get-PullRequestAuthorship -Identity $identity -Content ''
        $createdUtc = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $node -Name 'createdAt'))
        $beforeCommit = Get-JsonProperty -Node (Get-JsonProperty -Node $node -Name 'beforeCommit') -Name 'oid'
        $afterCommit = Get-JsonProperty -Node (Get-JsonProperty -Node $node -Name 'afterCommit') -Name 'oid'
        $id = Get-JsonProperty -Node $node -Name 'id'

        [ordered]@{
            id               = $id
            url              = $null
            publishedUtc     = $createdUtc
            lastUpdatedUtc   = $createdUtc
            eventUtc         = $createdUtc
            rawStatus        = Get-JsonProperty -Node $node -Name '__typename'
            resolution       = 'indeterminate'
            kind             = 'refUpdate'
            isOutdated       = $false
            context          = $null
            anchorKey        = $id
            properties       = [ordered]@{
                beforeCommit = $beforeCommit
                afterCommit  = $afterCommit
            }
            # A force push carries no comment, so the thread names no opener and the shared model
            # attributes it from the decision instead.
            openingCommentId = $null
            decision         = [ordered]@{
                type               = 'refUpdate'
                value              = $afterCommit
                label              = 'force pushed'
                by                 = $identity
                utc                = $createdUtc
                authorship         = $authorship['authorship']
                authorshipEvidence = @($authorship['evidence'])
            }
            comments         = @()
        }
    }

    return , @($threads)
}

# Resolves the commit the reviewed branch diverged from.
#
# The compare endpoint is addressed by commit rather than by branch name because a branch is deleted
# when a pull request closes, while the commits stay reachable. A head in a fork is addressed with
# its owner prefix, which is how GitHub expresses a cross-repository comparison; the unqualified
# form follows as a fallback because forks share a commit network with their upstream. When neither
# resolves - a deleted fork, most commonly - the merge base is reported as absent, since losing the
# whole capture over a convenience anchor would be the worse outcome.
function Get-GitHubMergeBaseCommit {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        $PullRequest
    )

    $base = Get-JsonProperty -Node $PullRequest -Name 'base'
    $head = Get-JsonProperty -Node $PullRequest -Name 'head'
    $baseSha = [string] (Get-JsonProperty -Node $base -Name 'sha')
    $headSha = [string] (Get-JsonProperty -Node $head -Name 'sha')
    if (-not $baseSha -or -not $headSha) {
        return $null
    }

    $baseRepository = Get-JsonProperty -Node (Get-JsonProperty -Node $base -Name 'repo') -Name 'full_name'
    $headRepositoryNode = Get-JsonProperty -Node $head -Name 'repo'
    $headRepository = Get-JsonProperty -Node $headRepositoryNode -Name 'full_name'

    $specifications = @()
    if ($headRepository -and $headRepository -ne $baseRepository) {
        $headOwner = [string] (Get-JsonProperty -Node (Get-JsonProperty -Node $headRepositoryNode -Name 'owner') -Name 'login')
        if ($headOwner) {
            $specifications += "$baseSha...${headOwner}:$headSha"
        }
    }
    $specifications += "$baseSha...$headSha"

    $owner = [uri]::EscapeDataString($Target['organization'])
    $repository = [uri]::EscapeDataString($Target['repository'])

    foreach ($specification in $specifications) {
        try {
            $comparison = Invoke-GitHubRestRequest `
                -Transport $Transport `
                -Path "repos/$owner/$repository/compare/$specification"
            return Get-JsonProperty -Node (Get-JsonProperty -Node $comparison -Name 'merge_base_commit') -Name 'sha'
        } catch {
            # Only an unresolvable comparison justifies trying the next address form. Anything else -
            # a rate limit that outlasted its retries, a transport fault - must surface.
            if ($_.Exception.Message -notmatch '(?i)HTTP 404|not found') {
                throw
            }
        }
    }

    return $null
}

# Builds the reviewer roster from submitted reviews and outstanding review requests.
#
# The roster is who has looked at the pull request or has been asked to. What position each of them
# holds is not recorded: approval standing belongs to whoever merges the pull request rather than to
# a reviewer composing comments (docs/design.md, "Deterministic discussion capture").
#
# The caller passes reviews that have already had unsubmitted drafts removed. An account holding
# nothing but a draft has not reviewed anything, and listing it here would report the running
# account's own unsent work as a review it had performed.
function Get-GitHubReviewerSummary {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Review,

        $RequestedReviewers
    )

    $order = [System.Collections.Generic.List[string]]::new()
    $byKey = [ordered]@{}

    # The loop variable is deliberately not named $review: a parameter's type constraint follows its
    # variable, so reusing the $Review name would coerce each record into a one-element array.
    foreach ($record in $Review) {
        $identity = New-GitHubIdentityFromRestAccount -Account (Get-JsonProperty -Node $record -Name 'user')
        $key = [string] $identity['id']
        if (-not $key) { $key = [string] $identity['uniqueName'] }
        if (-not $key) { continue }
        if ($byKey.Contains($key)) { continue }

        $order.Add($key)
        Set-JsonProperty -Node $byKey -Name $key -Value ([ordered]@{ identity = $identity; isTeam = $false })
    }

    $requestedAccounts = Get-JsonArrayProperty -Node $RequestedReviewers -Name 'users'
    foreach ($account in $requestedAccounts) {
        $identity = New-GitHubIdentityFromRestAccount -Account $account
        $key = [string] $identity['id']
        if (-not $key) { $key = [string] $identity['uniqueName'] }
        if (-not $key) { continue }
        if ($byKey.Contains($key)) { continue }
        $order.Add($key)
        Set-JsonProperty -Node $byKey -Name $key -Value ([ordered]@{ identity = $identity; isTeam = $false })
    }

    $requestedTeams = Get-JsonArrayProperty -Node $RequestedReviewers -Name 'teams'
    foreach ($team in $requestedTeams) {
        $identity = New-GitHubIdentityFromTeam -Team $team
        $key = "team:$([string] $identity['uniqueName'])"
        if ($byKey.Contains($key)) { continue }
        $order.Add($key)
        Set-JsonProperty -Node $byKey -Name $key -Value ([ordered]@{ identity = $identity; isTeam = $true })
    }

    $reviewers = foreach ($key in $order) {
        $entry = Get-JsonProperty -Node $byKey -Name $key
        [ordered]@{
            identity   = Get-JsonProperty -Node $entry -Name 'identity'
            # GitHub reports required reviewers only through branch protection, which is a repository
            # setting rather than a property of the pull request, so no reviewer is marked required.
            isRequired = $false
            isTeam     = Get-JsonProperty -Node $entry -Name 'isTeam'
        }
    }

    return , @($reviewers)
}

# Returns the identity of the account the run is authenticated as.
#
# The numeric account identifier is the join key everywhere else in the capture. A login is not,
# because it can be renamed and because the same account is reported under different logins by
# different endpoints.
function Get-GitHubAuthenticatedIdentity {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport
    )

    $account = Invoke-GitHubRestRequest -Transport $Transport -Path 'user'
    New-GitHubIdentityFromRestAccount -Account $account
}


# Completes the target a caller named into the form the adapter acts on.
#
# GitHub addresses a pull request by owner, repository and number, all of which a caller has to
# supply, so completing a target costs no round trip. The adapter still resolves rather than trusting
# what it was handed, because the registration reports the target it acted on and that report has to
# state the true location of the write whichever platform served it.
#
# The browser URL comes from the target library, which owns the mapping from coordinates to browser
# address for every platform. A second derivation here would be free to drift from the one every
# other caller sees, and the symptom of that drift is a reported link that opens nothing.
function Resolve-GitHubPullRequestTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $organization = [string] (Get-JsonProperty -Node $Target -Name 'organization')
    $repository = [string] (Get-JsonProperty -Node $Target -Name 'repository')
    $pullRequestId = Get-JsonProperty -Node $Target -Name 'pullRequestId'

    if ([string]::IsNullOrWhiteSpace($organization) -or [string]::IsNullOrWhiteSpace($repository)) {
        throw 'A GitHub target requires an organization and a repository.'
    }
    if ($null -eq $pullRequestId -or [int] $pullRequestId -le 0) {
        throw 'A GitHub target requires a positive pull request number.'
    }

    $resolved = [ordered]@{
        platform      = 'gitHub'
        # Azure DevOps addresses a pull request inside a project inside a collection and GitHub has
        # neither, so both keys are carried holding null and a consumer reads one target shape
        # whichever platform produced it.
        collectionUrl = $null
        organization  = $organization
        project       = $null
        repository    = $repository
        pullRequestId = [int] $pullRequestId
        webUrl        = $null
    }
    Set-JsonProperty -Node $resolved -Name 'webUrl' -Value (Get-PullRequestWebUrl -Target $resolved)
    return $resolved
}


# Captures the complete pull request discussion in canonical form.
#
# Threads come from sources that never overlap: a review comment belongs to its GraphQL thread
# and is deliberately not repeated under the review that carried it, a review contributes only its
# own summary text, and a non-threaded comment contributes itself. Counting a review comment twice
# would inflate every downstream measure of how much of a review is unaddressed.
#
# The result is ordered by event time so the capture reads as the discussion happened, regardless of
# which source each thread came from.
function Get-GitHubPullRequestContext {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $owner = [uri]::EscapeDataString($Target['organization'])
    $repository = [uri]::EscapeDataString($Target['repository'])
    $number = $Target['pullRequestId']
    $pullRequestPath = "repos/$owner/$repository/pulls/$number"

    $pullRequest = Invoke-GitHubRestRequest -Transport $Transport -Path $pullRequestPath
    # Every array-returning call is assigned rather than wrapped in @( ): a command that emits a
    # single array emits it as one object, so wrapping would produce an array holding the array.
    $reviews = Invoke-GitHubRestRequest -Transport $Transport -Path "$pullRequestPath/reviews" -Paginate
    # A pending review has not been submitted. GitHub returns it only to the account that owns it,
    # which is the account this run authenticates as, so admitting it would read the run's own unsent
    # draft back as feedback that had been given. The filter is applied once, at the point the list
    # enters the adapter, so that every consumer below - the summary threads and the reviewer roster
    # alike - sees only submitted reviews.
    $submittedReviews = @(
        foreach ($review in $reviews) {
            if ([string] (Get-JsonProperty -Node $review -Name 'state') -eq $script:GitHubPendingReviewState) { continue }
            $review
        }
    )
    $issueComments = Invoke-GitHubRestRequest -Transport $Transport -Path "repos/$owner/$repository/issues/$number/comments" -Paginate
    $requestedReviewers = Invoke-GitHubRestRequest -Transport $Transport -Path "$pullRequestPath/requested_reviewers"

    $reviewThreads = Get-GitHubReviewThread -Transport $Transport -Target $Target
    $issueCommentMetadata = Get-GitHubCommentMetadata `
        -Transport $Transport `
        -Target $Target `
        -Query $script:GitHubIssueCommentEditQuery `
        -ConnectionName 'comments' `
        -Description 'issue comments'
    $reviewMetadata = Get-GitHubCommentMetadata `
        -Transport $Transport `
        -Target $Target `
        -Query $script:GitHubReviewEditQuery `
        -ConnectionName 'reviews' `
        -Description 'reviews'
    $forcePushThreads = Get-GitHubForcePushThread -Transport $Transport -Target $Target

    $reviewSummaryThreads = @(
        foreach ($review in $submittedReviews) {
            $state = [string] (Get-JsonProperty -Node $review -Name 'state')
            $body = [string] (Get-JsonProperty -Node $review -Name 'body' -Default '')
            # A commenting review with no text of its own is only the envelope that carried review
            # comments, and those comments are already captured as threads.
            if ($state -eq 'COMMENTED' -and [string]::IsNullOrWhiteSpace($body)) { continue }
            $nodeId = [string] (Get-JsonProperty -Node $review -Name 'node_id')
            $metadata = if ($nodeId -and $reviewMetadata.Contains($nodeId)) { $reviewMetadata[$nodeId] } else { $null }
            $lastEditedAt = Get-JsonProperty -Node $metadata -Name 'lastEditedAt'
            $comments = Get-JsonProperty -Node $metadata -Name 'comments'
            $commentCount = Get-JsonProperty -Node $comments -Name 'totalCount'
            ConvertTo-GitHubReviewSummaryThread -Review $review -LastEditedAt $lastEditedAt -CommentCount $commentCount
        }
    )

    $issueCommentThreads = @(
        foreach ($comment in $issueComments) {
            $nodeId = [string] (Get-JsonProperty -Node $comment -Name 'node_id')
            $metadata = if ($nodeId -and $issueCommentMetadata.Contains($nodeId)) { $issueCommentMetadata[$nodeId] } else { $null }
            $lastEditedAt = Get-JsonProperty -Node $metadata -Name 'lastEditedAt'
            ConvertTo-GitHubIssueCommentThread -Comment $comment -LastEditedAt $lastEditedAt
        }
    )

    # The threads of every source are concatenated and left in that order. Chronological ordering
    # is applied by the shared model once every adapter has returned, and sorting here would also put
    # each dictionary through Sort-Object, which wraps its output.
    $threads = @($reviewThreads) + @($reviewSummaryThreads) + @($issueCommentThreads) + @($forcePushThreads)

    $head = Get-JsonProperty -Node $pullRequest -Name 'head'
    $base = Get-JsonProperty -Node $pullRequest -Name 'base'
    $headRepository = Get-JsonProperty -Node (Get-JsonProperty -Node $head -Name 'repo') -Name 'full_name'
    $baseRepository = Get-JsonProperty -Node (Get-JsonProperty -Node $base -Name 'repo') -Name 'full_name'
    # A fork branch name alone is ambiguous, so a head outside the pull request's repository is
    # reported in the owner-qualified form GitHub labels it with.
    $sourceRef = if ($headRepository -and $headRepository -ne $baseRepository) {
        Get-JsonProperty -Node $head -Name 'label'
    } else {
        Get-JsonProperty -Node $head -Name 'ref'
    }

    $reviewerSummary = Get-GitHubReviewerSummary -Review $submittedReviews -RequestedReviewers $requestedReviewers

    [ordered]@{
        target                = $Target
        authenticatedIdentity = Get-GitHubAuthenticatedIdentity -Transport $Transport
        pullRequest           = [ordered]@{
            id                      = Get-JsonProperty -Node $pullRequest -Name 'number'
            title                   = Get-JsonProperty -Node $pullRequest -Name 'title'
            description             = Get-JsonProperty -Node $pullRequest -Name 'body' -Default ''
            isDraft                 = [bool] (Get-JsonProperty -Node $pullRequest -Name 'draft' -Default $false)
            author                  = New-GitHubIdentityFromRestAccount -Account (Get-JsonProperty -Node $pullRequest -Name 'user')
            createdUtc              = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $pullRequest -Name 'created_at'))
            sourceRef               = $sourceRef
            targetRef               = Get-JsonProperty -Node $base -Name 'ref'
            reviewedSourceCommit    = Get-JsonProperty -Node $head -Name 'sha'
            reviewedMergeBaseCommit = Get-GitHubMergeBaseCommit -Transport $Transport -Target $Target -PullRequest $pullRequest
            # Raw platform merge bookkeeping, retained unmodified and presented as a dictionary so
            # that both platform adapters report merge state as the same type. It carries conflict
            # bookkeeping only: whether the pull request has merged, like whether it has been
            # approved, is the concern of whoever merges it rather than of a reviewer composing
            # comments (docs/design.md, "Deterministic discussion capture").
            mergeState              = [ordered]@{
                status      = Get-JsonProperty -Node $pullRequest -Name 'mergeable_state'
                isMergeable = Get-JsonProperty -Node $pullRequest -Name 'mergeable'
                mergeCommit = Get-JsonProperty -Node $pullRequest -Name 'merge_commit_sha'
            }
        }
        reviewers             = @($reviewerSummary)
        # GitHub keeps no per-iteration record of a pull request. A rewritten branch appears as a
        # force push thread instead, which carries the commits the rewrite replaced.
        revisions             = @()
        threads               = @($threads)
    }
}

# --- Publication adapter -------------------------------------------------------------------------
#
# GitHub publication is assembled before it is performed. Every thread is added to an unsubmitted
# review, which nobody but its author can see, and the submit makes all of it visible in one act.
# A run that fails anywhere before the submit therefore leaves the pull request exactly as it found
# it, and the failed draft is discarded so the next run does not inherit it. That is not a
# transaction - the submit itself can still fail with part of a review recorded - but it removes
# every failure mode before the submit from the set of outcomes a reader of the pull request can
# observe.

# Reports the live state of the pull request publication is about to write to.
#
# The captured context deliberately records no open/closed status (docs/design.md, "Deterministic
# discussion capture"), and a capture is in any case older than the run that publishes from it, so
# the decision to write is taken against the service rather than against the file.
function Get-GitHubPullRequestPublicationState {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $resolved = Resolve-GitHubPullRequestTarget -Target $Target
    $owner = [uri]::EscapeDataString($resolved['organization'])
    $repository = [uri]::EscapeDataString($resolved['repository'])
    $number = $resolved['pullRequestId']

    $pullRequest = Invoke-GitHubRestRequest `
        -Transport $Transport `
        -Path "repos/$owner/$repository/pulls/$number"

    $state = [string] (Get-JsonProperty -Node $pullRequest -Name 'state')
    $isMerged = [bool] (Get-JsonProperty -Node $pullRequest -Name 'merged' -Default $false)
    $head = Get-JsonProperty -Node $pullRequest -Name 'head'

    # The draft belongs in the state rather than in a separate read, because whether one exists is
    # one of the conditions that decides whether the run may begin, and the identity that owns it is
    # the one this run would publish as.
    $identity = Get-GitHubAuthenticatedIdentity -Transport $Transport
    $login = [string] $identity['uniqueName']
    $pendingReviewId = $null
    if ($login) {
        $pendingReviewId = Get-GitHubPendingReviewId -Transport $Transport -Target $resolved -Login $login
    }

    # A draft pull request counts as active. GitHub accepts review comments on one, and a reviewer
    # who was asked to look at work in progress is entitled to answer.
    return [ordered]@{
        platform          = 'gitHub'
        pullRequestNodeId = [string] (Get-JsonProperty -Node $pullRequest -Name 'node_id')
        url               = [string] (Get-JsonProperty -Node $pullRequest -Name 'html_url')
        status            = $state
        isActive          = ($state -eq 'open') -and (-not $isMerged)
        isDraft           = [bool] (Get-JsonProperty -Node $pullRequest -Name 'draft' -Default $false)
        sourceCommit      = [string] (Get-JsonProperty -Node $head -Name 'sha')
        mergeBaseCommit   = [string] (Get-GitHubMergeBaseCommit -Transport $Transport -Target $resolved -PullRequest $pullRequest)
        identity          = $login
        pendingReviewId   = $pendingReviewId
    }
}

# Reports how GitHub identifies each file the review will comment on.
#
# GitHub addresses a file by its path in the head revision and needs nothing further, so the answer
# is empty. The function exists so the caller can ask both platforms the same question.
function Get-GitHubPullRequestFileIdentity {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]] $Path
    )

    return [ordered]@{}
}

# Reports the identifier of the unsubmitted review the given account already holds, if any.
#
# GitHub permits one unsubmitted review per identity, so an existing draft blocks the run. It may
# well be work the user started in the web interface, which is why discarding it is never implicit.
function Get-GitHubPendingReviewId {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [string] $Login
    )

    $resolved = Resolve-GitHubPullRequestTarget -Target $Target
    $response = Invoke-GitHubGraphQlQuery `
        -Transport $Transport `
        -Query $script:GitHubPendingReviewQuery `
        -Variables ([ordered]@{
            owner = $resolved['organization']
            repo  = $resolved['repository']
            num   = [int] $resolved['pullRequestId']
            login = $Login
            size  = $script:GitHubPendingReviewPageSize
        })

    $node = Get-GitHubGraphQlNode -Response $response -Path @('data', 'repository', 'pullRequest', 'reviews')
    $nodes = Get-JsonArrayProperty -Node $node -Name 'nodes'
    foreach ($review in $nodes) {
        $id = [string] (Get-JsonProperty -Node $review -Name 'id')
        if ($id) { return $id }
    }

    return $null
}

# Discards an unsubmitted review.
#
# Used both to clear a draft the caller elected to discard and to clean up a draft this run failed
# to finish. Nothing in a discarded draft was ever visible, so the discard removes work in progress
# rather than published comments.
function Remove-GitHubPendingReview {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [string] $ReviewId
    )

    Invoke-GitHubGraphQlQuery `
        -Transport $Transport `
        -Query $script:GitHubDeleteReviewMutation `
        -Variables ([ordered]@{ reviewId = $ReviewId }) `
        -NoRetry | Out-Null
}

# Opens the unsubmitted review every finding in the run is added to.
function Start-GitHubPullRequestReview {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State
    )

    $response = Invoke-GitHubGraphQlQuery `
        -Transport $Transport `
        -Query $script:GitHubStartReviewMutation `
        -Variables ([ordered]@{
            pullRequestId = [string] $State['pullRequestNodeId']
            commitOid     = [string] $State['sourceCommit']
        }) `
        -NoRetry

    $review = Get-GitHubGraphQlNode -Response $response -Path @('data', 'addPullRequestReview', 'pullRequestReview')
    $id = [string] (Get-JsonProperty -Node $review -Name 'id')
    if (-not $id) {
        throw 'GitHub accepted the request to open a review but reported no review identifier, so the run cannot tell which review it would be adding comments to.'
    }

    return [ordered]@{
        reviewId = $id
        state    = [string] (Get-JsonProperty -Node $review -Name 'state')
    }
}

# Adds one rendered finding to the unsubmitted review.
#
# Line-thread creation, file-thread creation and captured-thread replies differ only in which
# mutation carries the body. The body arrives already rendered, so this never decides how a finding
# reads.
function Add-GitHubPullRequestReviewFinding {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Operation
    )

    $reviewId = [string] $Review['reviewId']
    $body = [string] (Get-JsonProperty -Node $Operation -Name 'body')

    # Named apart from the $Operation parameter: PowerShell resolves variable names without regard to
    # case, so a local named $operation would rebind the typed parameter and fail on assignment.
    $operationName = [string] (Get-JsonProperty -Node $Operation -Name 'operation')

    switch ($operationName) {
        'reply' {
            $threadId = [string] (Get-JsonProperty -Node $Operation -Name 'threadId')
            $response = Invoke-GitHubGraphQlQuery `
                -Transport $Transport `
                -Query $script:GitHubAddThreadReplyMutation `
                -Variables ([ordered]@{ reviewId = $reviewId; threadId = $threadId; body = $body }) `
                -NoRetry

            $comment = Get-GitHubGraphQlNode -Response $response -Path @('data', 'addPullRequestReviewThreadReply', 'comment')
            return [ordered]@{
                threadId  = $threadId
                commentId = [string] (Get-JsonProperty -Node $comment -Name 'id')
            }
        }

        'newFileThread' {
            $fileAnchor = Get-JsonProperty -Node $Operation -Name 'anchor'
            $response = Invoke-GitHubGraphQlQuery `
                -Transport $Transport `
                -Query $script:GitHubAddFileThreadMutation `
                -Variables ([ordered]@{
                    reviewId = $reviewId
                    path     = [string] (Get-JsonProperty -Node $fileAnchor -Name 'path')
                    body     = $body
                }) `
                -NoRetry

            return Get-GitHubReviewThreadResult -Response $response
        }

        'newLineThread' {
            $anchor = Get-JsonProperty -Node $Operation -Name 'anchor'
            $endLine = [int] (Get-JsonProperty -Node $anchor -Name 'endLine')
            $startLine = [int] (Get-JsonProperty -Node $anchor -Name 'startLine' -Default $endLine)

            # GitHub names the two halves of a diff by side, and an anchor states the side the
            # finding was judged against rather than assuming the head revision.
            $side = ([string] (Get-JsonProperty -Node $anchor -Name 'side' -Default 'right')).ToUpperInvariant()

            $variables = [ordered]@{
                reviewId = $reviewId
                path     = [string] (Get-JsonProperty -Node $anchor -Name 'path')
                line     = $endLine
                side     = $side
                body     = $body
            }

            if ($startLine -gt 0 -and $startLine -lt $endLine) {
                $variables['startLine'] = $startLine
                $response = Invoke-GitHubGraphQlQuery `
                    -Transport $Transport `
                    -Query $script:GitHubAddLineRangeThreadMutation `
                    -Variables $variables `
                    -NoRetry
            } else {
                $response = Invoke-GitHubGraphQlQuery `
                    -Transport $Transport `
                    -Query $script:GitHubAddLineThreadMutation `
                    -Variables $variables `
                    -NoRetry
            }

            return Get-GitHubReviewThreadResult -Response $response
        }

        default { throw "The publication operation '$operationName' is not one GitHub knows how to perform." }
    }
}

# Reads the identifiers out of a thread creation response.
function Get-GitHubReviewThreadResult {
    param(
        [Parameter(Mandatory)]
        $Response
    )

    $thread = Get-GitHubGraphQlNode -Response $Response -Path @('data', 'addPullRequestReviewThread', 'thread')
    $threadId = [string] (Get-JsonProperty -Node $thread -Name 'id')
    if (-not $threadId) {
        throw 'GitHub accepted the request to add a review thread but reported no thread identifier, so the run cannot record where the comment landed.'
    }

    $commentId = ''
    $comments = Get-JsonProperty -Node $thread -Name 'comments'
    if ($null -ne $comments) {
        $nodes = Get-JsonArrayProperty -Node $comments -Name 'nodes'
        if ($nodes.Count -gt 0) {
            $commentId = [string] (Get-JsonProperty -Node $nodes[0] -Name 'id')
        }
    }

    return [ordered]@{
        threadId  = $threadId
        commentId = $commentId
    }
}

# Submits the review, publishing every thread it holds and the summary together.
#
# The submission event states the verdict: an approved merge is submitted as an approval, and a
# review that only reports findings is submitted as a comment (docs/design.md, "Review publication").
function Complete-GitHubPullRequestReview {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $SummaryBody,

        [switch] $ApproveMerge
    )

    $event = if ($ApproveMerge) { $script:GitHubApproveReviewEvent } else { $script:GitHubCommentReviewEvent }

    $response = Invoke-GitHubGraphQlQuery `
        -Transport $Transport `
        -Query $script:GitHubSubmitReviewMutation `
        -Variables ([ordered]@{ reviewId = [string] $Review['reviewId']; body = $SummaryBody; event = $event }) `
        -NoRetry

    $submitted = Get-GitHubGraphQlNode -Response $response -Path @('data', 'submitPullRequestReview', 'pullRequestReview')

    return [ordered]@{
        reviewId = [string] (Get-JsonProperty -Node $submitted -Name 'id')
        state    = [string] (Get-JsonProperty -Node $submitted -Name 'state')
        url      = [string] (Get-JsonProperty -Node $submitted -Name 'url')
    }
}

# Reads back the findings while the review is still pending.
#
# A draft review's comments report PENDING, which is the property that keeps them invisible. The
# publication layer treats that as present-but-not-published so it can verify the draft without
# requiring visibility before the submit.
function Confirm-GitHubPullRequestReviewFindings {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review
    )

    Confirm-GitHubPullRequestReviewCore -Transport $Transport -Review $Review
}

# Reads back what the review actually published.
#
# The read addresses the review node directly, so the answer describes this run's review rather than
# whatever else the pull request accumulated. Each comment reports whether it is visible, which lets
# the platform-neutral check count published comments without knowing GitHub's vocabulary.
function Confirm-GitHubPullRequestReview {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review
    )

    Confirm-GitHubPullRequestReviewCore -Transport $Transport -Review $Review
}

# Reads back this review's comments from the review node.
function Confirm-GitHubPullRequestReviewCore {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review
    )

    $reviewId = [string] $Review['reviewId']

    $fetchCommentPage = {
        param($Cursor)

        $response = Invoke-GitHubGraphQlQuery `
            -Transport $Transport `
            -Query $script:GitHubReviewVerificationQuery `
            -Variables ([ordered]@{
                reviewId = $reviewId
                cur      = $Cursor
                size     = $script:GitHubReviewCommentPageSize
            })

        Get-GitHubGraphQlNode -Response $response -Path @('data', 'node', 'comments')
    }

    $response = Invoke-GitHubGraphQlQuery `
        -Transport $Transport `
        -Query $script:GitHubReviewVerificationQuery `
        -Variables ([ordered]@{
            reviewId = $reviewId
            cur      = $null
            size     = $script:GitHubReviewCommentPageSize
        })

    $node = Get-GitHubGraphQlNode -Response $response -Path @('data', 'node')

    # The review's own fields are read from the first page, and its comments are walked to the end of
    # the connection. A review carries one comment per published finding, so a set large enough to
    # span pages must still be verified whole: a truncated read would report a finding the service
    # holds as one the service never accepted.
    $commentNodes = Get-GitHubGraphQlConnectionNode `
        -FetchPage $fetchCommentPage `
        -Description "comments of review $reviewId" `
        -FirstPage (Get-JsonProperty -Node $node -Name 'comments')

    $state = [string] (Get-JsonProperty -Node $node -Name 'state')
    $comments = [System.Collections.Generic.List[object]]::new()
    foreach ($comment in $commentNodes) {
        $comments.Add((ConvertTo-GitHubReviewVerificationComment -Comment $comment))
    }

    return [ordered]@{
        reviewId    = [string] (Get-JsonProperty -Node $node -Name 'id')
        state       = $state
        url         = [string] (Get-JsonProperty -Node $node -Name 'url')
        body        = [string] (Get-JsonProperty -Node $node -Name 'body')
        isPublished = ($state -ne $script:GitHubPendingReviewState)
        comments    = @($comments.ToArray())
    }
}

# Converts one review comment into the platform-neutral verification shape.
#
# The thread identifier and the diff side are absent because GitHub carries neither on the comment
# node; both are reported empty, which the anchor check treats as nothing to compare.
function ConvertTo-GitHubReviewVerificationComment {
    param(
        [Parameter(Mandatory)]
        $Comment
    )

    $commentState = [string] (Get-JsonProperty -Node $Comment -Name 'state')

    [ordered]@{
        id          = [string] (Get-JsonProperty -Node $Comment -Name 'id')
        threadId    = ''
        path        = [string] (Get-JsonProperty -Node $Comment -Name 'path')
        line        = Get-JsonProperty -Node $Comment -Name 'line'
        startLine   = Get-JsonProperty -Node $Comment -Name 'startLine'
        side        = ''
        subjectType = [string] (Get-JsonProperty -Node $Comment -Name 'subjectType')
        state       = $commentState
        isPublished = ($commentState -eq $script:GitHubPublishedReviewCommentState)
        body        = [string] (Get-JsonProperty -Node $Comment -Name 'body')
    }
}

# Reopens a resolved thread this run replied into.
#
# GitHub accepts a reply into a thread that is still resolved, so this runs after the submit. A run
# that never published therefore leaves no thread reopened without a reason attached to it.
function Resume-GitHubReviewThread {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [string] $ThreadId
    )

    Invoke-GitHubGraphQlQuery `
        -Transport $Transport `
        -Query $script:GitHubUnresolveThreadMutation `
        -Variables ([ordered]@{ threadId = $ThreadId }) `
        -NoRetry | Out-Null
}
