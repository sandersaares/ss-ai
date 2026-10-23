#Requires -Version 7.0

# Azure DevOps pull request adapter for the ss-review capture
# (docs/implementation.md, "Platform adapters"; docs/design.md, "Captured pull request context").
#
# This is the only place that knows how Azure DevOps names things. It reads a pull request, its
# iterations, its reviewers and its discussion, and returns the platform-neutral context shape the
# rest of the review consumes, so that a later stage never has to ask which platform a thread came
# from. It also performs the writes the review makes: registering the reviewing account on the pull
# request, and publishing the approved findings and their summary.
#
# Everything the service sent that a reviewer could act on is preserved. Comment bodies are copied
# verbatim and the platform's own thread status string is carried alongside the normalized
# resolution. Discarding at capture time would force a later stage to re-fetch to answer a question
# the capture already had the answer to. A thread or comment the service reports as deleted is the
# single exception: its author withdrew it, so it states nothing a reviewer can act on, and it is
# left out of the capture entirely. The capture carries no deletion field of any kind, because
# something that disappears between two captures is already visible in the comparison between them,
# and no further effort is warranted for feedback its author retracted.
#
# Approval standing is deliberately absent. The capture exists so that a reviewer composing comments
# can read what was already said (docs/design.md, "Deterministic discussion capture"); who has voted
# and whether the pull request can merge belong to whoever merges it, so neither reviewer votes nor
# the pull request status is recorded, and vote threads - which hold nothing but the vote - are
# dropped along with them.
#
# All network access goes through AdoRestTransport.ps1, which is what keeps the Azure CLI - and its
# measured deletion of non-ASCII characters - out of the data path.
#
# This is a library file: it is dot-sourced rather than executed, defines only functions and
# constants, and dot-sources nothing itself. It calls Get-JsonProperty, Get-JsonArrayProperty,
# Set-JsonProperty and ConvertTo-CanonicalTimestamp from JsonValue.ps1, New-PullRequestIdentity,
# Get-PullRequestAuthorship and Test-PullRequestIdentityMayMatch from PullRequestIdentity.ps1,
# Get-PullRequestWebUrl from PullRequestTarget.ps1, and Invoke-AdoRestRequest and
# Invoke-AdoRestListRequest from AdoRestTransport.ps1. The composition root loads all of them
# together.
#
# List-valued payload properties are read with Get-JsonArrayProperty into a variable before being
# iterated, and never wrapped in @( ) or piped directly: a command that emits a single array emits
# it as one object, so either form would hand the whole array over as a single element. Writes to a
# materialized node go through Set-JsonProperty for the matching reason, because a name-keyed write
# through a reference that is not typed as IDictionary binds to OrderedDictionary's integer indexer.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The connection data resource is preview-only. Asking for the transport's default version fails
# with VssInvalidPreviewVersionException, which states that a preview resource requires a version
# marked as preview, so this one resource carries its own version while every other call uses the
# transport default.
$script:AdoConnectionDataApiVersion = '7.1-preview'

# Azure DevOps records what a system thread is about in this property. Threads without it are
# ordinary discussion.
$script:AdoThreadTypeProperty = 'CodeReviewThreadType'

# The thread type Azure DevOps uses for a vote notification. A thread of this type holds a vote and
# an auto-generated sentence restating it, so it is excluded from the capture entirely: approval
# standing is not what the capture is for, and the sentence states nothing a reviewer can act on.
$script:AdoVoteThreadType = 'VoteUpdate'

# Property bag values arrive wrapped in an envelope that names the type alongside the scalar it
# carries. These are the envelope's keys, and callers want the value.
$script:AdoPropertyValueKey = '$value'
$script:AdoPropertyTypeKey = '$type'

# Known system thread types mapped to the canonical kind. An unrecognized non-empty type is reported
# as 'system' rather than dropped, so a thread type introduced by the service still reaches the
# reader as machine-generated activity instead of being misread as a person's comment.
$script:AdoThreadKinds = @{
    'RefUpdate'          = 'refUpdate'
    'ReviewersUpdate'    = 'reviewersUpdated'
    'PolicyStatusUpdate' = 'policy'
}

# Thread statuses that mean the discussion is settled, and those that mean it still demands an
# answer. Azure DevOps omits the property entirely on system threads, which are settled by nature
# but carry no status, so absence maps to 'indeterminate' rather than to either extreme.
$script:AdoResolvedThreadStatuses = @('fixed', 'closed', 'byDesign', 'wontFix')
$script:AdoActiveThreadStatuses = @('active', 'pending')

# The pull request page focuses a single discussion when given this query parameter.
$script:AdoDiscussionQueryParameter = 'discussionId'

# The iteration-changes resource answers with an envelope of its own rather than with the
# value/count one every other Azure DevOps collection uses: its entries live under this name, and it
# states where the next page begins in the response body rather than in a header.
$script:AdoIterationChangeCollectionProperty = 'changeEntries'
$script:AdoIterationChangeNextSkipProperty = 'nextSkip'

# Page size asked of the iteration-changes resource. Large enough that an ordinary pull request is
# read in a single request, and small enough that the service is not asked for a response it will
# truncate or refuse.
$script:AdoIterationChangePageSize = 1000

# Azure DevOps states an item path with a leading separator, which the finding pipeline does not use.
$script:AdoItemPathPrefix = '/'

# One change entry may state several change types at once, listed in one string.
$script:AdoChangeTypeSeparator = ','

# Unwraps one property bag entry.
#
# A value that is not a wrapped pair is returned unchanged, because Azure DevOps uses the wrapper
# only for scalars and a future structured value must survive rather than become $null.
function Get-AdoPropertyValue {
    param(
        [AllowNull()]
        $Node
    )

    if ($Node -is [System.Collections.IDictionary] -and $Node.Contains($script:AdoPropertyTypeKey)) {
        return Get-JsonProperty -Node $Node -Name $script:AdoPropertyValueKey
    }
    return $Node
}

# Flattens a thread property bag into plain values.
#
# Authorship classification inspects these keys, and the capture publishes them, so carrying the
# wrappers any further would make every consumer repeat the unwrapping.
function ConvertFrom-AdoPropertyBag {
    param(
        [AllowNull()]
        $Properties
    )

    $result = [ordered]@{}
    if ($null -eq $Properties -or $Properties -isnot [System.Collections.IDictionary]) {
        return $result
    }

    foreach ($key in $Properties.Keys) {
        Set-JsonProperty -Node $result -Name ([string] $key) -Value (Get-AdoPropertyValue -Node (Get-JsonProperty -Node $Properties -Name ([string] $key)))
    }
    return $result
}

# Converts one Azure DevOps identity reference into the canonical identity.
#
# The subject descriptor is the field that matters: it is the only identifier that appears in the
# same form on a comment, on a thread's identities map and in connection data, and its prefix is
# what tells a service principal apart from a person.
function New-AdoIdentity {
    param(
        [AllowNull()]
        $Node
    )

    if ($null -eq $Node -or $Node -isnot [System.Collections.IDictionary]) {
        return New-PullRequestIdentity -Platform 'azureDevOps'
    }

    New-PullRequestIdentity -Platform 'azureDevOps' `
        -Id ([string] (Get-JsonProperty -Node $Node -Name 'id')) `
        -DisplayName ([string] (Get-JsonProperty -Node $Node -Name 'displayName')) `
        -UniqueName ([string] (Get-JsonProperty -Node $Node -Name 'uniqueName')) `
        -Descriptor ([string] (Get-JsonProperty -Node $Node -Name 'descriptor')) `
        -Url ([string] (Get-JsonProperty -Node $Node -Name 'url'))
}

# Renders the browser URL of a pull request that has been resolved.
#
# The URL itself is derived by Get-PullRequestWebUrl, which is where both platforms' forms are
# decided. What this adds is the guard: every caller here holds a target the adapter has already
# resolved, so a missing project or repository is a defect in the caller rather than a URL to be
# rendered as best it can be, and it is reported where it happened instead of reaching a reader as
# a link that opens nothing.
function Get-AdoPullRequestBrowserUrl {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $collectionUrl = [string] (Get-JsonProperty -Node $Target -Name 'collectionUrl')
    $project = [string] (Get-JsonProperty -Node $Target -Name 'project')
    $repository = [string] (Get-JsonProperty -Node $Target -Name 'repository')
    $pullRequestId = Get-JsonProperty -Node $Target -Name 'pullRequestId'

    if ([string]::IsNullOrWhiteSpace($collectionUrl)) {
        throw 'An Azure DevOps target requires a collection URL.'
    }
    if ([string]::IsNullOrWhiteSpace($project) -or [string]::IsNullOrWhiteSpace($repository)) {
        throw "The Azure DevOps target for pull request $pullRequestId is missing its project or repository. Resolve the target before rendering its URL."
    }

    Get-PullRequestWebUrl -Target $Target
}

# Builds the REST path of a pull request resource, optionally one of its sub-resources.
function Get-AdoPullRequestResourcePath {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [string] $Suffix
    )

    $project = [uri]::EscapeDataString([string] (Get-JsonProperty -Node $Target -Name 'project'))
    $repository = [uri]::EscapeDataString([string] (Get-JsonProperty -Node $Target -Name 'repository'))
    $path = "$project/_apis/git/repositories/$repository/pullRequests/$(Get-JsonProperty -Node $Target -Name 'pullRequestId')"

    if (-not [string]::IsNullOrWhiteSpace($Suffix)) {
        $path = "$path/$($Suffix.TrimStart('/'))"
    }
    return $path
}

# Reports who the run is acting as.
#
# The subject descriptor returned here is exactly the descriptor that appears on that account's
# comments, which is what lets the review recognize its own earlier feedback without matching on
# display names.
function Get-AdoAuthenticatedIdentity {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport
    )

    $payload = Invoke-AdoRestRequest -Transport $Transport -Path '_apis/connectionData' -ApiVersion $script:AdoConnectionDataApiVersion
    $user = Get-JsonProperty -Node $payload -Name 'authenticatedUser'
    if ($null -eq $user) {
        throw 'Azure DevOps connection data did not report an authenticated user.'
    }

    # The account name lives in a wrapped property bag entry rather than beside the display name.
    $account = Get-AdoPropertyValue -Node (Get-JsonProperty -Node (Get-JsonProperty -Node $user -Name 'properties') -Name 'Account')

    New-PullRequestIdentity -Platform 'azureDevOps' `
        -Id ([string] (Get-JsonProperty -Node $user -Name 'id')) `
        -DisplayName ([string] (Get-JsonProperty -Node $user -Name 'providerDisplayName')) `
        -UniqueName ([string] $account) `
        -Descriptor ([string] (Get-JsonProperty -Node $user -Name 'subjectDescriptor'))
}

# Completes a target that names only the pull request.
#
# The organization-level pull request resource resolves both the project and the repository from the
# identifier alone, which is what lets a reviewer point the skill at a bare pull request number. The
# call is skipped when both are already known, because it costs a round trip and can only confirm
# what the caller already supplied.
function Resolve-AdoPullRequestTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $pullRequestId = Get-JsonProperty -Node $Target -Name 'pullRequestId'
    if ($null -eq $pullRequestId -or [int] $pullRequestId -le 0) {
        throw 'An Azure DevOps target requires a positive pull request identifier.'
    }

    $project = [string] (Get-JsonProperty -Node $Target -Name 'project')
    $repository = [string] (Get-JsonProperty -Node $Target -Name 'repository')

    if ([string]::IsNullOrWhiteSpace($project) -or [string]::IsNullOrWhiteSpace($repository)) {
        $payload = Invoke-AdoRestRequest -Transport $Transport -Path "_apis/git/pullrequests/$pullRequestId"
        $repositoryNode = Get-JsonProperty -Node $payload -Name 'repository'
        if ($null -eq $repositoryNode) {
            throw "Azure DevOps did not report a repository for pull request $pullRequestId."
        }
        $repository = [string] (Get-JsonProperty -Node $repositoryNode -Name 'name')
        $project = [string] (Get-JsonProperty -Node (Get-JsonProperty -Node $repositoryNode -Name 'project') -Name 'name')
    }

    $resolved = [ordered]@{
        platform      = 'azureDevOps'
        collectionUrl = [string] (Get-JsonProperty -Node $Target -Name 'collectionUrl')
        organization  = [string] (Get-JsonProperty -Node $Target -Name 'organization')
        project       = $project
        repository    = $repository
        pullRequestId = $pullRequestId
        webUrl        = $null
    }
    Set-JsonProperty -Node $resolved -Name 'webUrl' -Value (Get-AdoPullRequestBrowserUrl -Target $resolved)
    return $resolved
}

# Renders the browser URL that opens a pull request focused on one discussion thread.
function Get-AdoPullRequestThreadUrl {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        $ThreadId
    )

    $webUrl = [string] (Get-JsonProperty -Node $Target -Name 'webUrl')
    if ([string]::IsNullOrWhiteSpace($webUrl)) {
        $webUrl = Get-AdoPullRequestBrowserUrl -Target $Target
    }

    '{0}?{1}={2}' -f $webUrl, $script:AdoDiscussionQueryParameter, $ThreadId
}

# Normalizes a thread status into the three states a reader acts on.
function Get-AdoThreadResolution {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Status
    )

    if ([string]::IsNullOrWhiteSpace($Status)) { return 'indeterminate' }
    if ($script:AdoResolvedThreadStatuses -contains $Status) { return 'resolved' }
    if ($script:AdoActiveThreadStatuses -contains $Status) { return 'active' }
    return 'indeterminate'
}

# Classifies a thread from its property bag.
function Get-AdoThreadKind {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Properties
    )

    if (-not $Properties.Contains($script:AdoThreadTypeProperty)) { return 'text' }

    $type = [string] $Properties[$script:AdoThreadTypeProperty]
    if ([string]::IsNullOrWhiteSpace($type)) { return 'text' }
    if ($script:AdoThreadKinds.ContainsKey($type)) { return $script:AdoThreadKinds[$type] }
    return 'system'
}

# Reports whether a thread is a vote notification.
#
# Such a thread carries the vote in its property bag and a generated sentence restating it, and the
# capture records neither, so it is excluded before conversion rather than converted into a thread
# with nothing in it.
function Test-AdoVoteThread {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Properties
    )

    if (-not $Properties.Contains($script:AdoThreadTypeProperty)) { return $false }
    [string]::Equals([string] $Properties[$script:AdoThreadTypeProperty], $script:AdoVoteThreadType, [StringComparison]::OrdinalIgnoreCase)
}

# Reads one line number out of a thread position node.
function Get-AdoThreadContextLine {
    param(
        [AllowNull()]
        $Position
    )

    if ($null -eq $Position) { return $null }
    return Get-JsonProperty -Node $Position -Name 'line'
}

# Converts a thread's file anchor into the canonical line context.
#
# Azure DevOps expresses the anchor as a right-file position on the post-change side and a left-file
# position on the pre-change side, and a thread may carry either or both. Both are preserved: the
# right-file position says where the code is now, and the left-file position says which removed or
# replaced line the discussion was originally about, which is the only way to read a comment on a
# line that no longer exists.
function ConvertTo-AdoThreadContext {
    param(
        [AllowNull()]
        $ThreadContext
    )

    if ($null -eq $ThreadContext -or $ThreadContext -isnot [System.Collections.IDictionary]) {
        return $null
    }

    $filePath = Get-JsonProperty -Node $ThreadContext -Name 'filePath'
    $rightStart = Get-JsonProperty -Node $ThreadContext -Name 'rightFileStart'
    $rightEnd = Get-JsonProperty -Node $ThreadContext -Name 'rightFileEnd'
    $leftStart = Get-JsonProperty -Node $ThreadContext -Name 'leftFileStart'
    $leftEnd = Get-JsonProperty -Node $ThreadContext -Name 'leftFileEnd'

    if ([string]::IsNullOrWhiteSpace([string] $filePath) -and
        $null -eq $rightStart -and $null -eq $rightEnd -and $null -eq $leftStart -and $null -eq $leftEnd) {
        return $null
    }

    $side = $null
    if ($null -ne $rightStart -or $null -ne $rightEnd) {
        $side = 'right'
    } elseif ($null -ne $leftStart -or $null -ne $leftEnd) {
        $side = 'left'
    }

    [ordered]@{
        filePath          = ConvertFrom-AdoItemPath -Path ([string] $filePath)
        side              = $side
        startLine         = Get-AdoThreadContextLine -Position $rightStart
        endLine           = Get-AdoThreadContextLine -Position $rightEnd
        originalStartLine = Get-AdoThreadContextLine -Position $leftStart
        originalEndLine   = Get-AdoThreadContextLine -Position $leftEnd
    }
}

# Converts one comment.
#
# The body is copied exactly as the service sent it. Trimming, reflowing or re-encoding it here
# would put the review in the position of quoting text that differs from what its author wrote, and
# a single collapsed blank line is enough to change what a code fence means.
#
# Properties are the thread's property bag and are supplied only for the comment that opened the
# thread, because that is the only comment they describe. The automated reviewers that set them set
# them once, when they post the thread, and every later reply is somebody answering. Applying them
# to the whole thread would report every person who objects inside an automated thread as machine
# generated, and the discussion document is what a reviewer deduplicates against, so that objection
# would be discounted as noise. A reply is classified from its own account and its own text.
function ConvertTo-AdoComment {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Comment,

        [Parameter(Mandatory)]
        [string] $ThreadUrl,

        [AllowNull()]
        [System.Collections.IDictionary] $Properties
    )

    $author = New-AdoIdentity -Node (Get-JsonProperty -Node $Comment -Name 'author')
    $content = Get-JsonProperty -Node $Comment -Name 'content'
    $publishedUtc = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $Comment -Name 'publishedDate'))
    $lastEditedUtc = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $Comment -Name 'lastContentUpdatedDate'))

    # Azure DevOps reports the root comment of a thread with parent 0 rather than by omitting the
    # property, and a canonical parent of zero would read as a reference to a comment that exists.
    $parentId = Get-JsonProperty -Node $Comment -Name 'parentCommentId'
    if ($null -ne $parentId -and [long] $parentId -eq 0) { $parentId = $null }

    $authorship = Get-PullRequestAuthorship -Identity $author -Content ([string] $content) -Properties $Properties

    [ordered]@{
        id                 = Get-JsonProperty -Node $Comment -Name 'id'
        url                = $ThreadUrl
        parentId           = $parentId
        author             = $author
        content            = $content
        publishedUtc       = $publishedUtc
        lastEditedUtc      = $lastEditedUtc
        isEdited           = ($null -ne $lastEditedUtc -and $lastEditedUtc -ne $publishedUtc)
        authorship         = $authorship['authorship']
        authorshipEvidence = @($authorship['evidence'])
    }
}

# Reports whether the service has marked a thread or a comment as deleted.
#
# Azure DevOps omits the property on an item that stands, and strict mode turns a missing key into a
# terminating error, so the read goes through the existence-checking accessor with a default.
function Test-AdoDeletedNode {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Node
    )

    [bool] (Get-JsonProperty -Node $Node -Name 'isDeleted' -Default $false)
}

# Identifies which comment opened a thread, by position in the thread's collection.
#
# Azure DevOps numbers comments within a thread from one in posting order, so the lowest identifier
# is the one the thread was created with. The parent link cannot answer this: the service reports
# parentCommentId as 0 on every comment of a thread, replies included, so it separates nothing.
# Serialization order is the fallback for a comment that carries no identifier at all, because a
# thread always has an opener and reporting none would discard the property evidence entirely.
function Get-AdoThreadOpeningCommentIndex {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Comments
    )

    if ($Comments.Count -eq 0) { return -1 }

    $openingIndex = 0
    $openingId = $null
    for ($index = 0; $index -lt $Comments.Count; $index++) {
        $id = Get-JsonProperty -Node $Comments[$index] -Name 'id'
        if ($null -eq $id) { continue }
        if ($null -eq $openingId -or [long] $id -lt [long] $openingId) {
            $openingId = $id
            $openingIndex = $index
        }
    }
    return $openingIndex
}

# Converts one discussion thread.
#
# A thread is reported as outdated when its anchor was recorded against an earlier revision than the
# one under review. Azure DevOps sends no outdated flag and recomputes line positions in its own
# client, so this is the strongest statement the capture can make from the payload alone: the
# comment refers to a line as it stood in an earlier revision, and the capture cannot prove that
# line still stands.
function ConvertTo-AdoThread {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Thread,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        # The thread's flattened property bag. It is derived by the caller, which has to read the bag
        # anyway to decide whether the thread is captured at all, and deriving it a second time here
        # would leave two copies of the same reading to drift apart.
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Properties,

        [AllowNull()]
        $LatestRevisionId
    )

    $threadId = Get-JsonProperty -Node $Thread -Name 'id'
    $threadUrl = Get-AdoPullRequestThreadUrl -Target $Target -ThreadId $threadId

    $rawStatus = Get-JsonProperty -Node $Thread -Name 'status'
    $publishedUtc = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $Thread -Name 'publishedDate'))

    $pullRequestThreadContext = Get-JsonProperty -Node $Thread -Name 'pullRequestThreadContext'
    $iterationContext = Get-JsonProperty -Node $pullRequestThreadContext -Name 'iterationContext'
    $anchorRevisionId = Get-JsonProperty -Node $iterationContext -Name 'secondComparingIteration'

    $isOutdated = $false
    if ($null -ne $anchorRevisionId -and $null -ne $LatestRevisionId) {
        $isOutdated = [long] $anchorRevisionId -lt [long] $LatestRevisionId
    }

    $comments = [System.Collections.Generic.List[object]]::new()
    $commentNodes = Get-JsonArrayProperty -Node $Thread -Name 'comments'
    # The property bag belongs to the comment that opened the thread and to no other, so only that
    # comment is classified with it. See ConvertTo-AdoComment for why. The opener is identified
    # across every comment the service sent, deleted ones included, so that a thread whose opening
    # comment was deleted leaves the bag unapplied rather than transferring it to a reply somebody
    # else wrote.
    $openingIndex = Get-AdoThreadOpeningCommentIndex -Comments $commentNodes
    for ($index = 0; $index -lt $commentNodes.Count; $index++) {
        if (Test-AdoDeletedNode -Node $commentNodes[$index]) { continue }
        if ($index -eq $openingIndex) {
            $comments.Add((ConvertTo-AdoComment -Comment $commentNodes[$index] -ThreadUrl $threadUrl -Properties $Properties))
        } else {
            $comments.Add((ConvertTo-AdoComment -Comment $commentNodes[$index] -ThreadUrl $threadUrl))
        }
    }

    # The opener is named structurally so that the shared model can attribute the thread without
    # relying on presentation order (docs/implementation.md, "Canonical document"). It is resolved
    # over the comments the capture carries rather than over every comment the service sent, so the
    # identifier always names a comment a reader can find. A thread whose original opener was
    # deleted therefore reports its earliest surviving comment, which is the only opener the capture
    # can show.
    $openingComment = $null
    if ($comments.Count -gt 0) {
        $survivingOpeningIndex = Get-AdoThreadOpeningCommentIndex -Comments $comments.ToArray()
        if ($survivingOpeningIndex -ge 0) { $openingComment = $comments[$survivingOpeningIndex] }
    }

    [ordered]@{
        id               = $threadId
        url              = $threadUrl
        publishedUtc     = $publishedUtc
        lastUpdatedUtc   = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $Thread -Name 'lastUpdatedDate'))
        eventUtc         = $publishedUtc
        rawStatus        = $rawStatus
        resolution       = Get-AdoThreadResolution -Status ([string] $rawStatus)
        kind             = Get-AdoThreadKind -Properties $Properties
        isOutdated       = $isOutdated
        context          = ConvertTo-AdoThreadContext -ThreadContext (Get-JsonProperty -Node $Thread -Name 'threadContext')
        anchorKey        = Get-JsonProperty -Node $pullRequestThreadContext -Name 'changeTrackingId'
        properties       = $Properties
        openingCommentId = if ($null -eq $openingComment) { $null } else { $openingComment['id'] }
        # Azure DevOps reports no event this capture records as a decision. The key is carried
        # holding null so that a consumer reads one thread shape whichever platform produced it.
        decision         = $null
        comments         = @($comments)
    }
}

# Converts one reviewer entry.
#
# Azure DevOps sends no subject descriptor on reviewer entries, so the resulting identity carries
# only the account identifier and names. That is sufficient: the identifier is what the reviewer
# routes accept, and descriptor-based classification is only needed where a comment's author has to
# be judged as a person or a machine.
function ConvertTo-AdoReviewer {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Reviewer
    )

    [ordered]@{
        identity   = New-AdoIdentity -Node $Reviewer
        isRequired = [bool] (Get-JsonProperty -Node $Reviewer -Name 'isRequired' -Default $false)
        isTeam     = [bool] (Get-JsonProperty -Node $Reviewer -Name 'isContainer' -Default $false)
    }
}

# Converts one iteration into a canonical revision.
#
# The merge base is the iteration's common ref commit. It is not the target branch head: the head
# moves with every unrelated commit anyone lands on the target branch, and diffing against it would
# attribute other people's changes to this pull request.
function ConvertTo-AdoRevision {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Iteration
    )

    [ordered]@{
        id              = Get-JsonProperty -Node $Iteration -Name 'id'
        createdUtc      = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $Iteration -Name 'createdDate'))
        reason          = Get-JsonProperty -Node $Iteration -Name 'reason'
        sourceCommit    = Get-JsonProperty -Node (Get-JsonProperty -Node $Iteration -Name 'sourceRefCommit') -Name 'commitId'
        targetCommit    = Get-JsonProperty -Node (Get-JsonProperty -Node $Iteration -Name 'targetRefCommit') -Name 'commitId'
        mergeBaseCommit = Get-JsonProperty -Node (Get-JsonProperty -Node $Iteration -Name 'commonRefCommit') -Name 'commitId'
    }
}

# Picks the newest iteration out of a set of canonical revisions.
#
# The newest iteration is the reviewed state, which is what the branch looks like now. Reading the
# pull request's own lastMergeTargetCommit instead would name the target branch head, which is a
# different commit whenever anyone else has landed work since the iteration was created.
function Select-AdoLatestRevision {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Revision
    )

    $latest = $null
    foreach ($candidate in $Revision) {
        # Coerced rather than indexed directly, because a revision that has been through the
        # pipeline arrives wrapped in a PSObject, and indexing that wrapper by name binds to
        # OrderedDictionary's integer indexer.
        $entry = $candidate -as [System.Collections.IDictionary]
        if ($null -eq $entry) { continue }
        if ($null -eq $latest -or [long] $entry['id'] -gt [long] $latest['id']) {
            $latest = $entry
        }
    }
    return $latest
}

# Captures everything a review needs about one Azure DevOps pull request.
#
# The four reads are all of them: the pull request itself, its iterations, its reviewers and its
# discussion. Nothing here filters, samples or summarizes - the reduction is the reviewer's job, and
# a capture that decided what mattered would decide it without having read the guidelines.
function Get-AdoPullRequestContext {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $resolved = Resolve-AdoPullRequestTarget -Transport $Transport -Target $Target
    $authenticatedIdentity = Get-AdoAuthenticatedIdentity -Transport $Transport

    $pullRequest = Invoke-AdoRestRequest -Transport $Transport -Path (Get-AdoPullRequestResourcePath -Target $resolved)
    $iterationNodes = Invoke-AdoRestListRequest -Transport $Transport -Path (Get-AdoPullRequestResourcePath -Target $resolved -Suffix 'iterations')
    $reviewerNodes = Invoke-AdoRestListRequest -Transport $Transport -Path (Get-AdoPullRequestResourcePath -Target $resolved -Suffix 'reviewers')
    $threadNodes = Invoke-AdoRestListRequest -Transport $Transport -Path (Get-AdoPullRequestResourcePath -Target $resolved -Suffix 'threads')

    $revisions = [System.Collections.Generic.List[object]]::new()
    foreach ($iteration in $iterationNodes) {
        $revisions.Add((ConvertTo-AdoRevision -Iteration $iteration))
    }

    $latestRevision = Select-AdoLatestRevision -Revision @($revisions)

    $reviewers = [System.Collections.Generic.List[object]]::new()
    foreach ($reviewer in $reviewerNodes) {
        $reviewers.Add((ConvertTo-AdoReviewer -Reviewer $reviewer))
    }

    $latestRevisionId = if ($null -eq $latestRevision) { $null } else { $latestRevision['id'] }
    $threads = [System.Collections.Generic.List[object]]::new()
    foreach ($thread in $threadNodes) {
        if (Test-AdoDeletedNode -Node $thread) { continue }
        $threadProperties = ConvertFrom-AdoPropertyBag -Properties (Get-JsonProperty -Node $thread -Name 'properties')
        if (Test-AdoVoteThread -Properties $threadProperties) { continue }
        $threads.Add((ConvertTo-AdoThread -Thread $thread -Target $resolved -Properties $threadProperties -LatestRevisionId $latestRevisionId))
    }

    [ordered]@{
        target                = $resolved
        authenticatedIdentity = $authenticatedIdentity
        pullRequest           = [ordered]@{
            id                      = Get-JsonProperty -Node $pullRequest -Name 'pullRequestId'
            title                   = Get-JsonProperty -Node $pullRequest -Name 'title'
            description             = Get-JsonProperty -Node $pullRequest -Name 'description'
            isDraft                 = [bool] (Get-JsonProperty -Node $pullRequest -Name 'isDraft' -Default $false)
            author                  = New-AdoIdentity -Node (Get-JsonProperty -Node $pullRequest -Name 'createdBy')
            createdUtc              = ConvertTo-CanonicalTimestamp -Value ([string] (Get-JsonProperty -Node $pullRequest -Name 'creationDate'))
            sourceRef               = Get-JsonProperty -Node $pullRequest -Name 'sourceRefName'
            targetRef               = Get-JsonProperty -Node $pullRequest -Name 'targetRefName'
            reviewedSourceCommit    = if ($null -eq $latestRevision) { $null } else { $latestRevision['sourceCommit'] }
            reviewedMergeBaseCommit = if ($null -eq $latestRevision) { $null } else { $latestRevision['mergeBaseCommit'] }
            # Raw platform merge bookkeeping, retained unmodified. It is the only place
            # lastMergeTargetCommit appears, precisely so that no consumer can mistake the target
            # branch head for the merge base.
            mergeState              = [ordered]@{
                status                = Get-JsonProperty -Node $pullRequest -Name 'mergeStatus'
                mergeId               = Get-JsonProperty -Node $pullRequest -Name 'mergeId'
                lastMergeSourceCommit = Get-JsonProperty -Node (Get-JsonProperty -Node $pullRequest -Name 'lastMergeSourceCommit') -Name 'commitId'
                lastMergeTargetCommit = Get-JsonProperty -Node (Get-JsonProperty -Node $pullRequest -Name 'lastMergeTargetCommit') -Name 'commitId'
                lastMergeCommit       = Get-JsonProperty -Node (Get-JsonProperty -Node $pullRequest -Name 'lastMergeCommit') -Name 'commitId'
            }
        }
        reviewers             = @($reviewers)
        revisions             = @($revisions)
        threads               = @($threads)
    }
}

# Reports the identifier of the newest iteration of a pull request.
#
# A line comment's thread context is expressed against one iteration, so a publication has to name
# the same iteration the review read, which is the newest one.
function Get-AdoPullRequestLatestIterationId {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $resolved = Resolve-AdoPullRequestTarget -Transport $Transport -Target $Target
    $iterationNodes = Invoke-AdoRestListRequest -Transport $Transport -Path (Get-AdoPullRequestResourcePath -Target $resolved -Suffix 'iterations')

    $revisions = [System.Collections.Generic.List[object]]::new()
    foreach ($iteration in $iterationNodes) {
        $revisions.Add((ConvertTo-AdoRevision -Iteration $iteration))
    }

    $latest = Select-AdoLatestRevision -Revision @($revisions)
    if ($null -eq $latest) {
        throw "Azure DevOps reports no iteration for pull request $(Get-JsonProperty -Node $resolved -Name 'pullRequestId'), so there is no iteration to anchor a comment against."
    }
    return $latest['id']
}

# Lists the files changed by the pull request as of one iteration.
#
# This read exists to supply the change tracking identifier of each changed file, which Azure DevOps
# requires on the thread context of a line comment. It states identity and nothing more: the service
# reports which files changed and says nothing at all about which lines did, which is why the hunks
# a finding is anchored against come from the local repository instead.
#
# The request deliberately omits '$compareTo'. The Azure DevOps REST API defines that default as a
# comparison against the common commit, which makes the response the pull request's cumulative
# change set as of the iteration rather than the iteration's own delta against its predecessor. The
# preflight needs the cumulative set, because a finding can target a file last touched by any
# iteration in the pull request.
#
# The resource does not use the value/count envelope every other Azure DevOps collection uses. It
# names its entries differently and states the start of the next page in the response body, so it is
# read here rather than through Invoke-AdoRestListRequest, and the pages are followed to exhaustion
# because a pull request that changes more files than one page holds is ordinary.
#
# Entries are returned exactly as the service sent them, item paths included, so that the one place
# a path changes form is ConvertFrom-AdoItemPath.
function Get-AdoPullRequestIterationChange {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        $IterationId
    )

    $resolved = Resolve-AdoPullRequestTarget -Transport $Transport -Target $Target
    $path = Get-AdoPullRequestResourcePath -Target $resolved -Suffix "iterations/$IterationId/changes"

    $entries = [System.Collections.Generic.List[object]]::new()
    $skip = 0

    while ($true) {
        $query = @{ '$top' = $script:AdoIterationChangePageSize }
        if ($skip -gt 0) { $query['$skip'] = $skip }

        $payload = Invoke-AdoRestRequest -Transport $Transport -Path $path -Query $query

        # An absent collection is a terminating failure for the same reason it is in the list
        # envelope reader: a publication that read it as an empty set would report every file as
        # unchanged and reject every finding, which looks exactly like a correct refusal.
        if (-not (Test-JsonProperty -Node $payload -Name $script:AdoIterationChangeCollectionProperty)) {
            throw "Azure DevOps returned no '$script:AdoIterationChangeCollectionProperty' collection for '$path', so the response is not the change listing this read requires."
        }

        $page = Get-JsonArrayProperty -Node $payload -Name $script:AdoIterationChangeCollectionProperty
        foreach ($entry in $page) { $entries.Add($entry) }

        $nextSkip = Get-JsonProperty -Node $payload -Name $script:AdoIterationChangeNextSkipProperty
        if ($null -eq $nextSkip) { break }

        $nextSkipValue = [int] $nextSkip
        if ($nextSkipValue -le 0) { break }

        # A cursor that does not advance would be followed forever, and a service that keeps
        # answering with the same page is a fault this read must report rather than absorb.
        if ($nextSkipValue -le $skip) {
            throw "Azure DevOps answered '$path' with a paging cursor at $nextSkipValue that does not advance past $skip, so the change listing cannot be read to its end."
        }

        $skip = $nextSkipValue

        # The response also states a page size for the next request, and it is deliberately ignored:
        # the service answers a request for two entries that it served in full with a next page size
        # of one, so honouring it would shrink the page and read a large pull request an entry at a
        # time. The page size asked for is this reader's own on every request, and only the position
        # advances.
    }

    return , @($entries)
}

# Converts an Azure DevOps item path to the canonical form the finding pipeline uses.
#
# Azure DevOps states an item path rooted at the repository with a leading separator, while a finding
# names the same file repository-relative with no leading separator. Both directions live here so
# that a call site never trims or prepends a separator of its own.
function ConvertFrom-AdoItemPath {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [AllowNull()]
        [string] $Path
    )

    if ([string]::IsNullOrWhiteSpace($Path)) { return '' }

    $normalized = $Path.Replace('\', '/').Trim()
    return $normalized.TrimStart($script:AdoItemPathPrefix)
}

# Converts a canonical repository-relative path to the form Azure DevOps states and accepts.
function ConvertTo-AdoItemPath {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [AllowNull()]
        [string] $Path
    )

    $canonical = ConvertFrom-AdoItemPath -Path $Path
    if ($canonical -eq '') { return '' }

    return "$script:AdoItemPathPrefix$canonical"
}

# Reports the change types one entry states.
#
# A single change carries several types at once - a file that was both moved and edited is reported
# as 'edit, rename' - so the property is a set rather than a single token and every caller has to
# read it as one.
function ConvertFrom-AdoChangeType {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [AllowNull()]
        [string] $ChangeType
    )

    $tokens = [System.Collections.Generic.List[string]]::new()
    if ([string]::IsNullOrWhiteSpace($ChangeType)) { return , @($tokens) }

    foreach ($token in $ChangeType.Split($script:AdoChangeTypeSeparator)) {
        $trimmed = $token.Trim()
        if ($trimmed -ne '') { $tokens.Add($trimmed.ToLowerInvariant()) }
    }
    return , @($tokens)
}



# --- Publication ---------------------------------------------------------------------------------

# The Azure DevOps half of the publication adapter interface (docs/implementation.md, "Platform
# dispatch"). PullRequestPublication.ps1 owns what is published and in what order; everything below
# knows only how Azure DevOps is asked to accept it.
#
# Azure DevOps has no draft review, and that single fact shapes all of it. There is nothing to open,
# so beginning a review is bookkeeping and writes nothing; each finding is published as it is
# written and is visible to everyone the moment the service accepts it; the summary is an ordinary
# pull request comment posted after the findings it counts; and there is nothing to discard when a
# run fails partway, because what was written is already published. The result record is therefore
# the only account of a partial run, which is why writes retry only explicit throttling refusals.
#
# The review handle these functions pass among themselves accumulates what was published, because
# the service offers no object that groups this run's comments and the verification has to read back
# exactly the threads this run created rather than everything the pull request holds.

# The pull request status that still accepts comments. A draft pull request carries it too, and a
# reviewer asked to look at work in progress is entitled to answer.
$script:AdoActivePullRequestStatus = 'active'

# The thread status a published finding carries, and the status a reopen operation restores. A
# finding is an open question until somebody settles it, so finding threads are posted as open.
$script:AdoActiveThreadStatus = 'active'

# The summary is informative and asks nothing of the author, so Azure DevOps should not list it
# among discussions needing attention. The neutral settled status avoids verdict-bearing outcomes
# used for code discussions. Ref: docs/design.md, "Review publication".
$script:AdoReviewSummaryThreadStatus = 'closed'

# The reviewer vote that approves the pull request for merge. Azure DevOps scores a reviewer vote on
# a scale whose top value approves the merge, and that is the value a review the user approved the
# merge on records. Ref: docs/design.md, "Review publication"; Azure DevOps reviewer vote scale.
$script:AdoApproveVote = 10

# The comment type of prose a person reads, as opposed to the types the service generates for its
# own activity.
$script:AdoTextCommentType = 'text'

# The parent Azure DevOps states on a comment that opens a thread rather than answers one, which is
# the same value the capture reads back and discards (see ConvertTo-AdoComment).
$script:AdoRootCommentParentId = 0

# The column positions a line anchor is expressed with. Azure DevOps positions a thread by column as
# well as by line, while a review anchors whole lines and never a column range. The span therefore
# begins where the anchored region begins and ends just inside the last anchored line, which is the
# shortest non-empty span the service accepts and which still spans every line in between.
$script:AdoThreadAnchorStartOffset = 1
$script:AdoThreadAnchorEndOffset = 2

# The states the verification reports for reviews and comments. Azure DevOps has no draft review
# object and publishes each accepted comment immediately, so the platform-neutral check turns on
# whether the thing being verified is visible and these name that and nothing else.
$script:AdoPublishedReviewState = 'published'
$script:AdoUnpublishedReviewState = 'unpublished'

# What a published comment is attached to, in the vocabulary the GitHub adapter reports under the
# same name. A reader comparing the two platforms' verification records compares this field
# ordinally, so one concept must have one spelling whichever platform produced it. Azure DevOps also
# carries comments attached to the pull request itself, which GitHub's vocabulary has no word for,
# so that subject extends it in the same style.
$script:AdoLineCommentSubject = 'LINE'
$script:AdoFileCommentSubject = 'FILE'
$script:AdoPullRequestCommentSubject = 'PULL_REQUEST'

# Reports the live state of the pull request publication is about to write to.
#
# The commits reported are the newest iteration's, which is the same pair the capture recorded as
# reviewed, so the caller compares like with like when it decides whether the pull request has moved
# since the approval. The iteration is reported alongside them because every line comment this run
# writes is anchored against it.
#
# No pending review is ever reported: Azure DevOps publishes a comment the moment it accepts one, so
# there is no unpublished work of the reviewing account for a run to collide with.
function Get-AdoPullRequestPublicationState {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $resolved = Resolve-AdoPullRequestTarget -Transport $Transport -Target $Target

    $pullRequest = Invoke-AdoRestRequest -Transport $Transport -Path (Get-AdoPullRequestResourcePath -Target $resolved)
    $iterationNodes = Invoke-AdoRestListRequest -Transport $Transport -Path (Get-AdoPullRequestResourcePath -Target $resolved -Suffix 'iterations')

    $revisions = [System.Collections.Generic.List[object]]::new()
    foreach ($iteration in $iterationNodes) {
        $revisions.Add((ConvertTo-AdoRevision -Iteration $iteration))
    }

    $latest = Select-AdoLatestRevision -Revision @($revisions)
    if ($null -eq $latest) {
        throw "Azure DevOps reports no iteration for pull request $(Get-JsonProperty -Node $resolved -Name 'pullRequestId'), so there is no iteration to anchor a comment against."
    }

    # The authenticated account is read here rather than deduced later because every comment this run
    # writes is attributed to it, and the last moment to establish who is about to speak is before
    # anything is said.
    $identity = Get-AdoAuthenticatedIdentity -Transport $Transport
    $status = [string] (Get-JsonProperty -Node $pullRequest -Name 'status')

    [ordered]@{
        platform          = 'azureDevOps'
        # Named in both platforms' state so that a consumer reads one shape whichever produced it,
        # following the same convention the capture applies to a thread's decision. Azure DevOps
        # addresses a pull request by its identifier alone and has no second handle to report.
        pullRequestNodeId = $null
        url               = [string] (Get-JsonProperty -Node $resolved -Name 'webUrl')
        status            = $status
        isActive          = [string]::Equals($status, $script:AdoActivePullRequestStatus, [StringComparison]::OrdinalIgnoreCase)
        isDraft           = [bool] (Get-JsonProperty -Node $pullRequest -Name 'isDraft' -Default $false)
        sourceCommit      = [string] $latest['sourceCommit']
        mergeBaseCommit   = [string] $latest['mergeBaseCommit']
        identity          = [string] (Get-JsonProperty -Node $identity -Name 'uniqueName')
        pendingReviewId   = $null
        iterationId       = $latest['id']
    }
}

# Reports how Azure DevOps identifies each file the review will comment on.
#
# A line comment is addressed by the change tracking identifier the service assigned to the file
# within the iteration, not by path, so the identifiers are read once here for exactly the files the
# plan will write to.
#
# Every requested file must have one, and a file the iteration does not report stops the run here.
# This is the last point at which the review can be refused without cost: it runs before the plan is
# built and before the first comment is written, so the refusal is complete and a reader knows that
# nothing reached the pull request. Answering with an incomplete map instead would carry the problem
# into publication, where the finding that could not be positioned would be discovered partway
# through a sequence that had already published its predecessors, and the operator would be left
# holding a pull request that carries part of a review.
function Get-AdoPullRequestFileIdentity {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]] $Path
    )

    $identity = [ordered]@{}

    # A review whose findings all answer existing threads anchors nothing, and asking the service
    # which files it changed would answer a question this run is not going to ask.
    if ($Path.Count -eq 0) { return $identity }

    $iterationId = Get-JsonProperty -Node $State -Name 'iterationId'
    if ($null -eq $iterationId) {
        throw 'The publication state carries no iteration, so the change tracking identifiers of the anchored files cannot be read.'
    }

    $changes = Get-AdoPullRequestIterationChange -Transport $Transport -Target $Target -IterationId $iterationId

    $trackingIdByPath = [ordered]@{}
    foreach ($change in $changes) {
        $item = Get-JsonProperty -Node $change -Name 'item'
        if ($null -eq $item) { continue }

        $canonical = ConvertFrom-AdoItemPath -Path ([string] (Get-JsonProperty -Node $item -Name 'path'))
        if ($canonical -eq '') { continue }

        $trackingId = Get-JsonProperty -Node $change -Name 'changeTrackingId'
        if ($null -eq $trackingId) { continue }

        $trackingIdByPath[$canonical] = $trackingId
    }

    # Keyed by the path the caller asked about rather than by the path the service stated, so the
    # plan builder's lookup uses the same spelling the anchor carries.
    $unlisted = [System.Collections.Generic.List[string]]::new()
    foreach ($requested in $Path) {
        $canonical = ConvertFrom-AdoItemPath -Path $requested
        if (-not $trackingIdByPath.Contains($canonical)) {
            $unlisted.Add($canonical)
            continue
        }
        $identity[$requested] = $trackingIdByPath[$canonical]
    }

    if ($unlisted.Count -gt 0) {
        # The anchors were resolved against the diff of the very commits the pull request reports as
        # its newest iteration, so a file missing from that iteration's cumulative change listing
        # means the two disagree about what this pull request changes. Naming every such file at once
        # lets the disagreement be settled in one pass rather than one file per run.
        throw "Azure DevOps does not list $($unlisted -join ', ') among the files changed by the pull request as of iteration $iterationId of pull request $(Get-JsonProperty -Node $Target -Name 'pullRequestId'), so a comment cannot be positioned on them. Review against the iteration the pull request currently reports."
    }

    return $identity
}

# Begins a review and returns the handle every later call in the run is given.
#
# Nothing is written. There is no draft to open on Azure DevOps, so the review exists only as this
# handle, which carries the iteration each line comment is anchored against and accumulates what the
# run publishes. The verification reads that accumulation: the service has no object that groups a
# run's comments, so a read of the pull request's threads would otherwise answer with everything
# anyone has ever written on it.
function Start-AdoPullRequestReview {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State
    )

    $resolved = Resolve-AdoPullRequestTarget -Transport $Transport -Target $Target

    $iterationId = Get-JsonProperty -Node $State -Name 'iterationId'
    if ($null -eq $iterationId) {
        throw 'The publication state carries no iteration, so a comment cannot be anchored against the revision the review was produced against.'
    }

    [ordered]@{
        # Azure DevOps issues no identifier for a review because it holds no review. The run
        # identifies its publication by the iteration every comment it writes is anchored to, which
        # is the narrowest true statement available and is what the record reports.
        reviewId         = "iteration/$iterationId"
        state            = $script:AdoUnpublishedReviewState
        iterationId      = $iterationId
        target           = $resolved
        published        = [System.Collections.Generic.List[object]]::new()
        summaryThreadId  = $null
        summaryCommentId = $null
    }
}

# Publishes one rendered finding, as a new thread or as a reply into an existing one.
#
# The comment is visible as soon as the service accepts it, so this is the point of no return for
# that finding and only explicit throttling refusals are retried. The body arrives already rendered, so this
# never decides how a finding reads.
function Add-AdoPullRequestReviewFinding {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Operation
    )

    $resolved = Resolve-AdoPullRequestTarget -Transport $Transport -Target $Target
    $body = [string] (Get-JsonProperty -Node $Operation -Name 'body')

    # Named apart from the $Operation parameter: PowerShell resolves variable names without regard to
    # case, so a local named $operation would rebind the typed parameter and fail on assignment.
    $operationName = [string] (Get-JsonProperty -Node $Operation -Name 'operation')

    switch ($operationName) {
        'reply' {
            $threadId = [string] (Get-JsonProperty -Node $Operation -Name 'threadId')
            $response = Invoke-AdoRestRequest `
                -Transport $Transport `
                -Method 'POST' `
                -Path (Get-AdoPullRequestResourcePath -Target $resolved -Suffix "threads/$threadId/comments") `
                -Body (New-AdoReviewCommentRequest `
                    -Content $body `
                    -ParentCommentId (Get-JsonProperty -Node $Operation -Name 'commentId')) `
                -NoRetry

            $commentId = [string] (Get-JsonProperty -Node $response -Name 'id')
            if (-not $commentId) {
                throw "Azure DevOps accepted the reply into thread '$threadId' but reported no comment identifier, so the run cannot record where the comment landed."
            }

            return (Add-AdoPublishedReviewComment -Review $Review -ThreadId $threadId -CommentId $commentId)
        }

        { $_ -ceq 'newLineThread' -or $_ -ceq 'newFileThread' } {
            $anchor = Get-JsonProperty -Node $Operation -Name 'anchor'
            $changeTrackingId = Get-JsonProperty -Node $Operation -Name 'changeTrackingId'

            # Restates the guarantee Get-AdoPullRequestFileIdentity established before the plan was
            # built: every anchored file has an identifier, or the run was refused. It is checked
            # again here because the alternative is a thread request naming no change to position
            # itself against, and a comment the service accepted on those terms is published and
            # cannot be withdrawn.
            if ($null -ne $anchor -and $null -eq $changeTrackingId) {
                $path = [string] (Get-JsonProperty -Node $anchor -Name 'path')
                throw "The publication plan carries no change tracking identifier for '$path', so a comment on that file cannot be positioned. Read the file identities before building the plan."
            }

            $response = Invoke-AdoRestRequest `
                -Transport $Transport `
                -Method 'POST' `
                -Path (Get-AdoPullRequestResourcePath -Target $resolved -Suffix 'threads') `
                -Body (New-AdoReviewThreadRequest `
                    -Content $body `
                    -Anchor $anchor `
                    -ChangeTrackingId $changeTrackingId `
                    -IterationId (Get-JsonProperty -Node $Review -Name 'iterationId')) `
                -NoRetry

            $written = Get-AdoReviewThreadResult -Response $response
            return (Add-AdoPublishedReviewComment -Review $Review -ThreadId $written['threadId'] -CommentId $written['commentId'])
        }

        default { throw "The publication operation '$operationName' is not one Azure DevOps knows how to perform." }
    }
}

# Records the running account's approval of the merge by setting its reviewer vote.
#
# The write targets the single-reviewer route, so other reviewers and their votes are untouched.
# The existing entry's required status is carried into the write because omitting isRequired makes
# the reviewer optional (docs/implementation.md, "Publication"). The collection read also handles
# an absent reviewer without mistaking a failed lookup for absence; the write then creates an
# optional entry. Recording the same vote again lets a reconciled completion re-cast it.
#
# The target this is given must already be resolved, because only a resolved target carries the
# project and repository the reviewer route is built from.
function Set-AdoPullRequestApproval {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $identity = Get-AdoAuthenticatedIdentity -Transport $Transport
    $reviewerId = [string] (Get-JsonProperty -Node $identity -Name 'id')
    if ([string]::IsNullOrWhiteSpace($reviewerId)) {
        throw 'Approving the merge requires an authenticated identity carrying an account identifier.'
    }

    $body = [ordered]@{
        id         = $reviewerId
        vote       = $script:AdoApproveVote
        isRequired = $false
    }

    $reviewerNodes = Invoke-AdoRestListRequest -Transport $Transport -Path (Get-AdoPullRequestResourcePath -Target $Target -Suffix 'reviewers')
    foreach ($reviewer in $reviewerNodes) {
        if (Test-PullRequestIdentityMayMatch -Left (New-AdoIdentity -Node $reviewer) -Right $identity) {
            $body['isRequired'] = [bool] (Get-JsonProperty -Node $reviewer -Name 'isRequired' -Default $false)
            break
        }
    }

    $suffix = "reviewers/$([uri]::EscapeDataString($reviewerId))"
    Invoke-AdoRestRequest -Transport $Transport -Method 'PUT' -Path (Get-AdoPullRequestResourcePath -Target $Target -Suffix $suffix) -Body $body | Out-Null
}

# Finalizes the review by posting the summary onto the pull request itself.
#
# The summary counts the findings it accompanies, so it is written after them and carries no
# position of its own: it is about the review rather than about a line of code.
#
# When the user approved the merge during the audit, the running account's approving vote is recorded
# first, before the summary. The summary post cannot be repeated without duplicating a comment, so it
# is the last write of the completion; the vote is idempotent, so a reconciled re-run can re-cast it
# without consequence (docs/design.md, "Review publication").
function Complete-AdoPullRequestReview {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $SummaryBody,

        [switch] $ApproveMerge
    )

    $resolved = Resolve-AdoPullRequestTarget -Transport $Transport -Target $Target

    if ($ApproveMerge) {
        Set-AdoPullRequestApproval -Transport $Transport -Target $resolved
    }

    $response = Invoke-AdoRestRequest `
        -Transport $Transport `
        -Method 'POST' `
        -Path (Get-AdoPullRequestResourcePath -Target $resolved -Suffix 'threads') `
        -Body (New-AdoReviewThreadRequest `
            -Content $SummaryBody `
            -Status $script:AdoReviewSummaryThreadStatus) `
        -NoRetry

    $written = Get-AdoReviewThreadResult -Response $response
    Set-JsonProperty -Node $Review -Name 'summaryThreadId' -Value $written['threadId']
    Set-JsonProperty -Node $Review -Name 'summaryCommentId' -Value $written['commentId']
    Set-JsonProperty -Node $Review -Name 'state' -Value $script:AdoPublishedReviewState

    [ordered]@{
        reviewId = [string] (Get-JsonProperty -Node $Review -Name 'reviewId')
        state    = $script:AdoPublishedReviewState
        url      = Get-AdoPullRequestThreadUrl -Target $resolved -ThreadId $written['threadId']
    }
}

# Reads back the findings before the summary is posted.
#
# Azure DevOps has no draft review, so this is a read of already visible threads. The review-level
# visibility remains false because the pull-request-level summary is the boundary that makes the
# collection a complete review.
function Confirm-AdoPullRequestReviewFindings {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review
    )

    Confirm-AdoPullRequestReviewCore -Transport $Transport -Target $Target -Review $Review
}

# Reads back what the review published.
#
# The read is of the pull request's threads, which is everything anyone has written on it, so the
# answer is narrowed to the comments this run recorded as written. Each is reported as published or
# not, which lets the platform-neutral check count them without knowing Azure DevOps' vocabulary.
# The summary decides whether the review as a whole is visible: it is written last, so a pull request
# carrying it carries everything that went before.
function Confirm-AdoPullRequestReview {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review
    )

    Confirm-AdoPullRequestReviewCore -Transport $Transport -Target $Target -Review $Review -IncludeSummary
}

# Reads back this run's Azure DevOps comments and, when asked, its summary.
function Confirm-AdoPullRequestReviewCore {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review,

        [switch] $IncludeSummary
    )

    $resolved = Resolve-AdoPullRequestTarget -Transport $Transport -Target $Target
    $threadNodes = Invoke-AdoRestListRequest -Transport $Transport -Path (Get-AdoPullRequestResourcePath -Target $resolved -Suffix 'threads')

    $threadsById = [ordered]@{}
    foreach ($thread in $threadNodes) {
        $id = [string] (Get-JsonProperty -Node $thread -Name 'id')
        if ($id -eq '') { continue }
        $threadsById[$id] = $thread
    }

    $summaryThreadId = ''
    $summaryComment = $null
    if ($IncludeSummary) {
        $summaryThreadId = [string] (Get-JsonProperty -Node $Review -Name 'summaryThreadId')
        $summaryComment = Find-AdoPublishedComment `
            -ThreadsById $threadsById `
            -ThreadId $summaryThreadId `
            -CommentId ([string] (Get-JsonProperty -Node $Review -Name 'summaryCommentId'))
    }

    # The summary is deliberately absent from the comment listing. The check that consumes it counts
    # published comments against the findings the run wrote, and a summary counted among them would
    # make up for exactly one finding that never arrived.
    $comments = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in $Review['published']) {
        $threadId = [string] (Get-JsonProperty -Node $entry -Name 'threadId')
        $commentId = [string] (Get-JsonProperty -Node $entry -Name 'commentId')
        $comment = Find-AdoPublishedComment -ThreadsById $threadsById -ThreadId $threadId -CommentId $commentId

        $thread = if ($threadsById.Contains($threadId)) { $threadsById[$threadId] } else { $null }
        $context = if ($null -eq $thread) { $null } else { ConvertTo-AdoThreadContext -ThreadContext (Get-JsonProperty -Node $thread -Name 'threadContext') }

        $isPublished = ($null -ne $comment)
        $rawStatus = if ($null -eq $thread) { '' } else { [string] (Get-JsonProperty -Node $thread -Name 'status') }

        $comments.Add([ordered]@{
                id          = $commentId
                # The thread accompanies the comment identifier because Azure DevOps numbers comments
                # within their own thread, so the identifier alone does not locate the comment a
                # reader wants to open.
                threadId    = $threadId
                path        = ConvertFrom-AdoItemPath -Path ([string] (Get-JsonProperty -Node $context -Name 'filePath'))
                line        = Get-AdoPublishedCommentLine -Context $context
                subjectType = Get-AdoPublishedCommentSubject -Context $context
                # Comment state names visibility, matching the review-level state vocabulary.
                # Azure DevOps thread status is discussion resolution rather than visibility, so it
                # is carried as rawStatus in the same style the capture uses for thread resolution.
                state       = if ($isPublished) { $script:AdoPublishedReviewState } else { $script:AdoUnpublishedReviewState }
                rawStatus   = $rawStatus
                isPublished = $isPublished
                body        = if ($null -eq $comment) { '' } else { [string] (Get-JsonProperty -Node $comment -Name 'content') }
            })
    }

    [ordered]@{
        reviewId    = [string] (Get-JsonProperty -Node $Review -Name 'reviewId')
        state       = if ($null -eq $summaryComment) { $script:AdoUnpublishedReviewState } else { $script:AdoPublishedReviewState }
        url         = if ($summaryThreadId -eq '') { '' } else { Get-AdoPullRequestThreadUrl -Target $resolved -ThreadId $summaryThreadId }
        body        = if ($null -eq $summaryComment) { '' } else { [string] (Get-JsonProperty -Node $summaryComment -Name 'content') }
        isPublished = ($null -ne $summaryComment)
        comments    = @($comments)
    }
}

# Reopens a settled thread this run replied into.
#
# Azure DevOps accepts a reply into a thread whatever its status, so the reopen operation follows
# the reply rather than clearing the way for it. Running the reopen operation first would confront
# the thread's participants with a reopened discussion and no reason attached, and a run that then
# failed would leave it that way.
function Resume-AdoReviewThread {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [string] $ThreadId
    )

    $resolved = Resolve-AdoPullRequestTarget -Transport $Transport -Target $Target

    Invoke-AdoRestRequest `
        -Transport $Transport `
        -Method 'PATCH' `
        -Path (Get-AdoPullRequestResourcePath -Target $resolved -Suffix "threads/$ThreadId") `
        -Body ([ordered]@{ status = $script:AdoActiveThreadStatus }) `
        -NoRetry | Out-Null
}

# Refuses the discard of a review Azure DevOps never held unpublished.
#
# The caller reaches here while cleaning up after a run that failed before it finished, and the
# honest answer is that there is nothing to clean up and nothing to undo: every comment the run
# managed to write is already on the pull request. Reporting success would state that the failed
# run left no trace, which is the one thing a reader must not believe.
function Remove-AdoPullRequestPendingReview {
    param(
        [Parameter(Mandatory)]
        [string] $ReviewId
    )

    throw "Azure DevOps holds no draft review to discard for '$ReviewId'. Every comment the run wrote was published as it was written, so the publication result is the account of what reached the pull request."
}

# Builds the request that creates one thread.
#
# An anchored finding carries the file it is about and, for a line anchor, the region within it; a
# thread with no anchor is about the pull request and carries no position at all. The iteration
# context accompanies every anchored thread so that the service can follow the comment forward as
# later iterations move the code it refers to.
function New-AdoReviewThreadRequest {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Content,

        [AllowNull()]
        $Anchor,

        [AllowNull()]
        $ChangeTrackingId,

        [AllowNull()]
        $IterationId,

        [string] $Status = $script:AdoActiveThreadStatus
    )

    $request = [ordered]@{
        comments = @((New-AdoReviewCommentRequest -Content $Content))
        status   = $Status
    }

    if ($null -eq $Anchor) { return $request }

    $threadContext = [ordered]@{
        filePath = ConvertTo-AdoItemPath -Path ([string] (Get-JsonProperty -Node $Anchor -Name 'path'))
    }

    if ([string] (Get-JsonProperty -Node $Anchor -Name 'kind') -ceq 'line') {
        # The anchor names the side of the diff the finding was judged against, and Azure DevOps
        # expresses a position on the pre-change side under different names than one on the
        # post-change side, so the side selects the pair rather than being carried alongside it.
        $isLeftSide = [string] (Get-JsonProperty -Node $Anchor -Name 'side' -Default 'right') -ceq 'left'
        $startName = if ($isLeftSide) { 'leftFileStart' } else { 'rightFileStart' }
        $endName = if ($isLeftSide) { 'leftFileEnd' } else { 'rightFileEnd' }

        $endLine = [int] (Get-JsonProperty -Node $Anchor -Name 'endLine')
        $startLine = [int] (Get-JsonProperty -Node $Anchor -Name 'startLine' -Default $endLine)

        $threadContext[$startName] = [ordered]@{ line = $startLine; offset = $script:AdoThreadAnchorStartOffset }
        $threadContext[$endName] = [ordered]@{ line = $endLine; offset = $script:AdoThreadAnchorEndOffset }
    }

    $request['threadContext'] = $threadContext
    $request['pullRequestThreadContext'] = [ordered]@{
        changeTrackingId = $ChangeTrackingId
        iterationContext = [ordered]@{
            # Both ends name the iteration the review was produced against, because the comment is
            # about that revision as it stands rather than about what one revision changed relative
            # to another.
            firstComparingIteration  = $IterationId
            secondComparingIteration = $IterationId
        }
    }

    return $request
}

# Builds the request that creates one comment, whether it opens a thread or answers one.
function New-AdoReviewCommentRequest {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Content,

        [AllowNull()]
        $ParentCommentId
    )

    $parent = if ($null -eq $ParentCommentId -or [string] $ParentCommentId -eq '') {
        $script:AdoRootCommentParentId
    } else {
        [int] $ParentCommentId
    }

    [ordered]@{
        parentCommentId = $parent
        content         = $Content
        commentType     = $script:AdoTextCommentType
    }
}

# Reads the identifiers out of a thread creation response.
#
# Comment identifiers are ordinals within their own thread rather than identifiers of the pull
# request, so the pair is what locates the comment and neither half stands alone.
function Get-AdoReviewThreadResult {
    param(
        [Parameter(Mandatory)]
        $Response
    )

    $threadId = [string] (Get-JsonProperty -Node $Response -Name 'id')
    if (-not $threadId) {
        throw 'Azure DevOps accepted the request to create a thread but reported no thread identifier, so the run cannot record where the comment landed.'
    }

    $comments = Get-JsonArrayProperty -Node $Response -Name 'comments'
    $openingIndex = Get-AdoThreadOpeningCommentIndex -Comments $comments
    if ($openingIndex -lt 0) {
        throw "Azure DevOps accepted the request to create thread '$threadId' but reported no comment in it, so the run cannot record what it published."
    }

    [ordered]@{
        threadId  = $threadId
        commentId = [string] (Get-JsonProperty -Node $comments[$openingIndex] -Name 'id')
    }
}

# Records one published comment on the review handle and reports it to the caller.
#
# The record on the handle is what the verification reads back; the return value is what the run
# writes into the publication result. Both are produced here so that a comment cannot reach one
# without reaching the other.
function Add-AdoPublishedReviewComment {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review,

        [Parameter(Mandatory)]
        [string] $ThreadId,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $CommentId
    )

    $written = [ordered]@{
        threadId  = $ThreadId
        commentId = $CommentId
    }

    # Read through the indexer rather than through the JSON accessor: PowerShell unrolls a collection
    # a function returns, so an accessor would hand back the accumulated entries instead of the list
    # holding them, and an empty accumulation would come back as nothing at all.
    $published = $Review['published']
    $published.Add($written)

    return $written
}

# Locates one comment among the threads read back, reporting absence rather than refusing.
#
# A comment the service no longer reports is exactly what the verification exists to detect, so its
# absence is an answer rather than a fault. A comment its author has since withdrawn counts as
# absent too: the pull request no longer carries what the run published.
function Find-AdoPublishedComment {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $ThreadsById,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $ThreadId,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $CommentId
    )

    if ($ThreadId -eq '' -or -not $ThreadsById.Contains($ThreadId)) { return $null }

    $thread = $ThreadsById[$ThreadId]
    if (Test-AdoDeletedNode -Node $thread) { return $null }

    $comments = Get-JsonArrayProperty -Node $thread -Name 'comments'
    foreach ($comment in $comments) {
        if ([string] (Get-JsonProperty -Node $comment -Name 'id') -cne $CommentId) { continue }
        if (Test-AdoDeletedNode -Node $comment) { return $null }
        return $comment
    }

    return $null
}

# Reports the line a published comment sits on, whichever side of the diff it was written against.
#
# The last line of the anchored region is reported rather than the first, because that is the line
# the GitHub adapter reports under the same name, and a reader comparing the two platforms'
# verification records must not find one concept measured from opposite ends.
function Get-AdoPublishedCommentLine {
    param(
        [AllowNull()]
        $Context
    )

    if ($null -eq $Context) { return $null }

    foreach ($name in @('endLine', 'startLine', 'originalEndLine', 'originalStartLine')) {
        $line = Get-JsonProperty -Node $Context -Name $name
        if ($null -ne $line) { return $line }
    }
    return $null
}

# Reports what a published comment is attached to.
function Get-AdoPublishedCommentSubject {
    param(
        [AllowNull()]
        $Context
    )

    if ($null -eq $Context) { return $script:AdoPullRequestCommentSubject }
    if ($null -ne (Get-AdoPublishedCommentLine -Context $Context)) { return $script:AdoLineCommentSubject }
    return $script:AdoFileCommentSubject
}
