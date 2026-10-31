#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Root,
    [Parameter(Mandatory)][string] $Mode,
    [Parameter(Mandatory)][string] $Artifact,
    [Parameter(Mandatory)][string] $Decisions,
    [Parameter(Mandatory)][string] $Context,
    [Parameter(Mandatory)][string] $Diff
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$library = Join-Path $Root 'skills/ss-review/scripts/lib'
. (Join-Path $library 'JsonValue.ps1')
. (Join-Path $library 'PullRequestIdentity.ps1')
. (Join-Path $library 'PullRequestModel.ps1')
. (Join-Path $library 'WorkflowData.ps1')
. (Join-Path $library 'FindingModel.ps1')
. (Join-Path $library 'NativeWorkflowBridge.ps1')
. (Join-Path $library 'ReviewAuditState.ps1')
. (Join-Path $library 'GitDiff.ps1')
. (Join-Path $library 'PullRequestAnchor.ps1')

# The fixture paths are already canonical; comparison itself remains in the predecessor.
function Invoke-SkillsGit {
    param([string[]] $ArgumentList, [string] $StandardInput = '')
    if ($ArgumentList[0] -cne 'path') { throw 'Unexpected Git operation.' }
    [ordered]@{ path = $ArgumentList[2] }
}

# Contending operations wait for the test to finish the other transaction.
$script:contentionCount = 0
function Start-Sleep {
    param([int] $Milliseconds)
    $script:contentionCount++
    if ($script:contentionCount -ne 1) { throw 'Unexpected repeated contention.' }
    [Console]::WriteLine('contended')
    if ([Console]::ReadLine() -cne 'retry') { throw 'Expected retry signal.' }
}

if ($Mode -ceq 'read-text') {
    ConvertTo-Json -InputObject (Read-PullRequestFileText -Path $Diff) -Compress
    return
}

$artifactValue = Read-FindingArtifact -Path $Artifact
$capture = Read-PullRequestContextDocument -Path $Context -WithoutAuthorship
$diffValue = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $Diff)

switch ($Mode) {
    'seed' {
        $document = New-FindingDecisionDocument
        $requests = @(
            [ordered]@{ findingId = 'F-1'; decision = 'auto-approved'; reason = ''; feedback = '' }
            [ordered]@{ findingId = 'F-2'; decision = 'approved'; reason = ''; feedback = '' }
            [ordered]@{ findingId = 'F-3'; decision = 'approved'; reason = ''; feedback = '' }
            [ordered]@{ findingId = 'F-4'; decision = 'needs-audit'; reason = ''; feedback = '' }
        )
        Add-FindingDecisionBatch -Document $document -Request $requests `
            -NowUtc ([DateTimeOffset]::Parse('2024-05-06T09:10:11Z')) | Out-Null
        Write-FindingDecisionDocument -Path $Decisions -Document $document
    }
    'compare' {
        $document = Read-FindingDecisionDocument -Path $Decisions
        $work = Get-FindingAutoApprovalWork -Artifact $artifactValue -Decision $document
        $approved = $null
        $refused = $false
        try {
            $approved = New-ApprovedFindingDocument -Artifact $artifactValue -Decision $document -Context $capture
        } catch {
            if ($_.Exception.Message -notlike '*no decision*') { throw }
            $refused = $true
        }
        $unanchored = @()
        if ($null -ne $approved) {
            $unanchored = Remove-ApprovedFindingOutsideDiff -Document $approved -Diff $diffValue
        }
        $resolved = Resolve-PullRequestAnchorInDiff -Diff $diffValue -Anchor ([ordered]@{
            kind = 'line'; path = 'src/lib.rs'; startLine = 300; endLine = 305; side = 'right'
        })
        [ordered]@{ work = $work; refused = $refused; approved = $approved
            unanchored = $unanchored; resolved = $resolved } | ConvertTo-Json -Depth 100 -Compress
    }
    'merge-seed' {
        Set-ReviewMergeApproval -Path $Decisions -Context $capture -ApproveMerge $true | Out-Null
    }
    'merge-check' {
        Test-ReviewMergeApproval -Path $Decisions -PullRequestUrl $capture['pullRequestUrl'] `
            -SourceCommit $capture['pullRequest']['reviewedSourceCommit'] | ConvertTo-Json -Compress
    }
    'feedback-seed' {
        Add-ReviewAuditFeedback -Path $Decisions -FindingsPath $Artifact -ResponseId 'response-1' `
            -FindingId 'F-1' -Text "  First`nline  "
    }
    'feedback-read' {
        Get-ReviewAuditFeedback -Path $Decisions -FindingId 'F-1' | ConvertTo-Json -Compress
    }
    'feedback-contender' {
        Add-WorkflowListItem -Kind auditFeedback -Path $Decisions -Initialize -Item ([ordered]@{
            responseId = 'powershell'; findingId = 'F-1'; text = 'PowerShell response'
        })
        [Console]::WriteLine('complete')
    }
    'decision-contender' {
        $lock = Open-WorkflowDataLock -Path $Decisions
        try {
            $document = Open-FindingDecisionDocument -Path $Decisions
            Add-FindingDecisionRecord -Document $document -FindingId 'F-2' -Decision 'approved' `
                -NowUtc ([DateTimeOffset]::Parse('2024-05-06T09:10:11Z')) | Out-Null
            Write-FindingDecisionDocument -Path $Decisions -Document $document
        } finally { $lock.Dispose() }
        [Console]::WriteLine('complete')
    }
    default { throw 'Unexpected test mode.' }
}
