#Requires -Version 7.6

# The finding record and the artifacts that carry it (docs/design.md, "Findings";
# docs/implementation.md, "Finding artifacts").
#
# A finding is written once by the reviewer that found it and then travels through consolidation,
# quality improvement and publication, each of which runs as a separate process. The record therefore
# has to survive serialization unchanged and has to be refused the moment it is malformed, rather
# than at publication, where the whole review has already been paid for.
#
# Validation is deliberately unforgiving. The author of a finding is a language model writing JSON,
# and its most frequent mistake is inventing a field - a priority, a category, a severity - that
# nothing downstream reads. Accepting the document and dropping the field would discard the
# reviewer's judgement silently, so an unknown field is a rejection that names it.
#
# Functions from lib\JsonValue.ps1 and lib\PullRequestModel.ps1 are called directly; the executables
# dot-source all three.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The artifact format version. Findings are merged across artifacts written by different processes,
# so a reader that meets a shape it does not know refuses it instead of merging records it would
# misinterpret.
$script:FindingArtifactSchemaVersion = 1

# Approved records bind discussion as well as commits; working finding records have their own schema.
$script:ApprovedFindingSchemaVersion = 2

# The fields an author writes. Every one of them is required and must carry Markdown prose, because
# a finding that states a problem without stating why it matters or what to do about it cannot be
# triaged by the author of the pull request.
$script:FindingProseFields = @('title', 'description', 'reasoning', 'consequence', 'recommendedAction')

# How long a title may be. A title is a heading in a pull request comment and a field of its own in a
# tracker item, and a tracker refuses one that is too long: GitHub refuses an issue title above 256
# characters and the Azure DevOps title field holds 255. The bound is set below both rather than at
# either, because a customised Azure DevOps process may hold fewer still, so the service is left free
# to refuse what it will not take while a title long enough to be a paragraph is refused here, where
# the author can still fix it.
$script:FindingTitleMaximumLength = 255

# The fields the tooling owns. An author that sets one of these is overriding provenance the
# pipeline depends on, so they are refused with a message distinct from the unknown-field refusal.
$script:FindingToolingFields = @('schemaVersion', 'id', 'sourceFindingIds', 'origin')

# The optional structured fields. `anchor` is absent from this list because a finding that names no
# position cannot be published as a comment.
$script:FindingOptionalFields = @('references', 'impactedLocations', 'followUp')

$script:FindingOriginVocabulary = @('reviewer', 'synthesis')
$script:FindingAnchorKindVocabulary = @('line', 'file')

# What the approval phases may record about a finding. `needs-audit` completes policy
# classification but remains undecided for publication (docs/implementation.md, "Auto-approval").
# A request to clarify or amend a finding is kept as feedback, not a verdict.
#
# `auto-approved` is a verdict distinct from `approved` so the log records whether policy approved
# an obvious finding or the user approved it in person (SKILL.md, "Phase 6: auto-approval";
# "Phase 7: audit"). The two are stored differently so diagnostics can report the approval source;
# their outcome is identical, as $script:FindingApprovalDecisions records.
$script:FindingDecisionVocabulary = @('approved', 'auto-approved', 'needs-audit', 'rejected', 'dropped')

# The verdicts that carry a finding into the approved set. Auto-approval and user approval differ in
# who reached the verdict and nowhere else, so both are read as approval wherever the approved set is
# built or its size is counted.
$script:FindingApprovalDecisions = @('approved', 'auto-approved')

$script:FindingArtifactKindVocabulary = @('reviewer', 'consolidated')
$script:FindingArtifactStatusVocabulary = @('open', 'complete')

# The fields the approved set carries, as New-ApprovedFindingDocument writes it. It carries no kind,
# which is what lets publication refuse the working artifact handed to it in the approved set's
# place, and what lets a reader of either shape tell the two apart without being told which it holds.
$script:ApprovedFindingDocumentFields = @(
    'schemaVersion'
    'target'
    'reviewedSourceCommit'
    'reviewedMergeBaseCommit'
    'reviewedContextHash'
    'findings'
)

# The diff side an anchor sits on, stated in the vocabulary the capture uses for a thread's context
# side (docs/design.md, "Captured pull request context"), so a finding and a captured thread
# describe the same position the same way.
$script:FindingSideVocabulary = @('right', 'left')

# A finding that names no side is about a line the pull request added, which is where a review
# finding almost always sits; the removed side has to be asked for explicitly.
$script:DefaultFindingSide = 'right'

# Consolidated findings are numbered from a single letter, because this is the identifier a human
# quotes back in discussion and a reviewer-qualified one would be long and meaningless outside the run.
$script:ConsolidatedFindingIdPrefix = 'F'

# The audit's decision log, kept in session state beside the run's other records. Its location is
# fixed here because the command that appends to it and the command that resolves it are separate
# processes that must agree without being told.
$script:FindingDecisionFileName = 'audit-decisions.json'

# Creates an empty artifact.
#
# A reviewer artifact belongs to one reviewer subagent: it names the focus area that
# subagent was dispatched with, so the consolidated result can attribute every finding to the pass
# that produced it. A consolidated artifact belongs to the run and carries no such attribution,
# which is why the reviewer fields are refused on it rather than left empty.
function New-FindingArtifactDocument {
    param(
        [Parameter(Mandatory)]
        [ValidateSet('reviewer', 'consolidated')]
        [string] $Kind,

        [string] $ReviewerId,

        [string] $FocusArea
    )

    if ($Kind -ceq 'consolidated') {
        foreach ($entry in @(
                @{ name = '-ReviewerId'; value = $ReviewerId }
                @{ name = '-FocusArea'; value = $FocusArea }
            )) {
            if (-not [string]::IsNullOrWhiteSpace([string] $entry['value'])) {
                throw "$($entry['name']) describes the reviewer that produced a finding and does not apply to a consolidated artifact."
            }
        }

        return [ordered]@{
            schemaVersion = $script:FindingArtifactSchemaVersion
            kind          = 'consolidated'
            findings      = @()
            removed       = @()
        }
    }

    Assert-FindingReviewerId -ReviewerId $ReviewerId
    if ([string]::IsNullOrWhiteSpace($FocusArea)) {
        throw "The reviewer artifact '$ReviewerId' requires -FocusArea naming the focus area the reviewer applied."
    }

    [ordered]@{
        schemaVersion = $script:FindingArtifactSchemaVersion
        kind          = 'reviewer'
        reviewerId    = $ReviewerId
        focusArea     = $FocusArea
        # A reviewer that found nothing and a reviewer that died partway both leave an artifact with
        # no findings in it, and only the status tells them apart.
        status        = 'open'
        findings      = @()
    }
}

# Reads an artifact written by an earlier command.
#
# The version is enforced rather than merely recorded, for the reason the capture enforces its own:
# a document shaped differently would not fail, it would merge findings whose identifiers and
# anchors mean something else, and the result decides what gets posted to a pull request.
function Read-FindingArtifact {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $document = Read-FindingJsonDocument `
        -Path $Path `
        -Subject 'finding artifact' `
        -MissingRemedy 'Create it with New-FindingArtifact.ps1 before writing findings into it.' `
        -VersionRemedy 'Create the artifact again instead of writing into it.'

    $kind = [string] (Get-JsonProperty -Node $document -Name 'kind')
    if ($script:FindingArtifactKindVocabulary -cnotcontains $kind) {
        throw "The finding artifact at '$Path' reports kind '$kind'; a finding artifact is $(Get-FindingQuotedList -Item $script:FindingArtifactKindVocabulary)."
    }

    $document
}

# Reads either document a run renders findings from: the working artifact or the approved set
# derived from it (SKILL.md, "Phase 7: audit"; "Phase 9: publication").
#
# Which of the two it is, is read out of the document rather than declared by the caller. The same
# command renders both, at two points of the run, and a caller that had to state which shape it was
# passing could state the wrong one and be believed - publishing, in the worst case, findings the
# audit never approved. An artifact names the kind of reviewer work it holds and the approved set
# names none, so the document answers the question by itself.
#
# Only what the renderer reads is enforced here. Publication applies its own, stricter reading of an
# approved set, and that reading is what decides whether the set may be posted onto a pull request.
function Read-RenderableFindingDocument {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $document = Read-FindingJsonDocument `
        -Path $Path `
        -Subject 'finding document' `
        -MissingRemedy 'Rendering reads the artifact New-ConsolidatedFindings.ps1 wrote or the approved set New-ApprovedFindings.ps1 derived from it.' `
        -VersionRemedy 'Produce the document again with the command that writes it.' `
        -AllowApproved

    $kind = [string] (Get-JsonProperty -Node $document -Name 'kind')
    if (-not [string]::IsNullOrEmpty($kind)) {
        if ($script:FindingArtifactKindVocabulary -cnotcontains $kind) {
            throw "The finding document at '$Path' reports kind '$kind'; a finding artifact is $(Get-FindingQuotedList -Item $script:FindingArtifactKindVocabulary)."
        }

        return $document
    }

    # A non-PR review has no target or evidence binding and is rendered like a PR review.
    foreach ($name in $script:ApprovedFindingDocumentFields) {
        if ($document.Contains($name)) { continue }
        throw "The document at '$Path' names no kind, so it is read as an approved set, and it carries no '$name'. Render a finding artifact or the approved set New-ApprovedFindings.ps1 wrote."
    }

    $document
}

function Write-FindingArtifact {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document
    )

    Write-PullRequestOutputFileAtomically -Path $Path -Content (ConvertTo-JsonText -Value $Document)
}

# Appends one finding and reports the identifier it was given.
#
# Identifiers are assigned here rather than by the author, because the same reviewer prompt runs
# against many reviewers at once and any identifier a model chooses would collide across them.
function Add-FindingToArtifact {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        $Finding,

        [Parameter(Mandatory)]
        [ValidateSet('reviewer', 'synthesis')]
        [string] $Origin,

        [AllowEmptyCollection()]
        [string[]] $SourceFindingId = @(),

        [string] $SourceDescription = 'the finding'
    )

    # A completed artifact has already been counted as this reviewer's whole answer. Accepting more
    # findings into it would make the completion mean nothing, and the likeliest cause is a subagent
    # that was re-dispatched onto an artifact it already finished.
    $status = [string] (Get-JsonProperty -Node $Document -Name 'status')
    if ($status -ceq 'complete') {
        throw "This artifact is complete and accepts no further findings. Reopen it with New-FindingArtifact.ps1 -Resume only when the orchestrator resumes this reviewer."
    }

    $id = New-FindingIdentifier -Document $Document
    $record = New-FindingRecord -Finding $Finding -Id $id -Origin $Origin -SourceFindingId $SourceFindingId -SourceDescription $SourceDescription

    $findings = Get-JsonArrayProperty -Node $Document -Name 'findings'
    Set-JsonProperty -Node $Document -Name 'findings' -Value @($findings + @($record))

    $id
}

# Replaces the content of one finding, leaving its identity and provenance as they were.
#
# Quality improvement rewrites findings that have already been consolidated, and the identifier is
# how a rewritten finding is still recognized as the one a reviewer reported. Provenance is preserved
# for the same reason: a rewrite changes what the finding says, not where it came from.
function Set-FindingInArtifact {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [string] $FindingId,

        [Parameter(Mandatory)]
        $Finding,

        [string] $SourceDescription = 'the finding'
    )

    $findings = Get-JsonArrayProperty -Node $Document -Name 'findings'
    $index = Find-FindingIndex -Finding $findings -FindingId $FindingId
    $existing = $findings[$index]

    $record = New-FindingRecord `
        -Finding $Finding `
        -Id ([string] (Get-JsonProperty -Node $existing -Name 'id')) `
        -Origin ([string] (Get-JsonProperty -Node $existing -Name 'origin')) `
        -SourceFindingId ([string[]] (Get-JsonArrayProperty -Node $existing -Name 'sourceFindingIds')) `
        -SchemaVersion ([int] (Get-JsonProperty -Node $existing -Name 'schemaVersion' -Default $script:FindingArtifactSchemaVersion)) `
        -SourceDescription $SourceDescription

    $updated = @($findings)
    $updated[$index] = $record
    Set-JsonProperty -Node $Document -Name 'findings' -Value $updated
}

# Moves one finding out of the artifact and records why.
#
# Discarded findings are kept rather than deleted, because the pass that reports on the review's own
# quality has no other evidence of what quality improvement rejected and would otherwise only ever
# see what survived.
function Remove-FindingFromArtifact {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [string] $FindingId,

        [Parameter(Mandatory)]
        [string] $Reason,

        [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
    )

    if ([string]::IsNullOrWhiteSpace($Reason)) {
        throw "Removing finding '$FindingId' requires a reason, because the removal record is the only account of why the finding was discarded."
    }

    $findings = Get-JsonArrayProperty -Node $Document -Name 'findings'
    $index = Find-FindingIndex -Finding $findings -FindingId $FindingId

    $kept = [System.Collections.Generic.List[object]]::new()
    for ($position = 0; $position -lt $findings.Count; $position++) {
        if ($position -ne $index) { $kept.Add($findings[$position]) }
    }
    Set-JsonProperty -Node $Document -Name 'findings' -Value $kept.ToArray()

    $entry = [ordered]@{
        finding    = $findings[$index]
        reason     = $Reason
        removedUtc = $NowUtc.UtcDateTime.ToString('o', [cultureinfo]::InvariantCulture)
    }
    $removed = Get-JsonArrayProperty -Node $Document -Name 'removed'
    Set-JsonProperty -Node $Document -Name 'removed' -Value @($removed + @($entry))
}

# Records that a reviewer has been reviewed to the end.
#
# Completion is the only thing that distinguishes a reviewer which found nothing from one that never
# finished, since both leave an artifact with an empty finding list.
function Complete-FindingArtifactDocument {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document
    )

    $kind = [string] (Get-JsonProperty -Node $Document -Name 'kind')
    if ($kind -cne 'reviewer') {
        throw "Only a reviewer artifact records completion, because completion is a statement about one reviewer's pass; this artifact is '$kind'."
    }

    Set-JsonProperty -Node $Document -Name 'status' -Value 'complete'
}

# A replacement worker appends to the same artifact, including findings from unfinished groups.
# Ref: docs/implementation.md, "Review work queues".
function Open-FindingArtifactForResume {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $ReviewerId,

        [Parameter(Mandatory)]
        [string] $FocusArea
    )

    $document = Read-FindingArtifact -Path $Path
    $problems = Get-FindingArtifactProblem -Document $document -Path $Path
    if ($problems.Count -gt 0) { throw ($problems -join "`n") }
    if ($document['kind'] -cne 'reviewer' -or $document['reviewerId'] -cne $ReviewerId -or $document['focusArea'] -cne $FocusArea) {
        throw "Artifact '$Path' does not match reviewer '$ReviewerId' and focus area '$FocusArea'."
    }
    $document['status'] = 'open'
    $document
}

# Validates a finding as its author wrote it.
#
# `-SourceDescription` names the source the finding was read from and appears in every rejection,
# because the process that runs this is dispatching many findings from many files and a message
# that does not name one of them cannot be acted on.
function Assert-FindingContent {
    param(
        [Parameter(Mandatory)]
        $Finding,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    Assert-FindingShape -Finding $Finding -SourceDescription $SourceDescription -IncludeToolingField $false
}

# Reduces a path to the form every artifact stores, refusing the forms it will not rewrite.
#
# One spelling is enforced throughout the pipeline because paths are compared as text: consolidation
# groups findings by file, publication matches a finding's path against the path a captured thread is
# anchored to, and anchor resolution matches it against the local Git diff, whose paths use forward
# slashes. Two spellings of the same file would defeat all three.
#
# An absolute path and a backslash-separated one are refused rather than converted. The root an
# absolute path would have to be made relative to is not knowable here, and guessing wrong would
# anchor a comment to a file the pull request does not contain. Rewriting a backslash would also
# rewrite the rare filename that genuinely contains one, and would leave reviewers emitting a form
# nothing downstream reads.
function ConvertTo-CanonicalFindingPath {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Path,

        [Alias('Context')]
        [string] $SourceDescription
    )

    $subject = if ([string]::IsNullOrWhiteSpace($SourceDescription)) { "The path '$Path'" } else { "The path '$Path' from $SourceDescription" }
    $remedy = "Name the file by its path relative to the repository root, with forward slashes and no leading slash, such as 'src/lib.rs'."

    if ([string]::IsNullOrWhiteSpace($Path)) {
        throw "A finding names no file. $remedy"
    }

    # Rootedness is judged on a separator-normalized copy, so that a Windows path copied out of the
    # file inventory is reported as absolute, which is the reason it cannot be used, rather than as
    # merely carrying backslashes.
    $rooted = $Path.Replace('\', '/')
    if ($rooted -match '^[A-Za-z]:' -or $rooted.StartsWith('/')) {
        throw "$subject is absolute. $remedy"
    }

    if ($Path.Contains('\')) {
        throw "$subject separates its directories with backslashes. $remedy"
    }

    $segments = [System.Collections.Generic.List[string]]::new()
    foreach ($segment in $Path.Split('/')) {
        if ($segment -ceq '..') {
            throw "$subject leaves the repository through a '..' segment. $remedy"
        }
        # Empty and self-referential segments carry no meaning and would make two spellings of the
        # same file compare as different files.
        if ($segment -ceq '' -or $segment -ceq '.') { continue }
        $segments.Add($segment)
    }

    if ($segments.Count -eq 0) {
        throw "$subject names no file. $remedy"
    }

    $segments -join '/'
}

# Confirms that a finding's follow-up names a thread the platform will accept a reply into.
#
# The thread and comment identifiers are copied from context query results, and a mistyped one is
# invisible until publication, by which time the whole review has been produced. Checking it against
# the capture at the moment the finding is recorded turns that into an immediate refusal.
#
# `-Context` is the captured discussion document, as read by Read-PullRequestContextDocument.
function Assert-FindingFollowUpTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Context,

        [Parameter(Mandatory)]
        $Finding
    )

    $followUp = Get-JsonProperty -Node $Finding -Name 'followUp'
    if ($null -eq $followUp) { return }

    $threadId = [string] (Get-JsonProperty -Node $followUp -Name 'threadId')
    $commentId = [string] (Get-JsonProperty -Node $followUp -Name 'commentId')

    $thread = $null
    $threads = Get-JsonArrayProperty -Node $Context -Name 'threads'
    foreach ($candidate in $threads) {
        # Ordinal, because a GitHub node identifier is an opaque case-sensitive string and two
        # identifiers differing only in case belong to two different threads.
        if ([string]::Equals([string] (Get-JsonProperty -Node $candidate -Name 'id'), $threadId, [StringComparison]::Ordinal)) {
            $thread = $candidate
            break
        }
    }
    if ($null -eq $thread) {
        throw "The follow-up names thread '$threadId', which the captured discussion does not contain. Retrieve its identifier with Read-PullRequestContext.ps1 -Query threads."
    }

    # A reply is posted into the thread, so the comment a follow-up names is the one that raised the
    # point being answered. Naming a reply instead would claim the finding answers something the
    # thread's author never said.
    $openingCommentId = [string] (Get-JsonProperty -Node $thread -Name 'openingCommentId')
    if (-not [string]::Equals($commentId, $openingCommentId, [StringComparison]::Ordinal)) {
        $reported = if ([string]::IsNullOrEmpty($openingCommentId)) { 'no opening comment' } else { "comment '$openingCommentId'" }
        throw "The follow-up names comment '$commentId' in thread '$threadId', but that thread reports $reported. A follow-up answers the comment that opened the thread."
    }

    $kind = [string] (Get-JsonProperty -Node $thread -Name 'kind')
    $platform = [string] (Get-JsonProperty -Node $Context -Name 'platform')

    if ([string]::Equals($platform, 'gitHub', [StringComparison]::Ordinal)) {
        if ($kind -cne 'text') {
            throw "The follow-up names thread '$threadId', which is a '$kind' thread. GitHub accepts no reply posted into it, so answer it in a review thread on the code instead."
        }
        # On GitHub a text thread is either a review thread anchored to a diff position or a flat
        # comment on the pull request, and only the first accepts replies. The capture tells them
        # apart by the anchor: a review thread carries a context, an issue comment carries none.
        if ($null -eq (Get-JsonProperty -Node $thread -Name 'context')) {
            throw "The follow-up names thread '$threadId', which is a comment on the pull request rather than a review thread on the code. GitHub accepts no reply posted into it, so answer it in a review thread instead."
        }
        return
    }

    if ([string]::Equals($platform, 'azureDevOps', [StringComparison]::Ordinal)) {
        # Azure DevOps accepts a reply into every discussion thread, including one raised against the
        # pull request as a whole, which carries no file anchor. Only the threads the service
        # generates for its own activity are closed to replies.
        if ($kind -cne 'text') {
            throw "The follow-up names thread '$threadId', which is a '$kind' thread recording service activity rather than a discussion. Answer a discussion thread instead."
        }
        return
    }

    throw "The captured discussion reports platform '$platform', and this helper knows where a reply may be posted only on gitHub and azureDevOps."
}

# Reports whether a set of artifacts is fit to consolidate.
#
# `-Path` names either one artifact or the directory holding a run's reviewer artifacts. Naming the
# expected reviewers turns this from a count of files into a statement about coverage: a reviewer whose
# subagent never wrote an artifact and a reviewer whose subagent died before completing it are the two
# failures a "the files are all there" check cannot see.
#
# Problems are reported rather than thrown, because the caller is a gate that has to describe every
# defect in one pass instead of stopping at the first.
function Get-FindingArtifactValidation {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [AllowEmptyCollection()]
        [string[]] $ExpectedReviewerId = @()
    )

    $fullPath = Resolve-PullRequestPath -Path $Path
    $errors = [System.Collections.Generic.List[string]]::new()
    $artifacts = [System.Collections.Generic.List[object]]::new()
    $incompleteReviewers = [System.Collections.Generic.List[string]]::new()
    $unexpectedReviewers = [System.Collections.Generic.List[string]]::new()
    $presentReviewers = [System.Collections.Generic.List[string]]::new()
    $findingCount = 0

    $files = @()
    if (Test-Path -LiteralPath $fullPath -PathType Container) {
        # Ordered by name so two runs over the same directory report their problems in the same
        # order, which is what makes the output diffable.
        #
        # Hidden entries are left out because the ones that turn up in a session-state directory are
        # the filesystem's own bookkeeping rather than anything a run wrote.
        foreach ($entry in (Get-ChildItem -LiteralPath $fullPath | Sort-Object -Property Name)) {
            # A findings directory holds one artifact per reviewer and nothing else, so anything else in
            # it is reported by name. Skipping it quietly would leave a reviewer who wrote its work
            # to the wrong place reading a report that says its reviewer is simply missing.
            if ($entry -is [System.IO.DirectoryInfo]) {
                $errors.Add("The directory '$fullPath' holds the subdirectory '$($entry.Name)'. It holds one JSON artifact per reviewer and nothing else.")
                continue
            }
            # Retained locks and unique staging files belong to the shared artifact writer,
            # not to the reviewer set. A concurrent writer may still be using either file.
            if ($entry.Name -like '*.json.lock' -or $entry.Name -match '^\..+\.json\.[0-9a-f]{32}\.tmp$') {
                continue
            }
            if ($entry.Extension -ine '.json') {
                $errors.Add("The directory '$fullPath' holds '$($entry.Name)', which is not a finding artifact. It holds one JSON artifact per reviewer and nothing else.")
                continue
            }

            $files += $entry
        }
    } elseif (Test-Path -LiteralPath $fullPath -PathType Leaf) {
        $files = @(Get-Item -LiteralPath $fullPath)
    } else {
        $errors.Add("There is nothing to validate at '$Path'.")
    }

    foreach ($file in $files) {
        $artifactPath = $file.FullName
        $document = $null
        try {
            $document = Read-FindingArtifact -Path $artifactPath
        } catch {
            $errors.Add($_.Exception.Message)
            continue
        }

        $kind = [string] (Get-JsonProperty -Node $document -Name 'kind')
        $reviewerId = [string] (Get-JsonProperty -Node $document -Name 'reviewerId')
        $status = [string] (Get-JsonProperty -Node $document -Name 'status')

        foreach ($problem in (Get-FindingArtifactProblem -Document $document -Path $artifactPath)) {
            $errors.Add($problem)
        }

        $findings = Get-JsonArrayProperty -Node $document -Name 'findings'
        $findingCount += $findings.Count

        $artifacts.Add([ordered]@{
                path         = $artifactPath
                kind         = $kind
                reviewerId   = if ($kind -ceq 'reviewer') { $reviewerId } else { $null }
                focusArea    = if ($kind -ceq 'reviewer') { Get-JsonProperty -Node $document -Name 'focusArea' } else { $null }
                status       = if ($kind -ceq 'reviewer') { $status } else { $null }
                findingCount = $findings.Count
            })

        if ($kind -cne 'reviewer') { continue }

        if ($presentReviewers.Contains($reviewerId)) {
            $errors.Add("More than one artifact reports reviewer '$reviewerId'. Each focus-area worker owns one artifact.")
        }
        $presentReviewers.Add($reviewerId)
        if ($status -cne 'complete') { $incompleteReviewers.Add($reviewerId) }
        if ($ExpectedReviewerId.Count -gt 0 -and $ExpectedReviewerId -cnotcontains $reviewerId) {
            $unexpectedReviewers.Add($reviewerId)
        }
    }

    $missingReviewers = [System.Collections.Generic.List[string]]::new()
    foreach ($expected in $ExpectedReviewerId) {
        if ($presentReviewers -cnotcontains $expected) { $missingReviewers.Add($expected) }
    }

    [ordered]@{
        isValid             = ($errors.Count -eq 0 -and $missingReviewers.Count -eq 0 -and $incompleteReviewers.Count -eq 0 -and $unexpectedReviewers.Count -eq 0)
        artifacts           = $artifacts.ToArray()
        findingCount        = $findingCount
        errors              = $errors.ToArray()
        missingReviewers    = $missingReviewers.ToArray()
        incompleteReviewers = $incompleteReviewers.ToArray()
        unexpectedReviewers = $unexpectedReviewers.ToArray()
    }
}

# Composes the path of one reviewer's artifact within a directory of them.
#
# The filename is the reviewer identifier, so the command that writes an artifact and the commands that
# enumerate the directory agree on which reviewer a file belongs to without carrying a manifest.
function Get-FindingArtifactPath {
    param(
        [Parameter(Mandatory)]
        [string] $OutputDirectory,

        [Parameter(Mandatory)]
        [string] $ReviewerId
    )

    Assert-FindingReviewerId -ReviewerId $ReviewerId
    Resolve-PullRequestPath -Path (Join-Path $OutputDirectory "$ReviewerId.json")
}

# Reads a list of identifiers supplied on a command line.
#
# The skill starts every script with `pwsh -File`, and that form hands each argument to the script
# as one literal string: an array parameter given several values keeps only the first, silently.
# Splitting on commas here is what lets a caller name more than one identifier at all, and it costs
# nothing, because the identifiers this accepts are composed by the tooling from reviewer identifiers
# and ordinals, which never contain a comma.
#
# An empty value names nothing, so a caller that substitutes an argument it has no value for reads
# as a caller that named no identifiers.
function ConvertTo-FindingIdentifierList {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [AllowEmptyString()]
        [string[]] $Value
    )

    $identifiers = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in $Value) {
        foreach ($part in $entry.Split(',')) {
            $identifier = $part.Trim()
            if ($identifier) { $identifiers.Add($identifier) }
        }
    }

    , $identifiers.ToArray()
}

# Refuses to overwrite an artifact that already holds a reviewer's work.
#
# A subagent that misreads its instructions and creates its artifact a second time would otherwise
# discard everything it had already recorded, with nothing in the output to show that it had. The
# orchestrator resuming a reviewer preserves it with -Resume; -Force is for an explicitly new review.
function Assert-FindingArtifactWritable {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [switch] $Force
    )

    if ($Force) { return }

    $fullPath = Resolve-PullRequestPath -Path $Path
    if (Test-Path -LiteralPath $fullPath -PathType Leaf) {
        throw "There is already a finding artifact at '$fullPath'. Use -Resume with its -FileGroupQueuePath to keep its findings. -Force discards findings and is only for a deliberately new review, never interruption recovery."
    }
}

# Parses one JSON file with a path-bearing error.
#
# The parser reports a byte position in an unnamed document, so callers that validate
# reviewer-authored files need the path to know which document to fix.
function Read-FindingJsonValue {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Subject
    )

    $fullPath = Resolve-PullRequestPath -Path $Path
    $value = $null
    try {
        $value = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $fullPath)
    } catch {
        throw "The $Subject at '$Path' is not valid JSON: $($_.Exception.Message)"
    }

    if ($value -is [object[]]) { return , $value }
    $value
}

# Reads one finding document as its author wrote it.
function Read-FindingContent {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $fullPath = Resolve-PullRequestPath -Path $Path
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        throw "There is no finding at '$Path'."
    }

    $finding = Read-FindingJsonValue -Path $Path -Subject 'finding'
    if ($finding -isnot [System.Collections.IDictionary]) {
        throw "The finding at '$Path' is not a JSON object."
    }

    $finding
}

# Resolves the two ways an update may be requested into one list of loaded findings.
#
# The batch form exists because quality improvement rewrites many findings at once and would
# otherwise start one process per finding.
function Get-FindingUpdateRequest {
    param(
        [string] $FindingId,

        [string] $FindingPath,

        [string] $PlanPath
    )

    if ($PlanPath) {
        if ($FindingId -or $FindingPath) {
            throw 'Supply either -PlanPath or -FindingId with -FindingPath, not both.'
        }

        $entries = Get-FindingPlanEntry -PlanPath $PlanPath -ListName 'updates'
        $requests = [System.Collections.Generic.List[object]]::new()
        foreach ($entry in $entries) {
            $entryFindingId = [string] (Get-JsonProperty -Node $entry -Name 'findingId')
            $entryFindingPath = [string] (Get-JsonProperty -Node $entry -Name 'findingPath')
            if (-not $entryFindingId -or -not $entryFindingPath) {
                throw "Every entry in the update plan '$PlanPath' names a findingId and a findingPath."
            }

            $resolved = Resolve-FindingPlanRelativePath -PlanPath $PlanPath -Path $entryFindingPath
            $requests.Add([ordered]@{
                    findingId   = $entryFindingId
                    findingPath = $resolved
                    finding     = Read-FindingContent -Path $resolved
                })
        }
        return , $requests.ToArray()
    }

    if (-not $FindingId -or -not $FindingPath) {
        throw 'Supply -FindingId with -FindingPath, or -PlanPath naming a file that lists several updates.'
    }

    , @([ordered]@{
            findingId   = $FindingId
            findingPath = $FindingPath
            finding     = Read-FindingContent -Path $FindingPath
        })
}

# Applies a batch of updates and reports the identifiers it touched.
#
# Every request is checked before any of them is applied, so a batch naming one finding the artifact
# does not hold leaves the artifact as it was rather than half rewritten.
function Set-FindingBatchInArtifact {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Update
    )

    $findings = Get-JsonArrayProperty -Node $Document -Name 'findings'
    Assert-FindingRequestDistinct -Request $Update -ListName 'update'
    foreach ($request in $Update) {
        Find-FindingIndex -Finding $findings -FindingId ([string] $request['findingId']) | Out-Null
        Assert-FindingContent -Finding $request['finding'] -SourceDescription ([string] $request['findingPath'])
    }

    $applied = [System.Collections.Generic.List[string]]::new()
    foreach ($request in $Update) {
        Set-FindingInArtifact `
            -Document $Document `
            -FindingId ([string] $request['findingId']) `
            -Finding $request['finding'] `
            -SourceDescription ([string] $request['findingPath'])
        $applied.Add([string] $request['findingId'])
    }

    , $applied.ToArray()
}

# Resolves the two ways a removal may be requested into one list.
function Get-FindingRemovalRequest {
    param(
        [string] $FindingId,

        [string] $Reason,

        [string] $PlanPath
    )

    if ($PlanPath) {
        if ($FindingId -or $Reason) {
            throw 'Supply either -PlanPath or -FindingId with -Reason, not both.'
        }

        $entries = Get-FindingPlanEntry -PlanPath $PlanPath -ListName 'removals'
        $requests = [System.Collections.Generic.List[object]]::new()
        foreach ($entry in $entries) {
            $entryFindingId = [string] (Get-JsonProperty -Node $entry -Name 'findingId')
            $entryReason = [string] (Get-JsonProperty -Node $entry -Name 'reason')
            if (-not $entryFindingId -or -not $entryReason) {
                throw "Every entry in the removal plan '$PlanPath' names a findingId and a reason."
            }
            $requests.Add([ordered]@{ findingId = $entryFindingId; reason = $entryReason })
        }
        return , $requests.ToArray()
    }

    if (-not $FindingId -or -not $Reason) {
        throw 'Supply -FindingId with -Reason, or -PlanPath naming a file that lists several removals.'
    }

    , @([ordered]@{ findingId = $FindingId; reason = $Reason })
}

# Applies a batch of removals and reports the identifiers it moved.
#
# Every request is checked before any of them is applied, for the reason the update batch is.
function Remove-FindingBatchFromArtifact {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Removal,

        [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
    )

    $findings = Get-JsonArrayProperty -Node $Document -Name 'findings'
    Assert-FindingRequestDistinct -Request $Removal -ListName 'removal'
    foreach ($request in $Removal) {
        Find-FindingIndex -Finding $findings -FindingId ([string] $request['findingId']) | Out-Null
    }

    $removed = [System.Collections.Generic.List[string]]::new()
    foreach ($request in $Removal) {
        Remove-FindingFromArtifact `
            -Document $Document `
            -FindingId ([string] $request['findingId']) `
            -Reason ([string] $request['reason']) `
            -NowUtc $NowUtc
        $removed.Add([string] $request['findingId'])
    }

    , $removed.ToArray()
}

# Reads the reviewer artifacts of one run, in a fixed order.
#
# Ordered by name so that two runs over the same directory consolidate in the same order, which is
# what makes the consolidated artifact and the document rendered from it comparable between runs.
#
# An artifact that is not a reviewer's is refused rather than skipped. The likeliest way one appears
# here is a run that wrote its consolidated result back into the directory the reviewers write into,
# and consolidating that would count every finding twice while looking like it had worked.
function Read-FindingReviewerArtifactSet {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $fullPath = Resolve-PullRequestPath -Path $Path
    if (-not (Test-Path -LiteralPath $fullPath -PathType Container)) {
        throw "There is no reviewer artifact directory at '$Path'."
    }

    $files = @(Get-ChildItem -LiteralPath $fullPath -Filter '*.json' -File | Sort-Object -Property Name)
    $artifacts = [System.Collections.Generic.List[object]]::new()
    foreach ($file in $files) {
        $document = Read-FindingArtifact -Path $file.FullName
        $kind = [string] (Get-JsonProperty -Node $document -Name 'kind')
        if ($kind -cne 'reviewer') {
            throw "The artifact at '$($file.FullName)' is a '$kind' artifact rather than a reviewer's. Keep a run's own output out of the directory its reviewers write into."
        }
        $artifacts.Add([ordered]@{ path = $file.FullName; document = $document })
    }

    if ($artifacts.Count -eq 0) {
        throw "There are no reviewer artifacts in '$Path'. Every dispatched reviewer writes one, so an empty directory means nothing was reviewed."
    }

    , $artifacts.ToArray()
}

# Lists every recorded finding across a run's reviewer artifacts, without their full bodies.
#
# Deduplication has to consider every finding against every other, and full bodies do not fit. The
# index keeps the problem statement and recommended action needed to detect subsumption, but omits
# detailed reasoning, consequences and references. The adjudicator then reads only the candidates
# in full.
function New-FindingIndexDocument {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $ReviewerArtifact
    )

    $records = [System.Collections.Generic.List[object]]::new()
    foreach ($source in (Get-FindingSourceList -ReviewerArtifact $ReviewerArtifact)) {
        $finding = Get-JsonProperty -Node $source -Name 'finding'

        $paths = [System.Collections.Generic.List[string]]::new()
        $locations = Get-JsonArrayProperty -Node $finding -Name 'impactedLocations'
        foreach ($location in $locations) {
            $path = [string] (Get-JsonProperty -Node $location -Name 'path')
            if (-not $paths.Contains($path)) { $paths.Add($path) }
        }

        $record = [ordered]@{
            id            = Get-JsonProperty -Node $source -Name 'id'
            reviewerId    = Get-JsonProperty -Node $source -Name 'reviewerId'
            focusArea     = Get-JsonProperty -Node $source -Name 'focusArea'
            title         = [string] (Get-JsonProperty -Node $finding -Name 'title')
            description   = [string] (Get-JsonProperty -Node $finding -Name 'description')
            recommendedAction = [string] (Get-JsonProperty -Node $finding -Name 'recommendedAction')
            anchorPath    = [string] (Get-JsonProperty -Node (Get-JsonProperty -Node $finding -Name 'anchor') -Name 'path')
            impactedPaths = $paths.ToArray()
        }

        # Carried only where there is one, because a follow-up is rare and the index is read whole
        # by a process that pays for every token of it.
        $followUp = Get-JsonProperty -Node $finding -Name 'followUp'
        if ($null -ne $followUp) {
            $record['followUpThreadId'] = [string] (Get-JsonProperty -Node $followUp -Name 'threadId')
        }

        $records.Add($record)
    }

    $document = [ordered]@{
        schemaVersion = $script:FindingArtifactSchemaVersion
        findings      = $records.ToArray()
    }
    Assert-WorkflowData -Kind findingIndex -Value $document
    $document
}

# Reads the plan that says which findings merge.
#
# The plan holds only the groups that merge, and everything it does not name is carried over. An
# omitted plan or an explicitly empty groups array means nothing merged. A named but missing or
# malformed document is an error, not evidence that adjudication completed.
function Read-FindingMergePlan {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Path
    )

    if ([string]::IsNullOrWhiteSpace($Path)) { return , @() }

    $fullPath = Resolve-PullRequestPath -Path $Path
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) { throw "There is no merge plan at '$Path'." }

    $plan = Read-WorkflowData -Kind mergePlan -Path $Path
    if ($plan -isnot [System.Collections.IDictionary]) {
        throw "The merge plan at '$Path' is not a JSON object carrying 'groups'."
    }

    $groups = $plan['groups']
    $records = [System.Collections.Generic.List[object]]::new()
    for ($index = 0; $index -lt $groups.Count; $index++) {
        $identifiers = Get-JsonArrayProperty -Node $groups[$index] -Name 'sourceFindingIds'
        $sourceFindingIds = @($identifiers | ForEach-Object { [string] $_ })

        # A group of one would replace a reviewer's finding with text the adjudicator wrote while
        # claiming to have merged something, which is the one way this phase could lose a finding
        # without any check noticing.
        if ($sourceFindingIds.Count -lt 2) {
            throw "The merge plan at '$Path' has a group at position $index naming fewer than two findings. The plan holds only the groups that merge; every other finding is carried over as it stands."
        }

        $findingPath = [string] (Get-JsonProperty -Node $groups[$index] -Name 'findingPath')
        if ([string]::IsNullOrWhiteSpace($findingPath)) {
            throw "The merge plan at '$Path' has a group at position $index naming no 'findingPath'. A merged finding is written as a file, because it carries Markdown prose."
        }

        $records.Add([ordered]@{
                sourceFindingIds = $sourceFindingIds
                findingPath      = Resolve-FindingPlanRelativePath -PlanPath $Path -Path $findingPath
            })
    }

    , $records.ToArray()
}

# Builds the consolidated artifact from a run's reviewer artifacts and the groups that merge.
#
# Carrying over everything the plan does not name is what makes this phase safe: a duplicate the
# adjudicator missed reaches the author as a second comment saying much the same thing, while the
# inverse arrangement - keeping only what a plan lists - would discard a finding for every line the
# adjudicator forgot to write, and nothing downstream could tell that it had.
function New-ConsolidatedFindingDocument {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $ReviewerArtifact,

        [AllowEmptyCollection()]
        [object[]] $MergeGroup = @()
    )

    $sources = Get-FindingSourceList -ReviewerArtifact $ReviewerArtifact

    $positionById = [System.Collections.Generic.Dictionary[string, int]]::new([System.StringComparer]::Ordinal)
    for ($position = 0; $position -lt $sources.Count; $position++) {
        $positionById[[string] (Get-JsonProperty -Node $sources[$position] -Name 'id')] = $position
    }

    # A merged finding takes the place of the earliest of the findings it replaces, so the
    # consolidated artifact reads in the order the reviewers were reviewed in and does not depend on the
    # order the adjudicator happened to write its groups in.
    $claimedBy = [System.Collections.Generic.Dictionary[string, int]]::new([System.StringComparer]::Ordinal)
    $groupByPosition = [System.Collections.Generic.Dictionary[int, object]]::new()
    for ($index = 0; $index -lt $MergeGroup.Count; $index++) {
        $group = $MergeGroup[$index]
        $identifiers = Get-JsonArrayProperty -Node $group -Name 'sourceFindingIds'
        $anchorPosition = [int]::MaxValue
        foreach ($identifier in $identifiers) {
            $findingId = [string] $identifier
            if (-not $positionById.ContainsKey($findingId)) {
                throw "The merge plan names finding '$findingId', which no reviewer artifact holds."
            }
            if ($claimedBy.ContainsKey($findingId)) {
                throw "The merge plan names finding '$findingId' in more than one group. A finding merges into one place."
            }
            $claimedBy[$findingId] = $index
            if ($positionById[$findingId] -lt $anchorPosition) { $anchorPosition = $positionById[$findingId] }
        }
        $groupByPosition[$anchorPosition] = $group
    }

    $document = New-FindingArtifactDocument -Kind 'consolidated'

    for ($position = 0; $position -lt $sources.Count; $position++) {
        $source = $sources[$position]
        $findingId = [string] (Get-JsonProperty -Node $source -Name 'id')

        if ($groupByPosition.ContainsKey($position)) {
            Add-FindingMergeToArtifact `
                -Document $document `
                -Group $groupByPosition[$position] `
                -Source $sources `
                -PositionById $positionById | Out-Null
            continue
        }

        if ($claimedBy.ContainsKey($findingId)) { continue }

        $record = Get-JsonProperty -Node $source -Name 'finding'
        Add-FindingToArtifact `
            -Document $document `
            -Finding (ConvertTo-FindingContent -Record $record) `
            -Origin ([string] (Get-JsonProperty -Node $record -Name 'origin')) `
            -SourceFindingId @($findingId) `
            -SourceDescription "finding '$findingId'" | Out-Null
    }

    $document
}

# Reports what consolidation did, so the caller can report on it.
function Get-FindingConsolidationSummary {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document
    )

    $merged = 0
    $findings = Get-JsonArrayProperty -Node $Document -Name 'findings'
    foreach ($finding in $findings) {
        if ((Get-JsonArrayProperty -Node $finding -Name 'sourceFindingIds').Count -gt 1) { $merged++ }
    }

    [ordered]@{
        findingCount     = $findings.Count
        mergedCount      = $merged
        carriedOverCount = $findings.Count - $merged
    }
}

# Selects the findings a caller named, or all of them where it named none.
#
# An identifier the artifact does not hold is refused, because the caller that names one is asking
# for a finding it believes exists, and rendering the rest would look like it had succeeded.
function Select-FindingById {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding,

        [AllowEmptyCollection()]
        [string[]] $FindingId = @()
    )

    if ($FindingId.Count -eq 0) { return , $Finding }

    $requested = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($identifier in $FindingId) {
        Find-FindingIndex -Finding $Finding -FindingId $identifier | Out-Null
        $requested.Add($identifier) | Out-Null
    }

    # Artifact order is kept rather than the order the caller named, so the same set of findings is
    # always presented the same way.
    $selected = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in $Finding) {
        if ($requested.Contains([string] (Get-JsonProperty -Node $entry -Name 'id'))) { $selected.Add($entry) }
    }

    , $selected.ToArray()
}

# Creates an empty decision log.
#
# The log is a list of events rather than a verdict per finding, because the audit revisits findings:
# a finding approved in the audit can be dropped by the freshness check, and rewriting the earlier
# entry would erase the fact that it was ever approved, which is what phase 10 reads to learn what the
# user actually wanted.
function New-FindingDecisionDocument {
    [ordered]@{
        schemaVersion = $script:FindingArtifactSchemaVersion
        decisions     = @()
    }
}

# Reads a decision log written by an earlier command.
function Read-FindingDecisionDocument {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $fullPath = Resolve-PullRequestPath -Path $Path
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        throw "There is no decision log at '$Path'. The audit records a decision for every finding before an approved set can be built."
    }

    $document = Read-FindingJsonValue -Path $Path -Subject 'decision log'
    if ($document -isnot [System.Collections.IDictionary]) {
        throw "The decision log at '$Path' is not a JSON object."
    }

    $version = Get-JsonProperty -Node $document -Name 'schemaVersion'
    if ($version -ne $script:FindingArtifactSchemaVersion) {
        $reported = if ($null -eq $version) { 'no version' } else { "version $version" }
        throw "The decision log at '$Path' reports $reported, but this helper reads version $($script:FindingArtifactSchemaVersion)."
    }

    $document
}

# Reads a decision log, or begins one where the audit has not recorded anything yet.
function Open-FindingDecisionDocument {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    if (Test-Path -LiteralPath (Resolve-PullRequestPath -Path $Path) -PathType Leaf) {
        return Read-FindingDecisionDocument -Path $Path
    }

    New-FindingDecisionDocument
}

function Write-FindingDecisionDocument {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document
    )

    Write-PullRequestOutputFileAtomically -Path $Path -Content (ConvertTo-JsonText -Value $Document)
}

# Composes the path of a run's decision log.
function Get-FindingDecisionPath {
    param(
        [Parameter(Mandatory)]
        [string] $OutputDirectory
    )

    Resolve-PullRequestPath -Path (Join-Path $OutputDirectory $script:FindingDecisionFileName)
}

# Appends one decision.
#
# The free-form text the user supplied is carried as `feedback` rather than folded into `reason`,
# because the two are read by different consumers: the reason states why the run did what it did, and
# the feedback is the user's own words, which phase 10 mines for the instruction that was missing.
function Add-FindingDecisionRecord {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [string] $FindingId,

        [Parameter(Mandatory)]
        [string] $Decision,

        [string] $Reason,

        [string] $Feedback,

        [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
    )

    $record = [ordered]@{
        findingId = $FindingId
        decision  = ConvertTo-FindingDecisionValue -Decision $Decision -FindingId $FindingId
    }
    if (-not [string]::IsNullOrWhiteSpace($Reason)) { $record['reason'] = $Reason }
    if (-not [string]::IsNullOrWhiteSpace($Feedback)) { $record['feedback'] = $Feedback }
    $record['recordedUtc'] = $NowUtc.UtcDateTime.ToString('o', [cultureinfo]::InvariantCulture)

    $decisions = Get-JsonArrayProperty -Node $Document -Name 'decisions'
    Set-JsonProperty -Node $Document -Name 'decisions' -Value @($decisions + @($record))

    $record
}

# Reports the decision that stands for one finding, or nothing where none was recorded.
#
# The last entry wins. That is what lets a later phase overturn an earlier verdict without rewriting
# history, and it is what makes an interrupted audit resumable: the log is replayed rather than
# reconciled.
function Resolve-FindingDecision {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [string] $FindingId
    )

    $resolved = $null
    $decisions = Get-JsonArrayProperty -Node $Document -Name 'decisions'
    foreach ($record in $decisions) {
        if ([string]::Equals([string] (Get-JsonProperty -Node $record -Name 'findingId'), $FindingId, [StringComparison]::Ordinal)) {
            $resolved = $record
        }
    }

    if ($null -eq $resolved) { return $null }

    # A verdict the vocabulary does not know is refused rather than read as "not approved", because
    # that reading would drop the finding from the approved set while reporting nothing.
    $value = [string] (Get-JsonProperty -Node $resolved -Name 'decision')
    if ($script:FindingDecisionVocabulary -cnotcontains $value) {
        throw "The decision log records '$value' for finding '$FindingId'; an audit decides $(Get-FindingQuotedList -Item $script:FindingDecisionVocabulary)."
    }

    $resolved
}

# Applies a batch of decisions and reports the identifiers it recorded.
#
# Every request is checked before any of them is applied, for the reason the update batch is.
function Add-FindingDecisionBatch {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Request,

        [switch] $PolicyClassification,

        [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
    )

    Assert-FindingRequestDistinct -Request $Request -ListName 'decision'
    foreach ($entry in $Request) {
        $value = ConvertTo-FindingDecisionValue -Decision ([string] $entry['decision']) -FindingId ([string] $entry['findingId'])
        if ($PolicyClassification) {
            if ($value -cnotin @('auto-approved', 'needs-audit')) {
                throw "Policy classification cannot record '$value' for finding '$($entry['findingId'])'. Use auto-approved or needs-audit."
            }
            if (-not [string]::IsNullOrWhiteSpace([string] $entry['feedback'])) {
                throw 'Policy classification cannot record user feedback.'
            }
        }
    }

    $recorded = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in $Request) {
        Add-FindingDecisionRecord `
            -Document $Document `
            -FindingId ([string] $entry['findingId']) `
            -Decision ([string] $entry['decision']) `
            -Reason ([string] $entry['reason']) `
            -Feedback ([string] $entry['feedback']) `
            -NowUtc $NowUtc | Out-Null
        $recorded.Add([string] $entry['findingId'])
    }

    , $recorded.ToArray()
}

# Resolves the two ways a decision may be requested into one list.
function Get-FindingDecisionRequest {
    param(
        [string] $FindingId,

        [string] $Decision,

        [string] $Reason,

        [string] $Feedback,

        [string] $PlanPath
    )

    if ($PlanPath) {
        if ($FindingId -or $Decision -or $Reason -or $Feedback) {
            throw 'Supply either -PlanPath or -FindingId with -Decision, not both.'
        }

        $entries = Get-FindingPlanEntry -PlanPath $PlanPath -ListName 'decisions'
        $requests = [System.Collections.Generic.List[object]]::new()
        foreach ($entry in $entries) {
            $entryFindingId = [string] (Get-JsonProperty -Node $entry -Name 'findingId')
            $entryDecision = [string] (Get-JsonProperty -Node $entry -Name 'decision')
            if (-not $entryFindingId -or -not $entryDecision) {
                throw "Every entry in the decision plan '$PlanPath' names a findingId and a decision."
            }
            $requests.Add([ordered]@{
                    findingId = $entryFindingId
                    decision  = $entryDecision
                    reason    = [string] (Get-JsonProperty -Node $entry -Name 'reason')
                    feedback  = [string] (Get-JsonProperty -Node $entry -Name 'feedback')
                })
        }
        return , $requests.ToArray()
    }

    if (-not $FindingId -or -not $Decision) {
        throw 'Supply -FindingId with -Decision, or -PlanPath naming a file that lists several decisions.'
    }

    , @([ordered]@{ findingId = $FindingId; decision = $Decision; reason = $Reason; feedback = $Feedback })
}

# Polls one unclassified finding without consuming it. The append-only decision log is also the
# completion journal, so interruption before a decision leaves the same item available on resume.
# Ref: docs/implementation.md, "Auto-approval".
function Get-FindingAutoApprovalWork {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Artifact,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Decision
    )

    if ((Get-JsonProperty -Node $Artifact -Name 'kind') -cne 'consolidated') {
        throw 'Auto-approval requires a consolidated finding artifact.'
    }
    if ((Get-JsonProperty -Node $Artifact -Name 'findings') -isnot [System.Collections.IList]) {
        throw 'The auto-approval input must contain a findings array.'
    }
    if ((Get-JsonProperty -Node $Decision -Name 'decisions') -isnot [System.Collections.IList]) {
        throw 'The decision log must contain a decisions array.'
    }
    $problems = Get-FindingArtifactProblem -Document $Artifact -Path 'auto-approval input'
    if ($problems.Count -gt 0) { throw ($problems -join "`n") }

    $nextFinding = $null
    $remainingCount = 0
    $auditFindingIds = [System.Collections.Generic.List[string]]::new()
    foreach ($finding in (Get-JsonArrayProperty -Node $Artifact -Name 'findings')) {
        $findingId = [string] (Get-JsonProperty -Node $finding -Name 'id')
        $record = Resolve-FindingDecision -Document $Decision -FindingId $findingId
        if ($null -eq $record) {
            if ($null -eq $nextFinding) { $nextFinding = $finding }
            $remainingCount++
            $auditFindingIds.Add($findingId)
        } elseif ((Get-JsonProperty -Node $record -Name 'decision') -ceq 'needs-audit') {
            $auditFindingIds.Add($findingId)
        }
    }

    [ordered]@{
        isComplete      = ($remainingCount -eq 0)
        remainingCount  = $remainingCount
        finding         = $nextFinding
        auditFindingIds = $auditFindingIds.ToArray()
    }
}

# Builds the set of findings the audit approved.
#
# A finding with no decision is a refusal rather than an omission. An audit that was interrupted
# would otherwise produce a set that looks complete and quietly leaves out everything the user never
# saw, and nothing after this point can tell that from a review where the user rejected those
# findings.
#
# `-Context` is a captured pull request. Supplying it binds the review to the commits it was
# performed against and revalidates every follow-up target, which is how a thread deleted or resolved
# while the review ran is discovered at the last moment where the run can still do something about
# it.
function New-ApprovedFindingDocument {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Artifact,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Decision,

        [AllowNull()]
        [System.Collections.IDictionary] $Context
    )

    $approved = [System.Collections.Generic.List[object]]::new()
    $undecided = [System.Collections.Generic.List[string]]::new()

    $findings = Get-JsonArrayProperty -Node $Artifact -Name 'findings'
    foreach ($finding in $findings) {
        $findingId = [string] (Get-JsonProperty -Node $finding -Name 'id')
        $record = Resolve-FindingDecision -Document $Decision -FindingId $findingId
        if ($null -eq $record -or (Get-JsonProperty -Node $record -Name 'decision') -ceq 'needs-audit') {
            $undecided.Add($findingId)
            continue
        }
        if ($script:FindingApprovalDecisions -cnotcontains [string] (Get-JsonProperty -Node $record -Name 'decision')) { continue }

        Assert-FindingShape `
            -Finding $finding `
            -SourceDescription "the consolidated artifact (finding '$findingId')" `
            -IncludeToolingField $true

        if ($null -ne $Context) {
            try {
                Assert-FindingFollowUpTarget -Context $Context -Finding $finding
            } catch {
                throw "Finding '$findingId' cannot be published: $($_.Exception.Message)"
            }
        }

        $approved.Add($finding)
    }

    if ($undecided.Count -gt 0) {
        throw "The audit recorded no decision for $(Get-FindingQuotedList -Item $undecided.ToArray()). Every finding is decided before an approved set is built."
    }

    $pullRequest = if ($null -eq $Context) { $null } else { Get-JsonProperty -Node $Context -Name 'pullRequest' }
    $target = if ($null -eq $Context) { $null } else { Get-JsonProperty -Node $Context -Name 'target' }
    $sourceCommit = if ($null -eq $pullRequest) { $null } else { Get-JsonProperty -Node $pullRequest -Name 'reviewedSourceCommit' }
    $mergeBaseCommit = if ($null -eq $pullRequest) { $null } else { Get-JsonProperty -Node $pullRequest -Name 'reviewedMergeBaseCommit' }

    [ordered]@{
        schemaVersion           = $script:ApprovedFindingSchemaVersion
        target                  = $target
        reviewedSourceCommit    = $sourceCommit
        reviewedMergeBaseCommit = $mergeBaseCommit
        reviewedContextHash     = if ($null -eq $Context) { $null } else { Get-PullRequestEvidenceHash -Document $Context }
        findings                = $approved.ToArray()
    }
}

# Counts what the audit decided, so the caller can report on it.
#
# Both approval verdicts contribute a finding to the approved identifiers, because a finding the
# audit auto-approved on the user's behalf is as much a member of the approved set as one the user
# approved in person. How that approval divides between the two verdicts is reported over the written
# set by Get-ApprovedFindingApprovalSplit rather than here, so the division reconciles with the
# published count even after the freshness check removes an unanchored finding.
function Get-FindingDecisionSummary {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Artifact,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Decision
    )

    $counts = [ordered]@{}
    foreach ($name in $script:FindingDecisionVocabulary) { $counts[$name] = 0 }
    $undecided = [System.Collections.Generic.List[string]]::new()
    $approvedIds = [System.Collections.Generic.List[string]]::new()

    $findings = Get-JsonArrayProperty -Node $Artifact -Name 'findings'
    foreach ($finding in $findings) {
        $findingId = [string] (Get-JsonProperty -Node $finding -Name 'id')
        $record = Resolve-FindingDecision -Document $Decision -FindingId $findingId
        if ($null -eq $record -or (Get-JsonProperty -Node $record -Name 'decision') -ceq 'needs-audit') {
            $undecided.Add($findingId)
            continue
        }

        $value = [string] (Get-JsonProperty -Node $record -Name 'decision')
        $counts[$value] = [int] $counts[$value] + 1
        if ($script:FindingApprovalDecisions -ccontains $value) { $approvedIds.Add($findingId) }
    }

    [ordered]@{
        approved           = $counts['approved']
        rejected           = $counts['rejected']
        dropped            = $counts['dropped']
        undecided          = $undecided.ToArray()
        approvedFindingIds = $approvedIds.ToArray()
    }
}

# Divides an approved set into what the user approved in person and what the audit auto-approved.
#
# The division is taken over the findings the approved set actually holds rather than over every
# approval the audit recorded, so a finding approved either way but then removed by the freshness
# check as unanchored is absent from it (SKILL.md, "Phase 7: audit"). This is what lets the diagnostic
# summary report a manually-approved and an auto-approved count that add up to the published count.
# The rejected and dropped counts cannot join that sum, because they name findings the approved set
# never held.
function Get-ApprovedFindingApprovalSplit {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Decision
    )

    $manuallyApproved = 0
    $autoApproved = 0

    $findings = Get-JsonArrayProperty -Node $Document -Name 'findings'
    foreach ($finding in $findings) {
        $record = Resolve-FindingDecision -Document $Decision -FindingId ([string] (Get-JsonProperty -Node $finding -Name 'id'))
        if ($null -eq $record) { continue }

        switch -CaseSensitive ([string] (Get-JsonProperty -Node $record -Name 'decision')) {
            'auto-approved' { $autoApproved++ }
            'approved' { $manuallyApproved++ }
        }
    }

    [ordered]@{
        manuallyApproved = $manuallyApproved
        autoApproved     = $autoApproved
    }
}

# Refuses a freshness check that could only half run (SKILL.md, "Phase 8: freshness check").
#
# The two inputs answer one question between them and neither answers it alone. The refreshed
# capture states which commits the review is bound to; the local clone is where the diff between
# those commits is read from. Given only the capture, a finding anchored to a file the pull request
# no longer changes is carried into publication and fails it there, after the user has approved
# every finding. Given only the clone, there are no commits to compare.
#
# The pair is therefore required rather than encouraged: the phase that supplies one always has the
# other, and a gate that may be omitted is a gate that will be.
function Assert-FindingFreshnessInput {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $ContextPath,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $RepositoryPath
    )

    if (-not [string]::IsNullOrWhiteSpace($ContextPath) -and [string]::IsNullOrWhiteSpace($RepositoryPath)) {
        throw 'Rebuilding the approved set against a refreshed capture also requires -RepositoryPath, naming a local clone that holds the reviewed commits, so that every approved anchor is checked against the reviewed diff.'
    }

    if (-not [string]::IsNullOrWhiteSpace($RepositoryPath) -and [string]::IsNullOrWhiteSpace($ContextPath)) {
        throw 'Checking approved anchors against the reviewed diff also requires -ContextPath, naming the refreshed capture, which is where the reviewed commits are taken from.'
    }
}

# Builds the record an artifact stores.
#
# The stored key order is fixed rather than inherited from the author's document, so that two
# findings written by two subagents differ in their content and nowhere else, and a rewritten
# finding produces a diff covering only what changed.
function New-FindingRecord {
    param(
        [Parameter(Mandatory)]
        $Finding,

        [Parameter(Mandatory)]
        [string] $Id,

        [Parameter(Mandatory)]
        [string] $Origin,

        [AllowEmptyCollection()]
        [string[]] $SourceFindingId = @(),

        [int] $SchemaVersion = $script:FindingArtifactSchemaVersion,

        [string] $SourceDescription = 'the finding'
    )

    Assert-FindingContent -Finding $Finding -SourceDescription $SourceDescription

    if ($script:FindingOriginVocabulary -cnotcontains $Origin) {
        throw "The finding from $SourceDescription reports origin '$Origin'; a finding originates as $(Get-FindingQuotedList -Item $script:FindingOriginVocabulary)."
    }

    $record = [ordered]@{
        schemaVersion     = $SchemaVersion
        id                = $Id
        sourceFindingIds  = @($SourceFindingId)
        origin            = $Origin
    }

    foreach ($name in $script:FindingProseFields) {
        $record[$name] = [string] (Get-JsonProperty -Node $Finding -Name $name)
    }

    # Assigned through variables rather than inline, because a command that emits a single array
    # emits it as one object and an inline @( ) around the call would store an array holding that
    # array.
    $references = ConvertTo-FindingReferenceList -Finding $Finding
    $locations = ConvertTo-FindingLocationList -Finding $Finding -SourceDescription $SourceDescription
    $record['references'] = $references
    $record['impactedLocations'] = $locations
    $record['anchor'] = ConvertTo-FindingAnchor -Anchor (Get-JsonProperty -Node $Finding -Name 'anchor') -SourceDescription $SourceDescription

    $followUp = Get-JsonProperty -Node $Finding -Name 'followUp'
    if ($null -ne $followUp) {
        $record['followUp'] = [ordered]@{
            threadId  = [string] (Get-JsonProperty -Node $followUp -Name 'threadId')
            commentId = [string] (Get-JsonProperty -Node $followUp -Name 'commentId')
        }
    }

    $record
}

# Assigns the next identifier under the scheme the artifact's kind uses.
#
# The ordinal continues past discarded findings rather than reusing their numbers, because a
# discarded finding is still quoted in the removal record and in whatever discussion preceded the
# removal, and reusing its identifier would attach that history to a different finding.
function New-FindingIdentifier {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document
    )

    $kind = [string] (Get-JsonProperty -Node $Document -Name 'kind')
    $prefix = switch ($kind) {
        'reviewer' {
            $reviewerId = [string] (Get-JsonProperty -Node $Document -Name 'reviewerId')
            Assert-FindingReviewerId -ReviewerId $reviewerId
            "$reviewerId-"
        }
        'consolidated' { $script:ConsolidatedFindingIdPrefix }
        default { throw "A finding cannot be numbered in an artifact of kind '$kind'." }
    }

    $highest = 0
    foreach ($id in (Get-FindingIdentifierList -Document $Document)) {
        if (-not $id.StartsWith($prefix, [StringComparison]::Ordinal)) { continue }
        $ordinal = 0
        if ([int]::TryParse($id.Substring($prefix.Length), [ref] $ordinal) -and $ordinal -gt $highest) {
            $highest = $ordinal
        }
    }

    "$prefix$($highest + 1)"
}

# Lists every identifier the artifact has ever assigned, including those of discarded findings.
function Get-FindingIdentifierList {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document
    )

    $identifiers = [System.Collections.Generic.List[string]]::new()

    $findings = Get-JsonArrayProperty -Node $Document -Name 'findings'
    foreach ($finding in $findings) {
        $identifiers.Add([string] (Get-JsonProperty -Node $finding -Name 'id'))
    }

    $removed = Get-JsonArrayProperty -Node $Document -Name 'removed'
    foreach ($entry in $removed) {
        $identifiers.Add([string] (Get-JsonProperty -Node (Get-JsonProperty -Node $entry -Name 'finding') -Name 'id'))
    }

    , $identifiers.ToArray()
}

# Locates a finding by identifier, refusing rather than reporting absence.
#
# Every caller of this treats a missing finding as a mistake in the request, so the refusal is
# stated once here instead of at each call site.
function Find-FindingIndex {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding,

        [Parameter(Mandatory)]
        [string] $FindingId
    )

    for ($index = 0; $index -lt $Finding.Count; $index++) {
        if ([string]::Equals([string] (Get-JsonProperty -Node $Finding[$index] -Name 'id'), $FindingId, [StringComparison]::Ordinal)) {
            return $index
        }
    }

    throw "This artifact holds no finding '$FindingId'."
}

# Refuses a batch that names the same finding twice.
#
# A batch is applied as a whole, and a repeated identifier makes the outcome depend on the order the
# entries happen to be read in: the second rewrite of a finding silently discards the first, and the
# second removal of one finds nothing to remove after the batch has already been half applied.
function Assert-FindingRequestDistinct {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Request,

        [Parameter(Mandatory)]
        [string] $ListName
    )

    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($entry in $Request) {
        $findingId = [string] $entry['findingId']
        if (-not $seen.Add($findingId)) {
            throw "The $ListName batch names '$findingId' more than once. A batch touches each finding once."
        }
    }
}

# Validates a finding, optionally including the fields the tooling assigns.
#
# One implementation serves both the finding an author writes and the record an artifact already
# holds, so a rule cannot be enforced on the way in and forgotten on the way out.
function Assert-FindingShape {
    param(
        [Parameter(Mandatory)]
        $Finding,

        [Parameter(Mandatory)]
        [Alias('Context')]
        [string] $SourceDescription,

        [Parameter(Mandatory)]
        [bool] $IncludeToolingField
    )

    $dictionary = $Finding -as [System.Collections.IDictionary]
    if ($null -eq $dictionary) {
        throw "The finding from $SourceDescription is not a JSON object."
    }

    $known = @($script:FindingToolingFields) + @($script:FindingProseFields) + @('anchor') + @($script:FindingOptionalFields)
    $authored = @($script:FindingProseFields) + @('anchor') + @($script:FindingOptionalFields)

    foreach ($key in @($dictionary.Keys)) {
        $name = [string] $key
        if (-not $IncludeToolingField -and $script:FindingToolingFields -ccontains $name) {
            throw "The finding from $SourceDescription sets '$name', which the tooling assigns. Remove it and let the artifact number and attribute the finding."
        }
        if ($known -cnotcontains $name) {
            $allowed = if ($IncludeToolingField) { $known } else { $authored }
            throw "The finding from $SourceDescription carries the unknown field '$name'. A finding carries only $(Get-FindingQuotedList -Item $allowed)."
        }
    }

    if ($IncludeToolingField) {
        foreach ($name in $script:FindingToolingFields) {
            if (-not (Test-JsonProperty -Node $dictionary -Name $name)) {
                throw "The finding from $SourceDescription is missing '$name'."
            }
        }
        $origin = [string] (Get-JsonProperty -Node $dictionary -Name 'origin')
        if ($script:FindingOriginVocabulary -cnotcontains $origin) {
            throw "The finding from $SourceDescription reports origin '$origin'; a finding originates as $(Get-FindingQuotedList -Item $script:FindingOriginVocabulary)."
        }
    }

    foreach ($name in $script:FindingProseFields) {
        Assert-FindingProseField `
            -Node $dictionary `
            -Name $name `
            -Field $name `
            -SourceDescription $SourceDescription `
            -AllowFormattingWhitespace:($name -cne 'title')
    }

    $title = [string] (Get-JsonProperty -Node $dictionary -Name 'title')
    if ($title.Length -gt $script:FindingTitleMaximumLength) {
        throw "The finding from $SourceDescription reports a title of $($title.Length) characters, and a title holds at most $($script:FindingTitleMaximumLength). State the point in a heading and leave the detail to the description."
    }

    $anchor = Get-JsonProperty -Node $dictionary -Name 'anchor'
    if ($null -eq $anchor) {
        throw "The finding from $SourceDescription is missing 'anchor'. A finding names the position it is about, so it can be posted where the code is."
    }
    Assert-FindingAnchor -Anchor $anchor -SourceDescription $SourceDescription

    $references = Get-JsonArrayProperty -Node $dictionary -Name 'references'
    for ($index = 0; $index -lt $references.Count; $index++) {
        Assert-FindingReference -Reference $references[$index] -Field "references[$index]" -SourceDescription $SourceDescription
    }

    $locations = Get-JsonArrayProperty -Node $dictionary -Name 'impactedLocations'
    for ($index = 0; $index -lt $locations.Count; $index++) {
        Assert-FindingLocation -Location $locations[$index] -Field "impactedLocations[$index]" -SourceDescription $SourceDescription
    }

    $followUp = Get-JsonProperty -Node $dictionary -Name 'followUp'
    if ($null -ne $followUp) {
        Assert-FindingKnownField -Node $followUp -Known @('threadId', 'commentId') -Field 'followUp' -SourceDescription $SourceDescription
        Assert-FindingTextField -Node $followUp -Name 'threadId' -Field 'followUp.threadId' -SourceDescription $SourceDescription
        Assert-FindingTextField -Node $followUp -Name 'commentId' -Field 'followUp.commentId' -SourceDescription $SourceDescription
    }
}

function Assert-FindingAnchor {
    param(
        [Parameter(Mandatory)]
        $Anchor,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    Assert-FindingKnownField -Node $Anchor -Known @('kind', 'path', 'startLine', 'endLine', 'side') -Field 'anchor' -SourceDescription $SourceDescription

    $kind = [string] (Get-JsonProperty -Node $Anchor -Name 'kind')
    if ($script:FindingAnchorKindVocabulary -cnotcontains $kind) {
        throw "The finding from $SourceDescription reports 'anchor.kind' as '$kind'; an anchor is $(Get-FindingQuotedList -Item $script:FindingAnchorKindVocabulary)."
    }

    Assert-FindingTextField -Node $Anchor -Name 'path' -Field 'anchor.path' -SourceDescription $SourceDescription
    ConvertTo-CanonicalFindingPath -Path ([string] (Get-JsonProperty -Node $Anchor -Name 'path')) -SourceDescription $SourceDescription | Out-Null

    if ($kind -ceq 'file') {
        foreach ($name in @('startLine', 'endLine', 'side')) {
            if (Test-JsonProperty -Node $Anchor -Name $name) {
                throw "The finding from $SourceDescription sets 'anchor.$name' on a file anchor. A finding about a file as a whole names no position within it."
            }
        }
        return
    }

    if (-not (Test-JsonProperty -Node $Anchor -Name 'startLine')) {
        throw "The finding from $SourceDescription is missing 'anchor.startLine'. A line anchor names the line the finding is about."
    }
    Assert-FindingLineRange -Node $Anchor -Field 'anchor' -SourceDescription $SourceDescription
    Assert-FindingSide -Node $Anchor -Field 'anchor' -SourceDescription $SourceDescription
}

function Assert-FindingReference {
    param(
        [Parameter(Mandatory)]
        $Reference,

        [Parameter(Mandatory)]
        [string] $Field,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    Assert-FindingKnownField -Node $Reference -Known @('text', 'url') -Field $Field -SourceDescription $SourceDescription
    Assert-FindingTextField -Node $Reference -Name 'text' -Field "$Field.text" -SourceDescription $SourceDescription
    if (Test-JsonProperty -Node $Reference -Name 'url') {
        Assert-FindingTextField -Node $Reference -Name 'url' -Field "$Field.url" -SourceDescription $SourceDescription
    }
}

function Assert-FindingLocation {
    param(
        [Parameter(Mandatory)]
        $Location,

        [Parameter(Mandatory)]
        [string] $Field,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    Assert-FindingKnownField -Node $Location -Known @('path', 'startLine', 'endLine', 'side') -Field $Field -SourceDescription $SourceDescription
    Assert-FindingTextField -Node $Location -Name 'path' -Field "$Field.path" -SourceDescription $SourceDescription
    ConvertTo-CanonicalFindingPath -Path ([string] (Get-JsonProperty -Node $Location -Name 'path')) -SourceDescription $SourceDescription | Out-Null
    Assert-FindingLineRange -Node $Location -Field $Field -SourceDescription $SourceDescription
    Assert-FindingSide -Node $Location -Field $Field -SourceDescription $SourceDescription

    # A location naming no line is about the file as a whole, and a file has no diff side, so a side
    # here would be an instruction the stored location has nowhere to keep.
    if ((Test-JsonProperty -Node $Location -Name 'side') -and -not (Test-JsonProperty -Node $Location -Name 'startLine')) {
        throw "The finding from $SourceDescription sets '$Field.side' without '$Field.startLine'. A location about a file as a whole names no diff side."
    }
}

# Checks a line range, where naming no line at all means the whole file.
function Assert-FindingLineRange {
    param(
        [Parameter(Mandatory)]
        $Node,

        [Parameter(Mandatory)]
        [string] $Field,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    $startLine = Get-JsonProperty -Node $Node -Name 'startLine'
    $endLine = Get-JsonProperty -Node $Node -Name 'endLine'

    foreach ($entry in @(
            @{ name = 'startLine'; value = $startLine }
            @{ name = 'endLine'; value = $endLine }
        )) {
        if ($null -eq $entry['value']) { continue }
        if (-not (Test-FindingPositiveInteger -Value $entry['value'])) {
            throw "The finding from $SourceDescription reports '$Field.$($entry['name'])' as '$($entry['value'])'; a line number is a positive integer."
        }
    }

    if ($null -ne $endLine -and $null -eq $startLine) {
        throw "The finding from $SourceDescription sets '$Field.endLine' without '$Field.startLine'."
    }
    if ($null -ne $startLine -and $null -ne $endLine -and [long] $endLine -lt [long] $startLine) {
        throw "The finding from $SourceDescription reports '$Field.endLine' before '$Field.startLine'."
    }
}

function Assert-FindingSide {
    param(
        [Parameter(Mandatory)]
        $Node,

        [Parameter(Mandatory)]
        [string] $Field,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    if (-not (Test-JsonProperty -Node $Node -Name 'side')) { return }

    $side = [string] (Get-JsonProperty -Node $Node -Name 'side')
    if ($script:FindingSideVocabulary -cnotcontains $side) {
        throw "The finding from $SourceDescription reports '$Field.side' as '$side'; a diff side is $(Get-FindingQuotedList -Item $script:FindingSideVocabulary)."
    }
}

function Assert-FindingTextField {
    param(
        [Parameter(Mandatory)]
        $Node,

        [Parameter(Mandatory)]
        [string] $Name,

        [Parameter(Mandatory)]
        [string] $Field,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    if (-not (Test-JsonProperty -Node $Node -Name $Name)) {
        throw "The finding from $SourceDescription is missing '$Field', which must be a non-empty string."
    }

    $value = Get-JsonProperty -Node $Node -Name $Name
    if ($value -isnot [string] -or [string]::IsNullOrWhiteSpace($value)) {
        throw "The finding from $SourceDescription reports '$Field' as something other than a non-empty string."
    }
}

function Assert-FindingProseField {
    param(
        [Parameter(Mandatory)]
        $Node,

        [Parameter(Mandatory)]
        [string] $Name,

        [Parameter(Mandatory)]
        [string] $Field,

        [Parameter(Mandatory)]
        [string] $SourceDescription,

        [switch] $AllowFormattingWhitespace
    )

    Assert-FindingTextField -Node $Node -Name $Name -Field $Field -SourceDescription $SourceDescription
    $value = Get-JsonProperty -Node $Node -Name $Name

    # Multiline Markdown uses tabs and line feeds. Other controls can alter terminal or publisher
    # interpretation without contributing visible prose, and title fields must remain on one line.
    $pattern = if ($AllowFormattingWhitespace) { '[\u0000-\u0008\u000B-\u001F]' } else { '[\u0000-\u001F]' }
    $controlCharacter = [regex]::Match($value, $pattern)
    if ($controlCharacter.Success) {
        $codePoint = 'U+{0:X4}' -f [int] [char] $controlCharacter.Value[0]
        $remedy = if ($AllowFormattingWhitespace) {
            'Use line feeds for line breaks and tabs for indentation.'
        } else {
            'Keep this field on one line without control characters.'
        }
        throw "The finding from $SourceDescription reports '$Field' with C0 control character $codePoint. $remedy"
    }
}

function Assert-FindingKnownField {
    param(
        [Parameter(Mandatory)]
        $Node,

        [Parameter(Mandatory)]
        [string[]] $Known,

        [Parameter(Mandatory)]
        [string] $Field,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    $dictionary = $Node -as [System.Collections.IDictionary]
    if ($null -eq $dictionary) {
        throw "The finding from $SourceDescription reports '$Field' as something other than a JSON object."
    }

    foreach ($key in @($dictionary.Keys)) {
        $name = [string] $key
        if ($Known -cnotcontains $name) {
            throw "The finding from $SourceDescription carries the unknown field '$Field.$name'. It carries only $(Get-FindingQuotedList -Item $Known)."
        }
    }
}

# Reports whether a materialized JSON value is a whole number above zero.
#
# The type test is what matters: a quoted line number reaches here as a string and would compare
# greater than zero after coercion, anchoring the finding to whatever that string happened to look
# like.
function Test-FindingPositiveInteger {
    param(
        [AllowNull()]
        $Value
    )

    if ($Value -isnot [long] -and $Value -isnot [int]) { return $false }
    [long] $Value -gt 0
}

function ConvertTo-FindingReferenceList {
    param(
        [Parameter(Mandatory)]
        $Finding
    )

    $references = Get-JsonArrayProperty -Node $Finding -Name 'references'
    $records = [System.Collections.Generic.List[object]]::new()
    foreach ($reference in $references) {
        $record = [ordered]@{ text = [string] (Get-JsonProperty -Node $reference -Name 'text') }
        if (Test-JsonProperty -Node $reference -Name 'url') {
            $record['url'] = [string] (Get-JsonProperty -Node $reference -Name 'url')
        }
        $records.Add($record)
    }

    , $records.ToArray()
}

# Builds the stored impacted locations, filling in what the author was allowed to leave out.
#
# A located line carries an end line and a diff side for the reason an anchor does: one stored shape
# spares every reader the defaults.
function ConvertTo-FindingLocationList {
    param(
        [Parameter(Mandatory)]
        $Finding,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    $locations = Get-JsonArrayProperty -Node $Finding -Name 'impactedLocations'
    $records = [System.Collections.Generic.List[object]]::new()
    foreach ($location in $locations) {
        $record = [ordered]@{
            path = ConvertTo-CanonicalFindingPath -Path ([string] (Get-JsonProperty -Node $location -Name 'path')) -SourceDescription $SourceDescription
        }
        if (Test-JsonProperty -Node $location -Name 'startLine') {
            $record['startLine'] = [int] (Get-JsonProperty -Node $location -Name 'startLine')
            $record['endLine'] = [int] (Get-JsonProperty -Node $location -Name 'endLine' -Default (Get-JsonProperty -Node $location -Name 'startLine'))
            $record['side'] = [string] (Get-JsonProperty -Node $location -Name 'side' -Default $script:DefaultFindingSide)
        }
        $records.Add($record)
    }

    , $records.ToArray()
}

# Builds the stored anchor, filling in what the author was allowed to leave out.
#
# A single-line finding names only its start line, and the stored record states the end line anyway
# so that everything reading an anchor reads one shape rather than reimplementing the default.
function ConvertTo-FindingAnchor {
    param(
        [Parameter(Mandatory)]
        $Anchor,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    $kind = [string] (Get-JsonProperty -Node $Anchor -Name 'kind')
    $record = [ordered]@{
        kind = $kind
        path = ConvertTo-CanonicalFindingPath -Path ([string] (Get-JsonProperty -Node $Anchor -Name 'path')) -SourceDescription $SourceDescription
    }

    if ($kind -ceq 'line') {
        $startLine = [int] (Get-JsonProperty -Node $Anchor -Name 'startLine')
        $record['startLine'] = $startLine
        $record['endLine'] = [int] (Get-JsonProperty -Node $Anchor -Name 'endLine' -Default $startLine)
        $record['side'] = [string] (Get-JsonProperty -Node $Anchor -Name 'side' -Default $script:DefaultFindingSide)
    }

    $record
}

# Reports everything wrong with one artifact, as messages rather than as a refusal.
function Get-FindingArtifactProblem {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [string] $Path
    )

    $problems = [System.Collections.Generic.List[string]]::new()
    $kind = [string] (Get-JsonProperty -Node $Document -Name 'kind')

    $known = if ($kind -ceq 'reviewer') {
        @('schemaVersion', 'kind', 'reviewerId', 'focusArea', 'status', 'findings', 'removed')
    } else {
        @('schemaVersion', 'kind', 'findings', 'removed')
    }
    foreach ($key in @($Document.Keys)) {
        $name = [string] $key
        if ($known -cnotcontains $name) {
            $problems.Add("The artifact at '$Path' carries the unknown field '$name'.")
        }
    }

    if ($kind -ceq 'reviewer') {
        foreach ($name in @('reviewerId', 'focusArea')) {
            if ([string]::IsNullOrWhiteSpace([string] (Get-JsonProperty -Node $Document -Name $name))) {
                $problems.Add("The artifact at '$Path' is missing '$name'.")
            }
        }
        $status = [string] (Get-JsonProperty -Node $Document -Name 'status')
        if ($script:FindingArtifactStatusVocabulary -cnotcontains $status) {
            $problems.Add("The artifact at '$Path' reports status '$status'; a reviewer artifact is $(Get-FindingQuotedList -Item $script:FindingArtifactStatusVocabulary).")
        }
    }

    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    $findings = Get-JsonArrayProperty -Node $Document -Name 'findings'
    foreach ($finding in $findings) {
        $id = [string] (Get-JsonProperty -Node $finding -Name 'id')
        if (-not $seen.Add($id)) {
            $problems.Add("The artifact at '$Path' holds more than one finding '$id'.")
        }
        try {
            Assert-FindingShape -Finding $finding -SourceDescription "'$Path' (finding '$id')" -IncludeToolingField $true
        } catch {
            $problems.Add($_.Exception.Message)
        }
    }

    $removed = Get-JsonArrayProperty -Node $Document -Name 'removed'
    for ($index = 0; $index -lt $removed.Count; $index++) {
        foreach ($name in @('finding', 'reason', 'removedUtc')) {
            if (-not (Test-JsonProperty -Node $removed[$index] -Name $name)) {
                $problems.Add("The artifact at '$Path' has a removal record at position $index that is missing '$name'.")
            }
        }
        # Removal history is persisted finding evidence, so it must retain the same valid record.
        try {
            Assert-FindingShape -Finding (Get-JsonProperty -Node $removed[$index] -Name 'finding') `
                -SourceDescription "'$Path' (removal record at position $index)" -IncludeToolingField $true
        } catch {
            $problems.Add($_.Exception.Message)
        }
    }

    , $problems.ToArray()
}

# Flattens a run's reviewer artifacts into one ordered list of findings and their attribution.
#
# Consolidation and the index both read every finding of every reviewer alongside the reviewer it came from,
# and neither has any use for the artifact boundaries themselves.
function Get-FindingSourceList {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $ReviewerArtifact
    )

    $sources = [System.Collections.Generic.List[object]]::new()
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)

    foreach ($artifact in $ReviewerArtifact) {
        $document = Get-JsonProperty -Node $artifact -Name 'document'
        $reviewerId = Get-JsonProperty -Node $document -Name 'reviewerId'
        $focusArea = Get-JsonProperty -Node $document -Name 'focusArea'

        $findings = Get-JsonArrayProperty -Node $document -Name 'findings'
        foreach ($finding in $findings) {
            $findingId = [string] (Get-JsonProperty -Node $finding -Name 'id')

            # Identifiers are qualified by reviewer, so a repeated one means two artifacts claim the same
            # reviewer. Merging them would attribute one reviewer's findings to another and would make a
            # merge plan naming that identifier ambiguous.
            if (-not $seen.Add($findingId)) {
                throw "More than one reviewer artifact holds finding '$findingId'. Two reviewers share an identifier, so the findings cannot be told apart."
            }

            $sources.Add([ordered]@{
                    id         = $findingId
                    reviewerId = $reviewerId
                    focusArea  = $focusArea
                    finding    = $finding
                })
        }
    }

    , $sources.ToArray()
}

# Records one merged finding, checking that it stands in for everything it replaces.
function Add-FindingMergeToArtifact {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        $Group,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Source,

        [Parameter(Mandatory)]
        [System.Collections.Generic.Dictionary[string, int]] $PositionById
    )

    $identifiers = Get-JsonArrayProperty -Node $Group -Name 'sourceFindingIds'
    $sourceFindingIds = @($identifiers | ForEach-Object { [string] $_ })
    $findingPath = [string] (Get-JsonProperty -Node $Group -Name 'findingPath')
    $sourceDescription = "'$findingPath' (merging $($sourceFindingIds -join ', '))"

    $replaced = [System.Collections.Generic.List[object]]::new()
    foreach ($findingId in $sourceFindingIds) {
        $replaced.Add((Get-JsonProperty -Node $Source[$PositionById[$findingId]] -Name 'finding'))
    }

    $finding = Read-FindingContent -Path $findingPath
    Assert-FindingContent -Finding $finding -SourceDescription $sourceDescription
    Assert-FindingMergeCoverage -Finding $finding -Source $replaced.ToArray() -SourceDescription $sourceDescription

    $followUp = Resolve-FindingMergeFollowUp -Finding $finding -Source $replaced.ToArray() -SourceDescription $sourceDescription
    if ($null -ne $followUp) {
        Set-JsonProperty -Node $finding -Name 'followUp' -Value $followUp
    }

    Add-FindingToArtifact `
        -Document $Document `
        -Finding $finding `
        -Origin (Resolve-FindingMergeOrigin -Source $replaced.ToArray()) `
        -SourceFindingId $sourceFindingIds `
        -SourceDescription $sourceDescription
}

# Projects a stored record back to the content an author writes.
#
# Consolidation records a carried-over finding through the same path a new one takes, so that the
# record it produces is validated and shaped exactly as every other record is, rather than being a
# copy that happens to look right.
function ConvertTo-FindingContent {
    param(
        [Parameter(Mandatory)]
        $Record
    )

    $content = [ordered]@{}
    foreach ($name in $script:FindingProseFields) {
        $content[$name] = Get-JsonProperty -Node $Record -Name $name
    }
    $content['anchor'] = Get-JsonProperty -Node $Record -Name 'anchor'

    foreach ($name in $script:FindingOptionalFields) {
        if (-not (Test-JsonProperty -Node $Record -Name $name)) { continue }
        $value = Get-JsonProperty -Node $Record -Name $name
        # An empty list and an absent one say the same thing, and the stored record always carries
        # both lists, so an empty one is dropped rather than carried as an empty field.
        if ($value -is [object[]] -and $value.Count -eq 0) { continue }
        $content[$name] = $value
    }

    $content
}

# Refuses a merged finding that speaks for less than the findings it replaces.
#
# A merge is meant to remove a repetition, not a claim: every position any of the merged findings
# named must still be named, or the author never hears about the file the merged text left out.
function Assert-FindingMergeCoverage {
    param(
        [Parameter(Mandatory)]
        $Finding,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Source,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    $covering = Get-FindingCoverageLocation -Finding $Finding -SourceDescription $SourceDescription

    $missing = [System.Collections.Generic.List[string]]::new()
    foreach ($replaced in $Source) {
        foreach ($required in (Get-FindingCoverageLocation -Finding $replaced -SourceDescription $SourceDescription)) {
            if (Test-FindingLocationCovered -Location $required -Covering $covering) { continue }
            $description = Get-FindingLocationDescription -Location $required
            if (-not $missing.Contains($description)) { $missing.Add($description) }
        }
    }

    if ($missing.Count -eq 0) { return }

    throw "The merged finding from $SourceDescription names neither an anchor nor an impacted location covering $($missing -join ', '). A merged finding lists every instance the findings it replaces named."
}

# Lists every position a finding speaks about, as one comparable set.
#
# The anchor is included because it is a position like any other: a finding merged into another one
# keeps its anchor only if the merged text still names that place.
function Get-FindingCoverageLocation {
    param(
        [Parameter(Mandatory)]
        $Finding,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    $locations = [System.Collections.Generic.List[object]]::new()

    $anchor = Get-JsonProperty -Node $Finding -Name 'anchor'
    if ($null -ne $anchor) {
        $locations.Add((ConvertTo-FindingAnchor -Anchor $anchor -SourceDescription $SourceDescription))
    }

    $impacted = ConvertTo-FindingLocationList -Finding $Finding -SourceDescription $SourceDescription
    foreach ($location in $impacted) { $locations.Add($location) }

    , $locations.ToArray()
}

# Reports whether one position is spoken for by any of the given positions.
#
# A wider range covers a narrower one and a whole-file location covers every line of that file, so a
# merged finding that broadens what it names is accepted; only a merge that narrows is refused.
function Test-FindingLocationCovered {
    param(
        [Parameter(Mandatory)]
        $Location,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Covering
    )

    $path = [string] (Get-JsonProperty -Node $Location -Name 'path')
    $startLine = Get-JsonProperty -Node $Location -Name 'startLine'

    foreach ($candidate in $Covering) {
        if (-not [string]::Equals([string] (Get-JsonProperty -Node $candidate -Name 'path'), $path, [StringComparison]::Ordinal)) { continue }

        $candidateStart = Get-JsonProperty -Node $candidate -Name 'startLine'
        if ($null -eq $candidateStart) { return $true }
        if ($null -eq $startLine) { continue }

        # The two sides of a diff are different text, so a position on one does not speak for the
        # same line numbers on the other.
        if (-not [string]::Equals(
                [string] (Get-JsonProperty -Node $candidate -Name 'side'),
                [string] (Get-JsonProperty -Node $Location -Name 'side'),
                [StringComparison]::Ordinal)) {
            continue
        }

        $end = [long] (Get-JsonProperty -Node $Location -Name 'endLine' -Default $startLine)
        $candidateEnd = [long] (Get-JsonProperty -Node $candidate -Name 'endLine' -Default $candidateStart)
        if ([long] $candidateStart -le [long] $startLine -and $candidateEnd -ge $end) { return $true }
    }

    $false
}

# Names one position in the form a reader can look it up by.
function Get-FindingLocationDescription {
    param(
        [Parameter(Mandatory)]
        $Location
    )

    $path = [string] (Get-JsonProperty -Node $Location -Name 'path')
    $startLine = Get-JsonProperty -Node $Location -Name 'startLine'
    if ($null -eq $startLine) { return $path }

    $endLine = Get-JsonProperty -Node $Location -Name 'endLine' -Default $startLine
    $text = if ([long] $endLine -gt [long] $startLine) { "${path}:$startLine-$endLine" } else { "${path}:$startLine" }

    # The removed side is stated because it is the exception; the added side is where a review
    # finding almost always sits and naming it on every position would be noise.
    if ([string] (Get-JsonProperty -Node $Location -Name 'side') -ceq 'left') { return "$text (left side)" }

    $text
}

# Settles which discussion thread a merged finding replies into.
#
# Findings answering two different threads cannot be merged at all: a comment is posted into one
# thread, so one of the two authors would be left without the reply the review owes them. Where the
# findings agree on a thread, the merged finding inherits it, because the reply destination is
# provenance the adjudicator is not asked to restate.
function Resolve-FindingMergeFollowUp {
    param(
        [Parameter(Mandatory)]
        $Finding,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Source,

        [Parameter(Mandatory)]
        [string] $SourceDescription
    )

    $targets = [System.Collections.Specialized.OrderedDictionary]::new([System.StringComparer]::Ordinal)
    foreach ($replaced in $Source) {
        $followUp = Get-JsonProperty -Node $replaced -Name 'followUp'
        if ($null -eq $followUp) { continue }
        $targets[(Get-FindingFollowUpKey -FollowUp $followUp)] = $followUp
    }

    if ($targets.Count -gt 1) {
        throw "The merge $SourceDescription joins findings that answer different discussion threads ($(@($targets.Keys) -join ', ')). A finding replies into one thread, so merging these would leave one of them unanswered."
    }

    $merged = Get-JsonProperty -Node $Finding -Name 'followUp'
    if ($targets.Count -eq 0) { return $merged }

    $target = $targets[0]
    if ($null -eq $merged) { return $target }

    $mergedKey = Get-FindingFollowUpKey -FollowUp $merged
    if ($mergedKey -cne (Get-FindingFollowUpKey -FollowUp $target)) {
        throw "The merged finding from $SourceDescription answers thread '$mergedKey', but the findings it replaces answer '$(Get-FindingFollowUpKey -FollowUp $target)'. A merged finding replies where the findings it replaces were raised."
    }

    $merged
}

function Get-FindingFollowUpKey {
    param(
        [Parameter(Mandatory)]
        $FollowUp
    )

    "$([string] (Get-JsonProperty -Node $FollowUp -Name 'threadId'))/$([string] (Get-JsonProperty -Node $FollowUp -Name 'commentId'))"
}

# Settles the origin of a merged finding.
#
# A merge of what reviewers found is still what reviewers found, said once instead of several times.
# A group drawing in a finding produced by synthesis is itself synthesis, because such a finding
# describes the review's overall picture and is presented ahead of the individual ones.
function Resolve-FindingMergeOrigin {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Source
    )

    foreach ($replaced in $Source) {
        if ([string] (Get-JsonProperty -Node $replaced -Name 'origin') -ceq 'synthesis') { return 'synthesis' }
    }

    'reviewer'
}

# Reads a decision as the vocabulary spells it.
#
# Leniency at the boundary only, for the reason the authorship classification is lenient: the value
# arrives from a language model that writes 'Approved' as readily as 'approved', while every
# comparison against the stored value stays case-sensitive.
function ConvertTo-FindingDecisionValue {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Decision,

        [Parameter(Mandatory)]
        [string] $FindingId
    )

    $value = $Decision.Trim().ToLowerInvariant()
    if ($script:FindingDecisionVocabulary -cnotcontains $value) {
        throw "The decision '$Decision' for finding '$FindingId' is not one an audit reaches; it decides $(Get-FindingQuotedList -Item $script:FindingDecisionVocabulary)."
    }

    $value
}

# Reads one list out of a plan file.
#
# Plans are written by a subagent that has just produced a batch of rewrites, so a plan naming a list
# that is empty, absent or not a list is a mistake in the batch rather than an instruction to do
# nothing.
function Get-FindingPlanEntry {
    param(
        [Parameter(Mandatory)]
        [string] $PlanPath,

        [Parameter(Mandatory)]
        [string] $ListName
    )

    $fullPath = Resolve-PullRequestPath -Path $PlanPath
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        throw "There is no plan at '$PlanPath'."
    }

    $plan = Read-WorkflowData -Kind $ListName -Path $PlanPath
    if ($plan -isnot [System.Collections.IDictionary]) {
        throw "The plan at '$PlanPath' is not a JSON object carrying '$ListName'."
    }

    $entries = $plan[$ListName]
    if ($entries.Count -eq 0) {
        throw "The plan at '$PlanPath' names no entries under '$ListName'."
    }

    , $entries
}

# Resolves a path a plan names.
#
# A relative path is taken against the plan's own directory, because the plan and the documents it
# names are written together by one subagent while the process that applies them runs from wherever
# the orchestrator happened to be.
function Resolve-FindingPlanRelativePath {
    param(
        [Parameter(Mandatory)]
        [string] $PlanPath,

        [Parameter(Mandatory)]
        [string] $Path
    )

    if ([System.IO.Path]::IsPathRooted($Path)) { return $Path }

    $planDirectory = Split-Path -Parent (Resolve-PullRequestPath -Path $PlanPath)
    Join-Path $planDirectory $Path
}

# Refuses a reviewer identifier that cannot name a file.
#
# The artifact filename is the reviewer identifier, so an identifier carrying a separator would write
# outside the findings directory and an identifier carrying a character the filesystem rejects would
# fail at the write with a message naming a path the caller never supplied.
function Assert-FindingReviewerId {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $ReviewerId
    )

    if ([string]::IsNullOrWhiteSpace($ReviewerId)) {
        throw 'A reviewer artifact requires a reviewer identifier.'
    }

    if ($ReviewerId -match '[\\/:*?"<>|]' -or $ReviewerId.Contains('..')) {
        throw "The reviewer identifier '$ReviewerId' cannot be used, because it names the artifact file. Use letters, digits, dashes and underscores."
    }
}

# Reads one of this library's documents off disk and confirms it is of the version this library
# writes.
#
# The wording is supplied by the caller because the same failure has to name the document that was
# asked for: a run rebuilding an approved set is not helped by being told to create a finding
# artifact.
#
# The version is enforced rather than merely recorded, for the reason the capture enforces its own:
# a document shaped differently would not fail, it would be read as findings whose identifiers and
# anchors mean something else, and the result decides what gets posted to a pull request.
function Read-FindingJsonDocument {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Subject,

        [Parameter(Mandatory)]
        [string] $MissingRemedy,

        [Parameter(Mandatory)]
        [string] $VersionRemedy,

        [switch] $AllowApproved
    )

    $fullPath = Resolve-PullRequestPath -Path $Path
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        throw "There is no $Subject at '$Path'. $MissingRemedy"
    }

    $document = Read-FindingJsonValue -Path $Path -Subject $Subject
    if ($document -isnot [System.Collections.IDictionary]) {
        throw "The $Subject at '$Path' is not a JSON object."
    }

    $version = Get-JsonProperty -Node $document -Name 'schemaVersion'
    $expectedVersion = if ($AllowApproved -and -not (Test-JsonProperty -Node $document -Name 'kind')) {
        $script:ApprovedFindingSchemaVersion
    } else { $script:FindingArtifactSchemaVersion }
    if ($version -ne $expectedVersion) {
        $reported = if ($null -eq $version) { 'no version' } else { "version $version" }
        throw "The $Subject at '$Path' reports $reported, but this helper reads version $expectedVersion. $VersionRemedy"
    }

    $document
}

# Renders a vocabulary as prose, so a rejection can state what it would have accepted.
function Get-FindingQuotedList {
    param(
        [Parameter(Mandatory)]
        [string[]] $Item
    )

    $quoted = @($Item | ForEach-Object { "'$_'" })
    if ($quoted.Count -eq 1) { return $quoted[0] }
    "$($quoted[0..($quoted.Count - 2)] -join ', ') or $($quoted[-1])"
}
