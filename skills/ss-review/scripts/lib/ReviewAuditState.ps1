#Requires -Version 7.6

# Durable user feedback and commit-bound merge decisions. Callers load WorkflowData.ps1 and
# FindingModel.ps1. These helpers validate records, not the human authority to create them.
# Ref: docs/implementation.md, "Machine-readable handoffs".
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Reads verbatim feedback in arrival order without interpreting user text as shell expressions.
function Get-ReviewAuditFeedback {
    param([Parameter(Mandatory)][string] $Path, [Parameter(Mandatory)][string] $FindingId)
    $document = Read-WorkflowData -Kind auditFeedback -Path $Path
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($item in $document['feedback']) {
        if (-not $seen.Add("$($item['responseId'])`0$($item['findingId'])")) { throw 'Duplicate audit feedback response.' }
    }
    @($document['feedback'] | Where-Object { $_['findingId'] -ceq $FindingId } | ForEach-Object { $_['text'] }) -join "`n`n"
}

# Records a human response once per finding and response ID; clarification is not an approval.
function Add-ReviewAuditFeedback {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][string] $FindingsPath,
        [Parameter(Mandatory)][string] $ResponseId,
        [Parameter(Mandatory)][string] $FindingId,
        [Parameter(Mandatory)][string] $Text
    )
    $artifact = Read-FindingArtifact -Path $FindingsPath
    Find-FindingIndex -Finding (Get-JsonArrayProperty -Node $artifact -Name 'findings') -FindingId $FindingId | Out-Null
    # Initialization and append share one lock so concurrent first responses cannot lose feedback.
    Add-WorkflowListItem -Kind auditFeedback -Path $Path -Initialize -Item ([ordered]@{
        responseId = $ResponseId; findingId = $FindingId; text = $Text
    })
}

# Publication reads the record itself instead of trusting an agent-translated approval switch.
function Test-ReviewMergeApproval {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][string] $PullRequestUrl,
        [Parameter(Mandatory)][string] $SourceCommit
    )
    $document = Read-WorkflowData -Kind mergeApproval -Path $Path
    if ($document['pullRequestUrl'] -cne $PullRequestUrl) { throw 'Merge approval belongs to a different pull request.' }
    # A refusal conveys no approval authority to carry across commits.
    if (-not $document['approveMerge']) { return $false }
    if ($document['approvedSourceCommit'] -cne $SourceCommit) { throw 'Merge approval belongs to a different source commit. Revalidate or obtain a new decision.' }
    $document['approveMerge']
}

# Captures the selected commit mechanically. Carry-forward can only advance an existing approval
# bound to the supplied baseline; deciding whether revalidation permits it remains orchestration.
function Set-ReviewMergeApproval {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][Collections.IDictionary] $Context,
        [bool] $ApproveMerge,
        [Collections.IDictionary] $PreviousContext,
        [switch] $CarryForward
    )
    $lock = Open-WorkflowDataLock -Path $Path
    try {
        if ($CarryForward) {
            if ($null -eq $PreviousContext) { throw 'Carry-forward requires the previous capture.' }
            if ($PreviousContext['pullRequestUrl'] -cne $Context['pullRequestUrl']) { throw 'Captures name different pull requests.' }
            if (-not (Test-Path -LiteralPath $Path)) { return 'NoApproval' }
            $existing = Read-WorkflowData -Kind mergeApproval -Path $Path
            # A lost success response may be retried with the same old/new captures.
            if ($existing['pullRequestUrl'] -ceq $Context['pullRequestUrl'] -and
                $existing['approvedSourceCommit'] -ceq $Context['pullRequest']['reviewedSourceCommit'] -and
                $existing['approveMerge']) { return 'Recorded' }
            $ApproveMerge = Test-ReviewMergeApproval -Path $Path -PullRequestUrl $PreviousContext['pullRequestUrl'] `
                -SourceCommit $PreviousContext['pullRequest']['reviewedSourceCommit']
            if (-not $ApproveMerge) { return 'NoApproval' }
        } elseif (Test-Path -LiteralPath $Path) {
            $existing = Read-WorkflowData -Kind mergeApproval -Path $Path
            if ($existing['pullRequestUrl'] -cne $Context['pullRequestUrl']) { throw 'Merge decision belongs to a different pull request.' }
        }
        if ($ApproveMerge -and $Context['pullRequest']['isAuthoredBySelf']) { throw 'Cannot approve the running account''s own pull request.' }
        $document = [ordered]@{
            schemaVersion = 1
            pullRequestUrl = $Context['pullRequestUrl']
            approveMerge = $ApproveMerge
            approvedSourceCommit = $Context['pullRequest']['reviewedSourceCommit']
        }
        Assert-WorkflowData -Kind mergeApproval -Value $document
        Write-PullRequestOutputFileAtomically -Path $Path -Content (ConvertTo-JsonText -Value $document)
        'Recorded'
    } finally { $lock.Dispose() }
}
