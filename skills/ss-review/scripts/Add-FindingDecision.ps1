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

    # A versioned decisions plan. Reason and feedback are optional.
    [string] $PlanPath,

    # Restricts worker writes to policy classification, never user verdicts or feedback.
    [switch] $PolicyClassification,

    [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"

if ($AuditFeedbackPath -and ($PolicyClassification -or $Feedback -or $PlanPath -or -not $FindingId)) {
    throw '-AuditFeedbackPath requires an individual user decision without inline feedback.'
}
$decisionsPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath((Join-Path $OutputDirectory 'audit-decisions.json'))
$arguments = @('audit-add-decisions', "--path=$decisionsPath", "--recorded-utc=$($NowUtc.UtcDateTime.ToString('o', [cultureinfo]::InvariantCulture))")
if ($PolicyClassification) { $arguments += '--policy-classification' }
if ($AuditFeedbackPath) {
    $feedbackPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($AuditFeedbackPath)
    $arguments += "--audit-feedback-path=$feedbackPath"
}
if ($PlanPath) {
    if ($FindingId -or $Decision -or $Reason -or $Feedback) { throw 'Supply either a plan or an individual decision, not both.' }
    $plan = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($PlanPath)
    Invoke-NativeReviewWorkflow -Arguments ($arguments + "--plan-path=$plan")
} else {
    if (-not $FindingId -or -not $Decision) { throw 'Supply a finding and decision, or a plan.' }
    # ValidateSet accepts mixed case; the decision journal uses canonical lowercase values.
    $entry = [ordered]@{ findingId = $FindingId; decision = $Decision.ToLowerInvariant() }
    if (-not [string]::IsNullOrWhiteSpace($Reason)) { $entry.reason = $Reason }
    if (-not [string]::IsNullOrWhiteSpace($Feedback)) { $entry.feedback = $Feedback }
    Invoke-NativeReviewWorkflowWithJson -Arguments $arguments -PathOption 'plan-path' `
        -Value ([ordered]@{ schemaVersion = 1; decisions = @($entry) })
}
