#Requires -Version 7.6

# Rules every publication of a review shares, whatever it publishes into (docs/implementation.md,
# "Publication").
#
# A review is published either as comments on a pull request or as items in a tracker. The two write
# to different services and produce different records, but they judge the same things beforehand:
# that the approved set is publishable, that the run can record what it did, and that the summary has
# the numbers it states. Those judgements live here so that both paths reach the same verdict on the
# same inputs, rather than each carrying its own reading of what an approved set has to satisfy.
#
# The publication roots load the finding and capture model libraries alongside this adapter.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# The approved artifact's format version, matching the version the finding model writes. The
# artifact binds an approved finding set to the content it was approved against, which is what makes
# publication refuse to write a review of one revision onto another.
#
# The target and the reviewed commits are named but may be null: a review of content that is no pull
# request has none, and its approved set is publishable as tracker items. Publication onto a pull
# request requires them and says so itself.
$script:ApprovedFindingArtifactSchemaVersion = 2
$script:ApprovedFindingArtifactFields = @(
    'schemaVersion'
    'target'
    'reviewedSourceCommit'
    'reviewedMergeBaseCommit'
    'reviewedContextHash'
    'findings'
)

# Reads the approved artifact the audit produced.
#
# The artifact carries what the approval was given against, not merely the findings, because
# approval is a statement about a specific revision of specific content. Publishing it elsewhere
# would attribute the reviewer's judgement to code they never saw.
function Read-ApprovedFindingArtifact {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $fullPath = Resolve-PullRequestPath -Path $Path
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        throw "There is no approved finding artifact at '$Path'. Derive it with New-ApprovedFindings.ps1 before publishing."
    }

    $document = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $fullPath)
    if ($document -isnot [System.Collections.IDictionary]) {
        throw "The approved finding artifact at '$Path' is not a JSON object."
    }

    $kind = [string] (Get-JsonProperty -Node $document -Name 'kind')
    if ($kind) {
        throw "The artifact at '$Path' reports kind '$kind'; publication reads the approved set New-ApprovedFindings.ps1 produced, not the working finding artifact it was derived from."
    }

    $version = Get-JsonProperty -Node $document -Name 'schemaVersion'
    if ($version -ne $script:ApprovedFindingArtifactSchemaVersion) {
        $reported = if ($null -eq $version) { 'no version' } else { "version $version" }
        throw "The approved finding artifact at '$Path' reports $reported, but this helper reads version $($script:ApprovedFindingArtifactSchemaVersion). Derive the approved set again instead of publishing it."
    }

    foreach ($key in @($document.Keys)) {
        $name = [string] $key
        if ($script:ApprovedFindingArtifactFields -cnotcontains $name) {
            throw "The approved finding artifact at '$Path' carries the unknown field '$name'."
        }
    }

    foreach ($name in $script:ApprovedFindingArtifactFields) {
        # Presence rather than a value, because a review of content that is no pull request records
        # its target and its reviewed commits as null and is publishable as tracker items. Whichever
        # of them a given destination requires is that destination's own rule to state.
        if ($document.Contains($name)) { continue }
        throw "The approved finding artifact at '$Path' is missing '$name'."
    }

    $hash = $document['reviewedContextHash']
    if ($null -ne $document['target'] -and $null -eq $hash) {
        throw 'A PR-backed approved finding artifact requires a non-null reviewedContextHash. Rebuild against the checked capture.'
    }
    if ($null -ne $hash -and ($hash -isnot [string] -or $hash -cnotmatch '^[A-F0-9]{64}$')) {
        throw 'The approved finding artifact has an invalid reviewedContextHash.'
    }
    $document
}

# The source evidence binding is independent of publication destination, including cross-platform
# tracker delivery. Ref: docs/implementation.md, "Tracker publication".
function Assert-ApprovedFindingContext {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $Approved,
        [AllowNull()][System.Collections.IDictionary] $Context
    )

    try {
        Invoke-NativeReviewWorkflowWithJson -Arguments @('approved', 'check-request') -PathOption 'request-path' -Value @{
            approved = $Approved
            context = $Context
        } | Out-Null
    } catch {
        if ($null -eq $Context -and (
            $null -ne (Get-JsonProperty -Node $Approved -Name 'target') -or
            $null -ne (Get-JsonProperty -Node $Approved -Name 'reviewedContextHash')
        )) {
            throw "PR-backed approved findings require -ContextPath naming the checked capture. $($_.Exception.Message)"
        }
        if ($null -eq $Context) {
            throw "Approved findings cannot be validated. $($_.Exception.Message)"
        }
        throw "Approved findings are not bound to this checked capture. $($_.Exception.Message)"
    }
}

# Confirms that the output directory will accept the record before anything is published.
#
# The record is the only account of what was written, so a directory that cannot be written to has
# to be discovered before the first write rather than after the last. The probe leaves nothing
# behind, so a run that fails preflight writes nothing at all.
function Assert-PublicationOutputDirectory {
    param(
        [Parameter(Mandatory)]
        [string] $OutputDirectory
    )

    $fullPath = Resolve-PullRequestPath -Path $OutputDirectory
    if (-not (Test-Path -LiteralPath $fullPath -PathType Container)) {
        New-Item -ItemType Directory -Path $fullPath -Force | Out-Null
    }

    $probePath = Join-Path $fullPath "publication-probe.$([guid]::NewGuid()).tmp"
    try {
        [System.IO.File]::WriteAllText($probePath, '')
    } catch {
        throw "The output directory '$OutputDirectory' cannot be written to, so the publication result could not be recorded: $($_.Exception.Message)"
    } finally {
        if (Test-Path -LiteralPath $probePath -PathType Leaf) {
            Remove-Item -LiteralPath $probePath -Force
        }
    }
}

# Reads the focus-area cache diagnostics the summary reports.
#
# The record is required rather than defaulted to empty, because the summary states what the review
# reused and a run that quietly reported nothing would be indistinguishable from one whose cache
# genuinely contributed nothing.
function Read-PublicationDiagnostics {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $fullPath = Resolve-PullRequestPath -Path $Path
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        throw "There is no focus-area cache diagnostics record at '$Path'. The review summary reports the cache outcome, so the record the restore left behind is required."
    }

    $document = $null
    try {
        $document = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $fullPath)
    } catch {
        throw "The focus-area cache diagnostics record at '$Path' cannot be read: $($_.Exception.Message)"
    }

    if ($document -isnot [System.Collections.IDictionary]) {
        throw "The focus-area cache diagnostics record at '$Path' is not a JSON object."
    }

    $document
}

# Refuses an approved set that cannot be published as it stands.
#
# Every finding is validated in the artifact's own stored shape, because a finding that reached
# publication malformed was malformed when it was approved, and the reviewer approved text that
# cannot be posted. An empty set is refused unless the caller allows it, which only the pull request
# merge-approval path does.
function Assert-ApprovedFinding {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding,

        [Parameter(Mandatory)]
        [string] $Path,

        # Permits an approved set that holds no findings. A pull request review uses this to record the
        # reviewer's approval of the merge with nothing to comment on (SKILL.md, "Phase 9:
        # publication"). Every other caller leaves it off, because a set with nothing to deliver is an
        # empty review.
        [switch] $AllowEmpty
    )

    if ($Finding.Count -eq 0) {
        if ($AllowEmpty) { return }
        throw "The approved finding artifact at '$Path' holds no findings, so there is nothing to publish."
    }

    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($item in $Finding) {
        $id = [string] (Get-JsonProperty -Node $item -Name 'id')
        if (-not $seen.Add($id)) {
            throw "The approved finding artifact at '$Path' holds more than one finding '$id'. Two comments would be published under one identifier."
        }
        Assert-FindingShape -Finding $item -Context "'$Path' (finding '$id')" -IncludeToolingField $true
    }
}

# Refuses a merge approval of the running account's own pull request.
#
# Approving the merge of a pull request one opened is not an independent review verdict, so the audit
# does not offer it and publication does not record it (docs/design.md, "Review publication"). On
# GitHub the platform also refuses such an approval outright: a submitted APPROVE returns 422, at the
# completion step after every finding comment has been posted, so a run that reached it would already
# have written to the pull request before failing. Azure DevOps would accept the approval, but the
# review withholds it there too. This refusal joins the preconditions checked before the first
# publication write (SKILL.md, "Phase 9: publication"), guarding the case where a stale or hand-edited
# merge decision reaches publication after the audit withheld the option.
function Assert-PullRequestMergeApproval {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Context,

        [switch] $ApproveMerge
    )

    if (-not $ApproveMerge) { return }

    $pullRequest = Get-JsonProperty -Node $Context -Name 'pullRequest'
    $isSelf = $false
    if ($null -ne $pullRequest) { $isSelf = [bool] (Get-JsonProperty -Node $pullRequest -Name 'isAuthoredBySelf') }
    if ($isSelf) {
        throw "This pull request was opened by the account this run authenticates as, and a reviewer does not approve the merge of their own pull request. Publish the findings without approving the merge."
    }
}

# Reports how many approved findings answer an existing thread, which the summary states.
function Get-FindingFollowUpCount {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding
    )

    $count = 0
    foreach ($item in $Finding) {
        if ($null -ne (Get-JsonProperty -Node $item -Name 'followUp')) { $count++ }
    }
    $count
}
