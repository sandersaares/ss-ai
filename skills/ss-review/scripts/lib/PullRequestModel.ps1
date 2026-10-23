#Requires -Version 7.6

# Assembles the captured pull request into the document the review consumes
# (docs/implementation.md, "Canonical document").
#
# The platform adapters already present their results in a common shape, so nothing here knows
# anything about Azure DevOps or GitHub. What is left is the work that must be identical on both
# platforms and must not drift between them: chronological ordering, marking what the running
# identity wrote, serialization, and the comparison between two captures that tells a re-review what
# changed since it last looked.
#
# Functions from lib\JsonValue.ps1 are called directly; the executable dot-sources both.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The document format version. A consumer that reads a capture written by an older helper can refuse
# it rather than misinterpret it.
$script:PullRequestContextSchemaVersion = 3

# Bump when the semantic classifier's evidence contract or policy changes, even if the record
# shape does not. Ref: docs/implementation.md, "Authorship classification".
$script:CommentAuthorshipPolicyVersion = '2'
$script:CommentAuthorshipSchemaPath = Join-Path $PSScriptRoot '..\..\schemas\comment-authorship.schema.json'

# Numeric identifiers are compared as text alongside opaque node identifiers, so they are left-padded
# to a width no realistic identifier reaches. Without this, thread 10 would sort before thread 9.
$script:IdentifierSortWidth = 20

function Get-IdentifierSortKey {
    param(
        [AllowNull()]
        $Value
    )

    $text = if ($null -eq $Value) { '' } else { [string] $Value }
    if ($text -match '^\d+$') {
        return $text.PadLeft($script:IdentifierSortWidth, '0')
    }
    return $text
}

# Sorts on the event's own time, then on identifier so that two events sharing a timestamp keep a
# stable order across captures. An empty timestamp sorts last rather than first, because an event we
# could not date is not evidence that it happened before everything else.
function Get-ChronologicalSortKey {
    param(
        [AllowNull()]
        [string] $Timestamp,

        [AllowNull()]
        $Id
    )

    $stamp = if ([string]::IsNullOrWhiteSpace($Timestamp)) { '9999-12-31T23:59:59.9999999Z' } else { $Timestamp }
    "$stamp|$(Get-IdentifierSortKey -Value $Id)"
}

function Get-RepositoryUrl {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    if ($Target['platform'] -eq 'gitHub') {
        $owner = [uri]::EscapeDataString($Target['organization'])
        $repository = [uri]::EscapeDataString($Target['repository'])
        return "https://github.com/$owner/$repository"
    }

    $project = [uri]::EscapeDataString($Target['project'])
    $repository = [uri]::EscapeDataString($Target['repository'])
    "$($Target['collectionUrl'])/$project/_git/$repository"
}

# Orders dictionaries by a computed text key.
#
# Sort-Object is avoided here because it returns its input wrapped in PSObject, and indexing such a
# wrapper by name binds to the integer indexer of OrderedDictionary rather than the key indexer,
# which turns a later property assignment into a type conversion failure. Sorting the keys and the
# items as parallel arrays also lets the comparison be stated as ordinal, so a capture sorts the same
# way under every locale.
function Sort-DictionaryByTextKey {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Item,

        [Parameter(Mandatory)]
        [scriptblock] $KeySelector
    )

    if ($Item.Count -eq 0) {
        return , @()
    }

    $keys = [string[]]::new($Item.Count)
    $values = [object[]]::new($Item.Count)
    for ($index = 0; $index -lt $Item.Count; $index++) {
        $keys[$index] = [string] (& $KeySelector $Item[$index])
        $values[$index] = $Item[$index]
    }

    [array]::Sort($keys, $values, [System.Collections.IComparer] [StringComparer]::Ordinal)
    return , $values
}

# Produces the capture document from what an adapter returned.
#
# Ordering is applied here rather than in the adapters so that both platforms are guaranteed to
# present the same discussion in the same order, which is what makes two captures comparable and the
# reading order stable between runs.
function New-PullRequestContextDocument {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Context,

        [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
    )

    $target = $Context['target']
    $authenticated = Get-JsonProperty -Node $Context -Name 'authenticatedIdentity'

    $threads = [System.Collections.Generic.List[object]]::new()
    $sourceThreads = Get-JsonArrayProperty -Node $Context -Name 'threads'
    foreach ($thread in $sourceThreads) {
        $comments = Get-JsonArrayProperty -Node $thread -Name 'comments'
        $ordered = Sort-DictionaryByTextKey -Item $comments -KeySelector {
            param($comment)
            Get-ChronologicalSortKey -Timestamp (Get-JsonProperty -Node $comment -Name 'publishedUtc') -Id (Get-JsonProperty -Node $comment -Name 'id')
        }

        foreach ($comment in $ordered) {
            $isSelf = Test-PullRequestIdentityIsSelf `
                -Identity (Get-JsonProperty -Node $comment -Name 'author') `
                -AuthenticatedIdentity $authenticated
            Set-JsonProperty -Node $comment -Name 'isAuthoredBySelf' -Value $isSelf
        }

        Set-JsonProperty -Node $thread -Name 'comments' -Value @($ordered)

        # A thread counts as the running identity's own when that identity opened it. Phase 9 uses
        # this to tell its own outstanding threads from those it must not reply to unprompted, and
        # that question is about who raised the point, not about who has since joined the exchange.
        #
        # The opener is the comment the adapter named structurally, not the first in presentation
        # order. Two comments can share a timestamp, and a comment can carry none at all, so the
        # first in presentation order is a reply often enough to matter, and attributing the thread
        # to a reply author would misreport whose point it is. A thread carrying no comment - a force
        # push - is attributed from its decision instead. Identifier comparison is ordinal, because a
        # GitHub node identifier is a case-sensitive opaque string.
        $openingCommentId = [string] (Get-JsonProperty -Node $thread -Name 'openingCommentId')
        $starter = $null
        if (-not [string]::IsNullOrEmpty($openingCommentId)) {
            foreach ($comment in $ordered) {
                if ([string]::Equals([string] (Get-JsonProperty -Node $comment -Name 'id'), $openingCommentId, [StringComparison]::Ordinal)) {
                    $starter = Get-JsonProperty -Node $comment -Name 'author'
                    break
                }
            }
        }
        if ($null -eq $starter) {
            $decision = Get-JsonProperty -Node $thread -Name 'decision'
            if ($null -ne $decision) { $starter = Get-JsonProperty -Node $decision -Name 'by' }
        }
        Set-JsonProperty -Node $thread -Name 'isAuthoredBySelf' -Value (Test-PullRequestIdentityIsSelf -Identity $starter -AuthenticatedIdentity $authenticated)

        $threads.Add($thread)
    }

    $orderedThreads = Sort-DictionaryByTextKey -Item $threads.ToArray() -KeySelector {
        param($thread)
        Get-ChronologicalSortKey -Timestamp (Get-JsonProperty -Node $thread -Name 'eventUtc') -Id (Get-JsonProperty -Node $thread -Name 'id')
    }

    $reviewers = Get-JsonArrayProperty -Node $Context -Name 'reviewers'
    $revisions = Get-JsonArrayProperty -Node $Context -Name 'revisions'

    $pullRequest = Get-JsonProperty -Node $Context -Name 'pullRequest'
    if ($null -ne $pullRequest) {
        # Whether the running account is the pull request's own author. The audit reads this to decide
        # whether to offer approving the merge, because a reviewer does not approve the merge of their
        # own pull request (docs/design.md, "Review publication"; SKILL.md, "Phase 7: audit"). Derived
        # here alongside the per-thread self attribution so the same identity comparison settles both.
        Set-JsonProperty -Node $pullRequest -Name 'isAuthoredBySelf' -Value (Test-PullRequestIdentityIsSelf -Identity (Get-JsonProperty -Node $pullRequest -Name 'author') -AuthenticatedIdentity $authenticated)
    }

    [ordered]@{
        schemaVersion         = $script:PullRequestContextSchemaVersion
        fetchedUtc            = $NowUtc.UtcDateTime.ToString('o', [cultureinfo]::InvariantCulture)
        platform              = $target['platform']
        repositoryUrl         = Get-RepositoryUrl -Target $target
        pullRequestUrl        = $target['webUrl']
        target                = $target
        authenticatedIdentity = $authenticated
        pullRequest           = $pullRequest
        reviewers             = $reviewers
        revisions             = $revisions
        threads               = $orderedThreads
    }
}

# Converts a path to an absolute filesystem path.
#
# PowerShell's current location and the process working directory are separate settings, and
# `Set-Location` moves only the former. A relative path therefore resolves to one directory through
# the PowerShell provider (`New-Item`, `Test-Path`) and to a different one through System.IO
# (`File::WriteAllText`, `Path::GetFullPath`), so a capture would create its directory in one place
# and write the file in another. Resolving through the provider up front makes every subsequent
# System.IO call agree with the caller's location.
function Resolve-PullRequestPath {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
}

# Recognizes actual file contention without treating other I/O failures as retryable.
# FileStream uses HRESULT_FROM_WIN32(ERROR_SHARING_VIOLATION) on Windows and
# the native EWOULDBLOCK errno through .NET's Unix Interop.IOErrors on Linux/macOS.
function Test-PullRequestFileContention {
    param([Parameter(Mandatory)][Exception] $Exception)
    $cause = $Exception.GetBaseException()
    $cause -is [IO.IOException] -and (
        ($IsWindows -and $cause.HResult -eq [int]0x80070020) -or
        ($IsLinux -and $cause.HResult -eq 11) -or ($IsMacOS -and $cause.HResult -eq 35))
}

# Retries only atomic file operations whose sharing violation means no change was committed.
# Never wrap an entire read-modify-write transaction or a remote operation in this retry.
# Ref: docs/implementation.md, "Shared workflow files".
function Invoke-PullRequestFileOperation {
    param([Parameter(Mandatory)][string] $Path, [Parameter(Mandatory)][scriptblock] $Operation)
    # Short polling keeps ordinary commands responsive; the budget accommodates bursts of
    # schema-validated updates on a busy host without waiting indefinitely for a stuck owner.
    $retryCount = 600
    $delayMilliseconds = 50
    for ($attempt = 0; $attempt -le $retryCount; $attempt++) {
        try { return & $Operation }
        catch [IO.IOException] {
            if (-not (Test-PullRequestFileContention -Exception $_.Exception)) { throw }
            if ($attempt -eq $retryCount) {
                throw [IO.IOException]::new("Shared file '$Path' remains busy after bounded contention retries.", $_.Exception)
            }
            Start-Sleep -Milliseconds $delayMilliseconds
        }
    }
}

# Retains the sibling lock so replacing data cannot split ownership across different file handles.
function Open-PullRequestFileLock {
    param([Parameter(Mandatory)][string] $Path, [switch] $Shared)
    $fullPath = Resolve-PullRequestPath -Path $Path
    [IO.Directory]::CreateDirectory((Split-Path -Parent $fullPath)) | Out-Null
    Invoke-PullRequestFileOperation -Path $fullPath -Operation {
        # Shared ownership protects readers of a multi-file resource from its exclusive installer.
        if ($Shared) {
            return [IO.File]::Open("$fullPath.lock", [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        }
        [IO.File]::Open("$fullPath.lock", [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    }
}

# Readers keep a complete opened version while allowing an atomic publisher to replace its name.
function Open-PullRequestFileRead {
    param([Parameter(Mandatory)][string] $Path)
    $fullPath = Resolve-PullRequestPath -Path $Path
    Invoke-PullRequestFileOperation -Path $fullPath -Operation {
        [IO.File]::Open($fullPath, [IO.FileMode]::Open, [IO.FileAccess]::Read,
            ([IO.FileShare]::Read -bor [IO.FileShare]::Delete))
    }
}

# Shared text reads preserve the encoding contract without blocking atomic replacement.
function Read-PullRequestFileText {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Text.Encoding] $Encoding = [Text.Encoding]::UTF8
    )
    $stream = Open-PullRequestFileRead -Path $Path
    try {
        $reader = [IO.StreamReader]::new($stream, $Encoding, $true)
        try { $reader.ReadToEnd() } finally { $reader.Dispose() }
    } finally { $stream.Dispose() }
}

# Hashing and binary publication read the same opened version, not a later replacement.
function Read-PullRequestFileBytes {
    param([Parameter(Mandatory)][string] $Path)
    $stream = Open-PullRequestFileRead -Path $Path
    try {
        $buffer = [IO.MemoryStream]::new()
        try { $stream.CopyTo($buffer); return ,$buffer.ToArray() } finally { $buffer.Dispose() }
    } finally { $stream.Dispose() }
}

# Only uniquely named staging files use direct writes; shared outputs go through atomic publication.
function Write-PullRequestStagingFile {
    param([Parameter(Mandatory)][string] $Path, [Parameter(Mandatory)][AllowEmptyString()][string] $Content)
    [IO.File]::WriteAllText($Path, $Content.Replace("`r`n", "`n"), [Text.UTF8Encoding]::new($false))
}

# ReplaceFile permits open delete-sharing readers on Windows; MoveFileEx overwrite does not.
# Creation uses a non-overwriting move, then handles a competing creator without losing its file.
function Publish-PullRequestStagingFile {
    param([Parameter(Mandatory)][string] $TemporaryPath, [Parameter(Mandatory)][string] $Path)
    Invoke-PullRequestFileOperation -Path $Path -Operation {
        if ([IO.File]::Exists($Path)) {
            [IO.File]::Replace($TemporaryPath, $Path, [NullString]::Value)
        } else {
            try { [IO.File]::Move($TemporaryPath, $Path) }
            catch [IO.IOException] {
                $code = $_.Exception.GetBaseException().HResult
                # Windows ERROR_FILE_EXISTS / ERROR_ALREADY_EXISTS and Unix EEXIST mean
                # another whole-file publisher won creation, not that the staging move committed.
                $created = ($IsWindows -and $code -in @([int]0x80070050, [int]0x800700B7)) -or
                    (($IsLinux -or $IsMacOS) -and $code -eq 17)
                if (-not $created) { throw }
                [IO.File]::Replace($TemporaryPath, $Path, [NullString]::Value)
            }
        }
    }
}

# Writes one complete capture output.
#
# Line endings are normalized and the byte order mark is omitted so that a capture taken on any host
# produces identical bytes, which is what lets a re-review diff two captures meaningfully. UTF-8 is
# stated explicitly rather than inherited from the console, since an inherited code page is how
# non-ASCII content gets destroyed in the first place.
function Write-PullRequestOutputFile {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Content
    )

    Write-PullRequestOutputFileAtomically -Path $Path -Content $Content
}

# Writes a replacement without exposing a partially serialized artifact or journal to readers.
# Finding artifacts and proactor recovery state use this when the previous complete document must
# remain available until the new one is ready. A same-directory rename is the commit point.
# Ref: docs/implementation.md, "Finding artifacts".
function Write-PullRequestOutputFileAtomically {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Content
    )

    $resolved = Resolve-PullRequestPath -Path $Path
    $directory = Split-Path -Parent $resolved
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    $temporary = Join-Path $directory ".$([IO.Path]::GetFileName($resolved)).$([guid]::NewGuid().ToString('N')).tmp"
    try {
        Write-PullRequestStagingFile -Path $temporary -Content $Content
        Publish-PullRequestStagingFile -TemporaryPath $temporary -Path $resolved
    } finally {
        if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) }
    }
}

# Reads a capture written by an earlier run.
#
# The schema version is enforced rather than merely recorded, because the comparison that consumes
# this document indexes into thread and comment identifiers. A capture whose identifiers were shaped
# differently would not fail: it would report every thread as removed and every thread as added, or
# report no change at all, and that verdict is what decides whether a review may be published.
function Read-PullRequestContextDocument {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [switch] $WithoutAuthorship
    )

    $fullPath = Resolve-PullRequestPath -Path $Path
    $document = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $fullPath)

    $version = Get-JsonProperty -Node $document -Name 'schemaVersion'
    if ($version -ne $script:PullRequestContextSchemaVersion) {
        $reported = if ($null -eq $version) { 'no version' } else { "version $version" }
        throw "The capture at '$Path' reports $reported, but this helper reads version $($script:PullRequestContextSchemaVersion). Capture the pull request again instead of comparing against it."
    }

    if (-not $WithoutAuthorship) {
        $decisions = Read-CommentAuthorship -Path $fullPath -Document $document
        Merge-CommentAuthorship -Document $document -Decisions $decisions | Out-Null
    }
    $document
}

# Hashes exact serialized inputs for snapshot binding, never normalizing body text.
function Get-PullRequestDocumentHash {
    param([Parameter(Mandatory)] $Value)
    $json = ConvertTo-Json -InputObject $Value -Depth 100 -Compress
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($json)))
}

# Reading decisions must bind policy as well as evidence, including a wholly indeterminate batch.
function Get-PullRequestContextSnapshotId {
    param([Parameter(Mandatory)][Collections.IDictionary] $Document)
    Get-PullRequestDocumentHash -Value ([ordered]@{
        policyVersion = $script:CommentAuthorshipPolicyVersion
        document = $Document
    })
}

# Compares review evidence without capture time, live votes or semantic classification progress.
# The publication guard uses the same fingerprint as freshness and approved-artifact binding.
function Get-PullRequestEvidenceHash {
    param([Parameter(Mandatory)][Collections.IDictionary] $Document)
    $pullRequest = [ordered]@{}
    foreach ($key in @('title', 'description', 'author', 'sourceRef', 'targetRef', 'reviewedSourceCommit', 'reviewedMergeBaseCommit')) {
        $pullRequest[$key] = Get-JsonProperty -Node $Document['pullRequest'] -Name $key
    }
    $threads = @(foreach ($thread in (Get-JsonArrayProperty -Node $Document -Name 'threads')) {
        $copy = ConvertFrom-JsonVerbatim -Text (ConvertTo-JsonText -Value $thread)
        foreach ($comment in (Get-JsonArrayProperty -Node $copy -Name 'comments')) {
            $comment.Remove('authorship')
            $evidence = Get-JsonArrayProperty -Node $comment -Name 'authorshipEvidence'
            $comment['authorshipEvidence'] = @($evidence | Where-Object { $_ -cne 'classification:reviewed' })
        }
        $copy
    })
    Get-PullRequestDocumentHash -Value ([ordered]@{
        platform = $Document['platform']; repositoryUrl = $Document['repositoryUrl']
        pullRequestUrl = $Document['pullRequestUrl']; pullRequest = $pullRequest; threads = $threads
    })
}

# Selects the classifier's entire evidence contract. Capture time and source movement are not
# authorship inputs; exact body, attribution and deterministic evidence are.
function Get-CommentAuthorshipInputHash {
    param(
        [Parameter(Mandatory)][Collections.IDictionary] $Document,
        [Parameter(Mandatory)][string] $ThreadId,
        [Parameter(Mandatory)][Collections.IDictionary] $Comment
    )
    $evidence = Get-JsonArrayProperty -Node $Comment -Name 'authorshipEvidence'
    $inputValue = [ordered]@{
        schemaVersion = 1; policyVersion = $script:CommentAuthorshipPolicyVersion
        platform = $Document['platform']; repositoryUrl = $Document['repositoryUrl']
        pullRequestUrl = $Document['pullRequestUrl']
        threadId = $ThreadId; commentId = [string] $Comment['id']
        content = $Comment['content']; author = $Comment['author']
        isAuthoredBySelf = Get-JsonProperty -Node $Comment -Name 'isAuthoredBySelf'
        evidence = @($evidence | Where-Object { $_ -cne 'classification:reviewed' })
    }
    Get-PullRequestDocumentHash -Value $inputValue
}

# Empty decisions are explicit. Completion is derived from the effective capture, not a saved flag.
function New-CommentAuthorship {
    param([Parameter(Mandatory)][Collections.IDictionary] $Document)
    [ordered]@{
        schemaVersion = 1; policyVersion = $script:CommentAuthorshipPolicyVersion
        captureHash = Get-PullRequestDocumentHash -Value $Document
        decisions = @()
    }
}

# Validates the separate semantic record at both producer and consumer boundaries.
function Assert-CommentAuthorship {
    param([Parameter(Mandatory)][Collections.IDictionary] $Decisions)
    $json = ConvertTo-Json -InputObject $Decisions -Depth 100 -Compress
    if (-not (Test-Json -Json $json -SchemaFile $script:CommentAuthorshipSchemaPath -ErrorAction Stop)) {
        throw 'Invalid comment authorship record.'
    }
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $Decisions['decisions']) {
        $key = ConvertTo-Json -InputObject @($entry['threadId'], $entry['commentId']) -Compress
        if (-not $seen.Add($key)) { throw 'Duplicate comment authorship decision.' }
    }
}

# The sidecar belongs to one immutable capture. Reuse across captures is explicit at capture time.
function Read-CommentAuthorship {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][Collections.IDictionary] $Document
    )
    $sidecar = "$(Resolve-PullRequestPath -Path $Path).authorship.json"
    if (-not [IO.File]::Exists($sidecar)) { return New-CommentAuthorship -Document $Document }
    $decisions = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $sidecar)
    Assert-CommentAuthorship -Decisions $decisions
    if ($decisions['captureHash'] -cne (Get-PullRequestDocumentHash -Value $Document)) {
        throw 'Authorship decisions do not belong to this capture. Preserve the original capture and refresh to a new path.'
    }
    $decisions
}

# Current provider checks win. Only semantic decisions with identical inputs and policy may apply.
# Removed comments stay only in the baseline record; they never enter the current capture.
function Merge-CommentAuthorship {
    param(
        [Parameter(Mandatory)][Collections.IDictionary] $Document,
        [Parameter(Mandatory)][Collections.IDictionary] $Decisions
    )
    Assert-CommentAuthorship -Decisions $Decisions
    $matched = [Collections.Generic.List[object]]::new()
    if ($Decisions['policyVersion'] -ceq $script:CommentAuthorshipPolicyVersion) {
        $index = Get-ThreadCommentIndex -Document $Document
        foreach ($entry in $Decisions['decisions']) {
            $threadId = $entry['threadId']; $commentId = $entry['commentId']
            if (-not $index.Contains($threadId) -or -not $index[$threadId]['comments'].Contains($commentId)) { continue }
            $comment = $index[$threadId]['comments'][$commentId]
            if ($comment['authorship'] -cne 'indeterminate') { continue }
            if ($entry['inputHash'] -cne (Get-CommentAuthorshipInputHash -Document $Document -ThreadId $threadId -Comment $comment)) { continue }
            $comment['authorship'] = $entry['kind']
            $comment['authorshipEvidence'] = @($comment['authorshipEvidence']) + 'classification:reviewed'
            $matched.Add($entry)
        }
    }
    , $matched.ToArray()
}

# Persists a validated semantic batch only after its immutable reading snapshot still matches.
# A single writer owns classification; cursors bind the effective document including this overlay.
function Save-CommentAuthorship {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][string] $SnapshotId,
        [Parameter(Mandatory)][AllowEmptyCollection()][object[]] $Classification
    )
    $raw = Read-PullRequestContextDocument -Path $Path -WithoutAuthorship
    $document = Read-PullRequestContextDocument -Path $Path
    if ((Get-PullRequestContextSnapshotId -Document $document) -cne $SnapshotId) {
        throw 'The classification snapshot changed. Restart the bounded reading batch without its cursor.'
    }
    $decisions = New-CommentAuthorship -Document $raw
    $existing = Read-CommentAuthorship -Path $Path -Document $raw
    $entries = [Collections.Generic.List[object]]::new()
    foreach ($entry in (Merge-CommentAuthorship -Document $raw -Decisions $existing)) { $entries.Add($entry) }
    $index = Get-ThreadCommentIndex -Document $document
    # Validate the entire batch before writing. The in-memory document may change; captured bytes do not.
    $summary = Set-PullRequestCommentAuthorship -Document $document -Classification $Classification
    foreach ($entry in $Classification) {
        $threadId = [string] $entry['threadId']; $commentId = [string] $entry['commentId']
        $comment = $index[$threadId]['comments'][$commentId]
        if ('classification:reviewed' -cnotin $comment['authorshipEvidence']) { continue }
        if (@($entries | Where-Object { $_['threadId'] -ceq $threadId -and $_['commentId'] -ceq $commentId }).Count) { continue }
        $entries.Add([ordered]@{
            threadId = $threadId; commentId = $commentId; kind = $comment['authorship']
            inputHash = Get-CommentAuthorshipInputHash -Document $document -ThreadId $threadId -Comment $comment
            origin = 'classified'
        })
    }
    $decisions['decisions'] = $entries.ToArray()
    Assert-CommentAuthorship -Decisions $decisions
    Write-PullRequestOutputFileAtomically -Path "$Path.authorship.json" -Content (ConvertTo-JsonText -Value $decisions)
    $summary
}

# Freezes a fresh capture and its reused decisions without replacing any earlier evidence.
function Write-PullRequestCapture {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][Collections.IDictionary] $Document,
        [Collections.IDictionary] $PreviousDecisions
    )
    $transactionLock = Open-PullRequestFileLock -Path "$Path.authorship.json"
    try {
        if ([IO.File]::Exists((Resolve-PullRequestPath -Path $Path)) -or [IO.File]::Exists((Resolve-PullRequestPath -Path "$Path.authorship.json"))) {
            throw 'Capture output already exists. Choose a new OutputPrefix to preserve every comparison and audit baseline.'
        }
        $rawText = ConvertTo-JsonText -Value $Document
        $decisions = New-CommentAuthorship -Document $Document
        if ($null -ne $PreviousDecisions) {
            $decisions['decisions'] = @(foreach ($entry in (Merge-CommentAuthorship -Document $Document -Decisions $PreviousDecisions)) {
                [ordered]@{
                    threadId = $entry['threadId']; commentId = $entry['commentId']; kind = $entry['kind']
                    inputHash = $entry['inputHash']; origin = 'reused'
                }
            })
        }
        Assert-CommentAuthorship -Decisions $decisions
        Write-PullRequestOutputFileAtomically -Path "$Path.authorship.json" -Content (ConvertTo-JsonText -Value $decisions)
        # The immutable capture is the commit point: any visible capture has its complete sidecar.
        Write-PullRequestOutputFileAtomically -Path $Path -Content $rawText
        $decisions['decisions'].Count
    } finally { $transactionLock.Dispose() }
}

function Get-ThreadCommentIndex {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document
    )

    # Ordinal keys, because the keys are platform identifiers rather than field names, and a GitHub
    # node identifier is a case-sensitive opaque string. The default comparer of a PowerShell
    # dictionary ignores case, which would let two distinct comments collapse onto one entry: the
    # delta would then miss a comment and a classification would be written to the wrong one.
    $index = [System.Collections.Specialized.OrderedDictionary]::new([System.StringComparer]::Ordinal)
    $threads = Get-JsonArrayProperty -Node $Document -Name 'threads'
    foreach ($thread in $threads) {
        $threadId = [string] (Get-JsonProperty -Node $thread -Name 'id')
        $comments = [System.Collections.Specialized.OrderedDictionary]::new([System.StringComparer]::Ordinal)
        $threadComments = Get-JsonArrayProperty -Node $thread -Name 'comments'
        foreach ($comment in $threadComments) {
            $comments[[string] (Get-JsonProperty -Node $comment -Name 'id')] = $comment
        }
        $index[$threadId] = [ordered]@{ thread = $thread; comments = $comments }
    }
    $index
}

# Compares an earlier capture with the current one.
#
# The freshness check that precedes publication has to know what appeared since the review began, and
# on GitHub it is the only way to notice a deletion at all, because a deleted comment leaves no
# tombstone and is simply absent the next time. Reporting the difference here keeps that judgement
# off the reviewer, who would otherwise have to spot it by eye across two large documents.
function Get-PullRequestContextDelta {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Previous,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Current
    )

    $previousIndex = Get-ThreadCommentIndex -Document $Previous
    $currentIndex = Get-ThreadCommentIndex -Document $Current

    $threadsAdded = [System.Collections.Generic.List[string]]::new()
    $threadsRemoved = [System.Collections.Generic.List[string]]::new()
    $commentsAdded = [System.Collections.Generic.List[object]]::new()
    $commentsRemoved = [System.Collections.Generic.List[object]]::new()
    $commentsEdited = [System.Collections.Generic.List[object]]::new()
    $resolutionChanged = [System.Collections.Generic.List[object]]::new()

    foreach ($threadId in $currentIndex.Keys) {
        if (-not $previousIndex.Contains($threadId)) {
            $threadsAdded.Add($threadId)

            # A new thread's comments are reported individually as well as through the thread. The
            # counts are what a reader sizes the new feedback by, and counting only the comments of
            # threads that already existed would understate a burst of fresh review, which is
            # exactly the case the freshness check exists to catch. The mirror image on removal
            # below reports a vanished thread's comments the same way.
            foreach ($commentId in $currentIndex[$threadId]['comments'].Keys) {
                $commentsAdded.Add([ordered]@{ threadId = $threadId; commentId = $commentId })
            }
            continue
        }

        $before = $previousIndex[$threadId]
        $after = $currentIndex[$threadId]

        foreach ($commentId in $after['comments'].Keys) {
            if (-not $before['comments'].Contains($commentId)) {
                $commentsAdded.Add([ordered]@{ threadId = $threadId; commentId = $commentId })
                continue
            }

            # An edit keeps the comment's identifier, so a comparison that only matches identifiers
            # reports no change while the text the reviewer read has been replaced. That verdict
            # gates publication, so a rewritten comment has to count as something that changed.
            $beforeEdit = Get-JsonProperty -Node $before['comments'][$commentId] -Name 'lastEditedUtc'
            $afterEdit = Get-JsonProperty -Node $after['comments'][$commentId] -Name 'lastEditedUtc'
            if ($beforeEdit -cne $afterEdit -or
                (Get-CommentAuthorshipInputHash -Document $Previous -ThreadId $threadId -Comment $before['comments'][$commentId]) -cne
                (Get-CommentAuthorshipInputHash -Document $Current -ThreadId $threadId -Comment $after['comments'][$commentId])) {
                $commentsEdited.Add([ordered]@{ threadId = $threadId; commentId = $commentId })
            }
        }
        foreach ($commentId in $before['comments'].Keys) {
            if (-not $after['comments'].Contains($commentId)) {
                $commentsRemoved.Add([ordered]@{ threadId = $threadId; commentId = $commentId })
            }
        }

        $beforeResolution = Get-JsonProperty -Node $before['thread'] -Name 'resolution'
        $afterResolution = Get-JsonProperty -Node $after['thread'] -Name 'resolution'
        if ($beforeResolution -ne $afterResolution) {
            $resolutionChanged.Add([ordered]@{ threadId = $threadId; from = $beforeResolution; to = $afterResolution })
        }
    }

    foreach ($threadId in $previousIndex.Keys) {
        if (-not $currentIndex.Contains($threadId)) {
            $threadsRemoved.Add($threadId)
            foreach ($commentId in $previousIndex[$threadId]['comments'].Keys) {
                $commentsRemoved.Add([ordered]@{ threadId = $threadId; commentId = $commentId })
            }
        }
    }

    $previousPullRequest = Get-JsonProperty -Node $Previous -Name 'pullRequest'
    $currentPullRequest = Get-JsonProperty -Node $Current -Name 'pullRequest'
    $previousCommit = Get-JsonProperty -Node $previousPullRequest -Name 'reviewedSourceCommit'
    $currentCommit = Get-JsonProperty -Node $currentPullRequest -Name 'reviewedSourceCommit'

    # The merge base is compared as well as the source commit, because the two move independently and
    # either one moving changes the diff under review. A commit landing on the target branch advances
    # the merge base while the source commit stands still, so comparing the source commit alone would
    # report an unchanged pull request whose content the reviewer has not seen.
    $previousMergeBase = Get-JsonProperty -Node $previousPullRequest -Name 'reviewedMergeBaseCommit'
    $currentMergeBase = Get-JsonProperty -Node $currentPullRequest -Name 'reviewedMergeBaseCommit'

    $hasCommitChange = $previousCommit -ne $currentCommit -or $previousMergeBase -ne $currentMergeBase
    $evidenceChanged = (Get-PullRequestEvidenceHash -Document $Previous) -cne (Get-PullRequestEvidenceHash -Document $Current)

    [ordered]@{
        threadsAdded                    = @($threadsAdded)
        threadsRemoved                  = @($threadsRemoved)
        commentsAdded                   = @($commentsAdded)
        commentsRemoved                 = @($commentsRemoved)
        commentsEdited                  = @($commentsEdited)
        resolutionChanged               = @($resolutionChanged)
        reviewedSourceCommitChanged     = $previousCommit -ne $currentCommit
        previousReviewedSourceCommit    = $previousCommit
        reviewedSourceCommit            = $currentCommit
        reviewedMergeBaseCommitChanged  = $previousMergeBase -ne $currentMergeBase
        previousReviewedMergeBaseCommit = $previousMergeBase
        reviewedMergeBaseCommit         = $currentMergeBase
        evidenceChanged                 = $evidenceChanged
        hasChanges                      = $evidenceChanged -or $hasCommitChange
    }
}

# Records the authorship the classification pass settled on
# (docs/design.md, "Authorship classification").
#
# The capture applies deterministic authorship rules and leaves the rest indeterminate, so a
# judgement has to be applied afterwards by a reader that can recognise the agent conventions no
# fixed rule captures. Applying it here rather than in the executable keeps the rules that protect
# the document testable: a classification that names a comment the document does not contain is a
# mistake the caller must hear about, because silently ignoring it would leave the comment
# unclassified while reporting that the pass had finished.
#
# Only an indeterminate comment can be settled. The semantic pass cannot override a deterministic
# classification, including the human default for GitHub vote-only reviews.
#
# The internal result retains unsettled identifiers for programmatic consumers. Model-facing
# commands emit counts; selective queries retrieve the comments needed by the classification pass.
function Set-PullRequestCommentAuthorship {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Classification
    )

    $index = Get-ThreadCommentIndex -Document $Document

    $applied = 0
    $unchanged = 0
    foreach ($entry in $Classification) {
        if ($entry -isnot [Collections.IDictionary] -or $entry.Count -ne 3 -or
            @($entry.Keys | Where-Object { $_ -cnotin @('threadId', 'commentId', 'kind') }).Count -gt 0) {
            throw 'A classification must contain only threadId, commentId and kind.'
        }
        foreach ($name in @('threadId', 'commentId', 'kind')) {
            if ($entry[$name] -isnot [string] -or [string]::IsNullOrWhiteSpace($entry[$name])) {
                throw "Classification '$name' must be a nonempty string."
            }
        }
        $threadId = [string] (Get-JsonProperty -Node $entry -Name 'threadId')
        $commentId = [string] (Get-JsonProperty -Node $entry -Name 'commentId')
        $requestedKind = [string] (Get-JsonProperty -Node $entry -Name 'kind')

        # Leniency at the boundary only. A language model produces this batch and writes 'Human' as
        # readily as 'human', while one rejected entry abandons the whole pass, so the word is
        # matched case-insensitively and stored in lower case. Every comparison against the stored
        # value stays case-sensitive, because the vocabulary itself is case-sensitive.
        $kind = $requestedKind.ToLowerInvariant()
        if ($kind -cne 'agent' -and $kind -cne 'human') {
            throw "Classification for comment '$commentId' in thread '$threadId' names authorship '$requestedKind'. Use 'agent' or 'human'."
        }
        if (-not $index.Contains($threadId)) {
            throw "The capture contains no thread '$threadId'."
        }
        if (-not $index[$threadId]['comments'].Contains($commentId)) {
            throw "The capture contains no comment '$commentId' in thread '$threadId'."
        }

        $comment = $index[$threadId]['comments'][$commentId]
        $current = [string] (Get-JsonProperty -Node $comment -Name 'authorship' -Default 'indeterminate')
        if ($current -cne 'indeterminate') {
            if ($current -ceq $kind) {
                $unchanged++
                continue
            }
            throw "Comment '$commentId' in thread '$threadId' is already classified as '$current' from the evidence the capture collected, so it cannot be reclassified as '$kind'."
        }

        Set-JsonProperty -Node $comment -Name 'authorship' -Value $kind

        # Assigned before use: Get-JsonArrayProperty comma-wraps its result so a single-element list
        # survives the pipeline whole, and wrapping the call in @( ) would nest it one level deeper.
        $evidence = Get-JsonArrayProperty -Node $comment -Name 'authorshipEvidence'
        Set-JsonProperty -Node $comment -Name 'authorshipEvidence' -Value @($evidence + 'classification:reviewed')
        $applied++
    }

    $remaining = [System.Collections.Generic.List[object]]::new()
    foreach ($threadId in $index.Keys) {
        foreach ($commentId in $index[$threadId]['comments'].Keys) {
            $comment = $index[$threadId]['comments'][$commentId]
            if ((Get-JsonProperty -Node $comment -Name 'authorship' -Default 'indeterminate') -cne 'indeterminate') { continue }

            # Preserve identifier types from the capture rather than the index's string keys.
            $remaining.Add([ordered]@{
                threadId  = Get-JsonProperty -Node $index[$threadId]['thread'] -Name 'id'
                commentId = Get-JsonProperty -Node $comment -Name 'id'
            })
        }
    }

    [ordered]@{
        applied               = $applied
        unchanged             = $unchanged
        indeterminate         = $remaining.Count
        indeterminateComments = @($remaining)
        isComplete            = $remaining.Count -eq 0
    }
}

# Refuses a comparison against a file the current capture is about to write.
#
# A refresh that reuses the original prefix would overwrite the baseline and then compare the new
# capture against itself, reporting that nothing changed while destroying the only evidence that
# anything had. That result is both wrong and unrecoverable, and the freshness check it feeds is
# what gates publication, so the run stops instead.
function Assert-PullRequestComparisonPath {
    param(
        [Parameter(Mandatory)]
        [string] $ComparePath,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]] $OutputPath
    )

    $comparisonFullPath = [System.IO.Path]::GetFullPath((Resolve-PullRequestPath -Path $ComparePath))
    foreach ($path in $OutputPath) {
        if ([string]::Equals($comparisonFullPath, [System.IO.Path]::GetFullPath((Resolve-PullRequestPath -Path $path)), [StringComparison]::OrdinalIgnoreCase)) {
            throw "The comparison document '$ComparePath' is one of the files this capture writes. Give the capture a distinct -OutputPrefix so the earlier capture is preserved."
        }
    }
}

# Summarizes a completed capture for the caller that invoked it.
#
# The capture executable writes its document but reports on standard output, and the summary is the
# only part of the run a calling agent reads directly. It lives here rather than in the executable
# so it can be exercised offline: the single-JSON-object contract is precisely what a stray value on
# the pipeline breaks, and that failure is invisible until something downstream parses the output.
#
# Counts keep this model-facing response independent of discussion volume. Identifiers and bodies
# are retrieved through selective queries (docs/implementation.md, "Bounded local query boundary").
function New-PullRequestCaptureResult {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [string] $ContextPath,

        [AllowNull()]
        [System.Collections.IDictionary] $Delta,

        [int] $ReusedClassificationCount = 0
    )

    $threads = Get-JsonArrayProperty -Node $Document -Name 'threads'
    $commentCount = 0
    $indeterminateCount = 0
    foreach ($thread in $threads) {
        $threadComments = Get-JsonArrayProperty -Node $thread -Name 'comments'
        $commentCount += $threadComments.Count
        foreach ($comment in $threadComments) {
            if ((Get-JsonProperty -Node $comment -Name 'authorship' -Default 'indeterminate') -cne 'indeterminate') { continue }
            $indeterminateCount++
        }
    }

    $reviewers = Get-JsonArrayProperty -Node $Document -Name 'reviewers'
    $pullRequest = Get-JsonProperty -Node $Document -Name 'pullRequest'

    [ordered]@{
        platform                  = Get-JsonProperty -Node $Document -Name 'platform'
        pullRequestUrl            = Get-JsonProperty -Node $Document -Name 'pullRequestUrl'
        repositoryUrl             = Get-JsonProperty -Node $Document -Name 'repositoryUrl'
        contextPath               = $ContextPath
        fetchedUtc                = Get-JsonProperty -Node $Document -Name 'fetchedUtc'
        reviewedSourceCommit      = Get-JsonProperty -Node $pullRequest -Name 'reviewedSourceCommit'
        reviewedMergeBaseCommit   = Get-JsonProperty -Node $pullRequest -Name 'reviewedMergeBaseCommit'
        threadCount               = $threads.Count
        commentCount              = $commentCount
        reviewerCount             = $reviewers.Count
        indeterminateCommentCount = $indeterminateCount
        reusedClassificationCount = $ReusedClassificationCount
        delta                     = if ($null -eq $Delta) { $null } else { Get-PullRequestContextDeltaSummary -Delta $Delta }
    }
}

function Get-PullRequestContextDeltaSummary {
    param([Parameter(Mandatory)] [System.Collections.IDictionary] $Delta)

    $result = [ordered]@{}
    foreach ($name in @('threadsAdded', 'threadsRemoved', 'commentsAdded', 'commentsRemoved', 'commentsEdited', 'resolutionChanged')) {
        $result["${name}Count"] = $Delta[$name].Count
    }
    foreach ($name in @(
        'reviewedSourceCommitChanged', 'previousReviewedSourceCommit', 'reviewedSourceCommit',
        'reviewedMergeBaseCommitChanged', 'previousReviewedMergeBaseCommit', 'reviewedMergeBaseCommit', 'evidenceChanged', 'hasChanges'
    )) {
        $result[$name] = $Delta[$name]
    }
    $result
}
