#Requires -Version 7.6

# Publishes an approved review onto a pull request (docs/design.md, "Review publication";
# docs/implementation.md, "Publication").
#
# This publishes the approved findings; initial overview publication is a separate preparation
# operation with its own best-effort journal. It is the composition root
# for publication: it dot-sources the libraries and holds no decision a library could hold, so every
# rule it applies is reachable from the test suite without a service.
#
# The run validates everything it can before it touches the network, because a publication that
# fails halfway leaves comments behind that nobody asked for. Once the writing starts, the record at
# publication-result.json is rewritten after every operation, so a run that dies partway still states
# exactly what reached the pull request. A run that finds such a record already in the output
# directory refuses rather than overwrite it, because that record is what a person needs in order to
# decide what a partial publication still leaves to do.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. The record goes to a file and errors to the error
# stream. A run whose review is published but whose follow-up could not be completed emits that
# object and then ends in failure, because the pull request is left with something for a person to
# attend to.

[CmdletBinding(PositionalBinding = $false)]
param(
    # Browser URL of the pull request the approved set is published onto.
    [Parameter(Mandatory)]
    [string] $Url,

    # The approved finding artifact the audit produced.
    [Parameter(Mandatory)]
    [string] $ApprovedFindingsPath,

    # The pull request capture the follow-up findings answer threads from.
    [Parameter(Mandatory)]
    [string] $ContextPath,

    # The focus-area cache diagnostics the review summary reports.
    [Parameter(Mandatory)]
    [string] $DiagnosticsPath,

    # A local clone holding the reviewed commits. The diff is read from here rather than from the
    # service, so the anchors are judged against the same hunks the review was produced against.
    [Parameter(Mandatory)]
    [string] $RepositoryPath,

    [Parameter(Mandatory)]
    [string] $OutputDirectory,

    # Discards the draft review the authenticated identity already holds. Never implicit: that draft
    # may be unsent work somebody is composing in the web interface.
    [switch] $DiscardPendingReview,

    # Publishes onto a pull request that is no longer open. Never implicit: a review posted onto a
    # merged or abandoned pull request may reach nobody, and filing the findings where work is
    # tracked is often the better answer, so the choice is the user's to make.
    [switch] $PublishToClosedPullRequest,

    # The commit-bound decision is validated here, not translated into a switch by the agent.
    # Omission means no merge approval; an explicitly named missing record is an error.
    [string] $MergeApprovalPath,

    # Azure DevOps only. The access token is issued for the current subscription's tenant, which is
    # usually but not always the organization's tenant.
    [string] $Tenant,

    [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PublicationFailure.ps1"
. "$PSScriptRoot\lib\PullRequestTarget.ps1"
. "$PSScriptRoot\lib\PullRequestIdentity.ps1"
. "$PSScriptRoot\lib\AdoRestTransport.ps1"
. "$PSScriptRoot\lib\AdoPullRequestApi.ps1"
. "$PSScriptRoot\lib\GitHubCliTransport.ps1"
. "$PSScriptRoot\lib\GitHubPullRequestApi.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"
. "$PSScriptRoot\lib\ReviewAuditState.ps1"
. "$PSScriptRoot\lib\FindingMarkdown.ps1"
. "$PSScriptRoot\lib\GitDiff.ps1"
. "$PSScriptRoot\lib\PullRequestAnchor.ps1"
. "$PSScriptRoot\lib\PullRequestTransportHost.ps1"
. "$PSScriptRoot\lib\PullRequestPlatform.ps1"
. "$PSScriptRoot\lib\ReviewPublication.ps1"
. "$PSScriptRoot\lib\PullRequestPublication.ps1"

# --- Preflight: everything that can be judged without the service ---------------------------------
#
# Preflight leaves the pull request and publication records unchanged. The retained sibling lock
# coordinates concurrent publishers without removing a lock that another invocation may be using.

Assert-PublicationOutputDirectory -OutputDirectory $OutputDirectory
$transactionLock = Open-PullRequestFileLock -Path (Get-PullRequestPublicationResultPath -OutputDirectory $OutputDirectory)
try {
    $target = Resolve-PullRequestTarget -Url $Url

    # The record an earlier run left is read before anything else is judged, because a new record is
    # written over it and a run that overwrote it would have destroyed the only account of what that
    # earlier run published.
    Assert-PullRequestPublicationRecord -OutputDirectory $OutputDirectory -Target $target

    $approved = Read-ApprovedFindingArtifact -Path $ApprovedFindingsPath
    Assert-PullRequestApprovedArtifact -Approved $approved -Path $ApprovedFindingsPath
    $context = Read-PullRequestContextDocument -Path $ContextPath
    $ApproveMerge = $false
    if ($MergeApprovalPath) {
        $ApproveMerge = Test-ReviewMergeApproval -Path $MergeApprovalPath -PullRequestUrl $context['pullRequestUrl'] `
            -SourceCommit $approved['reviewedSourceCommit']
    }
    $findings = Get-JsonArrayProperty -Node $approved -Name 'findings'
    # A merge approval publishes even when there is nothing to comment on, so the empty set that a clean
    # pull request produces is allowed here (SKILL.md, "Phase 9: publication").
    Assert-ApprovedFinding -Finding $findings -Path $ApprovedFindingsPath -AllowEmpty:$ApproveMerge

    Assert-PullRequestFollowUpTarget -Finding $findings -Context $context

    Assert-PullRequestPublicationTarget -Target $target -Approved $approved -Context $context
    Assert-ApprovedFindingContext -Approved $approved -Context $context

    # A reviewer does not approve the merge of their own pull request, so a merge approval on such a pull
    # request is refused here among the preconditions rather than at completion after the finding comments
    # were posted (docs/implementation.md, "Publication").
    Assert-PullRequestMergeApproval -Context $context -ApproveMerge:$ApproveMerge

    $reviewedSourceCommit = [string] (Get-JsonProperty -Node $approved -Name 'reviewedSourceCommit')
    $reviewedMergeBaseCommit = [string] (Get-JsonProperty -Node $approved -Name 'reviewedMergeBaseCommit')

    $diagnostics = Read-PublicationDiagnostics -Path $DiagnosticsPath

    $bodies = New-PullRequestFindingBody -Finding $findings

    # The local clone is examined last of the local checks, because it is the only one that runs another
    # program, and every cheaper refusal is already behind us by the time it does.
    $gitInvoker = Get-GitInvoker
    Assert-PullRequestPublicationRepository `
        -Invoker $gitInvoker `
        -RepositoryPath $RepositoryPath `
        -MergeBaseCommit $reviewedMergeBaseCommit `
        -SourceCommit $reviewedSourceCommit

    $diff = Get-GitDiff `
        -RepositoryPath $RepositoryPath `
        -MergeBaseCommit $reviewedMergeBaseCommit `
        -SourceCommit $reviewedSourceCommit

    $followUpCount = Get-FindingFollowUpCount -Finding $findings
    $summaryBody = New-ReviewSummaryMarkdown `
        -Finding $findings `
        -FollowUpCount $followUpCount `
        -Diagnostics $diagnostics `
        -Platform ([string] (Get-JsonProperty -Node $target -Name 'platform'))

    # --- Live state and plan: the service decides whether publication may begin at all ----------------
    #
    # The one write before the publication itself lives at the end of this step, where every refusal the
    # run is capable of is already behind it.

    $transport = New-PullRequestTransport -Target $target -Tenant $Tenant
    $session = New-PullRequestPublicationPlanForTarget `
        -Transport $transport `
        -Target $target `
        -Approved $approved `
        -Finding $findings `
        -Context $context `
        -Body $bodies `
        -Diff $diff `
        -DiscardPendingReview:$DiscardPendingReview `
        -PublishToClosedPullRequest:$PublishToClosedPullRequest

    # --- Publication ----------------------------------------------------------------------------------

    $resultPath = Get-PullRequestPublicationResultPath -OutputDirectory $OutputDirectory
    $result = New-PullRequestPublicationResult `
        -Target $target `
        -State $session['state'] `
        -Plan $session['plan'] `
        -NowUtc $NowUtc

    # The record is on disk before the first write, so a run that dies at the very first operation still
    # leaves an account of what it was about to do.
    Write-PullRequestPublicationResult -Result $result -Path $resultPath

    Invoke-PullRequestPublication `
        -Transport $transport `
        -Target $target `
        -State $session['state'] `
        -Plan $session['plan'] `
        -SummaryBody $summaryBody `
        -Result $result `
        -ResultPath $resultPath `
        -ApproveMerge:$ApproveMerge | Out-Null

    $outcome = New-PullRequestPublicationOutcome -Result $result -ResultPath $resultPath

    $outcome | ConvertTo-Json -Depth 32 -Compress

    # The outcome is emitted first so that a run reporting unfinished business still hands the caller the
    # whole account of what it published.
    Assert-PullRequestPublicationComplete -Result $result
} finally { $transactionLock.Dispose() }
