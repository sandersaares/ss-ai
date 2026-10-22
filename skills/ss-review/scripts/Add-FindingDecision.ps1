#Requires -Version 7.6

# Records policy classification or an audit verdict (SKILL.md, "Phase 6: auto-approval";
# "Phase 7: audit"). `needs-audit` leaves a finding awaiting the user's decision.
#
# The log is append-only and the last entry for a finding is the one that stands, so a later phase
# can drop a finding the audit approved without rewriting what the user was told at the time. It is
# also what makes an interrupted audit resumable: the decisions already reached are on disk.
#
# A request to clarify or amend a finding is not a decision. The orchestrator applies it with
# Update-Finding.ps1 and records a decision once the user settles, carrying the user's own words as
# feedback so that the pass which improves the review's instructions has them.
#
# This is a composition root: it dot-sources the libraries and reaches no decision a library could
# reach, so every rule it enforces is exercisable offline.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. The log goes to a file and errors to the error stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    # The session-state directory the run keeps its records in. The log is written beneath it.
    [Parameter(Mandatory)]
    [string] $OutputDirectory,

    # The finding decided. Either this with -Decision, or -PlanPath.
    [string] $FindingId,

    [ValidateSet('approved', 'auto-approved', 'needs-audit', 'rejected', 'dropped')]
    [string] $Decision,

    # Why the run reached this decision.
    [string] $Reason,

    # The user's own words, as they supplied them.
    [string] $Feedback,

    # Reloads durable user responses without making the workflow deserialize or join them.
    [string] $AuditFeedbackPath,

    # A JSON file holding `{ decisions: [ { findingId, decision, reason, feedback } ] }`, where the
    # reason and the feedback are optional.
    [string] $PlanPath,

    # Restricts worker writes to policy classification, never user verdicts or feedback.
    [switch] $PolicyClassification,

    [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"
. "$PSScriptRoot\lib\ReviewAuditState.ps1"

if ($AuditFeedbackPath) {
    if ($PolicyClassification -or $Feedback -or $PlanPath -or -not $FindingId) {
        throw '-AuditFeedbackPath requires an individual user decision without inline feedback.'
    }
    $Feedback = Get-ReviewAuditFeedback -Path $AuditFeedbackPath -FindingId $FindingId
}

$requests = Get-FindingDecisionRequest -FindingId $FindingId -Decision $Decision -Reason $Reason -Feedback $Feedback -PlanPath $PlanPath

$decisionsPath = Get-FindingDecisionPath -OutputDirectory $OutputDirectory
$lock = Open-WorkflowDataLock -Path $decisionsPath
try {
    $document = Open-FindingDecisionDocument -Path $decisionsPath
    $recorded = Add-FindingDecisionBatch -Document $document -Request $requests -PolicyClassification:$PolicyClassification -NowUtc $NowUtc

    # Rejected batches leave the previously committed decisions unchanged.
    Write-FindingDecisionDocument -Path $decisionsPath -Document $document
} finally { $lock.Dispose() }

[ordered]@{
    decisionsPath = $decisionsPath
    recorded      = $recorded
} | ConvertTo-Json -Depth 32 -Compress
