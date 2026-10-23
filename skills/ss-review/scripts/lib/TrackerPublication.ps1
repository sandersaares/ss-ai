#Requires -Version 7.6

# Files an approved review into a tracker (docs/design.md, "Publication"; docs/implementation.md,
# "Tracker publication").
#
# Publication onto a pull request assembles one review and submits it, so nothing is visible until
# the submit and a failure before it leaves nothing behind. A tracker has no such construct. Every
# item is an independent write that is visible the moment it is accepted, which changes what the run
# can promise and therefore how it is built:
#
# - the journal is the only account of what exists, so it is written before the first creation and
#   rewritten after every one;
# - a rerun over a journal that records a creation is refused outright, because a second run would
#   file the same review again rather than resume the first;
# - the first item is verified before the rest are created, because the description format Azure
#   DevOps stores cannot be changed afterwards and a wrong guess would otherwise cost a whole review
#   rather than one item;
# - a failure is reported with the items that were created, since there is no resume and that list is
#   the most valuable thing a failed run can hand back.
#
# This is a library file: it is dot-sourced rather than executed, defines only functions and
# constants, and dot-sources nothing itself. It calls functions from JsonValue.ps1,
# PullRequestModel.ps1, PublicationFailure.ps1, TrackerTarget.ps1, TrackerItem.ps1,
# GitHubIssueApi.ps1 and AdoWorkItemApi.ps1, which the composition root loads alongside this file.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The journal's format version. Stated independently of every other artifact's, because a document
# shaped differently would be read as a record of writes that did not happen.
$script:TrackerPublicationResultSchemaVersion = 1

# The filename the journal is published under. Stated once here because the executable writes it and
# the skill instructs its operator to read it after a failure.
$script:TrackerPublicationResultFileName = 'tracker-publication-result.json'

# The operations a journal records. A creation files one finding; the verification is the read that
# confirms every created item holds what was sent.
$script:TrackerCreateOperation = 'createItem'
$script:TrackerVerifyOperation = 'verifyItems'

# The outcomes that mean an item may exist. A refused creation is proven not to have happened, so it
# is not among them; everything else is either known to exist or unknown, and both block a rerun.
$script:TrackerUnsafeOutcomes = @('succeeded', 'ambiguous')

# Delay between creations. GitHub asks callers to leave at least a second between mutating requests
# and answers a burst with a secondary rate limit, so a review of
# many findings paced any faster would be refused partway through.
$script:TrackerCreationPacingMilliseconds = 1000

# The run states that stop a publication short of success while leaving it resumable. A body Azure
# DevOps rewrote in a way the run does not recognize pauses the run at 'needsAdjudication', naming the
# rewrite for a person to accept or reject; an accepted rewrite is recorded and the run moves to
# 'resumeReady', from which the continuation files whatever is still pending. Both are held apart from
# 'inProgress', because a run in either state has stopped deliberately and can be continued, whereas an
# 'inProgress' journal belongs to a run that never said what became of the creation it was making.
# Ref: docs/design.md, "Tracker publication"; docs/implementation.md, "Body verification".
$script:TrackerNeedsAdjudicationStatus = 'needsAdjudication'
$script:TrackerResumeReadyStatus = 'resumeReady'

# The lowest body similarity a rewrite may carry and still be offered for adjudication rather than
# refused outright. Below it the stored body no longer resembles the approved text closely enough to
# be a Markdown normalization artifact, so it is treated as corruption and fails the run. The value is
# the one chosen for issue #38; it is a single constant so the boundary can be retuned in one place.
$script:TrackerBodyAdjudicationThreshold = 0.90

# The largest edit-distance table, in cells, the similarity measurement will compute. A real Azure
# DevOps rewrite touches a small, localized region, so once the shared head and tail are removed the
# differing spans are short and their product is small. A product larger than this is not a small
# reformatting; the deviation is left undecided so it is adjudicated by a person rather than decided
# from a distance the run declines to spend the time computing. The value keeps the quadratic step to
# a few million character comparisons, which is well under a second.
$script:TrackerBodySimilarityCellCap = 4000000

# Characters of unchanged context shown on either side of a differing span in an adjudication excerpt,
# and the longest excerpt reproduced. The excerpt exists to let a person see what changed without
# opening the tracker; the context makes the change legible and the cap keeps a long span from filling
# the journal.
$script:TrackerBodyExcerptContext = 48
$script:TrackerBodyExcerptMaxLength = 240

# --- Dispatch ------------------------------------------------------------------------------------

# Reads what the tracker says about filing this review into it, before anything is created.
function Get-TrackerStateForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [string] $WorkItemType,

        [string] $AreaPath,

        [string] $DescriptionField,

        [int] $ParentWorkItemId
    )

    switch ($Target['platform']) {
        'gitHub' { return Get-GitHubIssueTrackerState -Transport $Transport -Target $Target }
        'azureDevOps' {
            return Get-AdoWorkItemTrackerState `
                -Transport $Transport `
                -Target $Target `
                -WorkItemType $WorkItemType `
                -AreaPath $AreaPath `
                -DescriptionField $DescriptionField `
                -ParentWorkItemId $ParentWorkItemId
        }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Files one item.
function New-TrackerItemForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Item
    )

    switch ($Target['platform']) {
        'gitHub' { return New-GitHubIssueForFinding -Transport $Transport -State $State -Item $Item }
        'azureDevOps' { return New-AdoWorkItemForFinding -Transport $Transport -Target $Target -State $State -Item $Item }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Reads one filed item back.
function Get-TrackerItemRecordForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Record
    )

    switch ($Target['platform']) {
        'gitHub' { return Get-GitHubIssueRecord -Transport $Transport -ItemNodeId ([string] $Record['itemNodeId']) }
        'azureDevOps' { return Get-AdoWorkItemRecord -Transport $Transport -Target $Target -State $State -ItemId ([string] $Record['itemId']) }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# --- The run -------------------------------------------------------------------------------------

# Files every item and keeps the journal current throughout.
#
# Every item is created and read back before the next is attempted. This run cannot create tracker
# drafts or withdraw filed items, so stopping at the first item that fails verification limits the
# irreversible writes without giving up the final verification of the complete set.
#
# The run resumes as well as begins. An item the journal already records as filed is re-read and
# re-verified rather than created again, and only the items still pending are filed, so a continuation
# picks up exactly where an earlier run paused. What stops the run short is one of three things: a
# rejection fails it and is raised after the journal records it; a rewrite Azure DevOps made that the
# run cannot judge on its own pauses it for a person to accept or reject; and a clean pass ends it in
# success. A failed or paused run leaves a list of what exists, which is what the caller needs in
# order to decide what to do about the rest.
function Invoke-TrackerPublication {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Item,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $ResultPath,

        # Injected so a test runs without waiting. Production runs leave it unset and get the pacing
        # the services ask for.
        [scriptblock] $Wait = { param([int] $Milliseconds) Start-Sleep -Milliseconds $Milliseconds }
    )

    try {
        $paused = $false
        $createdThisRun = 0
        foreach ($entry in $Item) {
            $operation = Find-TrackerPublicationOperation -Result $Result -FindingId ([string] $entry['findingId'])
            $acceptedRewrite = Get-JsonProperty -Node $operation -Name 'acceptedRewrite'

            if ([string] (Get-JsonProperty -Node $operation -Name 'outcome') -ceq 'succeeded') {
                # Filed by an earlier run. Read it back and verify it without creating it again or
                # pacing, so a rewrite that has changed since it was accepted is caught before the
                # rest are filed.
                $check = Test-TrackerItemStored `
                    -Transport $Transport -Target $Target -State $State -Item $entry `
                    -Result $Result -ResultPath $ResultPath -AcceptedRewrite $acceptedRewrite
                if ([bool] $check['paused']) { $paused = $true; break }
                continue
            }

            # About to file a new item. A continuation is moved off its resumable state first, and the
            # move is persisted before the creation request, so a run that dies mid-creation reads from
            # disk as one whose outcome is unknown - which the rerun guard refuses - rather than one
            # still waiting to be resumed, which a rerun would file a second time.
            if ([string] (Get-JsonProperty -Node $Result -Name 'status') -ceq $script:TrackerResumeReadyStatus) {
                Set-JsonProperty -Node $Result -Name 'status' -Value 'inProgress'
                Write-TrackerPublicationResult -Result $Result -Path $ResultPath
            }
            if ($createdThisRun -gt 0) { & $Wait -Milliseconds $script:TrackerCreationPacingMilliseconds | Out-Null }

            Invoke-TrackerPublicationCreation `
                -Transport $Transport -Target $Target -State $State -Item $entry `
                -Result $Result -ResultPath $ResultPath | Out-Null
            $createdThisRun++

            # A tracker item cannot be repaired or withdrawn by this run. Prove each item round-trips
            # before risking creation of the next one.
            $check = Test-TrackerItemStored `
                -Transport $Transport -Target $Target -State $State -Item $entry `
                -Result $Result -ResultPath $ResultPath -AcceptedRewrite $null
            if ([bool] $check['paused']) { $paused = $true; break }
        }

        if (-not $paused) {
            $verification = Invoke-TrackerPublicationVerification `
                -Transport $Transport `
                -Target $Target `
                -State $State `
                -Item $Item `
                -Result $Result `
                -ResultPath $ResultPath

            if ([string] (Get-JsonProperty -Node $Result -Name 'status') -cne $script:TrackerNeedsAdjudicationStatus) {
                Set-JsonProperty -Node $Result -Name 'verification' -Value $verification
                Set-JsonProperty -Node $Result -Name 'status' -Value 'succeeded'
            }
        }
    } catch {
        Set-JsonProperty -Node $Result -Name 'status' -Value 'failed'
        Set-JsonProperty -Node $Result -Name 'message' -Value $_.Exception.Message
        throw
    } finally {
        Write-TrackerPublicationResult -Result $Result -Path $ResultPath
    }

    $Result
}

# Reads one filed item back and judges what the tracker holds against what the review approved.
#
# Both the creation loop and the final verification reach the tracker through here, so the staged
# judgement is applied identically whether an item was just created or is being re-read on a resume. A
# match returns so the run proceeds; a rejection is raised to fail the run; a rewrite the run cannot
# judge on its own records the pause in the journal and returns 'paused', which stops the run without
# failing it.
function Test-TrackerItemStored {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Item,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $ResultPath,

        [System.Collections.IDictionary] $AcceptedRewrite
    )

    $operation = Find-TrackerPublicationOperation -Result $Result -FindingId ([string] $Item['findingId'])
    $record = Get-TrackerItemRecordForTarget -Transport $Transport -Target $Target -State $State -Record $operation
    $verdict = Get-TrackerItemStoredVerdict -Item $Item -Record $record -Platform ([string] $Target['platform']) -AcceptedRewrite $AcceptedRewrite

    switch ([string] $verdict['verdict']) {
        'match' { return [ordered]@{ paused = $false; record = $record } }
        'reject' { throw [string] $verdict['reason'] }
        'adjudicate' {
            Set-TrackerPublicationAdjudication -Result $Result -Adjudication ([System.Collections.IDictionary] $verdict['deviation'])
            Write-TrackerPublicationResult -Result $Result -Path $ResultPath
            return [ordered]@{ paused = $true; record = $record }
        }
        default { throw "The tracker publication produced an unknown verdict '$($verdict['verdict'])'." }
    }
}

# Records that the run has paused for a person to accept or reject a rewrite.
function Set-TrackerPublicationAdjudication {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Adjudication
    )

    Set-JsonProperty -Node $Result -Name 'adjudication' -Value $Adjudication
    Set-JsonProperty -Node $Result -Name 'status' -Value $script:TrackerNeedsAdjudicationStatus

    $operation = Find-TrackerPublicationOperation -Result $Result -FindingId ([string] $Adjudication['findingId'])
    Set-JsonProperty -Node $operation -Name 'message' -Value (Get-TrackerAdjudicationNote -Adjudication $Adjudication)
}

# Notes on the paused item what a person continuing the run needs to know about it.
function Get-TrackerAdjudicationNote {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Adjudication
    )

    $similarity = Get-JsonProperty -Node $Adjudication -Name 'similarity'
    $closeness = if ($null -eq $similarity) { 'a body too large to measure quickly' } else { "a body about $([Math]::Round([double] $similarity * 100))% similar to the approved text" }
    "Azure DevOps stored $closeness. Review the stored body against the approved text and either delete the work item to file the review afresh or accept the rewrite to file the rest."
}

# Creates one item and records what became of it before letting any failure through.
#
# The distinction between a refused creation and an unproven one is drawn here rather than at the
# call site, because it is the same distinction for every item and getting it wrong in one place
# would report an unknown creation as a definite failure - which is exactly the report that invites
# somebody to file the review a second time.
function Invoke-TrackerPublicationCreation {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Item,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $ResultPath
    )

    $findingId = [string] $Item['findingId']

    $created = $null
    try {
        $created = New-TrackerItemForTarget -Transport $Transport -Target $Target -State $State -Item $Item
    } catch {
        Set-TrackerPublicationOperationOutcome `
            -Result $Result `
            -Operation $script:TrackerCreateOperation `
            -FindingId $findingId `
            -Outcome (Get-PublicationFailureOutcome -Exception $_.Exception) `
            -Message $_.Exception.Message
        Write-TrackerPublicationResult -Result $Result -Path $ResultPath
        throw
    }

    Set-TrackerPublicationOperationOutcome `
        -Result $Result `
        -Operation $script:TrackerCreateOperation `
        -FindingId $findingId `
        -Outcome 'succeeded' `
        -Record $created
    Write-TrackerPublicationResult -Result $Result -Path $ResultPath

    $created
}

# Reads every created item back and confirms it holds what was sent.
#
# A read that fails is recorded as a failure rather than as an unknown outcome: a lost read response
# cannot mean an unseen item reached the tracker, and the creations it would be reporting on are
# already recorded individually.
#
# A rewrite the run cannot judge on its own pauses the verification the same way it pauses the
# creation loop: the journal records the pause and this hands back nothing, so the run neither reports
# the verification as done nor fails it.
function Invoke-TrackerPublicationVerification {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Item,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $ResultPath
    )

    try {
        $records = [System.Collections.Generic.List[object]]::new()
        foreach ($entry in $Item) {
            $operation = Find-TrackerPublicationOperation -Result $Result -FindingId ([string] $entry['findingId'])
            $acceptedRewrite = Get-JsonProperty -Node $operation -Name 'acceptedRewrite'
            $check = Test-TrackerItemStored `
                -Transport $Transport -Target $Target -State $State -Item $entry `
                -Result $Result -ResultPath $ResultPath -AcceptedRewrite $acceptedRewrite
            if ([bool] $check['paused']) { return $null }

            $record = $check['record']
            $records.Add([ordered]@{
                    findingId         = [string] $entry['findingId']
                    itemId            = [string] $record['itemId']
                    url               = [string] $record['url']
                    title             = [string] $record['title']
                    descriptionFormat = [string] $record['descriptionFormat']
                })
        }

        Set-TrackerPublicationOperationOutcome `
            -Result $Result `
            -Operation $script:TrackerVerifyOperation `
            -FindingId '' `
            -Outcome 'succeeded'

        return , $records.ToArray()
    } catch {
        Set-TrackerPublicationOperationOutcome `
            -Result $Result `
            -Operation $script:TrackerVerifyOperation `
            -FindingId '' `
            -Outcome 'failed' `
            -Message $_.Exception.Message
        Write-TrackerPublicationResult -Result $Result -Path $ResultPath
        throw
    }
}

# --- Body verification -------------------------------------------------------------------------

# Judges whether one filed item holds what the review approved (docs/design.md, "Tracker
# publication"; docs/implementation.md, "Body verification").
#
# The title is compared exactly on every platform and the body is compared first with the exact
# representation sent to that platform. The approved text is what a user read and accepted, while an
# adapter may apply a representation-only transformation needed to preserve that text at its
# destination. What differs between the platforms is what a body that did not survive that exact
# representation is taken to mean.
#
# GitHub stores a body verbatim, so any difference is a body nobody approved and is rejected. Azure
# DevOps stores a description through a Markdown pipeline that may reformat it - line endings and HTML
# entities among the changes it is entitled to make - and requires literal angle brackets to be
# encoded before publication. A body that differs is therefore put through a staged judgement: an
# equivalent body after that canonicalization is a match; a body a person already accepted is a match
# while the tracker still holds the exact bytes they accepted; a body that diverges too far to be a
# reformatting is rejected; and anything in between is left for a person to accept or reject.
#
# The verdict is 'match', 'reject' or 'adjudicate'. A rejection carries the reason to fail the run
# with; an adjudication carries the record a person reads to decide.
function Get-TrackerItemStoredVerdict {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Item,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Record,

        [Parameter(Mandatory)]
        [string] $Platform,

        # The rewrite an earlier run recorded as accepted for this item, when there is one.
        [System.Collections.IDictionary] $AcceptedRewrite
    )

    $findingId = [string] $Item['findingId']
    $itemId = [string] $Record['itemId']

    $sentTitle = [string] $Item['title']
    $storedTitle = [string] $Record['title']
    if (-not [string]::Equals($sentTitle, $storedTitle, [StringComparison]::Ordinal)) {
        return [ordered]@{ verdict = 'reject'; reason = (Get-TrackerItemMismatchReason -ItemId $itemId -FindingId $findingId -Label 'title'); deviation = $null }
    }

    # The stored format is judged before the body, because a body that survived the round trip
    # unchanged still renders as literal markup unless the format is Markdown.
    if ($Platform -ceq 'azureDevOps') {
        $format = [string] $Record['descriptionFormat']
        if (-not (Test-AdoWorkItemMarkdownFormat -Format $format)) {
            return [ordered]@{ verdict = 'reject'; reason = (Get-TrackerItemFormatReason -ItemId $itemId -FindingId $findingId -Format $format); deviation = $null }
        }
    }

    $approvedBody = [string] $Item['body']
    $sentBody = if ($Platform -ceq 'azureDevOps') {
        ConvertTo-AdoWorkItemMarkdownText -Text $approvedBody
    } else {
        $approvedBody
    }
    $storedBody = [string] $Record['body']

    if ([string]::Equals($sentBody, $storedBody, [StringComparison]::Ordinal)) {
        return [ordered]@{ verdict = 'match'; reason = $null; deviation = $null }
    }

    if ($Platform -cne 'azureDevOps') {
        return [ordered]@{ verdict = 'reject'; reason = (Get-TrackerItemMismatchReason -ItemId $itemId -FindingId $findingId -Label 'body'); deviation = $null }
    }

    # Comparing the normalized wire representation first preserves the distinction between an entity
    # already present in the approved Markdown and one the adapter introduced for an angle bracket.
    $sentNormalized = ConvertTo-TrackerCanonicalBody -Text $sentBody
    $storedNormalized = ConvertTo-TrackerCanonicalBody -Text $storedBody
    if ([string]::Equals($sentNormalized, $storedNormalized, [StringComparison]::Ordinal)) {
        return [ordered]@{ verdict = 'match'; reason = $null; deviation = $null }
    }

    # ADO may encode the complete approved body rather than only its brackets. Decode that stored
    # representation for comparison with the approved prose, but never accept raw angle brackets this
    # way: those are exactly the Markdown input that can be interpreted as HTML and disappear.
    $approvedNormalized = ConvertTo-TrackerCanonicalBody -Text $approvedBody
    $storedCanonical = ConvertTo-TrackerCanonicalBody -Text $storedBody -DecodeHtml
    if ($storedBody -notmatch '[<>]' -and [string]::Equals($approvedNormalized, $storedCanonical, [StringComparison]::Ordinal)) {
        return [ordered]@{ verdict = 'match'; reason = $null; deviation = $null }
    }

    # Judged after the exact and canonical matches, so a body since repaired is taken on its own
    # merit rather than held to the bytes a person accepted before it was repaired.
    if ($null -ne $AcceptedRewrite) {
        $acceptedSent = [string] (Get-JsonProperty -Node $AcceptedRewrite -Name 'expectedBodyHash')
        $acceptedStored = [string] (Get-JsonProperty -Node $AcceptedRewrite -Name 'storedBodyHash')
        if ((Get-TrackerBodyHash -Text $approvedBody) -ceq $acceptedSent -and (Get-TrackerBodyHash -Text $storedBody) -ceq $acceptedStored) {
            return [ordered]@{ verdict = 'match'; reason = $null; deviation = $null }
        }
        return [ordered]@{ verdict = 'reject'; reason = (Get-TrackerItemRewriteChangedReason -ItemId $itemId -FindingId $findingId); deviation = $null }
    }

    $deviation = Measure-TrackerBodyDeviation -Sent $approvedNormalized -Stored $storedCanonical
    if ([bool] $deviation['decided'] -and [double] $deviation['similarity'] -lt $script:TrackerBodyAdjudicationThreshold) {
        return [ordered]@{ verdict = 'reject'; reason = (Get-TrackerItemLowSimilarityReason -ItemId $itemId -FindingId $findingId -Similarity ([double] $deviation['similarity'])); deviation = $null }
    }

    [ordered]@{ verdict = 'adjudicate'; reason = $null; deviation = (New-TrackerBodyAdjudication -Item $Item -Record $Record -Deviation $deviation) }
}

# Names a stored value that is not the one the run sent.
function Get-TrackerItemMismatchReason {
    param(
        [Parameter(Mandatory)][string] $ItemId,
        [Parameter(Mandatory)][string] $FindingId,
        [Parameter(Mandatory)][string] $Label
    )

    "Item $ItemId, filed for finding '$FindingId', holds a $Label the run did not send. The tracker stored text other than what the review approved, so what it holds cannot be reported as the approved finding."
}

# Names a stored description whose format would render the finding as literal markup.
function Get-TrackerItemFormatReason {
    param(
        [Parameter(Mandatory)][string] $ItemId,
        [Parameter(Mandatory)][string] $FindingId,
        [AllowEmptyString()][AllowNull()][string] $Format
    )

    $reported = if ([string]::IsNullOrWhiteSpace($Format)) { 'no format at all' } else { "the format '$Format'" }
    "Work item $ItemId, filed for finding '$FindingId', reports $reported for its description rather than Markdown, so the finding would render as literal markup. Azure DevOps does not convert a description in place, so delete that work item, confirm the organization stores work item descriptions as Markdown, and publish again."
}

# Names a stored body that diverged too far from the approved text to be a reformatting of it.
function Get-TrackerItemLowSimilarityReason {
    param(
        [Parameter(Mandatory)][string] $ItemId,
        [Parameter(Mandatory)][string] $FindingId,
        [Parameter(Mandatory)][double] $Similarity
    )

    $percent = [Math]::Round($Similarity * 100)
    "Work item $ItemId, filed for finding '$FindingId', holds a description about $percent% similar to the approved text, too far apart to be a reformatting of it. The tracker stored something other than the approved finding, so delete that work item, confirm the organization stores work item descriptions as Markdown, and publish again."
}

# Names a stored body that no longer matches the rewrite a person accepted.
function Get-TrackerItemRewriteChangedReason {
    param(
        [Parameter(Mandatory)][string] $ItemId,
        [Parameter(Mandatory)][string] $FindingId
    )

    "Work item $ItemId, filed for finding '$FindingId', no longer holds the body the accepted rewrite was recorded against, so the description changed after it was accepted. Read the work item afresh before continuing: delete it and publish again, or adjudicate the current body rather than the one that was accepted."
}

# Normalizes a body for the canonical comparison and for a legible excerpt.
#
# Line endings are normalized to line feeds because a Markdown pipeline is entitled to store an ending
# other than the one it was sent. The HTML decode is applied only where the caller asks for it, so the
# comparison can decode the stored side without decoding the sent side. Decoding precedes the ending
# normalization so an encoded ending the decode reveals (such as &#13;&#10;) is normalized as well.
function ConvertTo-TrackerCanonicalBody {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Text,

        [switch] $DecodeHtml
    )

    $normalized = $Text
    if ($DecodeHtml) { $normalized = [System.Net.WebUtility]::HtmlDecode($normalized) }
    $normalized = $normalized -replace "`r`n", "`n" -replace "`r", "`n"
    $normalized
}

# Hashes a body so an accepted rewrite can be bound to the exact bytes it was accepted against.
#
# The raw body is hashed rather than the canonical one, because acceptance is a person's statement
# about the exact text the tracker holds, and the rendering that produced the approved text is
# deterministic (docs/design.md, "Tracker publication").
function Get-TrackerBodyHash {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Text
    )

    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
    [Convert]::ToHexString([System.Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
}

# Hashes the whole approved plan so a continuation cannot resume against a different one.
#
# The finding, title and body of every item are folded in, in order, so a continuation that rebuilt a
# different set of findings - or the same findings in a different order, or with a body since edited -
# produces a different hash and is refused before it files anything the paused run did not intend.
#
# Each field is prefixed with its length rather than joined by a separator, so no content a field may
# hold - including control characters - can imitate a field boundary and let a different plan produce
# the same hash.
function Get-TrackerPublicationPlanHash {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Item
    )

    $builder = [System.Text.StringBuilder]::new()
    foreach ($entry in $Item) {
        foreach ($field in @([string] $entry['findingId'], [string] $entry['title'], [string] $entry['body'])) {
            [void] $builder.Append($field.Length)
            [void] $builder.Append(':')
            [void] $builder.Append($field)
        }
    }
    Get-TrackerBodyHash -Text $builder.ToString()
}

# Measures how far a stored body is from the approved one, once both are canonical.
#
# The shared head and tail are removed first so the edit distance is computed only over the region
# that actually differs, which is what makes the quadratic step cheap for the small, localized
# rewrites a Markdown pipeline produces. A length gap wider than the threshold allows settles the
# question without the table, because the distance cannot be smaller than that gap. A differing region
# whose table would be larger than the cap is left undecided, so it is adjudicated rather than decided
# from a distance the run declines to compute.
#
# 'decided' states whether 'similarity' is a number to compare against the threshold. 'firstDifference'
# is where the bodies begin to differ, so an excerpt can be centred on it.
function Measure-TrackerBodyDeviation {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Sent,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Stored
    )

    $maxLength = [Math]::Max($Sent.Length, $Stored.Length)
    if ($maxLength -eq 0) { return [ordered]@{ decided = $true; similarity = 1.0; distance = 0; firstDifference = 0 } }

    $shared = [Math]::Min($Sent.Length, $Stored.Length)
    $prefix = 0
    while ($prefix -lt $shared -and $Sent[$prefix] -ceq $Stored[$prefix]) { $prefix++ }
    $suffix = 0
    while ($suffix -lt ($shared - $prefix) -and $Sent[$Sent.Length - 1 - $suffix] -ceq $Stored[$Stored.Length - 1 - $suffix]) { $suffix++ }

    $sentMiddle = $Sent.Substring($prefix, $Sent.Length - $prefix - $suffix)
    $storedMiddle = $Stored.Substring($prefix, $Stored.Length - $prefix - $suffix)

    # The length gap is a lower bound on the edit distance, so 'similarityCeiling' is an upper bound on
    # the similarity. When even that ceiling falls below the threshold the gap alone settles the
    # question without the table, judged with the same comparison the caller applies so the two never
    # disagree at the boundary. A ceiling at or above the threshold leaves the gap unable to settle it,
    # so the distance is computed.
    $lengthGap = [Math]::Abs($sentMiddle.Length - $storedMiddle.Length)
    $similarityCeiling = 1 - ($lengthGap / $maxLength)
    if ($similarityCeiling -lt $script:TrackerBodyAdjudicationThreshold) {
        return [ordered]@{ decided = $true; similarity = $similarityCeiling; distance = $null; firstDifference = $prefix }
    }

    if ((($sentMiddle.Length + 1) * ($storedMiddle.Length + 1)) -gt $script:TrackerBodySimilarityCellCap) {
        return [ordered]@{ decided = $false; similarity = $null; distance = $null; firstDifference = $prefix }
    }

    $distance = Get-TrackerLevenshteinDistance -Left $sentMiddle -Right $storedMiddle
    [ordered]@{ decided = $true; similarity = (1 - ($distance / $maxLength)); distance = $distance; firstDifference = $prefix }
}

# The Levenshtein edit distance between two strings, compared ordinally.
function Get-TrackerLevenshteinDistance {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Left,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Right
    )

    $n = $Left.Length
    $m = $Right.Length
    if ($n -eq 0) { return $m }
    if ($m -eq 0) { return $n }

    $previous = [int[]]::new($m + 1)
    $current = [int[]]::new($m + 1)
    for ($j = 0; $j -le $m; $j++) { $previous[$j] = $j }

    for ($i = 1; $i -le $n; $i++) {
        $current[0] = $i
        $leftChar = $Left[$i - 1]
        for ($j = 1; $j -le $m; $j++) {
            $cost = if ($leftChar -ceq $Right[$j - 1]) { 0 } else { 1 }
            $deletion = $previous[$j] + 1
            $insertion = $current[$j - 1] + 1
            $substitution = $previous[$j - 1] + $cost
            $best = $deletion
            if ($insertion -lt $best) { $best = $insertion }
            if ($substitution -lt $best) { $best = $substitution }
            $current[$j] = $best
        }
        $swap = $previous
        $previous = $current
        $current = $swap
    }

    $previous[$m]
}

# Reproduces the neighbourhood of a difference so a person can see it without opening the tracker.
function Get-TrackerBodyExcerpt {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Text,

        [Parameter(Mandatory)]
        [int] $At
    )

    $from = [Math]::Max(0, $At - $script:TrackerBodyExcerptContext)
    $to = [Math]::Min($Text.Length, $At + $script:TrackerBodyExcerptContext)
    $slice = $Text.Substring($from, $to - $from)
    if ($slice.Length -gt $script:TrackerBodyExcerptMaxLength) {
        $slice = $slice.Substring(0, $script:TrackerBodyExcerptMaxLength) + [char] 0x2026
    }
    $lead = if ($from -gt 0) { [char] 0x2026 } else { '' }
    $trail = if ($to -lt $Text.Length) { [char] 0x2026 } else { '' }
    "$lead$slice$trail"
}

# Builds the record a person reads to accept or reject a rewrite.
#
# The hashes are of the raw bodies, so the acceptance a person gives is bound to the exact bytes the
# tracker holds and the exact bytes the review approved. The excerpts are canonical, so the difference
# reads as the reformatting it is rather than as the encoded markup the tracker literally stores.
function New-TrackerBodyAdjudication {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Item,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Record,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Deviation
    )

    $sentBody = [string] $Item['body']
    $storedBody = [string] $Record['body']
    $sentCanonical = ConvertTo-TrackerCanonicalBody -Text $sentBody
    $storedCanonical = ConvertTo-TrackerCanonicalBody -Text $storedBody -DecodeHtml
    $at = [int] $Deviation['firstDifference']
    $similarity = if ($null -eq $Deviation['similarity']) { $null } else { [Math]::Round([double] $Deviation['similarity'], 4) }

    [ordered]@{
        findingId        = [string] $Item['findingId']
        itemId           = [string] $Record['itemId']
        title            = [string] $Item['title']
        similarity       = $similarity
        expectedBodyHash = Get-TrackerBodyHash -Text $sentBody
        storedBodyHash   = Get-TrackerBodyHash -Text $storedBody
        sentExcerpt      = Get-TrackerBodyExcerpt -Text $sentCanonical -At $at
        storedExcerpt    = Get-TrackerBodyExcerpt -Text $storedCanonical -At $at
    }
}

# --- Journal -------------------------------------------------------------------------------------

# Builds the record of what the run intends to do, with every creation still pending.
#
# The whole sequence is written down before anything is attempted, so a journal found after a crash
# states not only what was filed but what was going to be. The plan hash binds the whole approved set,
# so a continuation that rebuilt a different one is refused rather than filed against the wrong review.
function New-TrackerPublicationResult {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Item,

        [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
    )

    $operations = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in $Item) {
        $operations.Add((New-TrackerPublicationOperation `
                    -Operation $script:TrackerCreateOperation `
                    -FindingId ([string] $entry['findingId']) `
                    -Title ([string] $entry['title'])))
    }
    $operations.Add((New-TrackerPublicationOperation -Operation $script:TrackerVerifyOperation -FindingId ''))

    [ordered]@{
        schemaVersion    = $script:TrackerPublicationResultSchemaVersion
        startedUtc       = $NowUtc.UtcDateTime.ToString('o', [cultureinfo]::InvariantCulture)
        platform         = [string] (Get-JsonProperty -Node $Target -Name 'platform')
        target           = $Target
        trackerUrl       = Get-JsonProperty -Node $Target -Name 'webUrl'
        workItemType     = Get-JsonProperty -Node $State -Name 'workItemType'
        areaPath         = Get-JsonProperty -Node $State -Name 'areaPath'
        descriptionField = Get-JsonProperty -Node $State -Name 'descriptionField'
        parentItemId     = Get-JsonProperty -Node (Get-JsonProperty -Node $State -Name 'parent') -Name 'itemId'
        planHash         = Get-TrackerPublicationPlanHash -Item $Item
        status           = 'inProgress'
        message          = $null
        operations       = @($operations.ToArray())
        verification     = $null
        adjudication     = $null
    }
}

# Builds one pending operation record.
function New-TrackerPublicationOperation {
    param(
        [Parameter(Mandatory)]
        [ValidateSet('createItem', 'verifyItems')]
        [string] $Operation,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $FindingId,

        [AllowEmptyString()]
        [string] $Title
    )

    [ordered]@{
        operation  = $Operation
        findingId  = if ([string]::IsNullOrEmpty($FindingId)) { $null } else { $FindingId }
        title      = if ([string]::IsNullOrEmpty($Title)) { $null } else { $Title }
        outcome    = 'pending'
        itemId     = $null
        itemNodeId = $null
        url        = $null
        message    = $null
    }
}

# Records what became of one operation.
function Set-TrackerPublicationOperationOutcome {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $Operation,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $FindingId,

        [Parameter(Mandatory)]
        [ValidateSet('pending', 'succeeded', 'failed', 'ambiguous')]
        [string] $Outcome,

        [System.Collections.IDictionary] $Record,

        [AllowEmptyString()]
        [string] $Message
    )

    $entry = Find-TrackerPublicationOperation -Result $Result -Operation $Operation -FindingId $FindingId
    Set-JsonProperty -Node $entry -Name 'outcome' -Value $Outcome

    if ($null -ne $Record) {
        foreach ($name in @('itemId', 'itemNodeId', 'url')) {
            Set-JsonProperty -Node $entry -Name $name -Value ([string] (Get-JsonProperty -Node $Record -Name $name))
        }
    }

    if (-not [string]::IsNullOrEmpty($Message)) { Set-JsonProperty -Node $entry -Name 'message' -Value $Message }
}

# Locates the record of one operation, refusing rather than reporting absence.
#
# Every caller reaching here has just performed or attempted the operation it is naming, so an absent
# record means the journal was built from a different set of items than the one being filed, and
# continuing would report the run against work it did not do.
function Find-TrackerPublicationOperation {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $FindingId,

        [string] $Operation = $script:TrackerCreateOperation
    )

    $wanted = if ([string]::IsNullOrEmpty($FindingId)) { $null } else { $FindingId }
    foreach ($entry in (Get-JsonArrayProperty -Node $Result -Name 'operations')) {
        $candidate = $entry -as [System.Collections.IDictionary]
        if ($null -eq $candidate) { continue }
        if ([string] (Get-JsonProperty -Node $candidate -Name 'operation') -cne $Operation) { continue }
        if ([string]::Equals([string] (Get-JsonProperty -Node $candidate -Name 'findingId'), [string] $wanted, [StringComparison]::Ordinal)) {
            return $candidate
        }
    }

    $subject = if ($null -eq $wanted) { "'$Operation'" } else { "'$Operation' for finding '$FindingId'" }
    throw "The tracker publication journal holds no record of $subject, so it was not built from the items being filed."
}

# Publishes the journal.
function Write-TrackerPublicationResult {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $Path
    )

    Write-PullRequestOutputFile -Path $Path -Content (ConvertTo-JsonText -Value $Result)
}

# Composes the path the journal is published under.
function Get-TrackerPublicationResultPath {
    param(
        [Parameter(Mandatory)]
        [string] $OutputDirectory
    )

    Resolve-PullRequestPath -Path (Join-Path $OutputDirectory $script:TrackerPublicationResultFileName)
}

# Summarizes the run for the caller that reads stdout.
#
# The counts come from the journal rather than from the item set, so what is reported is what the
# tracker accepted rather than what the run intended. The items are listed in full because a caller
# looking at a run that did not finish needs to know exactly what exists before deciding what to do
# about the rest.
#
# The adjudication is carried through when the run paused on one, because it holds the hashes a person
# needs in order to accept the rewrite and continue. The summary is withheld from a run that did not
# succeed: it is written against the whole approved set, so relaying it over a run that filed part of
# that set would tell the reader a review was delivered that in fact is not there. Both keys stay so
# the shape of the outcome does not depend on how the run ended.
function New-TrackerPublicationOutcome {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $ResultPath,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $SummaryBody
    )

    $failed = 0
    $ambiguous = 0
    $items = [System.Collections.Generic.List[object]]::new()

    foreach ($entry in (Get-JsonArrayProperty -Node $Result -Name 'operations')) {
        $outcome = [string] (Get-JsonProperty -Node $entry -Name 'outcome')
        if ($outcome -ceq 'failed') { $failed++ }
        if ($outcome -ceq 'ambiguous') { $ambiguous++ }
        if ([string] (Get-JsonProperty -Node $entry -Name 'operation') -cne $script:TrackerCreateOperation) { continue }
        if ($outcome -cne 'succeeded') { continue }

        $items.Add([ordered]@{
                findingId = [string] (Get-JsonProperty -Node $entry -Name 'findingId')
                itemId    = [string] (Get-JsonProperty -Node $entry -Name 'itemId')
                url       = [string] (Get-JsonProperty -Node $entry -Name 'url')
                title     = [string] (Get-JsonProperty -Node $entry -Name 'title')
            })
    }

    $status = [string] (Get-JsonProperty -Node $Result -Name 'status')

    [ordered]@{
        schemaVersion           = $script:TrackerPublicationResultSchemaVersion
        status                  = $status
        message                 = Get-JsonProperty -Node $Result -Name 'message'
        platform                = [string] (Get-JsonProperty -Node $Result -Name 'platform')
        trackerUrl              = Get-JsonProperty -Node $Result -Name 'trackerUrl'
        resultPath              = $ResultPath
        publishedItemCount      = $items.Count
        failedOperationCount    = $failed
        ambiguousOperationCount = $ambiguous
        summary                 = if ($status -ceq 'succeeded') { $SummaryBody } else { $null }
        adjudication            = Get-JsonProperty -Node $Result -Name 'adjudication'
        items                   = @($items.ToArray())
    }
}

# Refuses to end a run in success while the tracker holds less than the whole review.
#
# This is where a run's exit status is decided, for a run that failed as much as for one that did
# not. The failure travels here through the journal rather than as an exception, because the journal
# is the account of the run that survives it and reporting anything the journal contradicts would
# leave the caller with two answers. The outcome reaches standard output before this is reached, so
# the caller holds the full list of what exists whichever way this goes.
#
# A run that paused for a decision is reported apart from one that failed, because what it needs is a
# person's judgement on a rewrite rather than a repair, and the message names the command that
# resumes it.
function Assert-TrackerPublicationComplete {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result
    )

    $status = [string] (Get-JsonProperty -Node $Result -Name 'status')
    if ($status -ceq 'succeeded') { return }

    if ($status -ceq $script:TrackerNeedsAdjudicationStatus) {
        throw (Get-TrackerAdjudicationRefusal -Adjudication (Get-JsonProperty -Node $Result -Name 'adjudication'))
    }

    $message = [string] (Get-JsonProperty -Node $Result -Name 'message')
    if ([string]::IsNullOrWhiteSpace($message)) {
        $message = "The tracker publication ended in state '$status'. Read the journal for the account of what it filed."
    }

    throw $message
}

# Describes a run paused on a rewrite and names the command that accepts it.
function Get-TrackerAdjudicationRefusal {
    param(
        [AllowNull()]
        [System.Collections.IDictionary] $Adjudication
    )

    if ($null -eq $Adjudication) {
        return 'The tracker publication paused for a decision on a rewritten body, but the journal records no account of it. Read the journal before continuing.'
    }

    $itemId = [string] (Get-JsonProperty -Node $Adjudication -Name 'itemId')
    $findingId = [string] (Get-JsonProperty -Node $Adjudication -Name 'findingId')
    $expected = [string] (Get-JsonProperty -Node $Adjudication -Name 'expectedBodyHash')
    $stored = [string] (Get-JsonProperty -Node $Adjudication -Name 'storedBodyHash')

    "The tracker publication paused on a body Azure DevOps rewrote for work item $itemId, filed for finding '$findingId'. Read the stored body against the approved text - the outcome carries an excerpt of each - then either delete that work item and move the journal aside to file the review afresh, or, if the rewrite only reformats the approved text, continue with -AcceptStoredRewrite -AcceptWorkItemId $itemId -AcceptExpectedBodyHash $expected -AcceptStoredBodyHash $stored to file the rest without recreating it."
}

# --- Rerun guard ---------------------------------------------------------------------------------

# Refuses to file a review into a tracker that a journal says already holds one.
#
# This is stricter than the guard publication onto a pull request applies, and deliberately so.
# There, the whole review is one draft and a single unfinished operation is one uncertainty within a
# transaction. Here every creation stands alone and is visible the instant it is accepted, so a
# journal recording one is a journal recording items that exist. Filing again would not resume that
# run; it would file the review a second time, and a tracker holding two copies of a review is worse
# than one holding half of it.
#
# A run that recorded its own end and whose creations were all refused is no obstacle: a refusal is
# the service stating the item was not created, and the creations after it were never attempted. A
# journal still reading as in progress is an obstacle whatever it records, because the run that wrote
# it did not live long enough to say what became of the creation it was making.
#
# A journal naming a different tracker is judged by exactly the same rule as one naming this tracker.
# The next run writes over this path whatever it holds, so passing a journal that records items in
# another tracker would destroy the only account of them, and a later run back to that tracker would
# then find nothing and file the review there a second time. The tracker the journal names is
# reported, so a reader knows where to look.
#
# There is deliberately no switch that skips this. Moving the journal aside is the confirmation,
# because it cannot be done without holding the record in hand.
function Assert-TrackerPublicationRecord {
    param(
        [Parameter(Mandatory)]
        [string] $OutputDirectory,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $path = Get-TrackerPublicationResultPath -OutputDirectory $OutputDirectory
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return }

    $document = $null
    try {
        $document = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $path)
    } catch {
        throw (Get-TrackerPublicationRecordRefusal -Path $path -Reason "it cannot be parsed: $($_.Exception.Message)")
    }

    if ($document -isnot [System.Collections.IDictionary]) {
        throw (Get-TrackerPublicationRecordRefusal -Path $path -Reason 'it is not a JSON object')
    }

    $version = Get-JsonProperty -Node $document -Name 'schemaVersion'
    if ($version -ne $script:TrackerPublicationResultSchemaVersion) {
        $reported = if ($null -eq $version) { 'no version' } else { "version $version" }
        throw (Get-TrackerPublicationRecordRefusal -Path $path -Reason "it reports $reported, which this helper does not read")
    }

    $recordedTarget = Get-JsonProperty -Node $document -Name 'target'
    if ($recordedTarget -isnot [System.Collections.IDictionary]) {
        throw (Get-TrackerPublicationRecordRefusal -Path $path -Reason 'it names no tracker')
    }

    # A journal paused for a decision on this tracker is not reconciled by moving it aside; it is
    # continued. The refusal names the continuation rather than the delete path, so a person does not
    # discard a run that is one decision away from finishing. A journal paused over another tracker
    # falls through to the account below, which refuses it for recording items that tracker holds.
    $status = [string] (Get-JsonProperty -Node $document -Name 'status')
    $paused = $status -ceq $script:TrackerNeedsAdjudicationStatus -or $status -ceq $script:TrackerResumeReadyStatus
    if ($paused -and (Test-TrackerTargetsMatch -Left $Target -Right $recordedTarget)) {
        throw (Get-TrackerPublicationContinuationRefusal -Path $path -Document $document)
    }

    $inProgress = $status -ceq 'inProgress'
    $created = 0
    $unknown = 0
    foreach ($entry in (Get-JsonArrayProperty -Node $document -Name 'operations')) {
        if ([string] (Get-JsonProperty -Node $entry -Name 'operation') -cne $script:TrackerCreateOperation) { continue }
        $outcome = [string] (Get-JsonProperty -Node $entry -Name 'outcome')
        if ($outcome -ceq 'succeeded') { $created++ }
        if ($outcome -ceq 'ambiguous') { $unknown++ }
        if ($outcome -ceq 'pending' -and $inProgress) { $unknown++ }
    }

    if ($created -eq 0 -and $unknown -eq 0) { return }

    $createdText = if ($created -eq 1) { '1 item' } else { "$created items" }
    $unknownText = if ($unknown -eq 1) { '1 creation whose outcome it could not determine' } else { "$unknown creations whose outcome it could not determine" }
    $reason = "it records $createdText filed and $unknownText"
    if (-not (Test-TrackerTargetsMatch -Left $Target -Right $recordedTarget)) {
        $reason = "$reason in $(Get-TrackerWebUrl -Target $recordedTarget), which is not the tracker this run was given"
    }

    throw (Get-TrackerPublicationRecordRefusal -Path $path -Reason $reason)
}

# Describes a journal that cannot be shown to have filed nothing.
function Get-TrackerPublicationRecordRefusal {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Reason
    )

    "An earlier run left a tracker publication journal at '$Path' and $Reason. Filing again would not resume that run; it would file the review into the tracker a second time. Read the journal against the tracker, then move the file elsewhere and run again. No switch skips this check: moving the journal aside is how a person states that they have reconciled it against the tracker."
}

# Describes a journal paused for a decision and names the command that resumes it.
#
# A paused run has stopped one decision short of finishing, so the refusal points at the continuation
# rather than at moving the journal aside. It reads the item and the hashes out of whichever record
# the pause left: the pending adjudication while the run still awaits a decision, or the accepted
# rewrite once one has been recorded.
function Get-TrackerPublicationContinuationRefusal {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document
    )

    $itemId = ''
    $expected = ''
    $stored = ''

    $adjudication = Get-JsonProperty -Node $Document -Name 'adjudication'
    if ($adjudication -is [System.Collections.IDictionary]) {
        $itemId = [string] (Get-JsonProperty -Node $adjudication -Name 'itemId')
        $expected = [string] (Get-JsonProperty -Node $adjudication -Name 'expectedBodyHash')
        $stored = [string] (Get-JsonProperty -Node $adjudication -Name 'storedBodyHash')
    } else {
        foreach ($entry in (Get-JsonArrayProperty -Node $Document -Name 'operations')) {
            $accepted = Get-JsonProperty -Node $entry -Name 'acceptedRewrite'
            if ($accepted -isnot [System.Collections.IDictionary]) { continue }
            $itemId = [string] (Get-JsonProperty -Node $entry -Name 'itemId')
            $expected = [string] (Get-JsonProperty -Node $accepted -Name 'expectedBodyHash')
            $stored = [string] (Get-JsonProperty -Node $accepted -Name 'storedBodyHash')
            break
        }
    }

    "An earlier run left a tracker publication journal at '$Path' and it paused for a decision on a body Azure DevOps rewrote for work item $itemId. Filing afresh would file the review a second time. Read the stored body against the approved text, then either delete that work item and move the journal aside to start again, or continue with -AcceptStoredRewrite -AcceptWorkItemId $itemId -AcceptExpectedBodyHash $expected -AcceptStoredBodyHash $stored to file the rest without recreating it."
}

# --- Continuation --------------------------------------------------------------------------------

# Reads the journal a paused run left and confirms it is the one this continuation is accepting.
#
# A continuation is the one path allowed to write over a journal that records filed items, so it is
# guarded at least as strictly as the rerun that is refused. The journal must be paused for a decision
# on this tracker, and the item and both body hashes the caller states must be the ones the journal
# paused on - the pending adjudication, or the accepted rewrite once one has been recorded, so that
# re-running the same acceptance after a crash is honoured rather than refused. A journal that clears
# every check is returned for the run to resume from; anything else is refused with the reason it
# cannot be continued.
function Read-TrackerContinuationRecord {
    param(
        [Parameter(Mandatory)]
        [string] $OutputDirectory,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [string] $AcceptWorkItemId,

        [Parameter(Mandatory)]
        [string] $AcceptExpectedBodyHash,

        [Parameter(Mandatory)]
        [string] $AcceptStoredBodyHash
    )

    $path = Get-TrackerPublicationResultPath -OutputDirectory $OutputDirectory
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw (Get-TrackerContinuationRefusal -Reason "there is no tracker publication journal at '$path', so there is no paused run to continue")
    }

    $document = $null
    try {
        $document = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $path)
    } catch {
        throw (Get-TrackerContinuationRefusal -Reason "the journal at '$path' cannot be parsed: $($_.Exception.Message)")
    }

    if ($document -isnot [System.Collections.IDictionary]) {
        throw (Get-TrackerContinuationRefusal -Reason "the journal at '$path' is not a JSON object")
    }

    $version = Get-JsonProperty -Node $document -Name 'schemaVersion'
    if ($version -ne $script:TrackerPublicationResultSchemaVersion) {
        $reported = if ($null -eq $version) { 'no version' } else { "version $version" }
        throw (Get-TrackerContinuationRefusal -Reason "the journal at '$path' reports $reported, which this helper does not read")
    }

    $recordedTarget = Get-JsonProperty -Node $document -Name 'target'
    if ($recordedTarget -isnot [System.Collections.IDictionary] -or -not (Test-TrackerTargetsMatch -Left $Target -Right $recordedTarget)) {
        throw (Get-TrackerContinuationRefusal -Reason "the journal at '$path' does not name the tracker this run was given")
    }

    $status = [string] (Get-JsonProperty -Node $document -Name 'status')
    if ($status -cne $script:TrackerNeedsAdjudicationStatus -and $status -cne $script:TrackerResumeReadyStatus) {
        throw (Get-TrackerContinuationRefusal -Reason "the journal at '$path' is not paused for a decision; it reads as '$status', so there is nothing to accept")
    }

    if ($status -ceq $script:TrackerNeedsAdjudicationStatus) {
        $adjudication = Get-JsonProperty -Node $document -Name 'adjudication'
        if ($adjudication -isnot [System.Collections.IDictionary]) {
            throw (Get-TrackerContinuationRefusal -Reason "the journal at '$path' is paused for a decision but records no account of the rewrite to accept")
        }
        Assert-TrackerContinuationBinding `
            -Source $adjudication `
            -AcceptWorkItemId $AcceptWorkItemId `
            -AcceptExpectedBodyHash $AcceptExpectedBodyHash `
            -AcceptStoredBodyHash $AcceptStoredBodyHash
    } else {
        $accepted = $null
        foreach ($entry in (Get-JsonArrayProperty -Node $document -Name 'operations')) {
            if ([string] (Get-JsonProperty -Node $entry -Name 'itemId') -cne $AcceptWorkItemId) { continue }
            $accepted = Get-JsonProperty -Node $entry -Name 'acceptedRewrite'
            break
        }
        if ($accepted -isnot [System.Collections.IDictionary]) {
            throw (Get-TrackerContinuationRefusal -Reason "the journal at '$path' records no accepted rewrite for work item $AcceptWorkItemId")
        }
        Assert-TrackerContinuationBinding `
            -Source $accepted `
            -AcceptWorkItemId $AcceptWorkItemId `
            -AcceptExpectedBodyHash $AcceptExpectedBodyHash `
            -AcceptStoredBodyHash $AcceptStoredBodyHash `
            -ItemId $AcceptWorkItemId
    }

    $document
}

# Confirms the acceptance names the item and the exact bodies the journal paused on.
function Assert-TrackerContinuationBinding {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Source,

        [Parameter(Mandatory)]
        [string] $AcceptWorkItemId,

        [Parameter(Mandatory)]
        [string] $AcceptExpectedBodyHash,

        [Parameter(Mandatory)]
        [string] $AcceptStoredBodyHash,

        # The adjudication carries the item id; the accepted rewrite does not, so the caller supplies
        # it after matching the operation.
        [string] $ItemId
    )

    $recordedItemId = if ($PSBoundParameters.ContainsKey('ItemId')) { $ItemId } else { [string] (Get-JsonProperty -Node $Source -Name 'itemId') }
    $expected = [string] (Get-JsonProperty -Node $Source -Name 'expectedBodyHash')
    $stored = [string] (Get-JsonProperty -Node $Source -Name 'storedBodyHash')

    if ($recordedItemId -cne $AcceptWorkItemId -or $expected -cne $AcceptExpectedBodyHash -or $stored -cne $AcceptStoredBodyHash) {
        throw (Get-TrackerContinuationRefusal -Reason 'the item and body hashes given do not match the rewrite the run paused on, so the decision names a different rewrite than the recorded one')
    }
}

# Records that a person accepted the rewrite the run paused on, leaving the journal ready to resume.
#
# The acceptance is bound to the item by both its expected and stored body hashes, so the resumed run
# treats the item as approved only while the tracker still holds the exact body that was accepted. The
# journal is moved to its resumable state and the pending adjudication cleared, so the acceptance is
# durable and re-running it is idempotent.
function Set-TrackerPublicationAcceptedRewrite {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $ItemId,

        [Parameter(Mandatory)]
        [string] $ExpectedBodyHash,

        [Parameter(Mandatory)]
        [string] $StoredBodyHash
    )

    $operation = $null
    foreach ($entry in (Get-JsonArrayProperty -Node $Result -Name 'operations')) {
        $candidate = $entry -as [System.Collections.IDictionary]
        if ($null -eq $candidate) { continue }
        if ([string] (Get-JsonProperty -Node $candidate -Name 'operation') -cne $script:TrackerCreateOperation) { continue }
        if ([string] (Get-JsonProperty -Node $candidate -Name 'itemId') -ceq $ItemId) { $operation = $candidate; break }
    }

    if ($null -eq $operation) {
        throw "The tracker publication journal records no filed work item $ItemId to accept a rewrite for."
    }

    Set-JsonProperty -Node $operation -Name 'acceptedRewrite' -Value ([ordered]@{
            expectedBodyHash = $ExpectedBodyHash
            storedBodyHash   = $StoredBodyHash
        })
    Set-JsonProperty -Node $Result -Name 'adjudication' -Value $null
    Set-JsonProperty -Node $Result -Name 'status' -Value $script:TrackerResumeReadyStatus
}

# Confirms the continuation resumes the same review, filed the same way, that the paused run recorded.
#
# The plan hash binds the whole approved set, so a continuation whose findings, titles or bodies
# differ from the paused run's is refused before it files anything. The work item coordinates are
# bound as well, because they decide how the items still pending would be created and a continuation
# that filed them under a different type or area would file a mixed review.
function Assert-TrackerContinuationPlan {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Item,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State
    )

    $planHash = Get-TrackerPublicationPlanHash -Item $Item
    if ($planHash -cne [string] (Get-JsonProperty -Node $Result -Name 'planHash')) {
        throw (Get-TrackerContinuationRefusal -Reason 'the approved findings differ from the ones the paused run recorded, so continuing would file a different review than the one that was paused')
    }

    $bindings = @(
        @{ name = 'workItemType'; recorded = (Get-JsonProperty -Node $Result -Name 'workItemType'); current = (Get-JsonProperty -Node $State -Name 'workItemType') }
        @{ name = 'areaPath'; recorded = (Get-JsonProperty -Node $Result -Name 'areaPath'); current = (Get-JsonProperty -Node $State -Name 'areaPath') }
        @{ name = 'descriptionField'; recorded = (Get-JsonProperty -Node $Result -Name 'descriptionField'); current = (Get-JsonProperty -Node $State -Name 'descriptionField') }
        @{ name = 'parentItemId'; recorded = (Get-JsonProperty -Node $Result -Name 'parentItemId'); current = (Get-JsonProperty -Node (Get-JsonProperty -Node $State -Name 'parent') -Name 'itemId') }
    )
    foreach ($binding in $bindings) {
        if ([string] $binding['recorded'] -cne [string] $binding['current']) {
            throw (Get-TrackerContinuationRefusal -Reason "the $($binding['name']) differs from the one the paused run recorded, so continuing would file the rest of the review differently than it began")
        }
    }
}

# Describes why a continuation cannot proceed.
function Get-TrackerContinuationRefusal {
    param(
        [Parameter(Mandatory)]
        [string] $Reason
    )

    "This run was asked to accept a stored rewrite and continue, but $Reason. Nothing was filed."
}

# Confirms the acceptance arguments are given together, and only when a continuation is asked for.
#
# The three coordinates name exactly the rewrite being accepted, so they travel as a set: a
# continuation missing one names no rewrite, and any of them given without -AcceptStoredRewrite is a
# fresh run mistaking itself for a continuation.
function Assert-TrackerContinuationArgument {
    param(
        [switch] $AcceptStoredRewrite,

        [AllowEmptyString()]
        [string] $AcceptWorkItemId,

        [AllowEmptyString()]
        [string] $AcceptExpectedBodyHash,

        [AllowEmptyString()]
        [string] $AcceptStoredBodyHash
    )

    $anyGiven = -not (
        [string]::IsNullOrWhiteSpace($AcceptWorkItemId) -and
        [string]::IsNullOrWhiteSpace($AcceptExpectedBodyHash) -and
        [string]::IsNullOrWhiteSpace($AcceptStoredBodyHash))

    if ($AcceptStoredRewrite) {
        if ([string]::IsNullOrWhiteSpace($AcceptWorkItemId) -or [string]::IsNullOrWhiteSpace($AcceptExpectedBodyHash) -or [string]::IsNullOrWhiteSpace($AcceptStoredBodyHash)) {
            throw 'Continuing a paused run needs -AcceptWorkItemId, -AcceptExpectedBodyHash and -AcceptStoredBodyHash together with -AcceptStoredRewrite, so the acceptance names exactly the rewrite the run paused on.'
        }
        return
    }

    if ($anyGiven) {
        throw 'The acceptance arguments -AcceptWorkItemId, -AcceptExpectedBodyHash and -AcceptStoredBodyHash apply only when continuing a paused run with -AcceptStoredRewrite.'
    }
}
