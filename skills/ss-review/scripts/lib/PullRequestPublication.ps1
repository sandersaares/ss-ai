#Requires -Version 7.0

# Publication of an approved review onto a pull request (docs/design.md, "Review publication";
# docs/implementation.md, "Publication").
#
# Role in the overall design: this library owns everything about publication that is the same on
# both services - what will be written, in what order, what the run records about each write, and
# what it checks afterwards - and dispatches the writes themselves to the adapter that serves the
# target's platform. It issues no request of its own and renders finding bodies through the shared
# renderer, so publication-specific refusals can be exercised without a service.
#
# The adapter interface is expressed by Start-Review, Add-ReviewFinding, Confirm-ReviewFinding,
# Complete-Review and Confirm-Review, and both platforms implement that contract even though their
# shapes differ. GitHub assembles a draft review, adds every thread to it, verifies the draft while
# it is still invisible and makes the whole thing visible in one submit; Azure DevOps publishes each
# thread as it goes, verifies those threads and posts the summary at the end. The interface
# accommodates both because Complete-Review is defined as "make the review final, carrying the
# summary": on GitHub that is the submit, on Azure DevOps it is the summary comment.
#
#   Start-Review      begins a review and returns the adapter state every later call is given
#   Add-ReviewFinding adds one rendered finding as a new line thread, a new file thread or a reply
#   Confirm-ReviewFinding reads back the findings before completion
#   Complete-Review   finalizes the review, carrying the summary body
#   Confirm-Review    reads back what was published
#
# Further operations are dispatched the same way, because the sequence needs them and an executable
# that branched on platform to reach them would defeat the point of the interface: the live state
# read that decides whether publication may start at all, the file identity read that resolves
# however the platform addresses a file, the reopening of a thread a reply was posted into, and the
# discard of a review that never reached its completion.
#
# Publication is not transactional. Nothing here pretends otherwise: the result record is the
# recovery mechanism, and it is rewritten after every operation so that a run which dies partway
# leaves behind an exact account of what was written. The one thing it cannot state exactly is a
# write whose response was lost, and that case is recorded as its own outcome rather than guessed at
# in either direction. Because that record is the only account there is, a run refuses outright to
# publish over one that reports anything as published or unknown: reconciling a partial publication
# against the pull request is a person's job, and the run cannot do it for them.
#
# This library calls Get-JsonProperty, Get-JsonArrayProperty, Set-JsonProperty, Test-JsonProperty,
# ConvertFrom-JsonVerbatim and ConvertTo-JsonText from JsonValue.ps1, Resolve-PullRequestPath,
# Write-PullRequestOutputFile from PullRequestModel.ps1, Assert-FindingShape,
# Assert-FindingFollowUpTarget and ConvertTo-CanonicalFindingPath from FindingModel.ps1,
# Assert-ApprovedFindingContext from ReviewPublication.ps1,
# Resolve-PullRequestAnchorInDiff from PullRequestAnchor.ps1, Test-GitRepositoryPresent and
# Test-GitCommitPresent from GitDiff.ps1, Get-UnsupportedPullRequestPlatformMessage from
# PullRequestPlatform.ps1, Get-PublicationFailureOutcome from PublicationFailure.ps1, the
# GitHub publication adapter from GitHubPullRequestApi.ps1, the Azure DevOps publication adapter
# from AdoPullRequestApi.ps1 and the finding renderer from FindingMarkdown.ps1.
# Libraries never dot-source one another; the executable composition root loads every one of them.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The result document's format version, enforced by whoever reads a result back for the same reason
# every other artifact enforces its own: a document shaped differently would be read as a record of
# writes that did not happen.
$script:PullRequestPublicationResultSchemaVersion = 1

# The filename the result is published under. Stated once here because the executable writes it and
# the skill instructs its operator to read it after a failure, and a name each of them spelled out
# separately would drift.
$script:PullRequestPublicationResultFileName = 'publication-result.json'

# The operations a result records. startReview and completeReview describe the review as a whole;
# newLineThread, newFileThread and reply are the ways one finding reaches a pull request;
# verifyFindings is the read that confirms those findings before completion; reopenThread reopens a
# thread this run replied into.
$script:PullRequestPublicationOperations = @(
    'startReview'
    'newLineThread'
    'newFileThread'
    'reply'
    'verifyFindings'
    'completeReview'
    'reopenThread'
)

# The operations that publish one finding, which is the subset Add-ReviewFinding serves.
$script:PullRequestFindingOperations = @('newLineThread', 'newFileThread', 'reply')

# The operations whose successful or ambiguous execution can leave visible pull request state.
# startReview is deliberately absent: GitHub creates only an invisible draft and Azure DevOps issues
# no request for it. verifyFindings is likewise absent because it is a read: a failed read can prove
# the run unsafe to complete, but it cannot by itself have put anything on the pull request. An
# ambiguous startReview is still handled explicitly, because GitHub may have accepted the draft-open
# request before its response was lost.
$script:PullRequestPublishingOperations = @('newLineThread', 'newFileThread', 'reply', 'completeReview', 'reopenThread')

# What became of one operation. An operation is written into the record as pending before anything
# is attempted, so a record that stops mid-sequence states which operation was in flight.
#
# The terminal outcomes are not a severity ordering. A failed write did not happen; an ambiguous
# one may or may not have happened, and nobody reading the record afterwards - or the pull request
# itself, where a duplicate comment and a missing one look equally plausible - can tell which.
# Repeating an ambiguous write is what publishes a finding twice, so it is never repeated
# automatically.
$script:PullRequestPublicationOutcomes = @('pending', 'succeeded', 'failed', 'ambiguous')

# The terminal state a run that published and verified everything reports when a follow-up step it
# could not complete leaves a thread resolved that should have been reopened.
#
# It exists because such a run is neither 'succeeded' nor 'failed'. Every finding is on the pull
# request, the summary is posted and the service confirmed all of it; calling that failed describes
# a published review as an unpublished one, and the ordinary response to an unpublished review is to
# publish it, which posts the whole review a second time.
$script:PullRequestPublicationFollowUpFailureStatus = 'publishedWithFollowUpFailure'

# The coordinates that identify one pull request. Stated once because both the refusal that names a
# mismatching coordinate and the comparison that merely reports one have to agree on what identity
# consists of.
$script:PullRequestTargetCoordinates = @('platform', 'organization', 'project', 'repository')

# The thread state that needs no reopening. Both adapters normalize a thread's state into this same
# vocabulary, so the decision to reopen is taken once here rather than per platform.
$script:PullRequestActiveThreadResolution = 'active'

# Publishes a whole review through the adapter and keeps the result record current throughout.
#
# The record is rewritten after every operation and before the next one is attempted, so the file on
# disk is never behind what the pull request has received. The finally block is what makes that hold
# when a transport raises: the outcome is recorded on the way past, and the write happens whether the
# run continues or not.
#
# A run ends by succeeding, failing - which is raised, because a failure before the review is
# complete leaves an incomplete review - or publishing and verifying everything while leaving a
# follow-up reopening unfinished. An unfinished reopening is reported rather than raised because
# there is nothing left to publish.
#
# On GitHub a failure before the completion is nearly harmless, because nothing a draft holds is
# visible to anyone until the submit. That is why the draft is discarded here rather than left for a
# later run to trip over: an abandoned draft is invisible work that blocks the next attempt.
# Discarding is attempted for its own sake and never masks the failure that prompted it.
function Invoke-PullRequestPublication {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Plan,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $SummaryBody,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $ResultPath,

        # Records the reviewer's approval of the merge as part of completion, when the user approved it
        # during the audit (docs/design.md, "Review publication").
        [switch] $ApproveMerge
    )

    $review = $null
    $isComplete = $false

    try {
        $review = Invoke-PullRequestPublicationOperation `
            -Result $Result `
            -ResultPath $ResultPath `
            -Operation 'startReview' `
            -FindingId '' `
            -Action { param($Transport, $Target, $State) Start-PullRequestReviewForTarget -Transport $Transport -Target $Target -State $State } `
            -ArgumentList @($Transport, $Target, $State)

        Set-JsonProperty -Node $Result -Name 'reviewId' -Value (Get-JsonProperty -Node $review -Name 'reviewId')
        Set-PullRequestPublicationOperationOutcome `
            -Result $Result `
            -Operation 'startReview' `
            -FindingId '' `
            -Outcome 'succeeded'
        Write-PullRequestPublicationResult -Result $Result -Path $ResultPath

        foreach ($entry in $Plan) {
            $written = Invoke-PullRequestPublicationOperation `
                -Result $Result `
                -ResultPath $ResultPath `
                -Operation ([string] (Get-JsonProperty -Node $entry -Name 'operation')) `
                -FindingId ([string] (Get-JsonProperty -Node $entry -Name 'findingId')) `
                -Action { param($Transport, $Target, $Review, $Operation) Add-PullRequestReviewFindingForTarget -Transport $Transport -Target $Target -Review $Review -Operation $Operation } `
                -ArgumentList @($Transport, $Target, $review, $entry)

            Set-PullRequestPublicationOperationOutcome `
                -Result $Result `
                -Operation ([string] (Get-JsonProperty -Node $entry -Name 'operation')) `
                -FindingId ([string] (Get-JsonProperty -Node $entry -Name 'findingId')) `
                -Outcome 'succeeded' `
                -ThreadId ([string] (Get-JsonProperty -Node $written -Name 'threadId')) `
                -CommentId ([string] (Get-JsonProperty -Node $written -Name 'commentId'))
            Write-PullRequestPublicationResult -Result $Result -Path $ResultPath
        }

        Invoke-PullRequestPublicationOperation `
            -Result $Result `
            -ResultPath $ResultPath `
            -Operation 'verifyFindings' `
            -FindingId '' `
            -ReadOnly `
            -Action {
                param($Transport, $Target, $Review, $Result, $ResultPath, $Plan)
                $verification = Confirm-PullRequestReviewFindingsForTarget -Transport $Transport -Target $Target -Review $Review
                Set-JsonProperty -Node $Result -Name 'preCompletionVerification' -Value $verification
                Write-PullRequestPublicationResult -Result $Result -Path $ResultPath
                Assert-PullRequestPublicationFindingsVerified -Result $Result -Plan $Plan -Verification $verification
                $verification
            } `
            -ArgumentList @($Transport, $Target, $review, $Result, $ResultPath, $Plan) | Out-Null

        Set-PullRequestPublicationOperationOutcome `
            -Result $Result `
            -Operation 'verifyFindings' `
            -FindingId '' `
            -Outcome 'succeeded'
        Write-PullRequestPublicationResult -Result $Result -Path $ResultPath

        $completion = Invoke-PullRequestPublicationOperation `
            -Result $Result `
            -ResultPath $ResultPath `
            -Operation 'completeReview' `
            -FindingId '' `
            -Action { param($Transport, $Target, $Review, $SummaryBody, $ApproveMerge) Complete-PullRequestReviewForTarget -Transport $Transport -Target $Target -Review $Review -SummaryBody $SummaryBody -ApproveMerge:$ApproveMerge } `
            -ArgumentList @($Transport, $Target, $review, $SummaryBody, [bool] $ApproveMerge)

        $isComplete = $true
        Set-JsonProperty -Node $Result -Name 'reviewUrl' -Value (Get-JsonProperty -Node $completion -Name 'url')
        Set-PullRequestPublicationOperationOutcome `
            -Result $Result `
            -Operation 'completeReview' `
            -FindingId '' `
            -Outcome 'succeeded'
        Write-PullRequestPublicationResult -Result $Result -Path $ResultPath

        $verification = Confirm-PullRequestReviewForTarget -Transport $Transport -Target $Target -Review $review
        Set-JsonProperty -Node $Result -Name 'verification' -Value $verification
        Write-PullRequestPublicationResult -Result $Result -Path $ResultPath
        Assert-PullRequestPublicationVerified -Result $Result -Plan $Plan -Verification $verification -SummaryBody $SummaryBody

        # Reopening happens only now, after the review is visible. A thread reopened before the
        # reply that explains it would confront its participants with a reopened thread and no
        # reason attached, and a run that then failed would leave it that way.
        $followUpFailure = @(Resume-PullRequestPublicationFollowUp `
                -Transport $Transport `
                -Target $Target `
                -Plan $Plan `
                -Result $Result `
                -ResultPath $ResultPath)

        # Every finding the run was given is on the pull request by now, the summary is posted and
        # the service was asked to confirm all of it, so a thread that could not be reopened is
        # reported as its own terminal state rather than as a failure of the publication. A
        # verification failure is not treated this way and keeps 'failed': a run that cannot confirm
        # what it published leaves the state of the pull request genuinely unknown.
        if ($followUpFailure.Count -gt 0) {
            Set-JsonProperty -Node $Result -Name 'status' -Value $script:PullRequestPublicationFollowUpFailureStatus
            Set-JsonProperty -Node $Result -Name 'message' -Value (Get-PullRequestPublicationFollowUpFailureMessage -Failure $followUpFailure)
        } else {
            Set-JsonProperty -Node $Result -Name 'status' -Value 'succeeded'
        }
    } catch {
        Set-JsonProperty -Node $Result -Name 'status' -Value 'failed'
        Set-JsonProperty -Node $Result -Name 'message' -Value $_.Exception.Message
        throw
    } finally {
        if ($null -ne $review -and -not $isComplete) {
            Remove-PullRequestPublicationDraft -Transport $Transport -Target $Target -Review $review -Result $Result
        }
        Write-PullRequestPublicationResult -Result $Result -Path $ResultPath
    }

    $Result
}

# Runs one dispatched operation, recording what became of it before letting the failure through.
#
# The recording happens here rather than at each call site because the distinction that matters -
# refused versus unknown - is the same for every write, and a call site that got it wrong would
# report an unknown outcome as a definite failure, which is precisely the report that invites
# somebody to run the publication again. Read-only operations are recorded as failed when they
# raise, because a lost read response cannot mean an unseen write reached the pull request.
function Invoke-PullRequestPublicationOperation {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $ResultPath,

        [Parameter(Mandatory)]
        [string] $Operation,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $FindingId,

        [Parameter(Mandatory)]
        [scriptblock] $Action,

        # Passed to the action positionally. The values travel as arguments rather than as captured
        # variables, so the action's behaviour does not depend on what the scope it runs in happens
        # to hold.
        [AllowEmptyCollection()]
        [object[]] $ArgumentList = @(),

        [switch] $ReadOnly
    )

    try {
        return & $Action @ArgumentList
    } catch {
        $outcome = if ($ReadOnly) { 'failed' } else { Get-PublicationFailureOutcome -Exception $_.Exception }
        Set-PullRequestPublicationOperationOutcome `
            -Result $Result `
            -Operation $Operation `
            -FindingId $FindingId `
            -Outcome $outcome `
            -Message $_.Exception.Message
        Write-PullRequestPublicationResult -Result $Result -Path $ResultPath
        throw
    }
}

# Reopens every thread a successful reply landed in, and reports the ones it could not reopen.
#
# A thread is reopened only where the capture recorded it as anything other than active, so a run
# never touches the state of a thread that was already open.
#
# A reopening that fails does not stop the ones behind it, and none of them is raised. This runs
# only after the review is published and verified, so each reopening stands alone and there is
# nothing left to abandon: stopping at the first failure would leave threads resolved that no later
# step is going to reopen, and raising would report a published review as a failed one.
function Resume-PullRequestPublicationFollowUp {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Plan,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $ResultPath
    )

    $failures = [System.Collections.Generic.List[object]]::new()

    foreach ($entry in $Plan) {
        if (-not [bool] (Get-JsonProperty -Node $entry -Name 'requiresReopen' -Default $false)) { continue }

        $findingId = [string] (Get-JsonProperty -Node $entry -Name 'findingId')
        $threadId = [string] (Get-JsonProperty -Node $entry -Name 'threadId')

        # The thread is named in the record before the attempt, so a failure states which thread
        # stayed resolved rather than only which finding wanted it reopened. The reader of the
        # record is the person who has to reopen it by hand.
        Set-PullRequestPublicationOperationOutcome `
            -Result $Result `
            -Operation 'reopenThread' `
            -FindingId $findingId `
            -Outcome 'pending' `
            -ThreadId $threadId

        try {
            Invoke-PullRequestPublicationOperation `
                -Result $Result `
                -ResultPath $ResultPath `
                -Operation 'reopenThread' `
                -FindingId $findingId `
                -Action { param($Transport, $Target, $ThreadId) Resume-PullRequestFollowUpThreadForTarget -Transport $Transport -Target $Target -ThreadId $ThreadId } `
                -ArgumentList @($Transport, $Target, $threadId) | Out-Null
        } catch {
            $failures.Add([ordered]@{
                    findingId = $findingId
                    threadId  = $threadId
                    message   = $_.Exception.Message
                })
            continue
        }

        Set-PullRequestPublicationOperationOutcome `
            -Result $Result `
            -Operation 'reopenThread' `
            -FindingId $findingId `
            -Outcome 'succeeded' `
            -ThreadId $threadId
        Write-PullRequestPublicationResult -Result $Result -Path $ResultPath
    }

    # The failures are emitted as individual items rather than as one array, because the caller
    # collects them with the array subexpression operator and an empty array emitted whole would
    # arrive there as a single element.
    $failures.ToArray()
}

# Describes the threads a run could not reopen, in the terms the reader has to act in.
#
# The message states what is left to do and what must not be done, because a reader who saw only
# that something failed would weigh publishing the review again, and the review is already there.
function Get-PullRequestPublicationFollowUpFailureMessage {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Failure
    )

    $rendered = [System.Collections.Generic.List[string]]::new()
    foreach ($item in $Failure) {
        $threadId = [string] (Get-JsonProperty -Node $item -Name 'threadId')
        $findingId = [string] (Get-JsonProperty -Node $item -Name 'findingId')
        $message = [string] (Get-JsonProperty -Node $item -Name 'message')
        $rendered.Add("thread '$threadId', answering finding '$findingId' ($message)")
    }

    "The review is published and the service confirmed it, but these threads a reply joined could not be reopened and are still resolved: $($rendered -join '; '). Reopen them by hand. The review itself needs nothing further and must not be published again."
}

# Discards a review that never reached its completion, and reports the attempt in the record.
#
# A discard that fails is recorded and swallowed. It runs while a failure is already on its way out,
# and replacing that failure with the failure of the cleanup would hide the cause behind its
# consequence. A platform that publishes as it writes has nothing to discard and says so, which
# reaches the record through the same path: the run is told that what was written stands.
function Remove-PullRequestPublicationDraft {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result
    )

    try {
        Remove-PullRequestPendingReviewForTarget `
            -Transport $Transport `
            -Target $Target `
            -ReviewId ([string] (Get-JsonProperty -Node $Review -Name 'reviewId')) | Out-Null
        Set-JsonProperty -Node $Result -Name 'draftDiscarded' -Value $true
    } catch {
        Set-JsonProperty -Node $Result -Name 'draftDiscarded' -Value $false
        Set-JsonProperty -Node $Result -Name 'draftDiscardMessage' -Value $_.Exception.Message
    }
}

# --- Adapter interface -------------------------------------------------------------------------

# Reads the state that decides whether publication may start.
#
# This is a read rather than part of Start-Review because everything it reports has to be acted on
# before the first write: a pull request that is no longer accepting comments, a head that has moved
# past what was reviewed, or a draft review already in progress each abort the run, and aborting
# after a review has been begun would leave that review behind.
function Get-PullRequestPublicationStateForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    switch ($Target['platform']) {
        'gitHub' { return Get-GitHubPullRequestPublicationState -Transport $Transport -Target $Target }
        'azureDevOps' { return Get-AdoPullRequestPublicationState -Transport $Transport -Target $Target }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Reports how the service identifies each file the review will comment on.
#
# Azure DevOps addresses a line comment by a per-iteration change tracking identifier rather than by
# path, so it has to ask the service for one before it can write. GitHub addresses a file by path
# and needs nothing, and answers with an empty map rather than with a special case at the call site.
# This is the only service-side read of what changed that publication performs, and it supplies
# identity alone: the hunks a finding is judged against come from the local repository.
#
# An adapter that requires an identity refuses here when it cannot produce one for every file it was
# asked about. This runs before the plan is built and before the first write, so such a refusal
# leaves the pull request untouched, which is the whole reason the read is a separate step rather
# than something each write does for itself.
function Get-PullRequestFileIdentityForTarget {
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

    switch ($Target['platform']) {
        'gitHub' { return Get-GitHubPullRequestFileIdentity -Path $Path }
        'azureDevOps' { return Get-AdoPullRequestFileIdentity -Transport $Transport -Target $Target -State $State -Path $Path }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Begins a review and returns the state every later call in the run is given.
function Start-PullRequestReviewForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State
    )

    switch ($Target['platform']) {
        'gitHub' { return Start-GitHubPullRequestReview -Transport $Transport -State $State }
        'azureDevOps' { return Start-AdoPullRequestReview -Transport $Transport -Target $Target -State $State }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Adds one rendered finding to the review under way.
function Add-PullRequestReviewFindingForTarget {
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

    switch ($Target['platform']) {
        'gitHub' { return Add-GitHubPullRequestReviewFinding -Transport $Transport -Review $Review -Operation $Operation }
        'azureDevOps' { return Add-AdoPullRequestReviewFinding -Transport $Transport -Target $Target -Review $Review -Operation $Operation }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Finalizes the review, carrying the summary.
function Complete-PullRequestReviewForTarget {
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

    switch ($Target['platform']) {
        'gitHub' { return Complete-GitHubPullRequestReview -Transport $Transport -Review $Review -SummaryBody $SummaryBody -ApproveMerge:$ApproveMerge }
        'azureDevOps' { return Complete-AdoPullRequestReview -Transport $Transport -Target $Target -Review $Review -SummaryBody $SummaryBody -ApproveMerge:$ApproveMerge }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Reads back the findings before the review is completed.
function Confirm-PullRequestReviewFindingsForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review
    )

    switch ($Target['platform']) {
        'gitHub' { return Confirm-GitHubPullRequestReviewFindings -Transport $Transport -Review $Review }
        'azureDevOps' { return Confirm-AdoPullRequestReviewFindings -Transport $Transport -Target $Target -Review $Review }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Reads back what the review published.
function Confirm-PullRequestReviewForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Review
    )

    switch ($Target['platform']) {
        'gitHub' { return Confirm-GitHubPullRequestReview -Transport $Transport -Review $Review }
        'azureDevOps' { return Confirm-AdoPullRequestReview -Transport $Transport -Target $Target -Review $Review }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Reopens a thread a reply was posted into.
function Resume-PullRequestFollowUpThreadForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [string] $ThreadId
    )

    switch ($Target['platform']) {
        'gitHub' { return Resume-GitHubReviewThread -Transport $Transport -ThreadId $ThreadId }
        'azureDevOps' { return Resume-AdoReviewThread -Transport $Transport -Target $Target -ThreadId $ThreadId }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Discards a review that has not been made visible.
function Remove-PullRequestPendingReviewForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [string] $ReviewId
    )

    switch ($Target['platform']) {
        'gitHub' { return Remove-GitHubPendingReview -Transport $Transport -ReviewId $ReviewId }
        'azureDevOps' { return Remove-AdoPullRequestPendingReview -ReviewId $ReviewId }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Renders the approved finding set into the bodies the publication plan carries.
#
# The rendering pass runs before service state is changed, because a finding that cannot become a
# comment body is a local refusal and must not be discovered after the pull request has received any
# part of the review. The returned map is keyed by finding identifier so the plan builder can attach
# each body without rendering the same finding again.
function New-PullRequestFindingBody {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding
    )

    $bodies = [ordered]@{}
    foreach ($item in $Finding) {
        $findingId = [string] (Get-JsonProperty -Node $item -Name 'id')
        try {
            $bodies[$findingId] = ConvertTo-FindingMarkdownText -Finding $item
        } catch {
            throw "Finding '$findingId' cannot be rendered for publication: $($_.Exception.Message)"
        }
    }

    $bodies
}

# --- Publication plan --------------------------------------------------------------------------

# Settles everything publication needs to know and clears the way for the first write.
#
# The order here is the substance of this function, and it is chosen so that every refusal a run is
# capable of happens while the pull request is still untouched. The anchors are resolved from the
# local diff first, because a finding anchored to a file the reviewed diff does not change is
# refused and that verdict costs nothing but the diff already in hand. The live state and the file
# identities are read next; both are reads and neither leaves a trace. The plan is assembled from
# them, which is the last thing that can refuse. Only then is a draft review discarded.
#
# Discarding is placed last because it is the only step here that writes, and it is irrevocable: a
# run that discarded the draft and then refused over something it could have judged beforehand would
# have destroyed work nobody can recover in order to publish nothing.
#
# Returns the live state the writes are performed against and the sequence of writes to perform.
function New-PullRequestPublicationPlanForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Approved,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Context,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Body,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Diff,

        [switch] $DiscardPendingReview,

        [switch] $PublishToClosedPullRequest
    )

    $anchored = Resolve-PullRequestFindingAnchor -Finding $Finding -Diff $Diff

    $state = Get-PullRequestPublicationStateForTarget -Transport $Transport -Target $Target
    Assert-PullRequestPublicationState -State $state -Approved $Approved -AllowClosed:$PublishToClosedPullRequest

    $fileIdentity = Get-PullRequestFileIdentityForTarget `
        -Transport $Transport `
        -Target $Target `
        -State $state `
        -Path (Get-PullRequestPublicationAnchoredPath -AnchoredFinding $anchored)

    $plan = New-PullRequestPublicationPlan `
        -AnchoredFinding $anchored `
        -Body $Body `
        -Context $Context `
        -FileIdentity $fileIdentity

    # Final read before any write, including discarding an existing draft. A refusal requires a new
    # capture and semantic revalidation, not silently substituting this live evidence.
    $liveContext = Get-PullRequestContextForTarget -Transport $Transport -Target $Target
    Assert-PullRequestPublicationEvidence -Approved $Approved -Context $Context -LiveContext $liveContext

    $state = Clear-PullRequestPendingReview `
        -Transport $Transport `
        -Target $Target `
        -State $state `
        -DiscardPendingReview:$DiscardPendingReview

    [ordered]@{
        state = $state
        plan  = @($plan)
    }
}

# Commits alone cannot detect discussion arriving during the human wait or publication preparation.
# Ref: docs/design.md, "Freshness check".
function Assert-PullRequestPublicationEvidence {
    param(
        [Parameter(Mandatory)][Collections.IDictionary] $Approved,
        [Parameter(Mandatory)][Collections.IDictionary] $Context,
        [Parameter(Mandatory)][Collections.IDictionary] $LiveContext
    )
    Assert-ApprovedFindingContext -Approved $Approved -Context $Context
    $expected = Get-JsonProperty -Node $Approved -Name 'reviewedContextHash'
    # Platform adapters return the source model, not the canonical document. Use a fixed, unused
    # capture instant because evidence comparison deliberately excludes wall-clock time.
    $live = New-PullRequestContextDocument -Context $LiveContext -NowUtc ([DateTimeOffset]::UnixEpoch)
    if ($expected -cne (Get-PullRequestEvidenceHash -Document $live)) {
        throw 'Pull request evidence changed immediately before publication. Refresh and revalidate before publishing.'
    }
}

# Builds the ordered list of writes a review will perform.
#
# The order is fixed so that two runs over the same approved set produce the same record and the
# same sequence of comments. New threads come first and replies last, because the reopening that
# follows publication concerns replies alone: keeping them together at the end puts each reply as
# close as the sequence allows to the moment its thread is reopened.
#
# A reply carries no anchor. Its position is the thread it answers, and recording the anchor the
# finding happens to name would claim a position the published comment does not have.
function New-PullRequestPublicationPlan {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $AnchoredFinding,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Body,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Context,

        [AllowNull()]
        [System.Collections.IDictionary] $FileIdentity
    )

    $threads = [System.Collections.Generic.List[object]]::new()
    $replies = [System.Collections.Generic.List[object]]::new()

    foreach ($anchored in $AnchoredFinding) {
        $finding = Get-JsonProperty -Node $anchored -Name 'finding'
        $findingId = [string] (Get-JsonProperty -Node $anchored -Name 'findingId')

        if (-not $Body.Contains($findingId)) {
            throw "Finding '$findingId' has no rendered body, so there is nothing to publish for it."
        }
        $text = [string] $Body[$findingId]
        if ([string]::IsNullOrWhiteSpace($text)) {
            throw "Finding '$findingId' rendered to no text, so publishing it would post an empty comment."
        }

        $followUp = Get-JsonProperty -Node $finding -Name 'followUp'
        if ($null -ne $followUp) {
            $threadId = [string] (Get-JsonProperty -Node $followUp -Name 'threadId')
            $replies.Add([ordered]@{
                    operation        = 'reply'
                    findingId        = $findingId
                    body             = $text
                    anchor           = $null
                    wasDowngraded    = $false
                    threadId         = $threadId
                    commentId        = [string] (Get-JsonProperty -Node $followUp -Name 'commentId')
                    requiresReopen   = Test-PullRequestThreadNeedsReopen -Context $Context -ThreadId $threadId
                    changeTrackingId = $null
                })
            continue
        }

        $anchor = Get-JsonProperty -Node $anchored -Name 'anchor'
        $kind = [string] (Get-JsonProperty -Node $anchor -Name 'kind')
        $path = [string] (Get-JsonProperty -Node $anchor -Name 'path')

        # An absent identity is recorded as absent rather than refused. A service that addresses a
        # file by path supplies none for any file, so a file without one is ordinary here; a service
        # that requires one refuses in its own file identity read, before this builder is reached.
        # Telling the two cases apart would require this builder to know which service it is
        # planning for, which is the knowledge the adapter interface exists to keep out of it.
        $identity = $null
        if ($null -ne $FileIdentity -and $FileIdentity.Contains($path)) {
            $identity = $FileIdentity[$path]
        }

        $threads.Add([ordered]@{
                operation        = if ($kind -eq 'line') { 'newLineThread' } else { 'newFileThread' }
                findingId        = $findingId
                body             = $text
                anchor           = $anchor
                wasDowngraded    = [bool] (Get-JsonProperty -Node $anchor -Name 'wasDowngraded' -Default $false)
                threadId         = $null
                commentId        = $null
                requiresReopen   = $false
                changeTrackingId = $identity
            })
    }

    , @(@($threads.ToArray()) + @($replies.ToArray()))
}

# Resolves the anchor of every finding that needs a position of its own.
#
# A follow-up is deliberately left unresolved. It is published into a thread that already exists, so
# the diff has nothing to say about where it goes, and failing the run because the file such a
# finding happens to name is no longer in the diff would refuse to answer a question that is still
# open.
function Resolve-PullRequestFindingAnchor {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Diff
    )

    $anchored = [System.Collections.Generic.List[object]]::new()
    foreach ($item in $Finding) {
        $findingId = [string] (Get-JsonProperty -Node $item -Name 'id')

        $anchor = $null
        if ($null -eq (Get-JsonProperty -Node $item -Name 'followUp')) {
            try {
                $anchor = Resolve-PullRequestAnchorInDiff -Anchor (Get-JsonProperty -Node $item -Name 'anchor') -Diff $Diff
            } catch {
                throw "Finding '$findingId' cannot be anchored: $($_.Exception.Message)"
            }
        }

        $anchored.Add([ordered]@{
                findingId = $findingId
                finding   = $item
                anchor    = $anchor
            })
    }

    , $anchored.ToArray()
}

# Lists the distinct files the plan will comment on.
#
# Azure DevOps addresses a line comment by the change tracking identifier of the file rather than by
# its path, so the run has to ask the service which identifier each anchored file carries. The list
# is produced here so that the question is asked about exactly the files that will be written to.
function Get-PullRequestPublicationAnchoredPath {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $AnchoredFinding
    )

    $paths = [System.Collections.Generic.List[string]]::new()
    foreach ($anchored in $AnchoredFinding) {
        $anchor = Get-JsonProperty -Node $anchored -Name 'anchor'
        if ($null -eq $anchor) { continue }
        $path = [string] (Get-JsonProperty -Node $anchor -Name 'path')
        if ($paths -notcontains $path) { $paths.Add($path) }
    }

    , $paths.ToArray()
}

# Reports whether the captured discussion shows a thread as anything other than open.
function Test-PullRequestThreadNeedsReopen {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Context,

        [Parameter(Mandatory)]
        [string] $ThreadId
    )

    $threads = Get-JsonArrayProperty -Node $Context -Name 'threads'
    foreach ($thread in $threads) {
        # Ordinal, because a GitHub node identifier is an opaque case-sensitive string and two
        # identifiers differing only in case belong to two different threads.
        if (-not [string]::Equals([string] (Get-JsonProperty -Node $thread -Name 'id'), $ThreadId, [StringComparison]::Ordinal)) { continue }
        return [string] (Get-JsonProperty -Node $thread -Name 'resolution') -ne $script:PullRequestActiveThreadResolution
    }

    $false
}

# --- Result record -----------------------------------------------------------------------------

# Builds the record of what the run intends to do, with every operation still pending.
#
# The whole sequence is written down before anything is attempted, so a record found after a crash
# states not only what was written but what was going to be, and the operation that was in flight is
# the first one still pending.
function New-PullRequestPublicationResult {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Plan,

        [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
    )

    $operations = [System.Collections.Generic.List[object]]::new()
    $operations.Add((New-PullRequestPublicationOperation -Operation 'startReview' -FindingId ''))

    foreach ($entry in $Plan) {
        $operations.Add((New-PullRequestPublicationOperation `
                    -Operation ([string] (Get-JsonProperty -Node $entry -Name 'operation')) `
                    -FindingId ([string] (Get-JsonProperty -Node $entry -Name 'findingId')) `
                    -Anchor (Get-JsonProperty -Node $entry -Name 'anchor') `
                    -WasDowngraded ([bool] (Get-JsonProperty -Node $entry -Name 'wasDowngraded' -Default $false))))
    }

    $operations.Add((New-PullRequestPublicationOperation -Operation 'verifyFindings' -FindingId ''))
    $operations.Add((New-PullRequestPublicationOperation -Operation 'completeReview' -FindingId ''))

    foreach ($entry in $Plan) {
        if (-not [bool] (Get-JsonProperty -Node $entry -Name 'requiresReopen' -Default $false)) { continue }
        $operations.Add((New-PullRequestPublicationOperation `
                    -Operation 'reopenThread' `
                    -FindingId ([string] (Get-JsonProperty -Node $entry -Name 'findingId'))))
    }

    [ordered]@{
        schemaVersion       = $script:PullRequestPublicationResultSchemaVersion
        startedUtc          = $NowUtc.UtcDateTime.ToString('o', [cultureinfo]::InvariantCulture)
        platform            = [string] (Get-JsonProperty -Node $Target -Name 'platform')
        target              = $Target
        pullRequestUrl      = Get-JsonProperty -Node $Target -Name 'webUrl'
        sourceCommit        = Get-JsonProperty -Node $State -Name 'sourceCommit'
        mergeBaseCommit     = Get-JsonProperty -Node $State -Name 'mergeBaseCommit'
        status              = 'inProgress'
        reviewId            = $null
        reviewUrl           = $null
        draftDiscarded      = $false
        draftDiscardMessage = $null
        message             = $null
        operations          = @($operations.ToArray())
        preCompletionVerification = $null
        verification        = $null
    }
}

# Builds one pending operation record.
function New-PullRequestPublicationOperation {
    param(
        [Parameter(Mandatory)]
        [ValidateSet('startReview', 'newLineThread', 'newFileThread', 'reply', 'verifyFindings', 'completeReview', 'reopenThread')]
        [string] $Operation,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $FindingId,

        [AllowNull()]
        $Anchor,

        [bool] $WasDowngraded = $false
    )

    [ordered]@{
        operation     = $Operation
        findingId     = if ([string]::IsNullOrEmpty($FindingId)) { $null } else { $FindingId }
        outcome       = 'pending'
        anchor        = $Anchor
        wasDowngraded = $WasDowngraded
        threadId      = $null
        commentId     = $null
        message       = $null
    }
}

# Records what became of one operation.
function Set-PullRequestPublicationOperationOutcome {
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

        [AllowEmptyString()]
        [string] $ThreadId,

        [AllowEmptyString()]
        [string] $CommentId,

        [AllowEmptyString()]
        [string] $Message
    )

    $entry = Find-PullRequestPublicationOperation -Result $Result -Operation $Operation -FindingId $FindingId
    Set-JsonProperty -Node $entry -Name 'outcome' -Value $Outcome
    if (-not [string]::IsNullOrEmpty($ThreadId)) { Set-JsonProperty -Node $entry -Name 'threadId' -Value $ThreadId }
    if (-not [string]::IsNullOrEmpty($CommentId)) { Set-JsonProperty -Node $entry -Name 'commentId' -Value $CommentId }
    if (-not [string]::IsNullOrEmpty($Message)) { Set-JsonProperty -Node $entry -Name 'message' -Value $Message }
}

# Locates the record of one operation, refusing rather than reporting absence.
#
# Every caller reaching here has just performed or attempted the operation it is naming, so an
# absent record means the record was built from a different plan than the one being published, and
# continuing would report the run against a sequence it did not perform.
function Find-PullRequestPublicationOperation {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $Operation,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $FindingId
    )

    $wanted = if ([string]::IsNullOrEmpty($FindingId)) { $null } else { $FindingId }
    $operations = Get-JsonArrayProperty -Node $Result -Name 'operations'
    foreach ($entry in $operations) {
        $candidate = $entry -as [System.Collections.IDictionary]
        if ($null -eq $candidate) { continue }
        if ([string] (Get-JsonProperty -Node $candidate -Name 'operation') -cne $Operation) { continue }
        $recorded = Get-JsonProperty -Node $candidate -Name 'findingId'
        if ([string]::Equals([string] $recorded, [string] $wanted, [StringComparison]::Ordinal)) {
            return $candidate
        }
    }

    $subject = if ($null -eq $wanted) { "'$Operation'" } else { "'$Operation' for finding '$FindingId'" }
    throw "The publication result holds no record of $subject, so it was not built from the plan being published."
}

# Publishes the record.
function Write-PullRequestPublicationResult {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $Path
    )

    Write-PullRequestOutputFile -Path $Path -Content (ConvertTo-JsonText -Value $Result)
}

# Composes the path the record is published under.
function Get-PullRequestPublicationResultPath {
    param(
        [Parameter(Mandatory)]
        [string] $OutputDirectory
    )

    Resolve-PullRequestPath -Path (Join-Path $OutputDirectory $script:PullRequestPublicationResultFileName)
}

# Summarizes the run for the caller that reads stdout.
#
# The counts are derived from the record rather than from the plan, so what is reported is what the
# service accepted rather than what the run intended. The ambiguous count is reported separately
# from the failed one because the two demand different handling: a failed write can be retried and
# an ambiguous one has to be looked at. The message is carried through so that a status the caller
# does not recognize still arrives with the account of what produced it.
function New-PullRequestPublicationOutcome {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [string] $ResultPath
    )

    $published = 0
    $failed = 0
    $ambiguous = 0
    $downgraded = 0

    $operations = Get-JsonArrayProperty -Node $Result -Name 'operations'
    foreach ($entry in $operations) {
        $operation = [string] (Get-JsonProperty -Node $entry -Name 'operation')
        $outcome = [string] (Get-JsonProperty -Node $entry -Name 'outcome')
        if ($outcome -ceq 'failed') { $failed++ }
        if ($outcome -ceq 'ambiguous') { $ambiguous++ }
        if ($script:PullRequestFindingOperations -cnotcontains $operation) { continue }
        if ($outcome -ceq 'succeeded') { $published++ }
        if ([bool] (Get-JsonProperty -Node $entry -Name 'wasDowngraded' -Default $false)) { $downgraded++ }
    }

    [ordered]@{
        schemaVersion         = $script:PullRequestPublicationResultSchemaVersion
        status                = [string] (Get-JsonProperty -Node $Result -Name 'status')
        message               = Get-JsonProperty -Node $Result -Name 'message'
        platform              = [string] (Get-JsonProperty -Node $Result -Name 'platform')
        pullRequestUrl        = Get-JsonProperty -Node $Result -Name 'pullRequestUrl'
        reviewId              = Get-JsonProperty -Node $Result -Name 'reviewId'
        reviewUrl             = Get-JsonProperty -Node $Result -Name 'reviewUrl'
        resultPath            = $ResultPath
        publishedFindingCount = $published
        failedOperationCount  = $failed
        ambiguousOperationCount = $ambiguous
        downgradedAnchorCount = $downgraded
    }
}

# Refuses to end a run in success while the pull request still needs somebody's attention.
#
# The outcome reaches standard output before this is reached, so the caller has the full account
# either way; what this decides is the exit status. A run whose follow-up could not be completed did
# leave a thread resolved that a person has to reopen, and ending in success would file that under a
# result nobody reads twice.
function Assert-PullRequestPublicationComplete {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result
    )

    $status = [string] (Get-JsonProperty -Node $Result -Name 'status')
    if ($status -ceq 'succeeded') { return }

    $message = [string] (Get-JsonProperty -Node $Result -Name 'message')
    if ([string]::IsNullOrWhiteSpace($message)) {
        $message = "The publication ended in state '$status'. Read the publication result for the account of what it wrote."
    }

    throw $message
}

# --- Preflight ---------------------------------------------------------------------------------

# Refuses an approved set that was not produced against a pull request.
#
# Approval is a statement about a specific revision of a specific pull request, so publishing onto
# one requires the artifact to name both. A review of content that is no pull request produces an
# artifact naming neither, which is publishable as tracker items and is refused here.
function Assert-PullRequestApprovedArtifact {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Approved,

        [Parameter(Mandatory)]
        [string] $Path
    )

    if ((Get-JsonProperty -Node $Approved -Name 'target') -isnot [System.Collections.IDictionary]) {
        throw "The approved finding artifact at '$Path' names no pull request, so it cannot be published onto one. File the findings where work is tracked with Publish-ReviewTrackerItems.ps1."
    }

    foreach ($name in @('reviewedSourceCommit', 'reviewedMergeBaseCommit', 'reviewedContextHash')) {
        if ([string]::IsNullOrWhiteSpace([string] (Get-JsonProperty -Node $Approved -Name $name))) {
            throw "The approved finding artifact at '$Path' reports no '$name', so there is nothing to check the pull request against."
        }
    }
}

# Refuses to publish over the record of a run that already published something.
#
# The record is the only account of what an earlier run put on the pull request, and a new run
# overwrites it before it attempts a single write. Reading it first is what stops a retry from
# destroying the evidence it would need, and on a service that publishes each thread as it is
# created it is also what stops the whole review from being posted a second time.
#
# There is deliberately no switch that skips this. The situation it guards is one where a human has
# to compare a partial publication against the pull request and decide what is still wanted, and a
# switch is something a caller passes without making that comparison. Moving the file elsewhere is
# the confirmation, because it cannot be done without holding the record in hand.
#
# A record that states nothing was published is no obstacle and is overwritten in silence. A record
# that cannot be read is treated as an obstacle, because a document that cannot be interpreted
# cannot be shown to hold nothing.
function Assert-PullRequestPublicationRecord {
    param(
        [Parameter(Mandatory)]
        [string] $OutputDirectory,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $path = Get-PullRequestPublicationResultPath -OutputDirectory $OutputDirectory
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return }

    $document = $null
    try {
        $document = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $path)
    } catch {
        throw (Get-PullRequestPublicationRecordRefusal -Path $path -Reason "it cannot be parsed: $($_.Exception.Message)")
    }

    if ($document -isnot [System.Collections.IDictionary]) {
        throw (Get-PullRequestPublicationRecordRefusal -Path $path -Reason 'it is not a JSON object')
    }

    $version = Get-JsonProperty -Node $document -Name 'schemaVersion'
    if ($version -ne $script:PullRequestPublicationResultSchemaVersion) {
        $reported = if ($null -eq $version) { 'no version' } else { "version $version" }
        throw (Get-PullRequestPublicationRecordRefusal -Path $path -Reason "it reports $reported, which this helper does not read")
    }

    $recordedTarget = Get-JsonProperty -Node $document -Name 'target'
    if ($recordedTarget -isnot [System.Collections.IDictionary]) {
        throw (Get-PullRequestPublicationRecordRefusal -Path $path -Reason 'it names no pull request')
    }

    # A record of a different pull request says nothing about this one, so it is treated as the
    # leftover it is.
    if (-not (Test-PullRequestTargetsMatch -Left $Target -Right $recordedTarget)) { return }

    $published = 0
    $unknown = 0
    $settled = 0
    $completed = $false
    foreach ($entry in (Get-JsonArrayProperty -Node $document -Name 'operations')) {
        $operation = [string] (Get-JsonProperty -Node $entry -Name 'operation')
        $outcome = [string] (Get-JsonProperty -Node $entry -Name 'outcome')
        $publishesToPullRequest = $script:PullRequestPublishingOperations -ccontains $operation
        $ambiguousObstacle = $outcome -ceq 'ambiguous' -and ($publishesToPullRequest -or $operation -ceq 'startReview')
        if ($ambiguousObstacle) {
            $unknown++
            $settled++
        } elseif ($outcome -ceq 'succeeded' -and $publishesToPullRequest) {
            $settled++
            if ($operation -ceq 'completeReview') { $completed = $true }
        }
        if ($script:PullRequestFindingOperations -cnotcontains $operation) { continue }
        if ($outcome -ceq 'succeeded') { $published++ }
    }

    if ($settled -eq 0) { return }

    $publishedText = if ($published -eq 1) { '1 finding' } else { "$published findings" }
    $completionText = if ($completed) { 'review completion recorded' } else { 'review completion not recorded' }
    $unknownText = if ($unknown -eq 1) { '1 operation whose outcome it could not determine' } else { "$unknown operations whose outcome it could not determine" }
    throw "An earlier run left a publication record for this pull request at '$path': $publishedText published, $completionText and $unknownText. The pull request state cannot be proven safe to overwrite from this file. Read the record against the pull request, then move the file elsewhere and run again. No switch skips this check: moving the record aside is how a person states that they have reconciled it against the pull request."
}

# Describes a record that cannot be shown to hold nothing.
function Get-PullRequestPublicationRecordRefusal {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Reason
    )

    "The publication result at '$Path' is left by an earlier run and $Reason, so it cannot be shown to record nothing as published. Read it against the pull request, then move the file elsewhere and run again. No switch skips this check: moving the record aside is how a person states that they have reconciled it against the pull request."
}

# Reports whether two targets name one pull request.
#
# The comparison ignores case for the same reason the refusal that names a mismatch does: both
# services treat organization, project and repository names as case-insensitive.
function Test-PullRequestTargetsMatch {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Left,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Right
    )

    foreach ($name in $script:PullRequestTargetCoordinates) {
        $leftValue = [string] (Get-JsonProperty -Node $Left -Name $name)
        $rightValue = [string] (Get-JsonProperty -Node $Right -Name $name)
        if (-not [string]::Equals($leftValue, $rightValue, [StringComparison]::OrdinalIgnoreCase)) { return $false }
    }

    [int] (Get-JsonProperty -Node $Left -Name 'pullRequestId' -Default 0) -eq
    [int] (Get-JsonProperty -Node $Right -Name 'pullRequestId' -Default 0)
}

# Confirms that every follow-up answers a thread the platform will accept a reply into.
function Assert-PullRequestFollowUpTarget {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Context
    )

    foreach ($item in $Finding) {
        $id = [string] (Get-JsonProperty -Node $item -Name 'id')
        try {
            Assert-FindingFollowUpTarget -Context $Context -Finding $item
        } catch {
            throw "Finding '$id' cannot be published as a follow-up: $($_.Exception.Message)"
        }
    }
}

# Confirms that the URL, the approved artifact and the capture all name one pull request.
#
# A mismatch is refused rather than resolved in favour of any one of them. The findings were
# anchored against one pull request's diff and their follow-ups name threads captured from another,
# so publishing the set would post new comments on one pull request while replying into threads that
# belong to a different one - a cross-wiring that produces perfectly well-formed comments in the
# wrong place.
function Assert-PullRequestPublicationTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Approved,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Context
    )

    $approvedTarget = Get-JsonProperty -Node $Approved -Name 'target'
    $contextTarget = Get-JsonProperty -Node $Context -Name 'target'

    Assert-PullRequestTargetsMatch -Left $Target -Right $approvedTarget -RightName 'the approved finding artifact'
    Assert-PullRequestTargetsMatch -Left $Target -Right $contextTarget -RightName 'the captured discussion'
}

# Compares two targets by the coordinates that identify a pull request.
#
# The comparison ignores case, because both services treat organization, project and repository
# names as case-insensitive and a target assembled from a URL carries whatever casing the URL used.
function Assert-PullRequestTargetsMatch {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Left,

        [Parameter(Mandatory)]
        $Right,

        [Parameter(Mandatory)]
        [string] $RightName
    )

    foreach ($name in $script:PullRequestTargetCoordinates) {
        # The locals are not named after the parameters: a parameter's type constraint follows its
        # variable regardless of casing, so assigning a string to $left would fail as a conversion
        # to the dictionary type the parameter declares.
        $leftValue = [string] (Get-JsonProperty -Node $Left -Name $name)
        $rightValue = [string] (Get-JsonProperty -Node $Right -Name $name)
        if ([string]::Equals($leftValue, $rightValue, [StringComparison]::OrdinalIgnoreCase)) { continue }
        throw "The pull request named on the command line and the one named by $RightName differ in $name ('$leftValue' against '$rightValue'). Publish the approved set against the pull request it was produced for."
    }

    $leftId = [int] (Get-JsonProperty -Node $Left -Name 'pullRequestId' -Default 0)
    $rightId = [int] (Get-JsonProperty -Node $Right -Name 'pullRequestId' -Default 0)
    if ($leftId -ne $rightId) {
        throw "The pull request named on the command line is $leftId and the one named by $RightName is $rightId. Publish the approved set against the pull request it was produced for."
    }
}

# Refuses to publish onto a pull request that has moved or closed since the approval.
#
# Both services accept a comment against a commit that is no longer the head and merely mark it
# outdated, so neither refuses this on the reviewer's behalf. Detecting it is this run's own job:
# publishing a review of code that has since been rewritten hands the author feedback on a revision
# they have already replaced.
#
# `-AllowClosed` publishes onto a pull request that is no longer open. A review that finished after
# the pull request did is still worth delivering, and whether the discussion is the right place for
# it or whether the findings belong in a tracker is a judgement the user makes, not one this check
# can make for them.
#
# Under that switch the merge base is no longer compared, for a pull request that is in fact no
# longer open. A completed pull request has by definition merged, which moves the merge base, so
# keeping the comparison would refuse every pull request the switch exists for. Nothing is lost by
# dropping it: the diff the review was formed against was computed locally from the reviewed commits
# and does not change when the target branch advances. The source commit is still compared, because a
# source commit that has moved means the author pushed code the review never saw, which invalidates
# the findings whatever the pull request's state.
function Assert-PullRequestPublicationState {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Approved,

        [switch] $AllowClosed
    )

    $isActive = [bool] (Get-JsonProperty -Node $State -Name 'isActive' -Default $false)
    if (-not $isActive) {
        $status = [string] (Get-JsonProperty -Node $State -Name 'status')
        if (-not $AllowClosed) {
            throw "The pull request is $status rather than open. Publish onto it anyway by passing -PublishToClosedPullRequest, or file the findings where work is tracked with Publish-ReviewTrackerItems.ps1."
        }
    }

    $comparisons = [System.Collections.Generic.List[hashtable]]::new()
    $comparisons.Add(@{ state = 'sourceCommit'; approved = 'reviewedSourceCommit'; label = 'source commit' })

    # The relaxation follows the pull request's state rather than the switch. A caller who passes the
    # switch pre-emptively against an open pull request would otherwise lose the comparison for a
    # reason that does not apply to it.
    if ($isActive -or -not $AllowClosed) {
        $comparisons.Add(@{ state = 'mergeBaseCommit'; approved = 'reviewedMergeBaseCommit'; label = 'merge base' })
    }

    foreach ($pair in $comparisons) {
        $live = [string] (Get-JsonProperty -Node $State -Name $pair['state'])
        $reviewed = [string] (Get-JsonProperty -Node $Approved -Name $pair['approved'])
        if ([string]::Equals($live, $reviewed, [StringComparison]::OrdinalIgnoreCase)) { continue }
        throw "The pull request's $($pair['label']) is now $live, but the approved findings were produced against $reviewed. Run the freshness check again and rebuild the approved set before publishing."
    }
}

# Clears the way past a draft review the identity already owns.
#
# A service that permits one draft per identity turns somebody else's unfinished work into an
# obstacle for this run, and the two are indistinguishable from here: a draft left by an abandoned
# publication looks exactly like one a person is composing in the web interface. Discarding it is
# therefore never implicit.
function Clear-PullRequestPendingReview {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [switch] $DiscardPendingReview
    )

    $pendingReviewId = [string] (Get-JsonProperty -Node $State -Name 'pendingReviewId')
    if ([string]::IsNullOrEmpty($pendingReviewId)) { return $State }

    if (-not $DiscardPendingReview) {
        throw "A draft review already exists on this pull request for the authenticated identity, and a review is assembled as a single draft, so publication cannot proceed. Confirm that the draft is not somebody's unsent work and pass -DiscardPendingReview to discard it."
    }

    Remove-PullRequestPendingReviewForTarget -Transport $Transport -Target $Target -ReviewId $pendingReviewId | Out-Null
    Set-JsonProperty -Node $State -Name 'pendingReviewId' -Value $null
    $State
}

# Refuses a repository that cannot supply the reviewed diff, naming what has to be fetched.
#
# The diff is read locally rather than from the service, so a checkout without the reviewed commits
# is an ordinary situation with an ordinary remedy, and the message states that remedy rather than
# leaving the reader to work out which commit is missing from a diff that came back empty.
function Assert-PullRequestPublicationRepository {
    param(
        [Parameter(Mandatory)]
        [scriptblock] $Invoker,

        [Parameter(Mandatory)]
        [string] $RepositoryPath,

        [Parameter(Mandatory)]
        [string] $MergeBaseCommit,

        [Parameter(Mandatory)]
        [string] $SourceCommit
    )

    if (-not (Test-GitRepositoryPresent -Invoker $Invoker -RepositoryPath $RepositoryPath)) {
        throw "There is no readable Git repository at '$RepositoryPath'. Publication reads the reviewed diff from a local clone, so name the checkout that holds the reviewed commits."
    }

    foreach ($commit in @($MergeBaseCommit, $SourceCommit)) {
        if (Test-GitCommitPresent -Invoker $Invoker -RepositoryPath $RepositoryPath -Commit $commit) { continue }
        throw "Commit $commit is not present in the Git repository at '$RepositoryPath'. Run 'git fetch' in that repository until both $MergeBaseCommit and $SourceCommit are present, then publish again."
    }
}

# Confirms that the service holds the finding comments the run believes it wrote.
#
# Pending GitHub review comments deliberately fail the visibility test because their state is
# PENDING rather than SUBMITTED. The pre-completion verification therefore checks presence, body and
# anchor without requiring visibility; the post-completion verification adds the visibility
# requirement because the submit is the point where GitHub makes the draft public.
function Assert-PullRequestPublicationFindingsVerified {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Plan,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Verification,

        [switch] $RequirePublished
    )

    $expected = @(Get-PullRequestExpectedPublicationComment -Result $Result -Plan $Plan)
    $comments = Get-JsonArrayProperty -Node $Verification -Name 'comments'

    if ($RequirePublished) {
        $published = 0
        foreach ($comment in $comments) {
            if ([bool] (Get-JsonProperty -Node $comment -Name 'isPublished' -Default $false)) { $published++ }
        }

        if ($published -lt $expected.Count) {
            throw "The published review carries $published published comments, but $($expected.Count) findings were accepted, so the review published fewer comments than it was given."
        }
    }

    foreach ($item in $expected) {
        $comment = Find-PullRequestVerifiedComment -Expected $item -VerifiedComment $comments
        $findingId = [string] (Get-JsonProperty -Node $item -Name 'findingId')
        if ($null -eq $comment) {
            throw "Finding '$findingId' was accepted but the service did not report its comment in the review verification."
        }

        $expectedBody = [string] (Get-JsonProperty -Node $item -Name 'body')
        $actualBody = [string] (Get-JsonProperty -Node $comment -Name 'body')
        if ($actualBody -cne $expectedBody) {
            throw "Finding '$findingId' was accepted but the service reported a different comment body for it."
        }

        if ($RequirePublished -and -not [bool] (Get-JsonProperty -Node $comment -Name 'isPublished' -Default $false)) {
            throw "Finding '$findingId' was accepted but the service did not report its comment as published."
        }

        Assert-PullRequestVerifiedCommentAnchor -Expected $item -Comment $comment
    }
}

# Confirms that the service holds what the completed review promises.
function Assert-PullRequestPublicationVerified {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Plan,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Verification,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $SummaryBody
    )

    if (-not [bool] (Get-JsonProperty -Node $Verification -Name 'isPublished' -Default $false)) {
        $state = [string] (Get-JsonProperty -Node $Verification -Name 'state')
        throw "The published review reports state '$state' rather than a published one, so the review is not visible on the pull request."
    }

    Assert-PullRequestPublicationFindingsVerified -Result $Result -Plan $Plan -Verification $Verification -RequirePublished

    $body = [string] (Get-JsonProperty -Node $Verification -Name 'body')
    if ($body -cne $SummaryBody) {
        throw 'The published review summary does not match the summary body the run submitted.'
    }
}

# Lists the finding comments the run accepted and therefore must be able to read back.
function Get-PullRequestExpectedPublicationComment {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Result,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Plan
    )

    $expected = [System.Collections.Generic.List[object]]::new()
    foreach ($planEntry in $Plan) {
        $operation = [string] (Get-JsonProperty -Node $planEntry -Name 'operation')
        if ($script:PullRequestFindingOperations -cnotcontains $operation) { continue }

        $findingId = [string] (Get-JsonProperty -Node $planEntry -Name 'findingId')
        $record = Find-PullRequestPublicationOperation -Result $Result -Operation $operation -FindingId $findingId
        if ([string] (Get-JsonProperty -Node $record -Name 'outcome') -cne 'succeeded') { continue }

        $expected.Add([ordered]@{
                operation = $operation
                findingId = $findingId
                body      = [string] (Get-JsonProperty -Node $planEntry -Name 'body')
                anchor    = Get-JsonProperty -Node $planEntry -Name 'anchor'
                threadId  = [string] (Get-JsonProperty -Node $record -Name 'threadId')
                commentId = [string] (Get-JsonProperty -Node $record -Name 'commentId')
            })
    }

    $expected.ToArray()
}

# Locates the service-side comment corresponding to one accepted finding.
function Find-PullRequestVerifiedComment {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Expected,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $VerifiedComment
    )

    $commentId = [string] (Get-JsonProperty -Node $Expected -Name 'commentId')
    $threadId = [string] (Get-JsonProperty -Node $Expected -Name 'threadId')
    $body = [string] (Get-JsonProperty -Node $Expected -Name 'body')

    foreach ($candidate in $VerifiedComment) {
        $candidateCommentId = [string] (Get-JsonProperty -Node $candidate -Name 'id')
        $candidateThreadId = [string] (Get-JsonProperty -Node $candidate -Name 'threadId')

        if ($commentId -ne '') {
            if ($candidateCommentId -cne $commentId) { continue }
            if ($threadId -ne '' -and $candidateThreadId -ne '' -and $candidateThreadId -cne $threadId) { continue }
            return $candidate
        }

        if ($threadId -eq '' -or $candidateThreadId -cne $threadId) { continue }
        if ([string] (Get-JsonProperty -Node $candidate -Name 'body') -cne $body) { continue }
        return $candidate
    }

    $null
}

# Confirms that a service-side comment is anchored where the plan placed it.
function Assert-PullRequestVerifiedCommentAnchor {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Expected,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Comment
    )

    $operation = [string] (Get-JsonProperty -Node $Expected -Name 'operation')
    $findingId = [string] (Get-JsonProperty -Node $Expected -Name 'findingId')
    if ($operation -ceq 'reply') {
        $expectedThreadId = [string] (Get-JsonProperty -Node $Expected -Name 'threadId')
        $actualThreadId = [string] (Get-JsonProperty -Node $Comment -Name 'threadId')
        if ($expectedThreadId -ne '' -and $actualThreadId -ne '' -and $actualThreadId -cne $expectedThreadId) {
            throw "Finding '$findingId' was accepted as a reply but the service reported it in a different thread."
        }
        return
    }

    $anchor = Get-JsonProperty -Node $Expected -Name 'anchor'
    $expectedPath = [string] (Get-JsonProperty -Node $anchor -Name 'path')
    $actualPath = [string] (Get-JsonProperty -Node $Comment -Name 'path')
    if ($actualPath -cne $expectedPath) {
        throw "Finding '$findingId' was accepted on '$expectedPath' but the service reported it on '$actualPath'."
    }

    $subjectType = [string] (Get-JsonProperty -Node $Comment -Name 'subjectType')
    if ($operation -ceq 'newFileThread') {
        if ($subjectType -cne 'FILE') {
            throw "Finding '$findingId' was accepted as a file comment but the service reported subject '$subjectType'."
        }

        $line = Get-JsonProperty -Node $Comment -Name 'line'
        if ($null -ne $line -and [string] $line -ne '') {
            throw "Finding '$findingId' was accepted as a file comment but the service reported a line anchor for it."
        }
        return
    }

    if ($subjectType -cne 'LINE') {
        throw "Finding '$findingId' was accepted as a line comment but the service reported subject '$subjectType'."
    }

    $expectedLine = [int] (Get-JsonProperty -Node $anchor -Name 'endLine')
    $actualLine = Get-JsonProperty -Node $Comment -Name 'line'
    if ($null -eq $actualLine -or [int] $actualLine -ne $expectedLine) {
        throw "Finding '$findingId' was accepted on line $expectedLine but the service reported line '$actualLine'."
    }

    $actualStartLine = Get-JsonProperty -Node $Comment -Name 'startLine'
    if ($null -ne $actualStartLine) {
        $expectedStartLine = [int] (Get-JsonProperty -Node $anchor -Name 'startLine' -Default $expectedLine)
        if ([int] $actualStartLine -ne $expectedStartLine) {
            throw "Finding '$findingId' was accepted with a line range start the service did not report back."
        }
    }

    $actualSide = [string] (Get-JsonProperty -Node $Comment -Name 'side')
    if ($actualSide -ne '') {
        $expectedSide = [string] (Get-JsonProperty -Node $anchor -Name 'side' -Default 'right')
        if ($actualSide.ToLowerInvariant() -cne $expectedSide.ToLowerInvariant()) {
            throw "Finding '$findingId' was accepted on the '$expectedSide' side but the service reported '$actualSide'."
        }
    }
}
