#Requires -Version 7.6

# Files an approved review into a tracker (docs/design.md, "Publication"; docs/implementation.md,
# "Tracker publication").
#
# This is the alternative to publishing onto a pull request, for the reviews that have no pull
# request to publish onto, for the ones whose pull request has closed since the review began, and for
# the ones a user simply wants tracked rather than discussed. It is the composition root for that
# path: it dot-sources the libraries and holds no decision a library could hold, so every rule it
# applies is reachable from the test suite without a service.
#
# The run validates everything it can before it touches the network, because every item it files is
# visible the moment the tracker accepts it and none of them can be withdrawn as a set. Once the
# filing starts, the journal at tracker-publication-result.json is rewritten after every creation, so
# a run that dies partway still states exactly what exists. A run that finds such a journal in the
# output directory refuses rather than overwrite it: filing again would file the review a second time
# rather than resume the first.
#
# The command takes no repository path and computes no diff. A tracker item carries no anchor into a
# diff, so there is nothing to resolve and nothing to downgrade.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. The journal goes to a file and errors to the error
# stream. The object is emitted even when the run fails, because with no resume the list of items
# already filed is the most valuable thing a failed run can hand back.

[CmdletBinding(PositionalBinding = $false)]
param(
    # Browser URL of the tracker: a GitHub repository, or an Azure DevOps project. Any URL naming
    # something inside one serves, including the pull request that was reviewed.
    [string] $Url,

    # The approved finding artifact the audit produced. Its target may be null, which is what a
    # review of content that is no pull request produces.
    [Parameter(Mandatory)]
    [string] $ApprovedFindingsPath,

    # The focus-area cache diagnostics the review summary reports.
    [Parameter(Mandatory)]
    [string] $DiagnosticsPath,

    [Parameter(Mandatory)]
    [string] $OutputDirectory,

    # Required for PR-backed findings to validate their checked evidence and supply source links.
    [string] $ContextPath,

    # Azure DevOps only, and required there: the type the work items are created as.
    [string] $WorkItemType,

    # Azure DevOps only, and required there: the area the work items are filed under, rooted at the
    # project name.
    [string] $AreaPath,

    # Azure DevOps only. The large text field the finding's body is written to. The default is the
    # description field every process defines; the Agile and Scrum bug forms show
    # 'Microsoft.VSTS.TCM.ReproSteps' instead and never the description.
    [string] $DescriptionField,

    # Azure DevOps only. Links every created work item to this one as its parent.
    [int] $ParentWorkItemId,

    # Continues a run that paused on a body Azure DevOps rewrote, filing whatever is still pending
    # without recreating the item that paused it. The three coordinates below name exactly the
    # rewrite being accepted; they are read from the paused run's outcome or journal.
    [switch] $AcceptStoredRewrite,

    [string] $AcceptWorkItemId,

    [string] $AcceptExpectedBodyHash,

    [string] $AcceptStoredBodyHash,

    # Explicit coordinates, overriding whatever the URL carried. A caller with no URL names the
    # tracker with these.
    [ValidateSet('azureDevOps', 'gitHub')]
    [string] $Platform,

    [string] $Organization,

    [string] $Project,

    [string] $Repository,

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
. "$PSScriptRoot\lib\TrackerTarget.ps1"
. "$PSScriptRoot\lib\PullRequestIdentity.ps1"
. "$PSScriptRoot\lib\AdoRestTransport.ps1"
. "$PSScriptRoot\lib\AdoPullRequestApi.ps1"
. "$PSScriptRoot\lib\AdoWorkItemApi.ps1"
. "$PSScriptRoot\lib\GitHubCliTransport.ps1"
. "$PSScriptRoot\lib\GitHubPullRequestApi.ps1"
. "$PSScriptRoot\lib\GitHubIssueApi.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
. "$PSScriptRoot\lib\FindingMarkdown.ps1"
. "$PSScriptRoot\lib\TrackerItem.ps1"
. "$PSScriptRoot\lib\PullRequestTransportHost.ps1"
. "$PSScriptRoot\lib\PullRequestPlatform.ps1"
. "$PSScriptRoot\lib\ReviewPublication.ps1"
. "$PSScriptRoot\lib\TrackerPublication.ps1"

# --- Preflight: everything that can be judged without the service ---------------------------------
#
# Preflight leaves the tracker and publication records unchanged. The retained sibling lock
# coordinates concurrent publishers without removing a lock that another invocation may be using.

Assert-PublicationOutputDirectory -OutputDirectory $OutputDirectory
$transactionLock = Open-PullRequestFileLock -Path (Get-TrackerPublicationResultPath -OutputDirectory $OutputDirectory)
try {
    $target = Resolve-TrackerTarget `
        -Url $Url `
        -Platform $Platform `
        -Organization $Organization `
        -Project $Project `
        -Repository $Repository

    # The journal an earlier run left is read before anything else is judged, because a new journal is
    # written over it and a run that overwrote it would have destroyed the only account of what that
    # earlier run filed. A continuation instead reads the paused journal it is resuming and confirms the
    # acceptance names the rewrite it paused on; only that path is allowed to write over a journal that
    # records filed items.
    Assert-TrackerContinuationArgument `
        -AcceptStoredRewrite:$AcceptStoredRewrite `
        -AcceptWorkItemId $AcceptWorkItemId `
        -AcceptExpectedBodyHash $AcceptExpectedBodyHash `
        -AcceptStoredBodyHash $AcceptStoredBodyHash

    $continuation = $null
    if ($AcceptStoredRewrite) {
        $continuation = Read-TrackerContinuationRecord `
            -OutputDirectory $OutputDirectory `
            -Target $target `
            -AcceptWorkItemId $AcceptWorkItemId `
            -AcceptExpectedBodyHash $AcceptExpectedBodyHash `
            -AcceptStoredBodyHash $AcceptStoredBodyHash
    } else {
        Assert-TrackerPublicationRecord -OutputDirectory $OutputDirectory -Target $target
    }

    $approved = Read-ApprovedFindingArtifact -Path $ApprovedFindingsPath
    $findings = Get-JsonArrayProperty -Node $approved -Name 'findings'
    Assert-ApprovedFinding -Finding $findings -Path $ApprovedFindingsPath

    Assert-TrackerArgument `
        -Target $target `
        -WorkItemType $WorkItemType `
        -AreaPath $AreaPath `
        -DescriptionField $DescriptionField `
        -ParentWorkItemId $ParentWorkItemId

    # The approved artifact's own target is deliberately not compared with the tracker. Filing GitHub
    # issues for a review of an Azure DevOps pull request is a supported case and is the reason the
    # tracker is named separately at all.
    $context = $null
    if ($ContextPath) {
        $context = Read-PullRequestContextDocument -Path $ContextPath
    }
    Assert-ApprovedFindingContext -Approved $approved -Context $context

    foreach ($finding in $findings) {
        Assert-TrackerFollowUpTarget -Finding $finding -Context $context
    }

    $diagnostics = Read-PublicationDiagnostics -Path $DiagnosticsPath

    # Rendering every item before anything is filed is what keeps a rendering failure from stopping the
    # run halfway through a review.
    $items = New-TrackerItemSet -Finding $findings -Context $context

    $summaryBody = New-ReviewSummaryMarkdown `
        -Finding $findings `
        -FollowUpCount (Get-FindingFollowUpCount -Finding $findings) `
        -Diagnostics $diagnostics `
        -Platform ([string] (Get-JsonProperty -Node $target -Name 'platform'))

    # --- Live state: the tracker decides whether filing may begin at all ------------------------------

    $transport = New-PullRequestTransport -Target $target -Tenant $Tenant
    $state = Get-TrackerStateForTarget `
        -Transport $transport `
        -Target $target `
        -WorkItemType $WorkItemType `
        -AreaPath $AreaPath `
        -DescriptionField $DescriptionField `
        -ParentWorkItemId $ParentWorkItemId

    # --- Publication ----------------------------------------------------------------------------------

    $resultPath = Get-TrackerPublicationResultPath -OutputDirectory $OutputDirectory

    if ($AcceptStoredRewrite) {
        # The continuation resumes the paused run's own journal. The plan and the work item coordinates
        # are confirmed to be the paused run's before the acceptance is recorded, so a continuation of a
        # different review, or one filed differently, is refused rather than filed.
        Assert-TrackerContinuationPlan -Result $continuation -Item $items -State $state

        if ([string] (Get-JsonProperty -Node $continuation -Name 'status') -ceq 'needsAdjudication') {
            Set-TrackerPublicationAcceptedRewrite `
                -Result $continuation `
                -ItemId $AcceptWorkItemId `
                -ExpectedBodyHash $AcceptExpectedBodyHash `
                -StoredBodyHash $AcceptStoredBodyHash
            # The acceptance is on disk before the resumed run touches the tracker, so a continuation that
            # dies before it files anything is idempotent when it is run again.
            Write-TrackerPublicationResult -Result $continuation -Path $resultPath
        }

        $result = $continuation
    } else {
        $result = New-TrackerPublicationResult -Target $target -State $state -Item $items -NowUtc $NowUtc

        # The journal is on disk before the first creation, so a run that dies at the very first item still
        # leaves an account of what it was about to file.
        Write-TrackerPublicationResult -Result $result -Path $resultPath
    }

    $failure = $null
    try {
        Invoke-TrackerPublication `
            -Transport $transport `
            -Target $target `
            -State $state `
            -Item $items `
            -Result $result `
            -ResultPath $resultPath | Out-Null
    } catch {
        # The failure is held rather than raised here. Everything the run reached recorded what became of
        # it in the journal, and the account of what exists has to reach the caller before the failure
        # does, because nothing resumes this run and the caller decides what to do next from that list.
        $failure = $_
    }

    New-TrackerPublicationOutcome -Result $result -ResultPath $resultPath -SummaryBody $summaryBody |
        ConvertTo-Json -Depth 32 -Compress

    Assert-TrackerPublicationComplete -Result $result

    # Reached only if the run raised something the journal did not record, which is a state the journal
    # cannot explain. The original failure is the only account there is of it.
    if ($null -ne $failure) { throw $failure }
} finally { $transactionLock.Dispose() }
