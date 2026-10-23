#Requires -Version 7.6

# Projects canonical evidence into bounded reading pages without changing the capture.
# Ref: docs/implementation.md, "Bounded local query boundary".
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# A small fraction of a review cell's context, measured after JSON escaping rather than as tokens.
$script:PullRequestQueryMaxBytes = 16KB
# Excerpts are discovery hints; conclusions require the attributed comment query.
$script:PullRequestQueryExcerptLength = 240

function Get-PullRequestQueryHash {
    param([Parameter(Mandatory)] $Value)

    Get-PullRequestDocumentHash -Value $Value
}

function ConvertTo-PullRequestQueryPath {
    param([Parameter(Mandatory)] [string] $Path)

    # Repository paths are service identifiers, not host filesystem paths.
    $Path.Replace('\', '/').TrimStart('/')
}

function New-PullRequestQueryOptions {
    param(
        [Parameter(Mandatory)] [string] $Query,
        [string] $Field,
        [AllowEmptyCollection()] [object[]] $Files = @(),
        [string] $Scope = 'all',
        [string] $Kind,
        [string] $Resolution,
        [string] $ThreadId,
        [string] $CommentId,
        [string] $Authorship,
        [switch] $IndexOnly,
        [string] $Text,
        # Keeps ordinary pages small even when the byte budget permits many short records.
        [int] $PageSize = 20
    )

    $Query = $Query.ToLowerInvariant()
    $Scope = $Scope.ToLowerInvariant()
    if ($Query -cnotin @('metadata', 'threads', 'comments', 'search', 'changes')) { throw 'Select metadata, threads, comments, search or changes.' }
    if ($Scope -cnotin @('all', 'files', 'global')) { throw 'Scope must be all, files or global.' }
    # Item and byte limits protect different budgets; a byte-small list can still be hard to inspect.
    if ($PageSize -lt 1 -or $PageSize -gt 50) { throw 'PageSize must be between 1 and 50.' }
    $fields = @(
        'platform', 'repositoryUrl', 'pullRequestUrl', 'fetchedUtc', 'target', 'authenticatedIdentity',
        'id', 'title', 'description', 'author', 'createdUtc', 'sourceRef', 'targetRef',
        'reviewedSourceCommit', 'reviewedMergeBaseCommit', 'isAuthoredBySelf', 'isDraft',
        'reviewers', 'revisions'
    )
    if ($Query -ceq 'metadata') {
        if ($Field -cnotin $fields) { throw 'Metadata requires one supported -Field; see query-pr-context.md.' }
    } elseif ($Field) { throw '-Field is only valid for metadata.' }
    if ($Query -ceq 'search') {
        if ([string]::IsNullOrWhiteSpace($Text)) { throw 'Search requires a nonempty literal -Text.' }
    } elseif ($Text) { throw '-Text is only valid for search.' }
    if ($Kind -and $Kind -cnotin @('text', 'reviewSummary', 'refUpdate', 'reviewersUpdated', 'policy', 'system')) { throw 'Unknown thread kind.' }
    if ($Resolution -and $Resolution -cnotin @('active', 'resolved', 'indeterminate')) { throw 'Unknown resolution.' }
    if ($Authorship -and $Authorship -cnotin @('agent', 'human', 'indeterminate')) { throw 'Unknown authorship.' }
    if ($Query -cne 'comments' -and ($ThreadId -or $CommentId -or $Authorship)) { throw 'ThreadId, CommentId and Authorship are only valid for comments.' }
    if ($IndexOnly -and $Query -cne 'comments') { throw '-IndexOnly is only valid for comments.' }
    if ($CommentId -and -not $ThreadId) { throw '-CommentId requires -ThreadId.' }
    if ($Query -cin @('metadata', 'changes') -and ($Files.Count -or $Scope -cne 'all' -or $Kind -or $Resolution)) {
        throw 'Discussion filters are only valid for threads, comments or search.'
    }

    $paths = [Collections.Generic.List[string]]::new()
    foreach ($file in $Files) {
        if ($file -isnot [string]) { throw 'File selectors must be strings.' }
        $normalized = ConvertTo-PullRequestQueryPath -Path $file
        if ([string]::IsNullOrWhiteSpace($normalized) -or
            @($normalized.Split('/') | Where-Object { $_ -in @('', '.', '..') }).Count -gt 0) {
            throw 'File selectors must be repository-relative paths without empty, dot or parent segments.'
        }
        $paths.Add($normalized)
    }
    if ($paths.Count -gt 0) {
        if ($Scope -ceq 'global') { throw 'File selectors cannot be combined with global scope.' }
        $Scope = 'files'
    } elseif ($Scope -ceq 'files') { throw 'Files scope requires file selectors.' }
    if ($Query -ceq 'comments' -and -not ($ThreadId -or $Authorship -or $Scope -cne 'all')) {
        throw 'Comments requires a thread, authorship, file or global selector. Discover discussion with threads or search first.'
    }
    [ordered]@{
        query = $Query; field = $Field; files = $paths.ToArray(); scope = $Scope
        kind = $Kind; resolution = $Resolution; threadId = $ThreadId; commentId = $CommentId
        authorship = $Authorship; indexOnly = [bool] $IndexOnly; text = $Text; pageSize = $PageSize
    }
}

function Get-PullRequestQueryExcerpt {
    param([AllowEmptyString()] [string] $Text, [int] $MatchOffset = 0)

    # Include a little preceding context without splitting UTF-16 surrogate pairs.
    $start = [Math]::Max(0, $MatchOffset - [int] ($script:PullRequestQueryExcerptLength / 4))
    if ($start -gt 0 -and [char]::IsLowSurrogate($Text[$start])) { $start-- }
    $length = [Math]::Min($script:PullRequestQueryExcerptLength, $Text.Length - $start)
    if ($length -gt 0 -and [char]::IsHighSurrogate($Text[$start + $length - 1])) { $length-- }
    [ordered]@{ excerpt = $Text.Substring($start, $length); excerptOffset = $start; contentLength = $Text.Length }
}

function Get-PullRequestQueryThreadMetadata {
    param([Parameter(Mandatory)] [Collections.IDictionary] $Thread)

    $result = [ordered]@{ threadId = $Thread['id'] }
    foreach ($key in @('url', 'openingCommentId', 'kind', 'resolution', 'isOutdated', 'context', 'decision', 'isAuthoredBySelf', 'eventUtc', 'lastUpdatedUtc')) {
        $result[$key] = Get-JsonProperty -Node $Thread -Name $key
    }
    $result
}

function Test-PullRequestQueryThread {
    param([Parameter(Mandatory)] $Thread, [Parameter(Mandatory)] $Options)

    if ($Options['kind'] -and $Options['kind'] -cne $Thread['kind']) { return $false }
    if ($Options['resolution'] -and $Options['resolution'] -cne $Thread['resolution']) { return $false }
    $path = [string] (Get-JsonProperty -Node (Get-JsonProperty -Node $Thread -Name 'context') -Name 'filePath')
    if ($Options['scope'] -ceq 'global') { return [string]::IsNullOrEmpty($path) }
    if ($Options['scope'] -ceq 'files') {
        if (-not $path) { return $false }
        return $Options['files'] -ccontains (ConvertTo-PullRequestQueryPath -Path $path)
    }
    $true
}

function Get-PullRequestQueryRecord {
    param(
        [Parameter(Mandatory)] [Collections.IDictionary] $Document,
        [Parameter(Mandatory)] [Collections.IDictionary] $Options,
        [AllowNull()] [Collections.IDictionary] $Previous
    )

    $query = $Options['query']
    if ($query -ceq 'metadata') {
        $field = $Options['field']
        $source = if ($field -cin @('platform', 'repositoryUrl', 'pullRequestUrl', 'fetchedUtc', 'target', 'authenticatedIdentity', 'reviewers', 'revisions')) { $Document } else { $Document['pullRequest'] }
        $value = Get-JsonProperty -Node $source -Name $field
        if ($field -cin @('reviewers', 'revisions')) {
            foreach ($entry in (Get-JsonArrayProperty -Node $source -Name $field)) {
                @{ data = [ordered]@{ field = $field; value = $entry }; text = $null }
            }
        } elseif ($value -is [string]) {
            @{ data = [ordered]@{ field = $field }; text = $value }
        } else {
            @{ data = [ordered]@{ field = $field; value = $value }; text = $null }
        }
        return
    }
    if ($query -ceq 'changes') {
        if ($null -eq $Previous) { throw 'Changes requires -ComparePath.' }
        if ($Document['pullRequestUrl'] -cne $Previous['pullRequestUrl']) { throw 'Cannot compare captures of different pull requests.' }
        $delta = Get-PullRequestContextDelta -Previous $Previous -Current $Document
        foreach ($name in @('threadsAdded', 'threadsRemoved', 'commentsAdded', 'commentsRemoved', 'commentsEdited', 'resolutionChanged')) {
            foreach ($change in $delta[$name]) {
                $capture = if ($name.EndsWith('Removed', [StringComparison]::Ordinal)) { 'baseline' } elseif ($name -cin @('commentsEdited', 'resolutionChanged')) { 'both' } else { 'current' }
                $data = [ordered]@{ change = $name; capture = $capture }
                if ($change -is [string]) { $data['threadId'] = $change }
                else { foreach ($key in $change.Keys) { $data[$key] = $change[$key] } }
                @{ data = $data; text = $null }
            }
        }
        foreach ($commit in @('reviewedSourceCommit', 'reviewedMergeBaseCommit')) {
            if ($delta["${commit}Changed"]) {
                $before = if ($commit -ceq 'reviewedSourceCommit') { 'previousReviewedSourceCommit' } else { 'previousReviewedMergeBaseCommit' }
                @{ data = [ordered]@{ change = $commit; capture = 'both'; from = $delta[$before]; to = $delta[$commit] }; text = $null }
            }
        }
        if ($delta['evidenceChanged']) {
            @{ data = [ordered]@{ change = 'reviewEvidence'; capture = 'both' }; text = $null }
        }
        return
    }

    if ($query -ceq 'search' -and $Options['scope'] -cne 'files' -and -not $Options['kind'] -and -not $Options['resolution']) {
        $description = [string] (Get-JsonProperty -Node $Document['pullRequest'] -Name 'description')
        $match = $description.IndexOf($Options['text'], [StringComparison]::OrdinalIgnoreCase)
        if ($match -ge 0) {
            $data = [ordered]@{ field = 'description'; url = $Document['pullRequestUrl']; matchOffset = $match }
            $excerpt = Get-PullRequestQueryExcerpt -Text $description -MatchOffset $match
            foreach ($key in $excerpt.Keys) { $data[$key] = $excerpt[$key] }
            @{ data = $data; text = $null }
        }
    }

    $threadFound = $false
    $commentFound = $false
    foreach ($thread in (Get-JsonArrayProperty -Node $Document -Name 'threads')) {
        if ($Options['threadId'] -and [string] $thread['id'] -cne $Options['threadId']) { continue }
        $threadFound = $true
        $comments = Get-JsonArrayProperty -Node $thread -Name 'comments'
        # Validate explicit identifiers independently of filters; zero filtered results is not a typo.
        if ($Options['commentId']) {
            foreach ($comment in $comments) {
                if ([string] $comment['id'] -ceq $Options['commentId']) { $commentFound = $true }
            }
        }
        if (-not (Test-PullRequestQueryThread -Thread $thread -Options $Options)) { continue }
        if ($query -ceq 'threads') {
            $data = Get-PullRequestQueryThreadMetadata -Thread $thread
            $data['commentCount'] = $comments.Count
            foreach ($comment in $comments) {
                if ([string] $comment['id'] -cne [string] $thread['openingCommentId']) { continue }
                $excerpt = Get-PullRequestQueryExcerpt -Text ([string] $comment['content'])
                foreach ($key in $excerpt.Keys) { $data[$key] = $excerpt[$key] }
                break
            }
            @{ data = $data; text = $null }
            continue
        }
        foreach ($comment in $comments) {
            if ($Options['commentId'] -and [string] $comment['id'] -cne $Options['commentId']) { continue }
            if ($Options['authorship'] -and $Options['authorship'] -cne (Get-JsonProperty -Node $comment -Name 'authorship' -Default 'indeterminate')) { continue }
            $content = [string] $comment['content']
            $match = if ($query -ceq 'search') { $content.IndexOf($Options['text'], [StringComparison]::OrdinalIgnoreCase) } else { 0 }
            if ($match -lt 0) { continue }
            $data = Get-PullRequestQueryThreadMetadata -Thread $thread
            $data['commentId'] = $comment['id']
            $data['commentUrl'] = Get-JsonProperty -Node $comment -Name 'url'
            foreach ($key in @('parentId', 'author', 'authorship', 'authorshipEvidence', 'isAuthoredBySelf', 'publishedUtc', 'lastEditedUtc')) {
                # Self attribution here describes the comment; thread attribution remains explicit.
                if ($key -ceq 'isAuthoredBySelf') { $data['threadIsAuthoredBySelf'] = $data[$key] }
                $data[$key] = Get-JsonProperty -Node $comment -Name $key
            }
            if ($query -ceq 'search') {
                $data['matchOffset'] = $match
                $excerpt = Get-PullRequestQueryExcerpt -Text $content -MatchOffset $match
                foreach ($key in $excerpt.Keys) { $data[$key] = $excerpt[$key] }
                @{ data = $data; text = $null }
            } elseif ($Options['indexOnly']) {
                $data['contentLength'] = $content.Length
                @{ data = $data; text = $null }
            } else { @{ data = $data; text = $content } }
        }
    }
    if ($Options['threadId'] -and -not $threadFound) { throw 'The capture contains no thread with the requested identifier.' }
    if ($Options['commentId'] -and -not $commentFound) { throw 'The requested thread contains no comment with that identifier.' }
}

function ConvertTo-PullRequestQueryPage {
    param(
        [Parameter(Mandatory)] [AllowEmptyCollection()] [object[]] $Records,
        [Parameter(Mandatory)] [Collections.IDictionary] $Options,
        [Parameter(Mandatory)] [string] $SnapshotId,
        [Parameter(Mandatory)] [string] $Fingerprint,
        [string] $Cursor
    )

    $index = 0
    $offset = 0
    if ($Cursor) {
        if ($Cursor -cnotmatch '^([A-F0-9]{64}):([0-9]+):([0-9]+)$') { throw 'Invalid query cursor.' }
        if ($Matches[1] -cne $Fingerprint) { throw 'Stale or mismatched query cursor. Restart the query against the current capture with the intended arguments.' }
        if (-not [int]::TryParse($Matches[2], [ref] $index) -or -not [int]::TryParse($Matches[3], [ref] $offset) -or $index -ge $Records.Count) {
            throw 'Query cursor position is outside the result.'
        }
        $text = $Records[$index]['text']
        if (($null -eq $text -and $offset -ne 0) -or
            ($null -ne $text -and ($offset -ge $text.Length -and $offset -ne 0)) -or
            ($null -ne $text -and $offset -gt 0 -and [char]::IsLowSurrogate($text[$offset]))) {
            throw 'Query cursor text offset is invalid.'
        }
    }

    $items = [Collections.Generic.List[object]]::new()
    $page = [ordered]@{
        schemaVersion = 1; query = $Options['query']; snapshotId = $SnapshotId
        totalCount = $Records.Count; items = @(); nextCursor = $null; complete = $false
    }
    $serialized = $null
    while ($index -lt $Records.Count -and $items.Count -lt $Options['pageSize']) {
        $record = $Records[$index]
        $text = $record['text']
        $hasText = $null -ne $text
        $remaining = if ($hasText) { $text.Length - $offset } else { 0 }
        $low = 0
        $high = [Math]::Min($remaining, $script:PullRequestQueryMaxBytes)
        $best = $null
        $bestLength = -1
        $bestJson = $null
        # Binary search the serialized size, not source length: escaping and attribution also cost space.
        while ($low -le $high) {
            $previousLow = $low
            $previousHigh = $high
            $candidateLength = $low + [int] [Math]::Floor(($high - $low) / 2)
            $length = $candidateLength
            if ($hasText -and $length -gt 0 -and [char]::IsHighSurrogate($text[$offset + $length - 1])) { $length-- }
            $item = [ordered]@{}
            foreach ($key in $record['data'].Keys) { $item[$key] = $record['data'][$key] }
            if ($hasText) {
                $item['content'] = $text.Substring($offset, $length)
                $item['contentOffset'] = $offset
                $item['contentLength'] = $text.Length
                $item['contentComplete'] = $offset + $length -eq $text.Length
            }
            $nextIndex = if ($length -eq $remaining) { $index + 1 } else { $index }
            $nextOffset = if ($nextIndex -eq $index) { $offset + $length } else { 0 }
            $page['items'] = @($items.ToArray()) + @($item)
            $page['complete'] = $nextIndex -eq $Records.Count
            $page['nextCursor'] = if ($page['complete']) { $null } else { "${Fingerprint}:${nextIndex}:${nextOffset}" }
            $json = ConvertTo-Json -InputObject $page -Depth 100 -Compress
            # Include the executable's platform newline in the output budget.
            if ([Text.Encoding]::UTF8.GetByteCount($json + [Environment]::NewLine) -le $script:PullRequestQueryMaxBytes) {
                $best = $item; $bestLength = $length; $bestJson = $json
                $low = $candidateLength + 1
            } else { $high = $candidateLength - 1 }
            if ($low -le $previousLow -and $high -ge $previousHigh) {
                throw 'Query page sizing made no progress.'
            }
        }
        if ($null -eq $best -or ($remaining -gt 0 -and $bestLength -eq 0)) {
            if ($items.Count -gt 0) { break }
            throw 'A query item cannot fit its attribution and a text fragment in the output budget. Narrow the metadata selection.'
        }
        $items.Add($best)
        $serialized = $bestJson
        if ($bestLength -lt $remaining) {
            # A partial comment ends the page; the cursor resumes at its next exact text slice.
            break
        }
        $index++
        $offset = 0
    }
    if ($null -eq $serialized) {
        $page['complete'] = $true
        $serialized = ConvertTo-Json -InputObject $page -Depth 100 -Compress
    }
    $serialized
}

function Read-PullRequestContextQuery {
    param(
        [Parameter(Mandatory)] [Collections.IDictionary] $Document,
        [Parameter(Mandatory)] [Collections.IDictionary] $Options,
        [AllowNull()] [Collections.IDictionary] $Previous,
        [string] $Cursor
    )

    if ($null -ne $Previous -and $Options['query'] -cne 'changes') { throw '-ComparePath is only valid for changes.' }
    $snapshot = Get-PullRequestContextSnapshotId -Document $Document
    $baseline = if ($null -ne $Previous) { Get-PullRequestQueryHash -Value $Previous } else { $null }
    $fingerprint = Get-PullRequestQueryHash -Value ([ordered]@{ version = 1; snapshot = $snapshot; baseline = $baseline; options = $Options })
    $records = @(Get-PullRequestQueryRecord -Document $Document -Options $Options -Previous $Previous)
    ConvertTo-PullRequestQueryPage -Records $records -Options $Options -SnapshotId $snapshot -Fingerprint $fingerprint -Cursor $Cursor
}
