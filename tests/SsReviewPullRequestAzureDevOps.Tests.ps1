BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:LibraryRoot = Join-Path $script:RepoRoot 'agents\.github\skills\ss-review\scripts\lib'

    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PublicationFailure.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'AdoRestTransport.ps1')
    . (Join-Path $script:LibraryRoot 'AdoPullRequestApi.ps1')

    # The suite is entirely offline. Every request and every wait is injected, so no assertion here
    # depends on a network, on a service being reachable, or on wall-clock time passing.

    # Characters are built from code points rather than typed literally, so this file stays pure
    # ASCII while the fixtures it produces do not. That is the point of the exercise: the bytes on
    # the wire carry the non-ASCII, and the assertions compare code points rather than appearance.
    $script:EmDash = [char] 0x2014
    $script:LeftDoubleQuote = [char] 0x201C
    $script:RightDoubleQuote = [char] 0x201D
    $script:RobotFace = [char]::ConvertFromUtf32(0x1F916)
    $script:SpecialContent = "Alpha $($script:EmDash) beta $($script:LeftDoubleQuote)gamma$($script:RightDoubleQuote) delta $($script:RobotFace) omega"

    # A comment body that is itself an ISO-8601 timestamp. ConvertFrom-Json would rewrite this into a
    # DateTime and back into locale-shaped text; the capture must return the author's exact string.
    $script:TimestampContent = '2026-08-04T10:07:33.677Z'

    function Get-CodePointText {
        param(
            [Parameter(Mandatory)]
            [AllowEmptyString()]
            [string] $Value
        )

        $points = [System.Collections.Generic.List[string]]::new()
        for ($index = 0; $index -lt $Value.Length; $index++) {
            $points.Add(('U+{0:X4}' -f [char]::ConvertToUtf32($Value, $index)))
            if ([char]::IsHighSurrogate($Value[$index])) { $index++ }
        }
        $points -join ' '
    }

    function Test-ByteSequence {
        param(
            [Parameter(Mandatory)]
            [byte[]] $Haystack,

            [Parameter(Mandatory)]
            [byte[]] $Needle
        )

        for ($start = 0; $start -le $Haystack.Length - $Needle.Length; $start++) {
            $matched = $true
            for ($offset = 0; $offset -lt $Needle.Length; $offset++) {
                if ($Haystack[$start + $offset] -ne $Needle[$offset]) { $matched = $false; break }
            }
            if ($matched) { return $true }
        }
        return $false
    }

    # Quotes a string as a JSON literal while leaving every non-ASCII character raw, so the encoded
    # fixture really does carry multi-byte UTF-8 rather than \u escapes.
    function ConvertTo-TestJsonString {
        param(
            [Parameter(Mandatory)]
            [AllowEmptyString()]
            [string] $Value
        )

        $builder = [System.Text.StringBuilder]::new()
        [void] $builder.Append('"')
        foreach ($character in $Value.ToCharArray()) {
            switch ($character) {
                '"' { [void] $builder.Append('\"'); break }
                '\' { [void] $builder.Append('\\'); break }
                "`n" { [void] $builder.Append('\n'); break }
                "`r" { [void] $builder.Append('\r'); break }
                "`t" { [void] $builder.Append('\t'); break }
                default {
                    if ([int] $character -lt 32) {
                        [void] $builder.Append(('\u{0:x4}' -f [int] $character))
                    } else {
                        [void] $builder.Append($character)
                    }
                }
            }
        }
        [void] $builder.Append('"')
        $builder.ToString()
    }

    function New-AdoTestResponse {
        param(
            [string] $Text,
            [byte[]] $Bytes,
            [int] $StatusCode = 200,
            [string] $ContentType = 'application/json; charset=utf-8',
            [hashtable] $Headers = @{}
        )

        $contentBytes = if ($null -ne $Bytes) {
            $Bytes
        } elseif ($null -ne $Text) {
            [System.Text.Encoding]::UTF8.GetBytes($Text)
        } else {
            [byte[]]::new(0)
        }

        [ordered]@{
            statusCode   = $StatusCode
            headers      = $Headers
            contentType  = $ContentType
            contentBytes = $contentBytes
        }
    }

    # Builds a transport whose HTTP call and whose wait are both fakes.
    #
    # Requests and requested delays are recorded so a test can assert what was sent and how long the
    # transport asked to wait, without anything being sent and without anything waiting.
    #
    # The recorded state is held at script scope rather than captured into a closure, because a
    # closure is bound to its own module and would no longer resolve the helper functions this file
    # defines. One transport is built per test, so a single set of recorders is enough.
    function New-FakeAdoTransport {
        param(
            [Parameter(Mandatory)]
            [scriptblock] $Respond,

            [string] $CollectionUrl = 'https://example.visualstudio.com',

            [int] $MaxAttempts = 5,

            [string] $Token = 'top-secret-bearer-material',

            # Successive tokens handed out by the fake acquirer, so a test can prove that a refresh
            # really re-acquires rather than replaying what it already held. The final entry is
            # returned for every acquisition beyond the sequence.
            [string[]] $Tokens
        )

        $script:FakeRequests = [System.Collections.Generic.List[object]]::new()
        $script:FakeWaits = [System.Collections.Generic.List[int]]::new()
        $script:FakeRespond = $Respond
        $script:FakeToken = $Token

        # A list rather than an array: assigning an array through an if statement lets the pipeline
        # unwrap a single-element one into a bare string, which then has no Count to read.
        $script:FakeTokens = [System.Collections.Generic.List[string]]::new()
        if ($null -ne $Tokens) {
            foreach ($value in $Tokens) { $script:FakeTokens.Add($value) }
        }
        if ($script:FakeTokens.Count -eq 0) { $script:FakeTokens.Add($Token) }
        $script:FakeTokenAcquisitions = 0

        $send = {
            param(
                [string] $Method,
                [string] $Url,
                [hashtable] $Headers,
                $Body
            )

            # Index is the ordinal of this call across the whole transport. A test that issues one
            # logical request reads it as the attempt number.
            $request = [pscustomobject]@{
                Method  = $Method
                Url     = $Url
                Headers = $Headers
                Body    = $Body
                Index   = $script:FakeRequests.Count + 1
            }
            $script:FakeRequests.Add($request)
            # Match the transport's configurable transient-budget ceiling, beyond these finite
            # fixtures, so a broken unbounded retry cannot hang the offline suite.
            if ($script:FakeRequests.Count -gt 100) { throw 'The fake Azure DevOps sender exceeded its request budget.' }
            & $script:FakeRespond $request
        }

        $wait = {
            param([int] $Milliseconds)
            $script:FakeWaits.Add($Milliseconds)
        }

        $getToken = {
            $script:FakeTokenAcquisitions++
            $index = [Math]::Min($script:FakeTokenAcquisitions, $script:FakeTokens.Count) - 1
            $script:FakeTokens[$index]
        }

        [pscustomobject]@{
            Transport = New-AdoRestTransport -CollectionUrl $CollectionUrl -SendRequest $send -GetAccessToken $getToken -Wait $wait -MaxAttempts $MaxAttempts
            Requests  = $script:FakeRequests
            Waits     = $script:FakeWaits
            Token     = $Token
        }
    }

    $script:ConnectionDataJson = @'
{
  "authenticatedUser": {
    "id": "11111111-1111-1111-1111-111111111111",
    "descriptor": "Microsoft.IdentityModel.Claims.ClaimsIdentity;tenant\\reviewer@example.com",
    "subjectDescriptor": "aad.cmV2aWV3ZXI",
    "providerDisplayName": "Review Runner",
    "properties": { "Account": { "$type": "System.String", "$value": "reviewer@example.com" } }
  }
}
'@

    $script:OrgPullRequestJson = @'
{
  "pullRequestId": 4242,
  "repository": {
    "id": "22222222-2222-2222-2222-222222222222",
    "name": "example repo",
    "project": { "id": "33333333-3333-3333-3333-333333333333", "name": "Example Project" }
  }
}
'@

    # lastMergeTargetCommit names the target branch head, which is deliberately not the merge base
    # any iteration reports. A capture that took the merge base from here would diff against work
    # that landed on the target branch after this pull request was last pushed.
    $script:PullRequestJson = @'
{
  "pullRequestId": 4242,
  "status": "active",
  "createdBy": {
    "id": "author-id",
    "displayName": "Pull Request Author",
    "uniqueName": "author@example.com",
    "descriptor": "aad.YXV0aG9y"
  },
  "creationDate": "2026-08-03T11:44:10.8515947Z",
  "title": "fix(io): improve queue coordination",
  "description": "Motivation and changes.",
  "sourceRefName": "refs/heads/u/reviewer/topic",
  "targetRefName": "refs/heads/master",
  "mergeStatus": "succeeded",
  "isDraft": false,
  "mergeId": "44444444-4444-4444-4444-444444444444",
  "lastMergeSourceCommit": { "commitId": "source-commit-2" },
  "lastMergeTargetCommit": { "commitId": "target-branch-head" },
  "lastMergeCommit": { "commitId": "merge-preview-commit" }
}
'@

    # Returned newest first, and the older iteration omits its reason, so the capture is held to
    # selecting by identifier rather than by position and to tolerating an absent property.
    $script:IterationsJson = @'
{
  "count": 2,
  "value": [
    {
      "id": 2,
      "createdDate": "2026-08-04T11:41:20.5289569Z",
      "reason": "push",
      "sourceRefCommit": { "commitId": "source-commit-2" },
      "targetRefCommit": { "commitId": "target-head-2" },
      "commonRefCommit": { "commitId": "merge-base-2" }
    },
    {
      "id": 1,
      "createdDate": "2026-08-03T11:44:10.8662081Z",
      "sourceRefCommit": { "commitId": "source-commit-1" },
      "targetRefCommit": { "commitId": "target-head-1" },
      "commonRefCommit": { "commitId": "merge-base-1" }
    }
  ]
}
'@

    # The team entry omits isRequired and isContainer, which Azure DevOps does for entries that carry
    # neither flag, and which strict mode turns into a terminating error on direct member access.
    $script:ReviewersJson = @'
{
  "count": 2,
  "value": [
    {
      "id": "reviewer-existing-id",
      "displayName": "Existing Reviewer",
      "uniqueName": "existing@example.com",
      "vote": 10,
      "isRequired": true,
      "isContainer": false
    },
    {
      "id": "team-id",
      "displayName": "[Example Project]\\Approvers",
      "uniqueName": "vstfs:///Classification/TeamProject/approvers",
      "vote": 0
    }
  ]
}
'@

    $script:ThreadsJsonTemplate = @'
{
  "count": 4,
  "value": [
    {
      "id": 101,
      "publishedDate": "2026-08-03T12:00:00.000Z",
      "lastUpdatedDate": "2026-08-03T12:30:00.000Z",
      "status": "active",
      "threadContext": {
        "filePath": "/src/lib.rs",
        "rightFileStart": { "line": 143, "offset": 1 },
        "rightFileEnd": { "line": 145, "offset": 9 },
        "leftFileStart": { "line": 120, "offset": 1 },
        "leftFileEnd": { "line": 121, "offset": 4 }
      },
      "pullRequestThreadContext": {
        "iterationContext": { "firstComparingIteration": 1, "secondComparingIteration": 1 },
        "changeTrackingId": 14
      },
      "properties": {
        "Microsoft.TeamFoundation.Discussion.UniqueID": { "$type": "System.String", "$value": "55555555555555555555555555555555" },
        "SupportsMarkdown": { "$type": "System.Int32", "$value": 1 }
      },
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "human-id", "displayName": "Review Author", "uniqueName": "human@example.com", "descriptor": "aad.aHVtYW4" },
          "content": __SPECIAL__,
          "publishedDate": "2026-08-03T12:00:00.000Z",
          "lastUpdatedDate": "2026-08-03T12:00:00.000Z",
          "lastContentUpdatedDate": "2026-08-03T12:00:00.000Z",
          "commentType": "text"
        },
        {
          "id": 2,
          "parentCommentId": 1,
          "author": { "id": "human-id", "displayName": "Review Author", "uniqueName": "human@example.com", "descriptor": "aad.aHVtYW4" },
          "content": __TIMESTAMP__,
          "publishedDate": "2026-08-03T12:15:00.000Z",
          "lastUpdatedDate": "2026-08-03T12:20:00.000Z",
          "lastContentUpdatedDate": "2026-08-03T12:20:00.000Z"
        },
        {
          "id": 3,
          "parentCommentId": 1,
          "author": { "id": "human-id", "displayName": "Review Author", "uniqueName": "human@example.com", "descriptor": "aad.aHVtYW4" },
          "content": "Withdrawn.",
          "publishedDate": "2026-08-03T12:25:00.000Z",
          "isDeleted": true,
          "commentType": "text"
        }
      ]
    },
    {
      "id": 102,
      "publishedDate": "2026-08-04T09:00:00.000Z",
      "lastUpdatedDate": "2026-08-04T09:00:00.000Z",
      "identities": {
        "1": { "id": "voter-id", "displayName": "Vote Caster", "uniqueName": "voter@example.com", "descriptor": "aad.dm90ZXI" }
      },
      "properties": {
        "CodeReviewThreadType": { "$type": "System.String", "$value": "VoteUpdate" },
        "CodeReviewVoteResult": { "$type": "System.String", "$value": "5" },
        "CodeReviewVotedByIdentity": { "$type": "System.String", "$value": "1" }
      },
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "notifier-id", "displayName": "Microsoft.VisualStudio.Services.TFS", "descriptor": "s2s.bm90aWZpZXI" },
          "content": "Vote Caster voted 5",
          "publishedDate": "2026-08-04T10:07:33.677Z",
          "commentType": "system"
        }
      ]
    },
    {
      "id": 103,
      "publishedDate": "2026-08-04T09:30:00.000Z",
      "lastUpdatedDate": "2026-08-04T09:30:00.000Z",
      "status": "byDesign",
      "properties": {
        "CodeReviewThreadType": { "$type": "System.String", "$value": "SomeFutureUpdate" },
        "LLMModel": { "$type": "System.String", "$value": "gpt-5" }
      },
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "automation-id", "displayName": "Automation", "uniqueName": "automation@example.com" },
          "content": "Automated finding.",
          "publishedDate": "2026-08-04T09:30:00.000Z",
          "commentType": "system"
        }
      ]
    },
    {
      "id": 104,
      "publishedDate": "2026-08-04T11:00:00.000Z",
      "lastUpdatedDate": "2026-08-04T11:05:00.000Z",
      "status": "fixed",
      "pullRequestThreadContext": {
        "iterationContext": { "firstComparingIteration": 2, "secondComparingIteration": 2 },
        "changeTrackingId": 21
      },
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "human-id", "displayName": "Review Author", "uniqueName": "human@example.com", "descriptor": "aad.aHVtYW4" },
          "content": "Addressed.",
          "publishedDate": "2026-08-04T11:00:00.000Z",
          "commentType": "text"
        }
      ]
    }
  ]
}
'@

    $script:ThreadsJson = $script:ThreadsJsonTemplate.
        Replace('__SPECIAL__', (ConvertTo-TestJsonString -Value $script:SpecialContent)).
        Replace('__TIMESTAMP__', (ConvertTo-TestJsonString -Value $script:TimestampContent))
    $script:ThreadsBytes = [System.Text.Encoding]::UTF8.GetBytes($script:ThreadsJson)

    # Project and repository names carry a space so that path construction is held to url-encoding
    # them rather than emitting a URL the service would reject.
    function New-TestTarget {
        param(
            [switch] $Unresolved
        )

        [ordered]@{
            platform      = 'azureDevOps'
            collectionUrl = 'https://example.visualstudio.com'
            organization  = 'example'
            project       = if ($Unresolved) { $null } else { 'Example Project' }
            repository    = if ($Unresolved) { $null } else { 'example repo' }
            pullRequestId = 4242
            webUrl        = $null
        }
    }

    # Serves the whole pull request surface from the fixtures, with per-test overrides.
    function New-AdoContextTransport {
        param(
            [string] $ThreadsJson,
            [string] $ReviewersJson,
            [string] $IterationsJson,
            [string] $PullRequestJson,
            [scriptblock] $OnReviewerWrite
        )

        $script:ServedThreadsJson = if ($ThreadsJson) { $ThreadsJson } else { $script:ThreadsJson }
        $script:ServedReviewersJson = if ($ReviewersJson) { $ReviewersJson } else { $script:ReviewersJson }
        $script:ServedIterationsJson = if ($IterationsJson) { $IterationsJson } else { $script:IterationsJson }
        $script:ServedPullRequestJson = if ($PullRequestJson) { $PullRequestJson } else { $script:PullRequestJson }
        $script:ServedReviewerWrite = $OnReviewerWrite

        $respond = {
            param($Request)

            $url = $Request.Url
            if ($url -match '/_apis/connectionData\?') {
                return New-AdoTestResponse -Text $script:ConnectionDataJson
            }
            if ($url -match '/_apis/git/repositories/') {
                if ($url -match '/threads\?') { return New-AdoTestResponse -Text $script:ServedThreadsJson }
                if ($url -match '/iterations\?') { return New-AdoTestResponse -Text $script:ServedIterationsJson }
                if ($url -match '/reviewers/[^/?]+\?') {
                    if ($null -eq $script:ServedReviewerWrite) { throw "unexpected reviewer write to $url" }
                    return & $script:ServedReviewerWrite $Request
                }
                if ($url -match '/reviewers\?') {
                    if ($null -ne $script:ServedReviewerWrite) { return & $script:ServedReviewerWrite $Request }
                    return New-AdoTestResponse -Text $script:ServedReviewersJson
                }
                return New-AdoTestResponse -Text $script:ServedPullRequestJson
            }
            if ($url -match '/_apis/git/pullrequests/\d+\?') {
                return New-AdoTestResponse -Text $script:OrgPullRequestJson
            }
            throw "unexpected request to $url"
        }

        New-FakeAdoTransport -Respond $respond
    }

    # A pull request carrying exactly one of everything.
    #
    # This is the shape that exposes the array handling, because a collection holding a single
    # element is indistinguishable from that element once PowerShell has unrolled it.
    $script:SingleThreadJson = @'
{
  "count": 1,
  "value": [
    {
      "id": 301,
      "publishedDate": "2026-08-04T09:00:00.000Z",
      "lastUpdatedDate": "2026-08-04T09:00:00.000Z",
      "status": "active",
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "human-id", "displayName": "Review Author", "descriptor": "aad.aHVtYW4" },
          "content": "Only comment.",
          "publishedDate": "2026-08-04T09:00:00.000Z",
          "commentType": "text"
        }
      ]
    }
  ]
}
'@

    $script:SingleIterationJson = @'
{
  "count": 1,
  "value": [
    {
      "id": 1,
      "createdDate": "2026-08-03T11:44:10.8662081Z",
      "reason": "push",
      "sourceRefCommit": { "commitId": "only-source" },
      "targetRefCommit": { "commitId": "only-target" },
      "commonRefCommit": { "commitId": "only-merge-base" }
    }
  ]
}
'@

    $script:SingleReviewerJson = @'
{
  "count": 1,
  "value": [
    { "id": "only-reviewer-id", "displayName": "Only Reviewer", "vote": -5 }
  ]
}
'@

    # Every documented vote value plus one the service does not document. None of it reaches the
    # capture, which is what the reviewer listing is held to.
    $script:VoteSpectrumReviewersJson = @'
{
  "count": 6,
  "value": [
    { "id": "vote-approved", "displayName": "A", "vote": 10 },
    { "id": "vote-approved-with-suggestions", "displayName": "B", "vote": 5 },
    { "id": "vote-none", "displayName": "C", "vote": 0 },
    { "id": "vote-waiting", "displayName": "D", "vote": -5 },
    { "id": "vote-rejected", "displayName": "E", "vote": -10 },
    { "id": "vote-undocumented", "displayName": "F", "vote": 7 }
  ]
}
'@

    # A reviewer whose account identifier differs from the identity the review presents, while the
    # account name is the same account. This is the shape a delegated token produces.
    $script:AliasReviewersJson = @'
{
  "count": 1,
  "value": [
    {
      "id": "reviewer-identifier-as-the-service-stored-it",
      "displayName": "Review Runner",
      "uniqueName": "Reviewer@Example.com",
      "descriptor": "aad.cmV2aWV3ZXI",
      "vote": 10,
      "isRequired": true
    }
  ]
}
'@

    $script:EmptyCollectionJson = '{ "count": 0, "value": [] }'

    # A thread an automated reviewer opened and a person replied to.
    #
    # The property bag markers are set once, by the tool that posts the thread, so they describe the
    # opening comment and nothing else. The reply carries a human subject descriptor and no agent
    # content marker, which is what a person disputing an automated finding looks like. Both comments
    # report parentCommentId 0, as the service does on every comment of a thread.
    $script:AgentThreadWithReplyJson = @'
{
  "count": 1,
  "value": [
    {
      "id": 501,
      "publishedDate": "2026-08-04T09:00:00.000Z",
      "lastUpdatedDate": "2026-08-04T09:40:00.000Z",
      "status": "active",
      "properties": {
        "LLMModel": { "$type": "System.String", "$value": "gpt-5" },
        "Focus_Area": { "$type": "System.String", "$value": "correctness" }
      },
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "automation-id", "displayName": "Automation", "uniqueName": "automation@example.com", "descriptor": "s2s.YXV0b21hdGlvbg" },
          "content": "Automated finding: this guard is missing.",
          "publishedDate": "2026-08-04T09:00:00.000Z",
          "commentType": "text"
        },
        {
          "id": 2,
          "parentCommentId": 0,
          "author": { "id": "person-id", "displayName": "Real Person", "uniqueName": "person@example.com", "descriptor": "aad.cGVyc29u" },
          "content": "The finding is wrong. The guard two lines above already covers that case.",
          "publishedDate": "2026-08-04T09:40:00.000Z",
          "commentType": "text"
        }
      ]
    }
  ]
}
'@

    # The same thread with the reply serialized ahead of the comment that opened it, so the opener is
    # identified by its identifier rather than by the order the service happened to send.
    $script:AgentThreadReorderedJson = @'
{
  "count": 1,
  "value": [
    {
      "id": 502,
      "publishedDate": "2026-08-04T09:00:00.000Z",
      "lastUpdatedDate": "2026-08-04T09:40:00.000Z",
      "status": "active",
      "properties": {
        "LLMModel": { "$type": "System.String", "$value": "gpt-5" }
      },
      "comments": [
        {
          "id": 2,
          "parentCommentId": 0,
          "author": { "id": "person-id", "displayName": "Real Person", "uniqueName": "person@example.com", "descriptor": "aad.cGVyc29u" },
          "content": "Disagreed.",
          "publishedDate": "2026-08-04T09:40:00.000Z",
          "commentType": "text"
        },
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "person-id", "displayName": "Real Person", "uniqueName": "person@example.com", "descriptor": "aad.cGVyc29u" },
          "content": "Automated finding posted under a human account.",
          "publishedDate": "2026-08-04T09:00:00.000Z",
          "commentType": "text"
        }
      ]
    }
  ]
}
'@

    # A thread whose comments were all deleted, leaving the thread present with nothing in it.
    $script:AllDeletedCommentsThreadJson = @'
{
  "count": 1,
  "value": [
    {
      "id": 621,
      "publishedDate": "2026-08-04T09:00:00.000Z",
      "lastUpdatedDate": "2026-08-04T12:30:00.000Z",
      "status": "active",
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "person-id", "displayName": "Real Person", "uniqueName": "person@example.com", "descriptor": "aad.cGVyc29u" },
          "content": "Withdrawn opener.",
          "publishedDate": "2026-08-04T10:07:33.677Z",
          "isDeleted": true,
          "commentType": "text"
        },
        {
          "id": 2,
          "parentCommentId": 0,
          "author": { "id": "runner-id", "displayName": "Review Runner", "uniqueName": "reviewer@example.com", "descriptor": "aad.cmV2aWV3ZXI" },
          "content": "Withdrawn reply.",
          "publishedDate": "2026-08-04T12:30:00.000Z",
          "isDeleted": true,
          "commentType": "text"
        }
      ]
    }
  ]
}
'@

    # A thread whose opening comment was deleted while a reply stands. Azure DevOps reports
    # parentCommentId 0 on every comment, so the surviving comment is the opener the capture can
    # name.
    $script:DeletedOpenerThreadJson = @'
{
  "count": 1,
  "value": [
    {
      "id": 611,
      "publishedDate": "2026-08-04T09:00:00.000Z",
      "lastUpdatedDate": "2026-08-04T12:30:00.000Z",
      "status": "active",
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "person-id", "displayName": "Real Person", "uniqueName": "person@example.com", "descriptor": "aad.cGVyc29u" },
          "content": "Withdrawn opener.",
          "publishedDate": "2026-08-04T10:07:33.677Z",
          "isDeleted": true,
          "commentType": "text"
        },
        {
          "id": 2,
          "parentCommentId": 0,
          "author": { "id": "runner-id", "displayName": "Review Runner", "uniqueName": "reviewer@example.com", "descriptor": "aad.cmV2aWV3ZXI" },
          "content": "The reply that stands.",
          "publishedDate": "2026-08-04T12:30:00.000Z",
          "commentType": "text"
        }
      ]
    }
  ]
}
'@

    # A thread the service sent with no status field, which is how it reports activity that carries
    # no resolution of its own.
    $script:StatuslessThreadJson = @'
{
  "count": 1,
  "value": [
    {
      "id": 601,
      "publishedDate": "2026-08-04T09:00:00.000Z",
      "lastUpdatedDate": "2026-08-04T09:00:00.000Z",
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "person-id", "displayName": "Real Person", "uniqueName": "person@example.com", "descriptor": "aad.cGVyc29u" },
          "content": "No status on this thread.",
          "publishedDate": "2026-08-04T09:00:00.000Z",
          "commentType": "text"
        }
      ]
    }
  ]
}
'@

    # A thread anchored to the pre-change side alone, which is what a comment on a deleted line
    # looks like.
    $script:LeftAnchoredThreadJson = @'
{
  "count": 1,
  "value": [
    {
      "id": 701,
      "publishedDate": "2026-08-04T09:00:00.000Z",
      "lastUpdatedDate": "2026-08-04T09:00:00.000Z",
      "status": "active",
      "threadContext": {
        "filePath": "/src/retired.rs",
        "leftFileStart": { "line": 88, "offset": 1 },
        "leftFileEnd": { "line": 90, "offset": 12 }
      },
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "human-id", "displayName": "Review Author", "uniqueName": "human@example.com", "descriptor": "aad.aHVtYW4" },
          "content": "State why this line went away.",
          "publishedDate": "2026-08-04T09:00:00.000Z",
          "commentType": "text"
        }
      ]
    }
  ]
}
'@

    # A discussion holding a thread the service reports as deleted alongside one that stands.
    $script:DeletedThreadJson = @'
{
  "count": 2,
  "value": [
    {
      "id": 801,
      "publishedDate": "2026-08-04T09:00:00.000Z",
      "lastUpdatedDate": "2026-08-04T09:05:00.000Z",
      "status": "active",
      "isDeleted": true,
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "human-id", "displayName": "Review Author", "uniqueName": "human@example.com", "descriptor": "aad.aHVtYW4" },
          "content": "Retracted in full.",
          "publishedDate": "2026-08-04T09:00:00.000Z",
          "commentType": "text"
        }
      ]
    },
    {
      "id": 802,
      "publishedDate": "2026-08-04T09:10:00.000Z",
      "lastUpdatedDate": "2026-08-04T09:10:00.000Z",
      "status": "active",
      "comments": [
        {
          "id": 1,
          "parentCommentId": 0,
          "author": { "id": "human-id", "displayName": "Review Author", "uniqueName": "human@example.com", "descriptor": "aad.aHVtYW4" },
          "content": "This one stands.",
          "publishedDate": "2026-08-04T09:10:00.000Z",
          "commentType": "text"
        }
      ]
    }
  ]
}
'@

    # What Azure DevOps returns when a resource read fails but the response still carries a success
    # status: an error object, with no collection anywhere in it.
    $script:ErrorShapedPayloadJson = '{ "$id": "1", "innerException": null, "message": "TF401019: The Git repository does not exist or you do not have permissions.", "typeKey": "GitRepositoryNotFoundException", "errorCode": 0 }'

    function New-StatusThreadJson {
        param(
            [Parameter(Mandatory)]
            [int] $Id,

            [AllowEmptyString()]
            [string] $Status
        )

        $statusText = if ([string]::IsNullOrEmpty($Status)) { '' } else { '"status": "' + $Status + '", ' }
        '{ "id": ' + $Id + ', ' + $statusText +
            '"publishedDate": "2026-08-04T09:00:00.000Z", "lastUpdatedDate": "2026-08-04T09:00:00.000Z", "comments": [] }'
    }
}

Describe 'ss-review Azure DevOps REST transport' {
    It 'decodes response bytes as UTF-8 without altering any code point' {
        # The fixture is only meaningful if its bytes really are multi-byte UTF-8 rather than
        # escaped ASCII, so that is asserted before anything is decoded.
        Test-ByteSequence -Haystack $script:ThreadsBytes -Needle ([byte[]] @(0xE2, 0x80, 0x94)) | Should -BeTrue
        Test-ByteSequence -Haystack $script:ThreadsBytes -Needle ([byte[]] @(0xE2, 0x80, 0x9C)) | Should -BeTrue
        Test-ByteSequence -Haystack $script:ThreadsBytes -Needle ([byte[]] @(0xE2, 0x80, 0x9D)) | Should -BeTrue
        Test-ByteSequence -Haystack $script:ThreadsBytes -Needle ([byte[]] @(0xF0, 0x9F, 0xA4, 0x96)) | Should -BeTrue

        $fake = New-FakeAdoTransport -Respond { param($Request) New-AdoTestResponse -Bytes $script:ThreadsBytes }
        $payload = Invoke-AdoRestRequest -Transport $fake.Transport -Path 'x/_apis/git/repositories/y/pullRequests/4242/threads'

        $content = $payload['value'][0]['comments'][0]['content']
        Get-CodePointText -Value $content | Should -Be (Get-CodePointText -Value $script:SpecialContent)
        [string]::Equals($content, $script:SpecialContent, [StringComparison]::Ordinal) | Should -BeTrue
        @($content.ToCharArray() | Where-Object { [char]::IsSurrogate($_) }).Count | Should -Be 2
    }

    It 'returns a comment body that is an ISO-8601 timestamp as the unchanged string' {
        $fake = New-FakeAdoTransport -Respond { param($Request) New-AdoTestResponse -Bytes $script:ThreadsBytes }
        $payload = Invoke-AdoRestRequest -Transport $fake.Transport -Path 'x/_apis/git/repositories/y/pullRequests/4242/threads'

        $content = $payload['value'][0]['comments'][1]['content']
        $content | Should -BeOfType ([string])
        [string]::Equals($content, $script:TimestampContent, [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'appends the api version and url-encodes query values' {
        $fake = New-FakeAdoTransport -Respond { param($Request) New-AdoTestResponse -Text '{}' }
        Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/git/repositories' -Query @{ searchCriteria = 'a b&c' } | Out-Null

        $fake.Requests[0].Url | Should -Be 'https://example.visualstudio.com/_apis/git/repositories?searchCriteria=a%20b%26c&api-version=7.1'
    }

    It 'orders query parameters ordinally rather than by culture' {
        # Ordinal ordering places every upper-case name ahead of every lower-case one; a
        # culture-sensitive comparison interleaves them. This set of names is therefore the one that
        # tells the two orderings apart, and the URL has to be byte identical across hosts for two
        # runs against the same resource to produce comparable diagnostics.
        $fake = New-FakeAdoTransport -Respond { param($Request) New-AdoTestResponse -Text '{}' }
        Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/git/repositories' -Query @{
            zebra  = '1'
            apple  = '2'
            Beta   = '3'
            banana = '4'
        } | Out-Null

        $fake.Requests[0].Url |
            Should -BeExactly 'https://example.visualstudio.com/_apis/git/repositories?Beta=3&apple=2&banana=4&zebra=1&api-version=7.1'
    }

    It 'honours throttling recovery time beyond the local backoff cap' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            if ($Request.Index -eq 1) {
                return New-AdoTestResponse -Text '{"message":"too many requests"}' -StatusCode 429 -Headers @{ 'retry-after' = '600' }
            }
            New-AdoTestResponse -Text '{"count":0,"value":[]}'
        }

        $payload = Invoke-AdoRestRequest -Transport $fake.Transport -Path 'x/_apis/git/repositories/y/pullRequests/4242/threads'

        $payload['count'] | Should -Be 0
        $fake.Requests.Count | Should -Be 2
        @($fake.Waits) | Should -Be @(600000)
    }

    It 'discards what the injected wait emits rather than returning it beside the payload' {
        # A recorder built on ArrayList.Add emits the index it inserted at. A transport that let
        # that value through would return an array of two rather than the payload, and every
        # property read against it would answer with the default rather than with the record.
        $script:EmittingWaits = [System.Collections.ArrayList]::new()
        $script:EmittingWaitAttempts = 0

        $send = {
            param(
                [string] $Method,
                [string] $Url,
                [hashtable] $Headers,
                $Body
            )

            $script:EmittingWaitAttempts++
            if ($script:EmittingWaitAttempts -eq 1) {
                return New-AdoTestResponse -Text '{"message":"too many requests"}' -StatusCode 429 -Headers @{ 'retry-after' = '2' }
            }
            New-AdoTestResponse -Text '{"pullRequestId":4242,"title":"fix(io): improve queue coordination"}'
        }

        $transport = New-AdoRestTransport -CollectionUrl 'https://example.visualstudio.com' `
            -SendRequest $send `
            -GetAccessToken { 'token' } `
            -Wait { param([int] $Milliseconds) $script:EmittingWaits.Add($Milliseconds) }

        $payload = Invoke-AdoRestRequest -Transport $transport -Path '_apis/git/pullrequests/4242'

        @($payload).Count | Should -Be 1
        ($payload -is [System.Collections.IDictionary]) | Should -BeTrue
        Get-JsonProperty -Node $payload -Name 'pullRequestId' | Should -Be 4242
        Get-JsonProperty -Node $payload -Name 'title' | Should -Be 'fix(io): improve queue coordination'
        @($script:EmittingWaits) | Should -Be @(2000)
    }

    It 'sends bearer authorization and asks for JSON' {
        $fake = New-FakeAdoTransport -Respond { param($Request) New-AdoTestResponse -Text '{}' }
        Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' -ApiVersion '7.1-preview' | Out-Null

        $fake.Requests[0].Headers['Authorization'] | Should -Be "Bearer $($fake.Token)"
        $fake.Requests[0].Headers['Accept'] | Should -Be 'application/json'
        $fake.Requests[0].Url | Should -Be 'https://example.visualstudio.com/_apis/connectionData?api-version=7.1-preview'
    }

    It 'fails when a success status carries an HTML sign-in document' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            New-AdoTestResponse -Text '<html><head><title>Azure DevOps Services</title></head></html>' -ContentType 'text/html; charset=utf-8'
        }

        { Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/git/pullrequests/4242' } |
            Should -Throw -ExpectedMessage '*access token*'
    }

    It 'fails on an HTML document even when the content type does not say so' {
        # The measured response to an unauthenticated read is HTTP 203 with a sign-in page, so the
        # guard cannot be conditioned on status 200 or on the content type alone.
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            New-AdoTestResponse -Text "  <!DOCTYPE html><html><body>Sign in</body></html>" -StatusCode 203 -ContentType 'application/octet-stream'
        }

        $failure = { Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/git/pullrequests/4242' } | Should -Throw -PassThru
        $failure.Exception.Message | Should -Match 'access token'
        $failure.Exception.Message | Should -Match 'tenant'
    }

    It 'fails when the service reports a continuation token' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            New-AdoTestResponse -Text '{"count":1,"value":[]}' -Headers @{ 'X-MS-ContinuationToken' = '900' }
        }

        { Invoke-AdoRestRequest -Transport $fake.Transport -Path 'x/_apis/git/repositories/y/pullRequests/4242/threads' } |
            Should -Throw -ExpectedMessage '*truncated*'
    }

    It 'returns paging metadata only to an explicit paging caller' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            New-AdoTestResponse -Text '{"count":1,"value":[{"commitId":"abc"}]}' -Headers @{ 'X-MS-ContinuationToken' = 'opaque-next' }
        }

        $page = Invoke-AdoRestRequest -Transport $fake.Transport -Path 'commits' -AsPage
        $page.continuationToken | Should -Be 'opaque-next'
        $page.payload.value[0].commitId | Should -Be 'abc'
    }

    It 'accepts an empty HTTP <StatusCode> deletion response only when explicitly requested' -ForEach @(
        @{ StatusCode = 200 }
        @{ StatusCode = 204 }
    ) {
        $script:EmptyDeletionStatusCode = $StatusCode
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            New-AdoTestResponse -Text '' -StatusCode $script:EmptyDeletionStatusCode
        }
        Invoke-AdoRestRequest -Transport $fake.Transport -Path 'comment' -Method DELETE -NoRetry -AllowEmptyResponse | Should -BeNullOrEmpty
        { Invoke-AdoRestRequest -Transport $fake.Transport -Path 'comment' -Method DELETE -NoRetry } | Should -Throw '*not a JSON*'
    }

    It 'fails on a non-success status without disclosing the token or any header' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            New-AdoTestResponse -Text '{"message":"TF401019: repository does not exist"}' -StatusCode 404
        }

        $failure = { Invoke-AdoRestRequest -Transport $fake.Transport -Path 'x/_apis/git/repositories/missing/pullRequests/1' } |
            Should -Throw -PassThru
        $failure.Exception.Message | Should -Match '404'
        $failure.Exception.Message | Should -Match 'TF401019'
        $failure.Exception.Message | Should -Not -Match ([regex]::Escape($fake.Token))
        $failure.Exception.Message | Should -Not -Match 'Bearer'
        $failure.Exception.Message | Should -Not -Match 'Authorization'
    }

    It 'fails when the body is not a JSON object or array' {
        $fake = New-FakeAdoTransport -Respond { param($Request) New-AdoTestResponse -Text 'true' }

        { Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' } |
            Should -Throw -ExpectedMessage '*not a JSON object or array*'
    }

    It 'retries a throttled request, honouring Retry-After, without waiting in real time' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            if ($Request.Index -eq 1) {
                return New-AdoTestResponse -Text '{"message":"too many requests"}' -StatusCode 429 -Headers @{ 'retry-after' = '3' }
            }
            New-AdoTestResponse -Text '{"count":0,"value":[]}'
        }

        $payload = Invoke-AdoRestRequest -Transport $fake.Transport -Path 'x/_apis/git/repositories/y/pullRequests/4242/threads'

        $payload['count'] | Should -Be 0
        $fake.Requests.Count | Should -Be 2
        @($fake.Waits) | Should -Be @(3000)
    }

    It 'retries a server fault with exponential backoff and then reports the failure' {
        $fake = New-FakeAdoTransport -MaxAttempts 3 -Respond {
            param($Request)
            New-AdoTestResponse -Text '{"message":"service unavailable"}' -StatusCode 503
        }

        { Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' } |
            Should -Throw -ExpectedMessage '*503*'

        $fake.Requests.Count | Should -Be 3
        @($fake.Waits) | Should -Be @(500, 1000)
    }

    It 'retries a connection failure that never produced a response' {
        # A reset connection, a DNS failure or a TLS handshake failure arrives as a thrown exception
        # rather than as a status, so the status-based retry would never see it and a blip would end
        # a capture that had already completed most of its reads.
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            if ($Request.Index -le 2) {
                throw [System.Net.Http.HttpRequestException]::new('The connection was reset by the peer.')
            }
            New-AdoTestResponse -Text '{"count":0,"value":[]}'
        }

        $payload = Invoke-AdoRestRequest -Transport $fake.Transport -Path 'x/_apis/git/repositories/y/pullRequests/4242/threads'

        $payload['count'] | Should -Be 0
        $fake.Requests.Count | Should -Be 3
        @($fake.Waits) | Should -Be @(500, 1000)
    }

    It 'retries a client-side timeout' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            if ($Request.Index -eq 1) { throw [System.Threading.Tasks.TaskCanceledException]::new('The request timed out.') }
            New-AdoTestResponse -Text '{}'
        }

        Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' | Out-Null

        $fake.Requests.Count | Should -Be 2
    }

    It 'gives up on a connection failure once the attempt budget is spent' {
        $fake = New-FakeAdoTransport -MaxAttempts 2 -Respond {
            param($Request)
            throw [System.Net.Http.HttpRequestException]::new('No such host is known.')
        }

        { Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' } |
            Should -Throw -ExpectedMessage '*No such host*'

        $fake.Requests.Count | Should -Be 2
        @($fake.Waits) | Should -Be @(500)
    }

    It 'reports a failure that repeating cannot fix without repeating it' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            throw [System.InvalidOperationException]::new('The sender is misconfigured.')
        }

        { Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' } |
            Should -Throw -ExpectedMessage '*misconfigured*'

        $fake.Requests.Count | Should -Be 1
        @($fake.Waits).Count | Should -Be 0
    }

    It 'acquires the access token once and reuses it for every later request' {
        $fake = New-FakeAdoTransport -Respond { param($Request) New-AdoTestResponse -Text '{}' }

        Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' | Out-Null
        Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/git/pullrequests/4242' | Out-Null
        Invoke-AdoRestRequest -Transport $fake.Transport -Path 'x/_apis/git/repositories/y/pullRequests/4242/threads' | Out-Null

        $fake.Requests.Count | Should -Be 3
        $script:FakeTokenAcquisitions | Should -Be 1
    }

    It 'discards a rejected token, acquires another and repeats the request' {
        $fake = New-FakeAdoTransport -Tokens @('expired-token', 'fresh-token') -Respond {
            param($Request)
            if ($Request.Index -eq 1) { return New-AdoTestResponse -Text '{"message":"TF400813: unauthorized"}' -StatusCode 401 }
            New-AdoTestResponse -Text '{"count":0,"value":[]}'
        }

        $payload = Invoke-AdoRestRequest -Transport $fake.Transport -Path 'x/_apis/git/repositories/y/pullRequests/4242/threads'

        $payload['count'] | Should -Be 0
        $fake.Requests.Count | Should -Be 2
        $script:FakeTokenAcquisitions | Should -Be 2
        $fake.Requests[0].Headers['Authorization'] | Should -Be 'Bearer expired-token'
        $fake.Requests[1].Headers['Authorization'] | Should -Be 'Bearer fresh-token'

        # The refreshed token replaces the held one, so the next request starts from it rather than
        # re-acquiring or presenting the rejected one again.
        Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' | Out-Null
        $script:FakeTokenAcquisitions | Should -Be 2
        $fake.Requests[2].Headers['Authorization'] | Should -Be 'Bearer fresh-token'
    }

    It 'reports a rejection that a fresh token does not resolve' {
        $fake = New-FakeAdoTransport -Tokens @('first-token', 'second-token') -Respond {
            param($Request)
            New-AdoTestResponse -Text '{"message":"TF400813: unauthorized"}' -StatusCode 401
        }

        { Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' } |
            Should -Throw -ExpectedMessage '*401*'

        # One refresh and no more: a token the service will never accept must surface as the
        # rejection it is rather than as an acquisition loop.
        $fake.Requests.Count | Should -Be 2
        $script:FakeTokenAcquisitions | Should -Be 2
    }

    It 'sends a request once when retrying is suppressed, even though the status is retryable' {
        # The second attempt would succeed, so a single request proves the suppression rather than
        # an exhausted attempt budget.
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            if ($Request.Index -eq 1) { return New-AdoTestResponse -Text '{"message":"busy"}' -StatusCode 503 }
            New-AdoTestResponse -Text '{"count":0,"value":[]}'
        }

        { Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' -NoRetry } |
            Should -Throw -ExpectedMessage '*503*'

        $fake.Requests.Count | Should -Be 1
        @($fake.Waits).Count | Should -Be 0
    }

    It 'repeats the same status when retrying is not suppressed' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            if ($Request.Index -eq 1) { return New-AdoTestResponse -Text '{"message":"busy"}' -StatusCode 503 }
            New-AdoTestResponse -Text '{"count":0,"value":[]}'
        }

        $payload = Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData'

        $payload['count'] | Should -Be 0
        $fake.Requests.Count | Should -Be 2
    }

    It 'sends a request once when retrying is suppressed and the connection fails' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            if ($Request.Index -eq 1) { throw [System.Net.Http.HttpRequestException]::new('The connection was reset by the peer.') }
            New-AdoTestResponse -Text '{}'
        }

        { Invoke-AdoRestRequest -Transport $fake.Transport -Path '_apis/connectionData' -NoRetry } |
            Should -Throw -ExpectedMessage '*connection was reset*'

        $fake.Requests.Count | Should -Be 1
        @($fake.Waits).Count | Should -Be 0
    }

    It 'does not present a fresh token when retrying is suppressed' {
        # The token refresh sends the same body a second time, so it is a repeat like any other and
        # a write that must not be repeated has to forgo it too.
        $fake = New-FakeAdoTransport -Tokens @('expired-token', 'fresh-token') -Respond {
            param($Request)
            if ($Request.Index -eq 1) { return New-AdoTestResponse -Text '{"message":"TF400813: unauthorized"}' -StatusCode 401 }
            New-AdoTestResponse -Text '{"count":0,"value":[]}'
        }

        { Invoke-AdoRestRequest -Transport $fake.Transport -Path 'x/_apis/git/repositories/y/pullRequests/4242/threads' -Method 'POST' -Body ([ordered]@{ content = 'a comment' }) -NoRetry } |
            Should -Throw -ExpectedMessage '*401*'

        $fake.Requests.Count | Should -Be 1
        $script:FakeTokenAcquisitions | Should -Be 1
    }

    It 'suppresses uncertain retries through the list wrapper' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            New-AdoTestResponse -Text '{"message":"busy"}' -StatusCode 503
        }

        { Invoke-AdoRestListRequest -Transport $fake.Transport -Path '_apis/git/repositories/y/pullRequests/4242/threads' -NoRetry } |
            Should -Throw -ExpectedMessage '*503*'

        $fake.Requests.Count | Should -Be 1
        @($fake.Waits).Count | Should -Be 0
    }

    It 'retries explicit write refusals beyond the transient budget with capped exponential backoff' {
        # This sequence reaches the cap and outlasts the default transient attempt budget.
        $fake = New-FakeAdoTransport -MaxAttempts 1 -Respond {
            param($Request)
            if ($Request.Index -le 8) { return New-AdoTestResponse -Text '{"message":"busy"}' -StatusCode 429 }
            if ($Request.Index -gt 9) { throw 'The transport repeated a successful write.' }
            New-AdoTestResponse -Text '{"id":123}'
        }

        $payload = Invoke-AdoRestRequest -Transport $fake.Transport -Path 'items' -Method POST `
            -Body '{"body":"Frozen report"}' -NoRetry

        $payload['id'] | Should -Be 123
        $fake.Requests.Count | Should -Be 9
        @($fake.Waits) | Should -Be @(500, 1000, 2000, 4000, 8000, 16000, 30000, 30000)
        foreach ($request in $fake.Requests) {
            $request.Method | Should -BeExactly 'POST'
            $request.Body | Should -BeExactly '{"body":"Frozen report"}'
        }
    }

    It 'does not spend the transient retry budget on throttling' {
        $fake = New-FakeAdoTransport -MaxAttempts 2 -Respond {
            param($Request)
            switch ($Request.Index) {
                1 { return New-AdoTestResponse -Text '{}' -StatusCode 429 }
                2 { return New-AdoTestResponse -Text '{}' -StatusCode 503 }
                3 { return New-AdoTestResponse -Text '{"ok":true}' }
                default { throw 'The transport repeated a successful request.' }
            }
        }

        $payload = Invoke-AdoRestRequest -Transport $fake.Transport -Path 'items'

        $payload['ok'] | Should -BeTrue
        @($fake.Waits) | Should -Be @(500, 500)
    }

    It 'keeps a lost write response following a throttling refusal ambiguous' {
        $fake = New-FakeAdoTransport -Respond {
            param($Request)
            if ($Request.Index -eq 1) { return New-AdoTestResponse -Text '{}' -StatusCode 429 }
            throw [System.Net.Http.HttpRequestException]::new('Connection reset')
        }

        $failure = { Invoke-AdoRestRequest -Transport $fake.Transport -Path 'items' -Method POST -Body '{}' -NoRetry } |
            Should -Throw -PassThru

        Get-PublicationFailureOutcome -Exception $failure.Exception | Should -BeExactly 'ambiguous'
        $fake.Requests.Count | Should -Be 2
        @($fake.Waits) | Should -Be @(500)
    }

    It 'records an interrupted throttling wait as a definite refusal' {
        $transport = New-AdoRestTransport -CollectionUrl 'https://example.invalid/org' -GetAccessToken { 'fake-token' } `
            -Wait { param([int] $Milliseconds) throw [OperationCanceledException]::new('Wait interrupted') } `
            -SendRequest { New-AdoTestResponse -Text '{}' -StatusCode 429 }

        $failure = { Invoke-AdoRestRequest -Transport $transport -Path 'items' -Method POST -Body '{}' -NoRetry } |
            Should -Throw -PassThru

        Get-PublicationFailureOutcome -Exception $failure.Exception | Should -BeExactly 'failed'
    }

    It 'saturates backoff safely for a long-running refusal sequence' {
        Get-AdoRestRetryDelayMilliseconds -RetryAfter $null -Attempt ([int]::MaxValue) -RateLimit | Should -Be 30000
    }

    It 'refuses a payload that carries no collection at all' {
        # A capture that read this as an empty collection would succeed and report nothing, which is
        # indistinguishable from a pull request nobody has commented on.
        $payload = ConvertFrom-JsonVerbatim -Text $script:ErrorShapedPayloadJson

        { Get-AdoRestListItem -Payload $payload -Source 'pullRequests/4242/threads' } |
            Should -Throw -ExpectedMessage "*no 'value' collection*"
    }

    It 'refuses a collection shorter than the count the service stated' {
        $payload = ConvertFrom-JsonVerbatim -Text '{ "count": 54, "value": [ { "id": 1 } ] }'

        { Get-AdoRestListItem -Payload $payload -Source 'pullRequests/4242/threads' } |
            Should -Throw -ExpectedMessage '*reported 54 items*but sent 1*'
    }

    It 'refuses an envelope whose stated count is not a number' {
        $payload = ConvertFrom-JsonVerbatim -Text '{ "count": "many", "value": [] }'

        { Get-AdoRestListItem -Payload $payload -Source 'pullRequests/4242/threads' } |
            Should -Throw -ExpectedMessage '*non-numeric item count*'
    }

    It 'accepts a collection that is present and empty' {
        $items = Get-AdoRestListItem -Payload (ConvertFrom-JsonVerbatim -Text $script:EmptyCollectionJson) -Source 'threads'

        $items.Count | Should -Be 0
    }

    It 'accepts an envelope that states no count' {
        $items = Get-AdoRestListItem -Payload (ConvertFrom-JsonVerbatim -Text '{ "value": [ { "id": 1 }, { "id": 2 } ] }') -Source 'threads'

        $items.Count | Should -Be 2
        $items[1]['id'] | Should -Be 2
    }

    It 'returns a collection holding one entry as a collection of one' {
        $items = Get-AdoRestListItem -Payload (ConvertFrom-JsonVerbatim -Text '{ "count": 1, "value": [ { "id": 7 } ] }') -Source 'threads'

        $items.Count | Should -Be 1
        $items[0]['id'] | Should -Be 7
    }
}

Describe 'ss-review Azure DevOps pull request capture' {
    It 'captures the pull request, its revisions, its reviewers and its threads' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $context['target']['project'] | Should -Be 'Example Project'
        $context['target']['webUrl'] | Should -Be 'https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242'
        $context['authenticatedIdentity']['descriptor'] | Should -Be 'aad.cmV2aWV3ZXI'
        $context['authenticatedIdentity']['uniqueName'] | Should -Be 'reviewer@example.com'
        $context['pullRequest']['id'] | Should -Be 4242
        $context['pullRequest']['title'] | Should -Be 'fix(io): improve queue coordination'
        $context['pullRequest']['description'] | Should -Be 'Motivation and changes.'
        $context['pullRequest']['isDraft'] | Should -BeFalse
        $context['pullRequest']['author']['kind'] | Should -Be 'person'
        $context['pullRequest']['createdUtc'] | Should -Be '2026-08-03T11:44:10.8515947Z'
        $context['pullRequest']['sourceRef'] | Should -Be 'refs/heads/u/reviewer/topic'
        $context['pullRequest']['targetRef'] | Should -Be 'refs/heads/master'
        @($context['revisions']).Count | Should -Be 2
        @($context['reviewers']).Count | Should -Be 2
        # The fixture serves four threads, one of which is a vote thread the capture excludes.
        @($context['threads']).Count | Should -Be 3
    }

    It 'records neither the pull request state nor a reviewer vote' {
        # Approval standing belongs to whoever merges the pull request rather than to a reviewer
        # composing comments, so the capture carries no trace of it even though the service reports
        # both (docs/design.md, "Captured pull request context").
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $context['pullRequest'].Contains('state') | Should -BeFalse
        $context['pullRequest'].Contains('platformState') | Should -BeFalse

        foreach ($reviewer in @($context['reviewers'])) {
            $reviewer.Contains('vote') | Should -BeFalse
            $reviewer.Contains('platformVote') | Should -BeFalse
            $reviewer.Contains('voteLabel') | Should -BeFalse
        }
    }

    It 'leaves a vote thread out of the capture' {
        # A vote thread carries a system-generated notification and the vote itself. With the vote
        # gone the thread states nothing a reviewer can act on, so it is dropped rather than
        # captured as an empty event.
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $script:ThreadsJson.Contains('VoteUpdate') | Should -BeTrue
        @(@($context['threads']) | Where-Object { $_['id'] -eq 102 }).Count | Should -Be 0
    }

    It 'takes the reviewed commits from the latest revision rather than from the target branch head' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $context['pullRequest']['reviewedSourceCommit'] | Should -Be 'source-commit-2'
        $context['pullRequest']['reviewedMergeBaseCommit'] | Should -Be 'merge-base-2'
        $context['pullRequest']['mergeState']['lastMergeTargetCommit'] | Should -Be 'target-branch-head'
        $context['pullRequest']['reviewedMergeBaseCommit'] |
            Should -Not -Be $context['pullRequest']['mergeState']['lastMergeTargetCommit']
        $context['pullRequest']['mergeState']['status'] | Should -Be 'succeeded'
    }

    It 'reports each revision with its own merge base' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $latest = @($context['revisions']) | Where-Object { $_['id'] -eq 2 }
        $latest['mergeBaseCommit'] | Should -Be 'merge-base-2'
        $latest['reason'] | Should -Be 'push'
        $earliest = @($context['revisions']) | Where-Object { $_['id'] -eq 1 }
        $earliest['reason'] | Should -BeNullOrEmpty
        $earliest['createdUtc'] | Should -Be '2026-08-03T11:44:10.8662081Z'
    }

    It 'parses threads whose commentType and status are absent' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $discussion = @($context['threads']) | Where-Object { $_['id'] -eq 101 }

        # The second comment carries no commentType.
        @($discussion['comments'])[1]['id'] | Should -Be 2

        # The thread with no status at all reports the absence rather than a resolution it was never
        # told.
        $fake = New-AdoContextTransport -ThreadsJson $script:StatuslessThreadJson
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $statusless = $context['threads'][0]
        $statusless['rawStatus'] | Should -BeNullOrEmpty
        $statusless['resolution'] | Should -Be 'indeterminate'
    }

    It 'preserves comment bodies verbatim, including non-ASCII and timestamp-shaped text' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $comments = @((@($context['threads']) | Where-Object { $_['id'] -eq 101 })['comments'])
        Get-CodePointText -Value $comments[0]['content'] | Should -Be (Get-CodePointText -Value $script:SpecialContent)
        [string]::Equals($comments[1]['content'], $script:TimestampContent, [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'omits a comment the service reports as deleted' {
        # The fixture thread carries a deleted comment alongside two that stand. A withdrawn comment
        # states nothing a reviewer can act on, so it leaves the capture entirely rather than
        # arriving with a flag every consumer would have to honour.
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $thread = @($context['threads']) | Where-Object { $_['id'] -eq 101 }
        $comments = @($thread['comments'])
        $comments.Count | Should -Be 2
        @($comments | ForEach-Object { $_['id'] }) | Should -Not -Contain 3
        @($comments | ForEach-Object { [string] $_['content'] }) | Should -Not -Contain 'Withdrawn.'
    }

    It 'omits a thread the service reports as deleted' {
        # A retracted thread states nothing a reviewer can act on. Its disappearance between two
        # captures is already visible in the comparison between them, so the capture spends no
        # field on saying that it went.
        $fake = New-AdoContextTransport -ThreadsJson $script:DeletedThreadJson
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $threads = @($context['threads'])
        $threads.Count | Should -Be 1
        $threads[0]['id'] | Should -Be 802
        $threads[0].Contains('isDeleted') | Should -BeFalse
        @($threads[0]['comments'])[0].Contains('isDeleted') | Should -BeFalse
    }

    It 'reports comment threading, editing and deep links' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $thread = @($context['threads']) | Where-Object { $_['id'] -eq 101 }
        $comments = @($thread['comments'])

        $comments[0]['parentId'] | Should -BeNullOrEmpty
        $comments[0]['isEdited'] | Should -BeFalse
        $comments[1]['parentId'] | Should -Be 1
        $comments[1]['isEdited'] | Should -BeTrue
        $comments[1]['lastEditedUtc'] | Should -Be '2026-08-03T12:20:00.0000000Z'
        $thread['url'] | Should -Be 'https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242?discussionId=101'
        $comments[0]['url'] | Should -Be $thread['url']
    }

    It 'preserves the line anchor on both sides of the change' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $thread = @($context['threads']) | Where-Object { $_['id'] -eq 101 }
        $thread['context']['filePath'] | Should -Be 'src/lib.rs'
        $thread['context']['side'] | Should -Be 'right'
        $thread['context']['startLine'] | Should -Be 143
        $thread['context']['endLine'] | Should -Be 145
        $thread['context']['originalStartLine'] | Should -Be 120
        $thread['context']['originalEndLine'] | Should -Be 121
        $thread['anchorKey'] | Should -Be 14
        $thread['isOutdated'] | Should -BeTrue

        $current = @($context['threads']) | Where-Object { $_['id'] -eq 104 }
        $current['context'] | Should -BeNullOrEmpty
        $current['anchorKey'] | Should -Be 21
        $current['isOutdated'] | Should -BeFalse
    }

    It 'reports a thread anchored only to the pre-change side as a left-side anchor' {
        $fake = New-AdoContextTransport -ThreadsJson $script:LeftAnchoredThreadJson
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $thread = $context['threads'][0]
        $thread['context']['filePath'] | Should -Be 'src/retired.rs'
        # Casing is asserted exactly: the canonical vocabulary is lower case and Should -Be would
        # accept any casing of it.
        $thread['context']['side'] | Should -BeExactly 'left'
        $thread['context']['startLine'] | Should -BeNullOrEmpty
        $thread['context']['endLine'] | Should -BeNullOrEmpty
        $thread['context']['originalStartLine'] | Should -Be 88
        $thread['context']['originalEndLine'] | Should -Be 90
    }

    It 'reports a plain thread event time as its publication time' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $thread = @($context['threads']) | Where-Object { $_['id'] -eq 101 }
        $thread['decision'] | Should -BeNullOrEmpty
        $thread['eventUtc'] | Should -Be $thread['publishedUtc']
        $thread['eventUtc'] | Should -Be '2026-08-03T12:00:00.0000000Z'
        $thread['lastUpdatedUtc'] | Should -Be '2026-08-03T12:30:00.0000000Z'
    }

    It 'classifies thread kinds from the thread type property' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        (@($context['threads']) | Where-Object { $_['id'] -eq 101 })['kind'] | Should -Be 'text'
        (@($context['threads']) | Where-Object { $_['id'] -eq 104 })['kind'] | Should -Be 'text'

        # An unrecognized thread type still reports as machine-generated activity rather than as a
        # person's comment.
        (@($context['threads']) | Where-Object { $_['id'] -eq 103 })['kind'] | Should -Be 'system'
    }

    It 'classifies a comment produced by an automated reviewer as agent authored' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $thread = @($context['threads']) | Where-Object { $_['id'] -eq 103 }
        @($thread['comments'])[0]['authorship'] | Should -BeExactly 'agent'
        @($thread['comments'])[0]['authorshipEvidence'] | Should -Contain 'property:LLMModel'
    }

    It 'keeps the markers of an automated thread off a person replying inside it' {
        # The thread property markers belong to the comment that opened the thread. Applying them to
        # the replies would report everyone objecting to an automated finding as machine-generated,
        # and the discussion document is what a later review deduplicates against, so the objection
        # would be discounted as noise.
        $fake = New-AdoContextTransport -ThreadsJson $script:AgentThreadWithReplyJson
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $comments = @($context['threads'][0]['comments'])
        $comments.Count | Should -Be 2

        $comments[0]['author']['displayName'] | Should -Be 'Automation'
        $comments[0]['authorship'] | Should -BeExactly 'agent'
        $comments[0]['authorshipEvidence'] | Should -Contain 'property:LLMModel'

        $comments[1]['author']['displayName'] | Should -Be 'Real Person'
        $comments[1]['authorship'] | Should -BeExactly 'indeterminate'
        $comments[1]['authorshipEvidence'] | Should -Not -Contain 'property:LLMModel'
        $comments[1]['authorshipEvidence'] | Should -Not -Contain 'property:Focus_Area'
    }

    It 'identifies the comment that opened a thread by its identifier rather than by its position' {
        $fake = New-AdoContextTransport -ThreadsJson $script:AgentThreadReorderedJson
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $thread = $context['threads'][0]
        $comments = @($thread['comments'])
        ($comments | Where-Object { $_['id'] -eq 1 })['authorship'] | Should -BeExactly 'agent'
        ($comments | Where-Object { $_['id'] -eq 2 })['authorship'] | Should -BeExactly 'indeterminate'

        # The thread names its opener so that the shared model attributes the thread structurally
        # rather than from whichever comment presentation order happens to place first.
        $thread['openingCommentId'] | Should -Be 1
    }

    It 'names no opening comment for a thread whose comments were all deleted' {
        # A deleted comment reaches no part of the capture, so an opener naming one would point at
        # evidence the reader cannot find.
        $fake = New-AdoContextTransport -ThreadsJson $script:AllDeletedCommentsThreadJson
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $thread = $context['threads'][0]
        @($thread['comments']).Count | Should -Be 0
        # The key is asserted present separately from its value, because indexing a dictionary for an
        # absent key yields null without throwing and a value assertion alone would also pass if the
        # field were never emitted.
        $thread.Contains('openingCommentId') | Should -BeTrue
        $thread['openingCommentId'] | Should -BeNullOrEmpty
    }

    It 'names the surviving opener when the comment that opened the thread was deleted' {
        $fake = New-AdoContextTransport -ThreadsJson $script:DeletedOpenerThreadJson
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $thread = $context['threads'][0]
        @($thread['comments']).Count | Should -Be 1
        $thread['openingCommentId'] | Should -Be 2
    }

    It 'reports no opening comment for a thread that carries none' {
        Get-AdoThreadOpeningCommentIndex -Comments @() | Should -Be -1
    }

    It 'falls back to serialization order when no comment carries an identifier' {
        $comments = @(
            (ConvertFrom-JsonVerbatim -Text '{ "content": "first" }'),
            (ConvertFrom-JsonVerbatim -Text '{ "content": "second" }')
        )

        Get-AdoThreadOpeningCommentIndex -Comments $comments | Should -Be 0
    }

    It 'fails the capture when a success response is not the list it must be' {
        $fake = New-AdoContextTransport -ThreadsJson $script:ErrorShapedPayloadJson

        { Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget) } |
            Should -Throw -ExpectedMessage "*no 'value' collection*"
    }

    It 'fails the capture when the service states more threads than it sent' {
        $fake = New-AdoContextTransport -ThreadsJson '{ "count": 12, "value": [] }'

        { Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget) } |
            Should -Throw -ExpectedMessage '*reported 12 items*but sent 0*'
    }

    It 'fails the capture when the iterations read reports no collection' {
        # An empty iteration list also leaves the reviewed commits unknown, which the freshness check
        # reads as a pull request nobody has pushed to.
        $fake = New-AdoContextTransport -IterationsJson $script:ErrorShapedPayloadJson

        { Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget) } |
            Should -Throw -ExpectedMessage "*no 'value' collection*"
    }

    It 'unwraps property bag type and value wrappers' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $thread = @($context['threads']) | Where-Object { $_['id'] -eq 101 }
        $thread['properties']['Microsoft.TeamFoundation.Discussion.UniqueID'] |
            Should -Be '55555555555555555555555555555555'
        $thread['properties']['SupportsMarkdown'] | Should -Be 1

        $system = @($context['threads']) | Where-Object { $_['id'] -eq 103 }
        $system['properties']['CodeReviewThreadType'] | Should -Be 'SomeFutureUpdate'
        $system['properties']['LLMModel'] | Should -Be 'gpt-5'
    }

    It 'reports a collection holding a single entry as a collection of one' {
        # A command that emits a single array emits it as one object, so a payload carrying exactly
        # one thread, one comment, one revision and one reviewer is where mishandled collection
        # reads surface: the capture would otherwise report the whole collection as its own first
        # element.
        $fake = New-AdoContextTransport -ThreadsJson $script:SingleThreadJson -IterationsJson $script:SingleIterationJson -ReviewersJson $script:SingleReviewerJson
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        @($context['threads']).Count | Should -Be 1
        @($context['revisions']).Count | Should -Be 1
        @($context['reviewers']).Count | Should -Be 1

        $thread = $context['threads'][0]
        $thread['id'] | Should -Be 301
        $thread['resolution'] | Should -Be 'active'
        @($thread['comments']).Count | Should -Be 1
        $thread['comments'][0]['content'] | Should -Be 'Only comment.'
        $thread['comments'][0]['authorship'] | Should -BeExactly 'indeterminate'

        $context['revisions'][0]['id'] | Should -Be 1
        $context['reviewers'][0]['identity']['id'] | Should -Be 'only-reviewer-id'
        $context['reviewers'][0]['isRequired'] | Should -BeFalse
        $context['pullRequest']['reviewedSourceCommit'] | Should -Be 'only-source'
        $context['pullRequest']['reviewedMergeBaseCommit'] | Should -Be 'only-merge-base'
    }

    It 'reports empty collections as empty rather than failing' {
        $fake = New-AdoContextTransport -ThreadsJson $script:EmptyCollectionJson -IterationsJson $script:EmptyCollectionJson -ReviewersJson $script:EmptyCollectionJson
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        @($context['threads']).Count | Should -Be 0
        @($context['revisions']).Count | Should -Be 0
        @($context['reviewers']).Count | Should -Be 0
        $context['pullRequest']['reviewedSourceCommit'] | Should -BeNullOrEmpty
        $context['pullRequest']['reviewedMergeBaseCommit'] | Should -BeNullOrEmpty
    }

    It 'reports a thread that carries no comments at all' {
        # System threads arrive with an empty comment collection, which must not be read as one
        # comment-shaped object.
        $payload = '{ "count": 1, "value": [' + (New-StatusThreadJson -Id 401 -Status 'closed') + '] }'
        $fake = New-AdoContextTransport -ThreadsJson $payload
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        @($context['threads']).Count | Should -Be 1
        @($context['threads'][0]['comments']).Count | Should -Be 0
        $context['threads'][0]['resolution'] | Should -Be 'resolved'
        $context['threads'][0]['decision'] | Should -BeNullOrEmpty
    }

    It 'maps every thread status to a resolution' {
        $expected = [ordered]@{
            'fixed'    = 'resolved'
            'closed'   = 'resolved'
            'byDesign' = 'resolved'
            'wontFix'  = 'resolved'
            'active'   = 'active'
            'pending'  = 'active'
            'unknown'  = 'indeterminate'
        }

        $threadJson = [System.Collections.Generic.List[string]]::new()
        $id = 200
        foreach ($status in $expected.Keys) {
            $threadJson.Add((New-StatusThreadJson -Id $id -Status $status))
            $id++
        }
        # An absent status is the shape Azure DevOps sends for system threads.
        $threadJson.Add((New-StatusThreadJson -Id $id -Status ''))

        $payload = '{ "count": ' + $threadJson.Count + ', "value": [' + ($threadJson -join ',') + '] }'
        $fake = New-AdoContextTransport -ThreadsJson $payload
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $id = 200
        foreach ($status in $expected.Keys) {
            $thread = @($context['threads']) | Where-Object { $_['id'] -eq $id }
            $thread['rawStatus'] | Should -Be $status
            $thread['resolution'] | Should -Be $expected[$status] -Because $status
            $id++
        }

        $absent = @($context['threads']) | Where-Object { $_['id'] -eq $id }
        $absent['rawStatus'] | Should -BeNullOrEmpty
        $absent['resolution'] | Should -Be 'indeterminate'
    }

    It 'reports reviewers with their requirement and team status' {
        $fake = New-AdoContextTransport
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $reviewers = @($context['reviewers'])
        $reviewers[0]['identity']['id'] | Should -Be 'reviewer-existing-id'
        $reviewers[0]['isRequired'] | Should -BeTrue
        $reviewers[0]['isTeam'] | Should -BeFalse

        # The team entry omits both flags, which must read as false rather than fail.
        $reviewers[1]['identity']['id'] | Should -Be 'team-id'
        $reviewers[1]['isRequired'] | Should -BeFalse
        $reviewers[1]['isTeam'] | Should -BeFalse
    }

    It 'resolves the project and repository from the pull request identifier alone' {
        $fake = New-AdoContextTransport
        $resolved = Resolve-AdoPullRequestTarget -Transport $fake.Transport -Target (New-TestTarget -Unresolved)

        $resolved['project'] | Should -Be 'Example Project'
        $resolved['repository'] | Should -Be 'example repo'
        $resolved['webUrl'] | Should -Be 'https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242'
        @($fake.Requests).Count | Should -Be 1
        $fake.Requests[0].Url | Should -Be 'https://example.visualstudio.com/_apis/git/pullrequests/4242?api-version=7.1'
    }

    It 'does not query the service when the project and repository are already known' {
        $fake = New-AdoContextTransport
        $resolved = Resolve-AdoPullRequestTarget -Transport $fake.Transport -Target (New-TestTarget)

        $resolved['repository'] | Should -Be 'example repo'
        @($fake.Requests).Count | Should -Be 0
    }

    It 'renders a thread deep link for a target that has not been resolved yet' {
        $url = Get-AdoPullRequestThreadUrl -Target (New-TestTarget) -ThreadId 101
        $url | Should -Be 'https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242?discussionId=101'
    }

    It 'renders the same pull request URL as the shared derivation' {
        # One derivation decides what a pull request URL looks like. A second form would drift
        # silently, and the drift reaches a reviewer only as a link that opens nothing.
        $target = New-TestTarget

        Get-AdoPullRequestBrowserUrl -Target $target | Should -BeExactly (Get-PullRequestWebUrl -Target $target)
        Get-AdoPullRequestThreadUrl -Target $target -ThreadId 101 |
            Should -BeExactly ((Get-PullRequestWebUrl -Target $target) + '?discussionId=101')
    }

    It 'renders the same pull request URL as the shared derivation for a collection URL ending in a separator' {
        # A collection URL is pasted in both forms, and each path segment supplies its own leading
        # separator, so a trailing one must not survive into the URL a reader is handed.
        $target = New-TestTarget
        $target['collectionUrl'] = 'https://example.visualstudio.com/'

        $expected = 'https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242'
        Get-PullRequestWebUrl -Target $target | Should -BeExactly $expected
        Get-AdoPullRequestBrowserUrl -Target $target | Should -BeExactly $expected
        Get-AdoPullRequestThreadUrl -Target $target -ThreadId 101 |
            Should -BeExactly "$expected`?discussionId=101"
    }

    It 'renders what it can for a target whose project and repository are not resolved yet' {
        # The shared derivation runs before the adapter has resolved the coordinates, so it renders
        # the collection-level form rather than refusing. The adapter's own entry point requires
        # them, because every caller of it holds a target that has already been resolved.
        $target = New-TestTarget -Unresolved
        $target['collectionUrl'] = 'https://example.visualstudio.com/'

        Get-PullRequestWebUrl -Target $target | Should -BeExactly 'https://example.visualstudio.com/_git/pullrequest/4242'
        { Get-AdoPullRequestBrowserUrl -Target $target } |
            Should -Throw -ExpectedMessage '*missing its project or repository*'
    }

    It 'reports every reviewer the service lists, whatever position each has taken' {
        # The service reports a vote on every reviewer. None of it reaches the capture, so a
        # reviewer listing is unaffected by the positions the reviewers hold.
        $fake = New-AdoContextTransport -ReviewersJson $script:VoteSpectrumReviewersJson
        $context = Get-AdoPullRequestContext -Transport $fake.Transport -Target (New-TestTarget)

        $reviewers = @($context['reviewers'])
        $reviewers.Count | Should -Be 6

        foreach ($reviewer in $reviewers) {
            (@($reviewer.Keys) -join ', ') | Should -BeExactly 'identity, isRequired, isTeam'
        }
    }
}

Describe 'ss-review Azure DevOps iteration changes' {
    BeforeAll {
        # Serves the change listing for one iteration from a queue of pages, so a test states the
        # pages the service answers with and the read is held to following them.
        function New-AdoChangeTransport {
            param(
                [Parameter(Mandatory)]
                [string[]] $PageJson
            )

            $script:ServedChangePages = $PageJson

            $respond = {
                param($Request)

                if ($Request.Url -notmatch '/iterations/\d+/changes\?') { throw "unexpected request to $($Request.Url)" }

                $index = @($script:FakeRequests).Count - 1
                if ($index -ge $script:ServedChangePages.Count) { throw "the read asked for page $($index + 1) of $($script:ServedChangePages.Count)" }
                New-AdoTestResponse -Text $script:ServedChangePages[$index]
            }

            New-FakeAdoTransport -Respond $respond
        }
    }

    It 'reads a single page of changes' {
        $fake = New-AdoChangeTransport -PageJson @('{"changeEntries":[{"changeTrackingId":11,"item":{"path":"/src/lib.rs"}}]}')

        $changes = Get-AdoPullRequestIterationChange -Transport $fake.Transport -Target (New-TestTarget) -IterationId 2

        @($changes).Count | Should -Be 1
        @($changes)[0]['changeTrackingId'] | Should -Be 11
        $fake.Requests.Count | Should -Be 1
        $fake.Requests[0].Url | Should -BeLike '*/pullRequests/4242/iterations/2/changes?*'
    }

    It 'asks for cumulative changes by leaving compareTo at the service default' {
        $fake = New-AdoChangeTransport -PageJson @('{"changeEntries":[{"changeTrackingId":11}]}')

        Get-AdoPullRequestIterationChange -Transport $fake.Transport -Target (New-TestTarget) -IterationId 2 | Out-Null

        $fake.Requests[0].Url | Should -Not -BeLike '*compareTo*'
    }

    It 'follows the cursor the service states until the pages are exhausted' {
        $fake = New-AdoChangeTransport -PageJson @(
            '{"changeEntries":[{"changeTrackingId":1},{"changeTrackingId":2}],"nextSkip":2,"nextTop":1}'
            '{"changeEntries":[{"changeTrackingId":3},{"changeTrackingId":4}],"nextSkip":4,"nextTop":1}'
            '{"changeEntries":[{"changeTrackingId":5}]}'
        )

        $changes = Get-AdoPullRequestIterationChange -Transport $fake.Transport -Target (New-TestTarget) -IterationId 2

        @($changes).Count | Should -Be 5
        @($changes | ForEach-Object { $_['changeTrackingId'] }) | Should -Be @(1, 2, 3, 4, 5)

        $fake.Requests.Count | Should -Be 3
        $fake.Requests[0].Url | Should -Not -BeLike '*%24skip*'
        $fake.Requests[1].Url | Should -BeLike '*%24skip=2*'
        $fake.Requests[2].Url | Should -BeLike '*%24skip=4*'
    }

    It 'asks for the same page size throughout, whatever size the service states for the next page' {
        # Azure DevOps answers a fully served request for two entries with a next page size of one,
        # so a reader that honoured it would page a large pull request an entry at a time.
        $fake = New-AdoChangeTransport -PageJson @(
            '{"changeEntries":[{"changeTrackingId":1}],"nextSkip":1,"nextTop":1}'
            '{"changeEntries":[{"changeTrackingId":2}]}'
        )

        Get-AdoPullRequestIterationChange -Transport $fake.Transport -Target (New-TestTarget) -IterationId 2 | Out-Null

        $requestedTop = [regex]::Match($fake.Requests[0].Url, '%24top=(\d+)').Groups[1].Value
        $requestedTop | Should -Not -BeNullOrEmpty
        $fake.Requests[1].Url | Should -BeLike "*%24top=$requestedTop*"
    }

    It 'refuses a cursor that does not advance' {
        # A service repeating the same cursor would otherwise be followed forever, and a review that
        # never returns is worse than one that reports the fault.
        $fake = New-AdoChangeTransport -PageJson @(
            '{"changeEntries":[{"changeTrackingId":1}],"nextSkip":1,"nextTop":1}'
            '{"changeEntries":[{"changeTrackingId":1}],"nextSkip":1,"nextTop":1}'
        )

        { Get-AdoPullRequestIterationChange -Transport $fake.Transport -Target (New-TestTarget) -IterationId 2 } |
            Should -Throw -ExpectedMessage '*does not advance*'

        $fake.Requests.Count | Should -Be 2
    }

    It 'refuses a payload that carries no change collection' {
        # Read as an empty set, this would report every file as unchanged and reject every finding,
        # which looks exactly like a correct refusal.
        $fake = New-AdoChangeTransport -PageJson @('{"message":"TF401019: the iteration does not exist"}')

        { Get-AdoPullRequestIterationChange -Transport $fake.Transport -Target (New-TestTarget) -IterationId 9 } |
            Should -Throw -ExpectedMessage "*no 'changeEntries' collection*"
    }

    It 'accepts an iteration that changed nothing' {
        $fake = New-AdoChangeTransport -PageJson @('{"changeEntries":[]}')

        $changes = Get-AdoPullRequestIterationChange -Transport $fake.Transport -Target (New-TestTarget) -IterationId 2

        @($changes).Count | Should -Be 0
    }

    It 'reports the newest iteration rather than the last one listed' {
        $fake = New-AdoContextTransport

        Get-AdoPullRequestLatestIterationId -Transport $fake.Transport -Target (New-TestTarget) | Should -Be 2
    }

    It 'refuses a pull request that reports no iteration at all' {
        $fake = New-AdoContextTransport -IterationsJson '{"count":0,"value":[]}'

        { Get-AdoPullRequestLatestIterationId -Transport $fake.Transport -Target (New-TestTarget) } |
            Should -Throw -ExpectedMessage '*no iteration*'
    }

    It 'reports each item path as the service stated it' {
        # The service form is preserved so that the single conversion point stays the only place a
        # path changes shape.
        $fake = New-AdoChangeTransport -PageJson @('{"changeEntries":[{"changeTrackingId":1,"changeType":"edit","item":{"path":"/.pipelines/stages/code-guidelines.yml"}}]}')

        $changes = Get-AdoPullRequestIterationChange -Transport $fake.Transport -Target (New-TestTarget) -IterationId 2

        @($changes)[0]['item']['path'] | Should -Be '/.pipelines/stages/code-guidelines.yml'
    }

    It 'converts an item path to the canonical form and back' {
        ConvertFrom-AdoItemPath -Path '/src/lib.rs' | Should -Be 'src/lib.rs'
        ConvertTo-AdoItemPath -Path 'src/lib.rs' | Should -Be '/src/lib.rs'

        # A path already in the target form survives the conversion unchanged, so a call site that
        # cannot tell which form it holds is still safe.
        ConvertFrom-AdoItemPath -Path 'src/lib.rs' | Should -Be 'src/lib.rs'
        ConvertTo-AdoItemPath -Path '/src/lib.rs' | Should -Be '/src/lib.rs'

        ConvertFrom-AdoItemPath -Path 'src\lib.rs' | Should -Be 'src/lib.rs'
        ConvertFrom-AdoItemPath -Path '' | Should -Be ''
        ConvertTo-AdoItemPath -Path '' | Should -Be ''
    }

    It 'reads a change type that states several types at once' {
        # Joined for the comparison so that a single token and a set are asserted the same way.
        ((ConvertFrom-AdoChangeType -ChangeType 'edit') -join '|') | Should -Be 'edit'
        ((ConvertFrom-AdoChangeType -ChangeType 'edit, rename') -join '|') | Should -Be 'edit|rename'
        ((ConvertFrom-AdoChangeType -ChangeType 'Add') -join '|') | Should -Be 'add'

        # Assigned before it is counted: a command that emits a single array emits it as one object,
        # so counting the call directly would count the array rather than what it holds.
        $noTokens = ConvertFrom-AdoChangeType -ChangeType ''
        @($noTokens).Count | Should -Be 0
    }
}
