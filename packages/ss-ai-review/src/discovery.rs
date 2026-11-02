use std::collections::HashSet;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::Path;

use jiff::Timestamp;
use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::ado_approval::Evidence as AdoEvidence;
use crate::api::Api;
use crate::approval::{Decision, Evidence};
use crate::assessment::{Assessment, Judgment};
use crate::assessment_budget::{AssessmentBudget, BudgetStatus};
use crate::clock::{is_recent, timestamp};
use crate::discussion;
use crate::error::{classification, fail};
use crate::github_approval::Evidence as GitHubEvidence;
use crate::journal::{FileJournal, read_bytes};
use crate::json::{boolean, field, identifier, required_optional, text};
use crate::objects;
use crate::publication::{self, Outcome, Receipt, Scope};
use crate::renewal::Anchor;
use crate::review_notice::{self, Notice};
use crate::target::{Platform, Target};
use crate::{Clock, Process};
use ss_ai_assessment::{BatchJudgmentRequest, BatchReadRequest, ContextRequest, Note};

/// Provider-selected approval baselines require a fresh assessment contract.
const RUN_SCHEMA: u32 = 15;

/// Durable metadata-only stage; recent candidates still require approval assessment.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscoveryRecord {
    schema_version: u32,
    run_id: String,
    repository_url: String,
    repository_id: String,
    platform: Platform,
    account_id: String,
    utility_version: String,
    #[serde(deserialize_with = "required_optional")]
    tenant: Option<String>,
    policy: Policy,
    dry_run: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    assessment_budget_enabled: bool,
    enumeration_complete: bool,
    state: RunState,
    inventory: Vec<Entry>,
    #[serde(deserialize_with = "required_optional")]
    error: Option<Diagnostic>,
    /// Recovery must not clear a fatal diagnostic originating from another PR.
    #[serde(deserialize_with = "required_optional")]
    error_pull_request_id: Option<NonZeroU32>,
}

/// Keeps the trusted policy bytes and digest together across interrupted runs.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Policy {
    source_path: String,
    sha256: String,
    content: String,
}

/// Each nondraft PR has a recency record, even if it never becomes a candidate.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    pull_request_id: NonZeroU32,
    url: String,
    title: String,
    draft: bool,
    recency: Recency,
    #[serde(deserialize_with = "required_optional")]
    inspection: Option<Inspection>,
    #[serde(deserialize_with = "required_optional")]
    assessment: Option<Assessment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assessment_budget: Option<AssessmentBudget>,
    #[serde(deserialize_with = "required_optional")]
    execution_error: Option<Diagnostic>,
    #[serde(deserialize_with = "required_optional")]
    publication: Option<Receipt>,
    #[serde(deserialize_with = "required_optional")]
    review_notice: Option<Notice>,
    recovery: Recovery,
}

/// Stale recaptures retain distinct identities for subsequent notice attempts.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Recovery {
    attempt_ids: Vec<String>,
    #[serde(deserialize_with = "required_optional")]
    stale: Option<Diagnostic>,
    #[serde(deserialize_with = "required_optional")]
    error: Option<Diagnostic>,
}

/// Permit repeated movement to settle without allowing a continuously changing PR to loop.
const STALE_RECOVERY_LIMIT: usize = 2;

/// Inspection is either a complete immutable observation or an explicit failure.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Inspection {
    captured_at: String,
    #[serde(deserialize_with = "required_optional")]
    snapshot_digest: Option<String>,
    #[serde(deserialize_with = "required_optional")]
    evidence: Option<Evidence>,
    #[serde(deserialize_with = "required_optional")]
    decision: Option<Decision>,
    #[serde(deserialize_with = "required_optional")]
    error: Option<Diagnostic>,
}

/// Required nullable fields distinguish unfinished work from proven exclusion.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Recency {
    state: RecencyState,
    #[serde(deserialize_with = "required_optional")]
    checked_at: Option<String>,
    #[serde(deserialize_with = "required_optional")]
    latest_nonfuture_commit: Option<String>,
    evidence_complete: bool,
    #[serde(deserialize_with = "required_optional")]
    error: Option<Diagnostic>,
}

/// Stable failure categories remain machine-readable without parsing a backtrace.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Diagnostic {
    kind: String,
    message: String,
}

/// Processing is intentionally not completion: approval history has not been read.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum RunState {
    Enumerating,
    Processing,
    Complete,
    Partial,
    Failed,
}

/// Omit the new flag when retaining a run created before budget enforcement.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde calls serialization predicates with a shared reference"
)]
fn is_false(value: &bool) -> bool {
    !value
}

/// Recency never turns malformed timestamp evidence into an old-PR exclusion.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum RecencyState {
    Pending,
    Recent,
    NoRecentCommit,
    Failed,
}

impl DiscoveryRecord {
    /// Each refreshed PR selects its own intent without changing other PRs' recovery.
    fn operation_run_id(&self, number: NonZeroU32) -> Result<&str, AppError> {
        self.inventory
            .iter()
            .find(|entry| entry.pull_request_id == number)
            .map(|entry| entry.operation_run_id(&self.run_id))
            .ok_or_else(|| fail("invalidInput", "Unknown PR"))
    }

    fn publication_scope(&self, number: NonZeroU32) -> Scope {
        Scope {
            repository_id: self.repository_id.clone(),
            account_id: self.account_id.clone(),
            pull_request_id: number.get(),
        }
    }

    fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != RUN_SCHEMA
            || Uuid::parse_str(&self.run_id).is_err()
            || self.repository_id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.utility_version.trim().is_empty()
            || self.policy.content.trim().is_empty()
            || !Path::new(&self.policy.source_path).is_absolute()
            || digest(self.policy.content.as_bytes()) != self.policy.sha256
        {
            return Err(fail(
                "invalidState",
                "Invalid discovery identity, schema, or captured policy",
            ));
        }
        let target = Target::repository(&self.repository_url)?;
        if target.platform != self.platform {
            return Err(fail(
                "invalidState",
                "Discovery platform disagrees with repository",
            ));
        }
        let mut previous = None;
        for entry in &self.inventory {
            entry.recovery.validate(&self.run_id)?;
            if entry
                .assessment_budget
                .as_ref()
                .is_some_and(|budget| !budget.valid())
                || !self.assessment_budget_enabled && entry.assessment_budget.is_some()
                || self.assessment_budget_enabled
                    && entry.assessment.is_some()
                    && entry.assessment_budget.is_none()
            {
                return Err(fail("invalidState", "Invalid assessment budget record"));
            }
            // Numeric pagination requires a strictly ordered, duplicate-free inventory.
            if previous.is_some_and(|id| id >= entry.pull_request_id)
                || entry.url != target.candidate(entry.pull_request_id).web_url
                || entry.title.trim().is_empty()
            {
                return Err(fail(
                    "invalidState",
                    "Unordered, duplicate, or invalid discovery PR metadata",
                ));
            }
            entry.recency.validate()?;
            if let Some(inspection) = &entry.inspection {
                if entry.draft || entry.recency.state != RecencyState::Recent {
                    return Err(fail(
                        "invalidState",
                        "Inspection requires an eligible discovery entry",
                    ));
                }
                if let Some(assessment) = &entry.assessment {
                    let (snapshot, evidence) = entry.assessment_inputs()?;
                    assessment.validate(
                        snapshot,
                        &self.policy.sha256,
                        &self.policy.content,
                        &discussion::render(evidence)?,
                    )?;
                    if assessment.status(false) == "failed" && entry.execution_error.is_none() {
                        return Err(fail(
                            "invalidState",
                            "Uncertain evaluation requires an explicit execution failure",
                        ));
                    }
                    let (old, new) = entry.source_endpoints()?;
                    if assessment.inventory.old_commit != old
                        || assessment.inventory.new_commit != new
                    {
                        return Err(fail(
                            "invalidState",
                            "Assessment has another source comparison",
                        ));
                    }
                }
                if let Some(error) = &entry.execution_error {
                    entry.assessment_inputs()?;
                    error.validate()?;
                }
                if let Some(receipt) = &entry.publication {
                    receipt.validate(self.platform)?;
                    if self.dry_run
                        || receipt.run_id != entry.operation_run_id(&self.run_id)
                        || receipt.scope != self.publication_scope(entry.pull_request_id)
                        || entry.execution_error.is_some()
                        || entry
                            .assessment
                            .as_ref()
                            .is_none_or(|assessment| assessment.status(false) != "readyToReapprove")
                        || entry
                            .assessment
                            .as_ref()
                            .and_then(|assessment| assessment.digest.as_ref())
                            != Some(&receipt.assessment_digest)
                    {
                        return Err(fail(
                            "invalidState",
                            "Publication receipt has another assessment scope",
                        ));
                    }
                }
                if let Some(notice) = &entry.review_notice {
                    timestamp(&notice.created_at)?;
                    if self.dry_run
                        || notice.comment_id.trim().is_empty()
                        || notice.rationale.trim().is_empty()
                        || entry
                            .assessment
                            .as_ref()
                            .is_none_or(|assessment| !assessment.needs_review())
                    {
                        return Err(fail(
                            "invalidState",
                            "Invalid published needs-review notice",
                        ));
                    }
                }
                inspection.validate(
                    self.platform,
                    &self.account_id,
                    &self.repository_id,
                    entry.pull_request_id.get(),
                    entry.recovery.attempt_ids.last().map(String::as_str),
                )?;
            } else if entry.assessment.is_some()
                || entry.execution_error.is_some()
                || entry.publication.is_some()
                || entry.review_notice.is_some()
            {
                return Err(fail(
                    "invalidState",
                    "Assessment and publication require inspection evidence",
                ));
            }
            previous = Some(entry.pull_request_id);
        }
        let pending = self
            .inventory
            .iter()
            .any(|entry| entry.recency.state == RecencyState::Pending);
        let failed = self.inventory.iter().any(Entry::failed);
        let candidates = self
            .inventory
            .iter()
            .any(|entry| entry.candidate(self.dry_run));
        let valid = match self.state {
            RunState::Enumerating => {
                !self.enumeration_complete && self.inventory.is_empty() && self.error.is_none()
            }
            RunState::Processing => {
                self.enumeration_complete
                    && self.error.is_none()
                    && (pending || (!failed && candidates))
            }
            RunState::Complete => {
                self.enumeration_complete
                    && !pending
                    && !failed
                    && !candidates
                    && self.error.is_none()
            }
            RunState::Partial => {
                self.enumeration_complete && !pending && failed && self.error.is_none()
            }
            RunState::Failed => {
                self.error.is_some() && (self.enumeration_complete || self.inventory.is_empty())
            }
        };
        if !valid {
            return Err(fail(
                "invalidState",
                "Discovery lifecycle disagrees with its inventory",
            ));
        }
        if let Some(error) = &self.error {
            error.validate()?;
        }
        if let Some(number) = self.error_pull_request_id
            && (self.error.is_none()
                || !self
                    .inventory
                    .iter()
                    .any(|entry| entry.pull_request_id == number))
        {
            return Err(fail("invalidState", "Run failure has an invalid PR origin"));
        }
        Ok(())
    }

    fn save(&self, journal: &FileJournal<'_>) -> Result<(), AppError> {
        self.validate()?;
        journal.write_bytes(&serde_json::to_vec_pretty(self)?)
    }

    /// Remove an excluded PR without returning another candidate as its result.
    fn remove_draft(
        &mut self,
        number: NonZeroU32,
        journal: &FileJournal<'_>,
    ) -> Result<Value, AppError> {
        let index = self
            .inventory
            .iter()
            .position(|entry| entry.pull_request_id == number)
            .ok_or_else(|| fail("invalidState", "Draft PR is missing from inventory"))?;
        self.inventory.remove(index);
        finish_state(self);
        self.save(journal)?;
        let mut summary = self.summary(0, NonZeroUsize::MIN)?;
        *summary
            .get_mut("items")
            .ok_or_else(|| fail("invalidState", "Summary lacks items"))? = json!([]);
        *summary
            .get_mut("nextAfter")
            .ok_or_else(|| fail("invalidState", "Summary lacks pagination"))? = Value::Null;
        Ok(summary)
    }

    fn load(journal: &FileJournal<'_>) -> Result<Option<Self>, AppError> {
        let Some(bytes) = journal.read_bytes()? else {
            return Ok(None);
        };
        let document: Value = serde_json::from_slice(&bytes).map_err(|error| {
            crate::error::caused_by(
                "invalidState",
                "Malformed discovery record".to_owned(),
                error,
            )
        })?;
        // Earlier contracts cannot establish that judgments considered all requested context.
        if document.get("schemaVersion") != Some(&json!(RUN_SCHEMA)) {
            return Err(fail(
                "invalidState",
                "Unsupported discovery schema; start a fresh assessment",
            ));
        }
        let mut run: Self = serde_json::from_value(document).map_err(|error| {
            crate::error::caused_by(
                "invalidState",
                "Malformed discovery record".to_owned(),
                error,
            )
        })?;
        run.validate().map_err(|error| {
            crate::error::caused_by(
                "invalidState",
                "Invalid discovery record".to_owned(),
                error.into_std_error(),
            )
        })?;
        // Older runs can contain drafts from discovery or an inspection; neither
        // can have a publication attempt, so they can be removed safely.
        let previous_len = run.inventory.len();
        run.inventory.retain(|entry| {
            !entry.draft
                && !matches!(entry.decision(), Some(Decision::Ineligible { reason }) if reason == "draft")
        });
        if run.inventory.len() != previous_len {
            if run.state != RunState::Failed {
                finish_state(&mut run);
            }
            run.save(journal)?;
        }
        Ok(Some(run))
    }

    /// Bounded summaries never expand older PRs into per-PR agent work.
    fn summary(&self, after: u32, limit: NonZeroUsize) -> Result<Value, AppError> {
        let included = self
            .inventory
            .iter()
            .filter(|entry| {
                matches!(
                    entry.recency.state,
                    RecencyState::Recent | RecencyState::Failed
                ) && entry.pull_request_id.get() > after
            })
            .collect::<Vec<_>>();
        let page = included
            .iter()
            .take(limit.get())
            .map(|entry| {
                let decision = entry.decision();
                let progress = entry
                    .assessment
                    .as_ref()
                    .map(|assessment| {
                        let (_, evidence) = entry.assessment_inputs()?;
                        let discussion = discussion::render(evidence)?;
                        Ok::<_, AppError>(assessment.progress_with_evidence(&self.policy.content, &discussion)?)
                    })
                    .transpose()?;
                Ok::<_, AppError>(json!({
                    "pullRequestId": entry.pull_request_id, "url": entry.url,
                    "title": entry.title,
                    "status": entry.status(self.dry_run),
                    "reason": decision.and_then(Decision::reason),
                    "assessmentReason": entry.assessment.as_ref().and_then(Assessment::rationale),
                    "approvalEventId": decision.and_then(Decision::approval_event_id),
                    "capturedAt": entry.inspection.as_ref().map(|inspection| &inspection.captured_at),
                    "snapshotDigest": entry.inspection.as_ref().and_then(|inspection| inspection.snapshot_digest.as_ref()),
                    "assessmentDigest": entry.assessment.as_ref().and_then(|assessment| assessment.digest.as_ref()),
                    "assessmentProgress": progress,
                    "assessmentBudget": entry.assessment_budget.as_ref().map(AssessmentBudget::diagnostics),
                    "publication": entry.publication,
                    "reviewNotice": entry.review_notice.as_ref().or(match decision {
                        Some(Decision::NeedsReview { notice }) => Some(notice),
                        _ => None,
                    }),
                    "staleRecovery": {
                        "attempts": entry.recovery.attempt_ids.len(),
                        "limit": STALE_RECOVERY_LIMIT,
                        "exhausted": entry.recovery.error.is_some()
                    },
                    "error": entry.recovery.error.as_ref().or(entry.recovery.stale.as_ref()).or(entry.execution_error.as_ref()).or_else(|| entry.inspection.as_ref()?.error.as_ref()).or(entry.recency.error.as_ref())
                }))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next = if included.len() > limit.get() {
            included
                .get(limit.get().saturating_sub(1))
                .map(|entry| entry.pull_request_id)
        } else {
            None
        };
        Ok(json!({
            "schemaVersion": self.schema_version, "runId": self.run_id,
            "repositoryUrl": self.repository_url, "state": self.state,
            "enumerationComplete": self.enumeration_complete, "dryRun": self.dry_run,
            "policyDigest": self.policy.sha256,
            "counts": {
                "open": self.inventory.len(),
                "noRecentCommit": self.inventory.iter().filter(|entry| entry.recency.state == RecencyState::NoRecentCommit).count(),
                "assessmentRequired": self.inventory.iter().filter(|entry| entry.candidate(self.dry_run) && entry.status(self.dry_run) == "assessmentRequired").count(),
                "needsReview": self.inventory.iter().filter(|entry| entry.status(self.dry_run) == "needsReview").count(),
                "wouldReapprove": self.inventory.iter().filter(|entry| entry.status(self.dry_run) == "wouldReapprove").count(),
                "readyToReapprove": self.inventory.iter().filter(|entry| entry.status(self.dry_run) == "readyToReapprove").count(),
                "reapproved": self.inventory.iter().filter(|entry| entry.status(self.dry_run) == "reapproved").count(),
                "failed": self.inventory.iter().filter(|entry| entry.failed() && !entry.stale()).count(),
                "staleDuringExecution": self.inventory.iter().filter(|entry| entry.stale()).count(),
                "pending": self.inventory.iter().filter(|entry| entry.recency.state == RecencyState::Pending).count(),
                "alreadyValid": self.inventory.iter().filter(|entry| entry.status(self.dry_run) == "alreadyValid").count(),
                "noPriorApproval": self.inventory.iter().filter(|entry| matches!(entry.decision(), Some(Decision::NoPriorApproval))).count(),
                "ineligible": self.inventory.iter().filter(|entry| entry.status(self.dry_run) == "ineligible").count(),
                "indeterminate": self.inventory.iter().filter(|entry| entry.status(self.dry_run) == "indeterminate").count()
            },
            "items": page, "nextAfter": next, "error": self.error
        }))
    }
}

impl Entry {
    /// The latest recapture identifies its notice attempt independently of the run.
    fn operation_run_id<'a>(&'a self, run_id: &'a str) -> &'a str {
        self.recovery
            .attempt_ids
            .last()
            .map_or(run_id, String::as_str)
    }

    fn candidate(&self, dry_run: bool) -> bool {
        self.publication.is_none()
            && !self.failed()
            && !self.draft
            && self.recency.state == RecencyState::Recent
            && (self.inspection.is_none()
                || matches!(self.decision(), Some(Decision::AssessmentRequired { .. }))
                    && self.assessment.as_ref().is_none_or(|assessment| {
                        matches!(
                            assessment.status(dry_run),
                            "assessmentRequired" | "readyToReapprove"
                        ) || !dry_run
                            && assessment.status(false) == "needsReview"
                            && self.review_notice.is_none()
                    }))
    }

    fn status(&self, dry_run: bool) -> &'static str {
        match self.recency.state {
            RecencyState::Pending => return "pending",
            RecencyState::NoRecentCommit => return "noRecentCommit",
            _ => {}
        }
        if self.recovery.error.is_some() {
            return "failed";
        }
        if let Some(receipt) = &self.publication {
            return receipt.outcome.as_str();
        }
        if self
            .assessment_budget
            .as_ref()
            .is_some_and(AssessmentBudget::exhausted)
        {
            return "failed";
        }
        if self.stale() {
            "staleDuringExecution"
        } else if self.failed() {
            "failed"
        } else if self.draft {
            "ineligible"
        } else if let Some(assessment) = &self.assessment {
            assessment.status(dry_run)
        } else if let Some(decision) = self.decision() {
            decision.status()
        } else {
            "assessmentRequired"
        }
    }

    fn assessment_inputs(&self) -> Result<(&str, &Evidence), AppError> {
        if !matches!(self.decision(), Some(Decision::AssessmentRequired { .. })) {
            return Err(fail(
                "invalidState",
                "PR has no source-bound renewal eligibility",
            ));
        }
        let inspection = self
            .inspection
            .as_ref()
            .ok_or_else(|| fail("invalidState", "Missing inspection"))?;
        Ok((
            inspection
                .snapshot_digest
                .as_deref()
                .ok_or_else(|| fail("invalidState", "Missing snapshot digest"))?,
            inspection
                .evidence
                .as_ref()
                .ok_or_else(|| fail("invalidState", "Missing snapshot evidence"))?,
        ))
    }

    fn source_endpoints(&self) -> Result<(&str, &str), AppError> {
        let (_, evidence) = self.assessment_inputs()?;
        let Some(Decision::AssessmentRequired { anchor, .. }) = self.decision() else {
            return Err(fail(
                "invalidState",
                "Missing provider-selected approval baseline",
            ));
        };
        let source = match evidence {
            Evidence::GitHub(GitHubEvidence::History { pull_request, .. }) => {
                text(field(field(pull_request, "head")?, "sha")?)?
            }
            Evidence::AzureDevOps(AdoEvidence::History { source_commit, .. }) => source_commit,
            _ => {
                return Err(fail(
                    "invalidState",
                    "This provider mode does not require renewal",
                ));
            }
        };
        Ok((&anchor.source_commit, source))
    }

    fn decision(&self) -> Option<&Decision> {
        self.inspection.as_ref()?.decision.as_ref()
    }

    fn failed(&self) -> bool {
        self.publication.as_ref().is_some_and(|receipt| {
            matches!(
                receipt.outcome,
                Outcome::Failed | Outcome::StaleDuringExecution
            )
        }) || self.recovery.error.is_some()
            || self
                .assessment_budget
                .as_ref()
                .is_some_and(AssessmentBudget::exhausted)
            || self.recovery.stale.is_some()
            || self.execution_error.is_some()
            || self.recency.state == RecencyState::Failed
            || self
                .inspection
                .as_ref()
                .is_some_and(|inspection| inspection.error.is_some())
    }

    fn stale(&self) -> bool {
        if self.recovery.error.is_some()
            || self
                .assessment_budget
                .as_ref()
                .is_some_and(AssessmentBudget::exhausted)
        {
            return false;
        }
        if self.recovery.stale.is_some() {
            return true;
        }
        if self
            .publication
            .as_ref()
            .is_some_and(|receipt| receipt.outcome == Outcome::StaleDuringExecution)
        {
            return true;
        }
        self.execution_error
            .as_ref()
            .or_else(|| self.inspection.as_ref()?.error.as_ref())
            .is_some_and(|error| error.kind == "staleEvidence")
    }
}

impl Recovery {
    /// Reject malformed retry state before it can select another publication intent.
    fn validate(&self, run_id: &str) -> Result<(), AppError> {
        let mut ids = HashSet::new();
        if self.attempt_ids.len() > STALE_RECOVERY_LIMIT
            || self
                .attempt_ids
                .iter()
                .any(|id| id == run_id || Uuid::parse_str(id).is_err() || !ids.insert(id))
            || self
                .stale
                .as_ref()
                .is_some_and(|error| error.kind != "staleEvidence")
            || self.error.as_ref().is_some_and(|error| {
                error.kind != "retryExhausted" || self.attempt_ids.len() != STALE_RECOVERY_LIMIT
            })
        {
            return Err(fail(
                "invalidState",
                "Invalid stale-evidence recovery state",
            ));
        }
        for error in self.stale.iter().chain(self.error.iter()) {
            error.validate()?;
        }
        Ok(())
    }
}

impl Inspection {
    fn validate(
        &self,
        platform: Platform,
        account: &str,
        repository: &str,
        number: u32,
        attempt_id: Option<&str>,
    ) -> Result<(), AppError> {
        let observed = timestamp(&self.captured_at)?;
        match (
            &self.evidence,
            &self.decision,
            &self.snapshot_digest,
            &self.error,
        ) {
            (Some(evidence), Some(decision), Some(hash), None) => {
                let current = snapshot_digest(evidence, &self.captured_at, attempt_id)?;
                if evidence.decide(platform, account, repository, number, observed)? != *decision
                    || *hash != current
                {
                    return Err(fail(
                        "invalidState",
                        "Stored inspection disagrees with its snapshot",
                    ));
                }
            }
            (None, None, None, Some(error)) => error.validate()?,
            _ => return Err(fail("invalidState", "Incomplete inspection record")),
        }
        Ok(())
    }
}

impl Recency {
    fn pending() -> Self {
        Self {
            state: RecencyState::Pending,
            checked_at: None,
            latest_nonfuture_commit: None,
            evidence_complete: false,
            error: None,
        }
    }

    fn validate(&self) -> Result<(), AppError> {
        let checked = self.checked_at.as_deref().map(timestamp).transpose()?;
        let latest = self
            .latest_nonfuture_commit
            .as_deref()
            .map(timestamp)
            .transpose()?;
        let valid = match self.state {
            RecencyState::Pending => {
                checked.is_none()
                    && latest.is_none()
                    && !self.evidence_complete
                    && self.error.is_none()
            }
            RecencyState::Failed => {
                checked.is_some()
                    && latest.is_none()
                    && !self.evidence_complete
                    && self.error.is_some()
            }
            RecencyState::Recent | RecencyState::NoRecentCommit => checked.is_some_and(|checked| {
                self.evidence_complete
                    && self.error.is_none()
                    && latest.is_none_or(|latest| latest <= checked)
                    && is_recent(latest, checked) == (self.state == RecencyState::Recent)
            }),
        };
        if !valid {
            return Err(fail(
                "invalidState",
                "Recency state disagrees with its evidence",
            ));
        }
        if let Some(error) = &self.error {
            error.validate()?;
        }
        Ok(())
    }
}

impl Diagnostic {
    fn from_error(error: &AppError) -> Self {
        Self {
            kind: classification(error).to_owned(),
            message: error.to_string(),
        }
    }

    fn validate(&self) -> Result<(), AppError> {
        if !matches!(
            self.kind.as_str(),
            "invalidInput"
                | "invalidState"
                | "authentication"
                | "permissionDenied"
                | "service"
                | "ambiguousWrite"
                | "staleEvidence"
                | "retryExhausted"
                | "scopeChanged"
                | "rateLimited"
                | "contention"
                | "io"
        ) || self.message.trim().is_empty()
        {
            return Err(fail("invalidState", "Invalid discovery diagnostic"));
        }
        Ok(())
    }
}

/// Input ownership remains separate from the persisted record's provider evidence.
pub(crate) struct DiscoveryInput<'a> {
    pub(crate) repository: &'a Target,
    pub(crate) state_path: &'a Path,
    pub(crate) policy_path: &'a Path,
    pub(crate) tenant: Option<&'a str>,
    pub(crate) dry_run: bool,
    pub(crate) limit: NonZeroUsize,
}

/// Initializes or resumes only pending reads; it has no remote write operation.
pub(crate) fn discover(
    api: &mut Api<'_>,
    clock: &dyn Clock,
    input: &DiscoveryInput<'_>,
) -> Result<Value, AppError> {
    if !input.policy_path.is_absolute() || !input.state_path.is_absolute() {
        return Err(fail(
            "invalidInput",
            "Policy and state paths must be absolute",
        ));
    }
    let policy = read_bytes(input.policy_path, clock)?
        .ok_or_else(|| fail("invalidInput", "Significance policy is missing"))?;
    let policy = String::from_utf8(policy).map_err(|error| {
        crate::error::caused_by(
            "invalidInput",
            "Significance policy must be UTF-8".to_owned(),
            error,
        )
    })?;
    if policy.trim().is_empty() || input.policy_path.to_str().is_none() {
        return Err(fail(
            "invalidInput",
            "Supply a nonempty policy at a UTF-8 path",
        ));
    }
    let identity = api.identity()?;
    let account = identity
        .id
        .as_deref()
        .ok_or_else(|| fail("authentication", "No stable account ID"))?;
    let repository = api
        .transport
        .request(&input.repository.repository_api(), "GET", None)?;
    let repository_id = identifier(field(&repository, "id")?)?;
    let journal = FileJournal::open(input.state_path, clock)?;
    let mut run = if let Some(run) = DiscoveryRecord::load(&journal)? {
        if run.repository_url != input.repository.web_url
            || run.repository_id != repository_id
            || run.platform != identity.platform
            || run.account_id != account
            || run.dry_run != input.dry_run
            || run.tenant.as_deref() != input.tenant
            || run.policy.sha256 != digest(policy.as_bytes())
        {
            return Err(fail(
                "invalidState",
                "Discovery repository, identity, policy, tenant, or mode changed",
            ));
        }
        if run.state == RunState::Failed {
            return Err(fail(
                "invalidState",
                "Failed discovery requires a fresh run",
            ));
        }
        run
    } else {
        DiscoveryRecord {
            schema_version: RUN_SCHEMA,
            run_id: Uuid::new_v4().to_string(),
            repository_url: input.repository.web_url.clone(),
            repository_id,
            platform: identity.platform,
            account_id: account.to_owned(),
            utility_version: env!("CARGO_PKG_VERSION").to_owned(),
            tenant: input.tenant.map(str::to_owned),
            policy: Policy {
                source_path: input
                    .policy_path
                    .to_str()
                    .ok_or_else(|| fail("invalidInput", "Policy path must be UTF-8"))?
                    .to_owned(),
                sha256: digest(policy.as_bytes()),
                content: policy,
            },
            dry_run: input.dry_run,
            assessment_budget_enabled: true,
            enumeration_complete: false,
            state: RunState::Enumerating,
            inventory: Vec::new(),
            error: None,
            error_pull_request_id: None,
        }
    };
    run.save(&journal)?;
    if !run.enumeration_complete {
        match metadata(api, input.repository) {
            Ok(inventory) => {
                run.inventory = inventory;
                run.enumeration_complete = true;
                run.state = if run.inventory.is_empty() {
                    RunState::Complete
                } else {
                    RunState::Processing
                };
                run.save(&journal)?;
            }
            Err(error) => {
                run.state = RunState::Failed;
                run.error = Some(Diagnostic::from_error(&error));
                run.save(&journal)?;
                return Err(error);
            }
        }
    }
    for index in 0..run.inventory.len() {
        let entry = run
            .inventory
            .get_mut(index)
            .ok_or_else(|| fail("invalidState", "Inventory changed during scan"))?;
        if entry.recency.state != RecencyState::Pending {
            continue;
        }
        let now = clock.now();
        let target = input.repository.candidate(entry.pull_request_id);
        match api.commit_recency(&target, now) {
            Ok((latest, recent)) => {
                entry.recency = Recency {
                    state: if recent {
                        RecencyState::Recent
                    } else {
                        RecencyState::NoRecentCommit
                    },
                    checked_at: Some(utc(now)),
                    latest_nonfuture_commit: latest.map(utc),
                    evidence_complete: true,
                    error: None,
                };
            }
            Err(error) => {
                let diagnostic = Diagnostic::from_error(&error);
                let scope_failure =
                    diagnostic.kind == "authentication" || diagnostic.kind == "permissionDenied";
                entry.recency = Recency {
                    state: RecencyState::Failed,
                    checked_at: Some(utc(now)),
                    latest_nonfuture_commit: None,
                    evidence_complete: false,
                    error: Some(diagnostic),
                };
                if scope_failure {
                    run.state = RunState::Failed;
                    run.error = Some(Diagnostic::from_error(&error));
                    run.save(&journal)?;
                    return Err(error);
                }
            }
        }
        // Save each completed read so interruption never turns missing work into exclusion.
        finish_state(&mut run);
        run.save(&journal)?;
    }
    run.summary(0, input.limit)
}

/// Captures candidates, or refreshes clean stale assessment attempts.
pub(crate) fn inspect(
    api: &mut Api<'_>,
    clock: &dyn Clock,
    repository: &Target,
    path: &Path,
    tenant: Option<&str>,
    number: NonZeroU32,
    recovery_directory: Option<&Path>,
) -> Result<Value, AppError> {
    if !path.is_absolute() {
        return Err(fail(
            "invalidInput",
            "Inspection requires an absolute state path",
        ));
    }
    // Review notice recovery locks its record before the run.
    let scope = if recovery_directory.is_some() {
        let journal = FileJournal::open(path, clock)?;
        let run = DiscoveryRecord::load(&journal)?
            .ok_or_else(|| fail("invalidState", "Discovery record is missing"))?;
        Some(run.publication_scope(number))
    } else {
        None
    };
    let notices = recovery_directory
        .zip(scope.as_ref())
        .map(|(directory, scope)| {
            let notices = review_notice::Store::open(repository.platform, directory, scope, clock)?;
            notices.ensure_resolved()?;
            Ok::<_, AppError>(notices)
        })
        .transpose()?;
    let journal = FileJournal::open(path, clock)?;
    let mut run = DiscoveryRecord::load(&journal)?
        .ok_or_else(|| fail("invalidState", "Discovery record is missing"))?;
    if run.repository_url != repository.web_url
        || run.tenant.as_deref() != tenant
        || !run.enumeration_complete
        || run.state == RunState::Failed
    {
        return Err(fail(
            "invalidState",
            "Inspection scope or discovery state changed",
        ));
    }
    let index = run
        .inventory
        .iter()
        .position(|entry| entry.pull_request_id == number)
        .ok_or_else(|| {
            fail(
                "invalidInput",
                "PR is not in the complete discovery inventory",
            )
        })?;
    let entry = run
        .inventory
        .get(index)
        .ok_or_else(|| fail("invalidState", "Missing inventory entry"))?;
    if entry.draft || entry.recency.state != RecencyState::Recent {
        return Err(fail(
            "invalidInput",
            "Only recent nondraft PRs may collect approval evidence",
        ));
    }
    let policy = read_bytes(Path::new(&run.policy.source_path), clock)?
        .ok_or_else(|| fail("invalidState", "Captured significance policy is missing"))?;
    if digest(&policy) != run.policy.sha256 {
        return Err(fail(
            "invalidState",
            "Significance policy changed; start a fresh run",
        ));
    }
    if entry.inspection.is_none() || recovery_directory.is_some() {
        assessment_budget_step(
            &mut run,
            index,
            clock,
            &journal,
            BudgetPhase::StartInspection,
        )?;
    }
    let entry = run
        .inventory
        .get(index)
        .ok_or_else(|| fail("invalidState", "Inventory changed"))?;
    let identity = api.identity()?;
    let current_repository = api
        .transport
        .request(&repository.repository_api(), "GET", None)?;
    if identity.id.as_deref() != Some(&run.account_id)
        || identifier(field(&current_repository, "id")?)? != run.repository_id
    {
        return Err(fail(
            "invalidState",
            "Inspection identity or repository changed",
        ));
    }
    if let Some(notices) = &notices {
        if scope.as_ref() != Some(&run.publication_scope(number)) {
            return Err(fail("invalidState", "Refresh scope changed"));
        }
        let operation_run_id = entry.operation_run_id(&run.run_id);
        if notices.has_attempt(operation_run_id)
            || entry
                .publication
                .as_ref()
                .is_some_and(|receipt| receipt.outcome != Outcome::StaleDuringExecution)
        {
            return Err(fail(
                "invalidState",
                "A completed publication cannot refresh its assessment; start a fresh run",
            ));
        }
        if !entry.stale() && entry.recovery.error.is_none() {
            return Err(fail("invalidState", "Only stale evidence permits refresh"));
        }
        let entry = run
            .inventory
            .get_mut(index)
            .ok_or_else(|| fail("invalidState", "Inventory changed"))?;
        if entry.recovery.attempt_ids.len() == STALE_RECOVERY_LIMIT {
            entry.recovery.error = Some(Diagnostic {
                kind: "retryExhausted".to_owned(),
                message: "Stale-evidence recovery exhausted; evidence kept changing after the permitted fresh attempts".to_owned(),
            });
            finish_state(&mut run);
            run.save(&journal)?;
            return Err(fail(
                "retryExhausted",
                "Stale-evidence recovery exhausted; no remote effect was retried",
            ));
        }
        entry.recovery.attempt_ids.push(Uuid::new_v4().to_string());
        entry.recovery.stale = None;
        entry.inspection = None;
        entry.assessment = None;
        entry.execution_error = None;
        entry.publication = None;
        entry.review_notice = None;
        // Persist the consumed budget before reacquisition, including interrupted captures.
        finish_state(&mut run);
        run.save(&journal)?;
    }
    let entry = run
        .inventory
        .get(index)
        .ok_or_else(|| fail("invalidState", "Inventory changed"))?;
    if entry.inspection.is_none() {
        let observation = Evidence::capture(api, &repository.candidate(number));
        let captured_at = utc(clock.now());
        let observation = observation.and_then(|evidence| {
            if api.identity()?.id.as_deref() != Some(&run.account_id) {
                return Err(fail(
                    "authentication",
                    "Authenticated account changed during inspection",
                ));
            }
            let decision = evidence.decide(
                run.platform,
                &run.account_id,
                &run.repository_id,
                number.get(),
                timestamp(&captured_at)?,
            )?;
            let snapshot_digest = snapshot_digest(
                &evidence,
                &captured_at,
                entry.recovery.attempt_ids.last().map(String::as_str),
            )?;
            Ok((evidence, decision, snapshot_digest))
        });
        if matches!(&observation, Ok((_, Decision::Ineligible { reason }, _)) if reason == "draft")
        {
            // A PR can become a draft after enumeration; do not retain its captured evidence.
            return run.remove_draft(number, &journal);
        }
        let mut scope_error = None;
        let inspection = match observation {
            Ok((evidence, decision, snapshot_digest)) => Inspection {
                captured_at,
                evidence: Some(evidence),
                decision: Some(decision),
                snapshot_digest: Some(snapshot_digest),
                error: None,
            },
            Err(error) => {
                let diagnostic = Diagnostic::from_error(&error);
                if matches!(
                    diagnostic.kind.as_str(),
                    "authentication" | "permissionDenied"
                ) {
                    run.state = RunState::Failed;
                    run.error = Some(Diagnostic::from_error(&error));
                    scope_error = Some(error);
                }
                Inspection {
                    captured_at,
                    evidence: None,
                    decision: None,
                    snapshot_digest: None,
                    error: Some(diagnostic),
                }
            }
        };
        run.inventory
            .get_mut(index)
            .ok_or_else(|| fail("invalidState", "Inventory changed"))?
            .inspection = Some(inspection);
        if matches!(
            run.inventory.get(index).and_then(Entry::decision),
            Some(Decision::AssessmentRequired { .. })
        ) {
            assessment_budget_step(&mut run, index, clock, &journal, BudgetPhase::FinishWork)?;
        }
        if run.state != RunState::Failed {
            finish_state(&mut run);
        }
        run.save(&journal)?;
        if let Some(error) = scope_error {
            return Err(error);
        }
    }
    run.summary(number.get().saturating_sub(1), NonZeroUsize::MIN)
}

/// Attempt identity separates recaptures even when evidence and timestamps match.
fn snapshot_digest(
    evidence: &Evidence,
    captured_at: &str,
    attempt_id: Option<&str>,
) -> Result<String, AppError> {
    let document = if let Some(attempt_id) = attempt_id {
        json!({ "capturedAt": captured_at, "evidence": evidence, "attemptId": attempt_id })
    } else {
        json!({ "capturedAt": captured_at, "evidence": evidence })
    };
    Ok(digest(&serde_json::to_vec(&document)?))
}

/// Reads bounded persisted results without credentials or provider requests.
pub(crate) fn results(
    path: &Path,
    clock: &dyn Clock,
    after: u32,
    limit: NonZeroUsize,
) -> Result<Value, AppError> {
    let journal = FileJournal::open(path, clock)?;
    let run = DiscoveryRecord::load(&journal)?
        .ok_or_else(|| fail("invalidState", "Discovery record is missing"))?;
    run.summary(after, limit)
}

/// Classification follows the complete inventory, not whether the current page is empty.
fn finish_state(run: &mut DiscoveryRecord) {
    run.state = if run
        .inventory
        .iter()
        .any(|entry| entry.recency.state == RecencyState::Pending)
    {
        RunState::Processing
    } else if run.inventory.iter().any(Entry::failed) {
        RunState::Partial
    } else if run
        .inventory
        .iter()
        .any(|entry| entry.candidate(run.dry_run))
    {
        RunState::Processing
    } else {
        RunState::Complete
    };
}

/// Persist consumption before work so interruption cannot renew its limits.
fn assessment_budget_step(
    run: &mut DiscoveryRecord,
    index: usize,
    clock: &dyn Clock,
    journal: &FileJournal<'_>,
    phase: BudgetPhase,
) -> Result<(), AppError> {
    if !run.assessment_budget_enabled {
        return Ok(());
    }
    let entry = run
        .inventory
        .get_mut(index)
        .ok_or_else(|| fail("invalidState", "Missing assessment budget owner"))?;
    let now = clock.now().as_millisecond();
    if matches!(phase, BudgetPhase::StartInspection) && entry.assessment_budget.is_none() {
        entry.assessment_budget = Some(AssessmentBudget::new(now));
    }
    let budget = entry
        .assessment_budget
        .as_mut()
        .ok_or_else(|| fail("invalidState", "Missing assessment budget"))?;
    let status = match phase {
        BudgetPhase::StartInspection | BudgetPhase::BeginWork => budget.begin_work(now),
        BudgetPhase::FinishWork => budget.finish_work(now),
        BudgetPhase::Publish => budget.before_publication(now),
    };
    if run.state != RunState::Failed {
        finish_state(run);
    }
    if !matches!(phase, BudgetPhase::FinishWork) || status != BudgetStatus::Available {
        run.save(journal)?;
    }
    match status {
        BudgetStatus::Available => Ok(()),
        BudgetStatus::Exhausted => Err(fail(
            "budgetExceeded",
            "Assessment budget exhausted; retain incomplete work without approval or notice",
        )),
        BudgetStatus::ClockMovedBackwards => Err(fail(
            "budgetExceeded",
            "Clock moved backwards during assessment; elapsed time cannot be established",
        )),
    }
}

/// Reconciliation of an attempted effect never uses the new-publication phase.
#[derive(Clone, Copy)]
enum BudgetPhase {
    StartInspection,
    BeginWork,
    FinishWork,
    Publish,
}

/// Every mutation is a whole journal transaction; agents never assemble records.
pub(crate) enum WorkAction {
    Prepare,
    StartWorker,
    View {
        after: usize,
        limit: NonZeroUsize,
    },
    Units {
        reference: Option<String>,
        after: usize,
        limit: NonZeroUsize,
    },
    Checks {
        after: usize,
        limit: NonZeroUsize,
    },
    Note {
        id: String,
        note: Note,
        previous_digest: Option<String>,
    },
    Resolve {
        id: String,
        judgment: Judgment,
        previous_digest: Option<String>,
    },
    Checkpoint,
    Read {
        reference: String,
        offset: usize,
        limit: NonZeroUsize,
    },
    ReadBatch {
        request: BatchReadRequest,
    },
    JudgeBatch {
        request: BatchJudgmentRequest,
    },
    SourceContext {
        request: ContextRequest,
        limit: NonZeroUsize,
    },
    Judge {
        id: String,
        judgment: Judgment,
        previous_digest: Option<String>,
    },
    Complete,
}

/// Publication binds explicit caller scope to the same immutable run and assessment.
pub(crate) struct PublishInput<'a> {
    pub(crate) path: &'a Path,
    pub(crate) number: NonZeroU32,
    pub(crate) snapshot: &'a str,
    pub(crate) repository: &'a Target,
    pub(crate) attribution: &'a str,
    pub(crate) directory: &'a Path,
    pub(crate) tenant: Option<&'a str>,
}

/// Identity needed to read back a notice from a previous-format run.
struct PreviousNotice {
    run_id: String,
    operation_run_id: String,
    scope: Scope,
    snapshot: String,
    assessment_digest: String,
    policy_digest: String,
    anchor: Anchor,
    source_commit: String,
    url: String,
    title: String,
}

/// Checks only the stored bindings needed for notice recovery, not old judgments.
fn previous_notice(document: &Value, input: &PublishInput<'_>) -> Result<PreviousNotice, AppError> {
    let invalid = || {
        fail(
            "invalidState",
            "Previous notice recovery binding is invalid",
        )
    };
    let tenant = document.get("tenant").ok_or_else(invalid)?;
    if field(document, "schemaVersion")? != &json!(14)
        || boolean(field(document, "dryRun")?)?
        || text(field(document, "repositoryUrl")?)? != input.repository.web_url
        || field(document, "platform")? != &serde_json::to_value(input.repository.platform)?
        || tenant.as_str() != input.tenant
        || !tenant.is_null() && tenant.as_str().is_none()
    {
        return Err(invalid());
    }
    let run_id = text(field(document, "runId")?)?;
    if Uuid::parse_str(run_id).is_err() {
        return Err(invalid());
    }
    let scope = Scope {
        repository_id: text(field(document, "repositoryId")?)?.to_owned(),
        account_id: text(field(document, "accountId")?)?.to_owned(),
        pull_request_id: input.number.get(),
    };
    if scope.repository_id.trim().is_empty() || scope.account_id.trim().is_empty() {
        return Err(invalid());
    }
    let entries = field(document, "inventory")?
        .as_array()
        .ok_or_else(invalid)?;
    let mut ids = HashSet::new();
    let mut entry = None;
    for candidate in entries {
        let id = field(candidate, "pullRequestId")?
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .and_then(NonZeroU32::new)
            .ok_or_else(invalid)?;
        if !ids.insert(id) {
            return Err(invalid());
        }
        if id == input.number {
            entry = Some(candidate);
        }
    }
    let entry = entry.ok_or_else(invalid)?;
    let url = text(field(entry, "url")?)?;
    let title = text(field(entry, "title")?)?;
    if url != input.repository.candidate(input.number).web_url
        || title.trim().is_empty()
        || boolean(field(entry, "draft")?)?
    {
        return Err(invalid());
    }
    let recovery: Recovery = serde_json::from_value(field(entry, "recovery")?.clone())?;
    recovery.validate(run_id)?;
    let operation_run_id = recovery.attempt_ids.last().map_or(run_id, String::as_str);
    let inspection = field(entry, "inspection")?;
    let captured_at = text(field(inspection, "capturedAt")?)?;
    timestamp(captured_at)?;
    let snapshot = text(field(inspection, "snapshotDigest")?)?;
    let evidence = field(inspection, "evidence")?;
    let digest_input = if recovery.attempt_ids.is_empty() {
        json!({"capturedAt": captured_at, "evidence": evidence})
    } else {
        json!({"capturedAt": captured_at, "evidence": evidence, "attemptId": operation_run_id})
    };
    if snapshot != input.snapshot || digest(&serde_json::to_vec(&digest_input)?) != snapshot {
        return Err(invalid());
    }
    let evidence: Evidence = serde_json::from_value(evidence.clone())?;
    evidence.validate_scope(&scope.repository_id, scope.pull_request_id)?;
    if !matches!(
        (&evidence, input.repository.platform),
        (Evidence::GitHub(_), Platform::GitHub) | (Evidence::AzureDevOps(_), Platform::AzureDevOps)
    ) {
        return Err(invalid());
    }
    let decision = field(inspection, "decision")?;
    if text(field(decision, "status")?)? != "assessmentRequired" {
        return Err(invalid());
    }
    let anchor: Anchor = serde_json::from_value(field(decision, "anchor")?.clone())?;
    let assessment = field(entry, "assessment")?;
    let policy = field(document, "policy")?;
    let policy_digest = text(field(policy, "sha256")?)?;
    let inventory = field(assessment, "inventory")?;
    let source_commit = text(field(inventory, "newCommit")?)?;
    let assessment_digest = text(field(assessment, "assessmentDigest")?)?;
    let observed_source = match &evidence {
        Evidence::GitHub(GitHubEvidence::History { pull_request, .. }) => {
            text(field(field(pull_request, "head")?, "sha")?)?
        }
        Evidence::AzureDevOps(AdoEvidence::History { source_commit, .. }) => source_commit,
        _ => return Err(invalid()),
    };
    if !boolean(field(assessment, "complete")?)?
        || text(field(assessment, "snapshotDigest")?)? != snapshot
        || text(field(assessment, "policyDigest")?)? != policy_digest
        || text(field(inventory, "oldCommit")?)? != anchor.source_commit
        || source_commit != observed_source
        || digest(text(field(policy, "content")?)?.as_bytes()) != policy_digest
        || assessment_digest.len() != 64
        || !assessment_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid());
    }
    Ok(PreviousNotice {
        run_id: run_id.to_owned(),
        operation_run_id: operation_run_id.to_owned(),
        scope,
        snapshot: snapshot.to_owned(),
        assessment_digest: assessment_digest.to_owned(),
        policy_digest: policy_digest.to_owned(),
        anchor,
        source_commit: source_commit.to_owned(),
        url: url.to_owned(),
        title: title.to_owned(),
    })
}

/// Publishes once after validating the completed assessment and current evidence.
pub(crate) fn publish(
    api: &mut Api<'_>,
    clock: &dyn Clock,
    input: &PublishInput<'_>,
) -> Result<Value, AppError> {
    if input.attribution.trim().is_empty() {
        return Err(fail("invalidInput", "Publication requires attribution"));
    }
    let journal = FileJournal::open(input.path, clock)?;
    let mut run =
        DiscoveryRecord::load(&journal)?.ok_or_else(|| fail("invalidState", "Missing run"))?;
    let scope = run.publication_scope(input.number);
    let operation_run_id = run.operation_run_id(input.number)?.to_owned();
    if run.repository_url != input.repository.web_url
        || run.tenant.as_deref() != input.tenant
        || run.dry_run
        || run.state == RunState::Failed
    {
        return Err(fail(
            "invalidState",
            "Publication scope changed or this run cannot publish",
        ));
    }
    if let Some(entry) = run
        .inventory
        .iter()
        .find(|entry| entry.pull_request_id == input.number && entry.publication.is_some())
    {
        if entry.assessment_inputs()?.0 != input.snapshot {
            return Err(fail("invalidState", "Publication snapshot changed"));
        }
        return run.summary(input.number.get().saturating_sub(1), NonZeroUsize::MIN);
    }
    let policy = read_bytes(Path::new(&run.policy.source_path), clock)?
        .ok_or_else(|| fail("invalidState", "Captured significance policy is missing"))?;
    if digest(&policy) != run.policy.sha256 {
        return Err(fail(
            "invalidState",
            "Policy changed; publication requires a new assessment",
        ));
    }
    let index = run
        .inventory
        .iter()
        .position(|entry| entry.pull_request_id == input.number)
        .ok_or_else(|| fail("invalidInput", "Unknown publication PR"))?;
    assessment_budget_step(&mut run, index, clock, &journal, BudgetPhase::Publish)?;
    let entry = run
        .inventory
        .iter_mut()
        .find(|entry| entry.pull_request_id == input.number)
        .ok_or_else(|| fail("invalidInput", "Unknown publication PR"))?;
    let effect_deadline = entry
        .assessment_budget
        .as_ref()
        .map(AssessmentBudget::effect_deadline);
    let (snapshot, evidence) = entry.assessment_inputs()?;
    let assessment = entry
        .assessment
        .as_ref()
        .ok_or_else(|| fail("invalidState", "No completed assessment"))?;
    if snapshot != input.snapshot
        || assessment.status(false) != "readyToReapprove"
        || entry.execution_error.is_some()
        || entry.recovery.error.is_some()
    {
        return Err(fail(
            "invalidState",
            "Publication requires the exact completed insignificant assessment",
        ));
    }
    let Some(Decision::AssessmentRequired { anchor, .. }) = entry.decision() else {
        return Err(fail("invalidState", "No provider approval baseline"));
    };
    let target = input.repository.candidate(input.number);
    let result = publication::publish(
        api,
        &target,
        clock,
        &scope,
        &publication::Input {
            run_id: &operation_run_id,
            assessment_digest: assessment
                .digest
                .as_deref()
                .ok_or_else(|| fail("invalidState", "Missing assessment digest"))?,
            policy_digest: &run.policy.sha256,
            evidence,
            anchor,
            source_commit: &assessment.inventory.new_commit,
            attribution: input.attribution,
            explanation: assessment.explanation()?,
            effect_deadline,
        },
    );
    match result {
        Ok(receipt) => {
            if receipt
                .error
                .as_ref()
                .is_some_and(|error| error.kind == "draft")
            {
                return run.remove_draft(input.number, &journal);
            }
            let fatal = receipt
                .error
                .as_ref()
                .filter(|error| {
                    matches!(
                        error.kind.as_str(),
                        "ambiguousWrite" | "authentication" | "permissionDenied" | "scopeChanged"
                    )
                })
                .map(|error| Diagnostic {
                    kind: error.kind.clone(),
                    message: error.message.clone(),
                });
            entry.publication = Some(receipt);
            run.error = fatal;
            run.error_pull_request_id = run.error.as_ref().map(|_| input.number);
            if run.error.is_some() {
                run.state = RunState::Failed;
            } else {
                finish_state(&mut run);
            }
        }
        Err(error) => {
            let kind = classification(&error);
            if kind == "draft" {
                return run.remove_draft(input.number, &journal);
            }
            if kind == "budgetExceeded"
                && let Some(budget) = entry.assessment_budget.as_mut()
            {
                budget.before_publication(clock.now().as_millisecond());
            }
            let diagnostic = Diagnostic::from_error(&error);
            if kind == "staleEvidence" {
                entry.execution_error = Some(diagnostic);
                finish_state(&mut run);
            } else {
                run.state = RunState::Failed;
                run.error = Some(diagnostic);
                run.error_pull_request_id = Some(input.number);
            }
            run.save(&journal)?;
            return Err(error);
        }
    }
    run.save(&journal)?;
    run.summary(input.number.get().saturating_sub(1), NonZeroUsize::MIN)
}

/// Publishes a completed nonqualifying judgment under the host-before-run lock order.
pub(crate) fn notify(
    api: &mut Api<'_>,
    clock: &dyn Clock,
    input: &PublishInput<'_>,
) -> Result<Value, AppError> {
    if input.attribution.trim().is_empty() {
        return Err(fail(
            "invalidInput",
            "Needs-review publication requires attribution",
        ));
    }
    let previous = {
        let journal = FileJournal::open(input.path, clock)?;
        let bytes = journal
            .read_bytes()?
            .ok_or_else(|| fail("invalidState", "Missing run"))?;
        let document: Value = serde_json::from_slice(&bytes)?;
        if document.get("schemaVersion") == Some(&json!(14)) {
            Some((previous_notice(&document, input)?, bytes))
        } else {
            None
        }
    };
    if let Some((previous, original)) = previous {
        let mut notices = review_notice::Store::open(
            input.repository.platform,
            input.directory,
            &previous.scope,
            clock,
        )?;
        // Host-before-run locking preserves the same attempt while provider reads run.
        let journal = FileJournal::open(input.path, clock)?;
        if journal.read_bytes()?.as_deref() != Some(original.as_slice()) {
            return Err(fail("invalidState", "Previous run changed during recovery"));
        }
        let notice = notices.recover_previous(
            api,
            &input.repository.candidate(input.number),
            clock,
            &review_notice::PreviousAttempt {
                run_id: &previous.operation_run_id,
                assessment_digest: &previous.assessment_digest,
                policy_digest: &previous.policy_digest,
                anchor: &previous.anchor,
                source_commit: &previous.source_commit,
            },
        )?;
        return Ok(json!({
            "schemaVersion": 14,
            "runId": previous.run_id,
            "repositoryUrl": input.repository.web_url,
            "state": "reconciled",
            "recoveryOnly": true,
            "items": [{
                "pullRequestId": input.number,
                "url": previous.url,
                "title": previous.title,
                "status": "needsReview",
                "snapshotDigest": previous.snapshot,
                "assessmentDigest": previous.assessment_digest,
                "reviewNotice": notice
            }],
            "nextAfter": null
        }));
    }
    let (scope, run_id) = {
        let journal = FileJournal::open(input.path, clock)?;
        let run =
            DiscoveryRecord::load(&journal)?.ok_or_else(|| fail("invalidState", "Missing run"))?;
        if run.dry_run {
            return Err(fail(
                "invalidInput",
                "Dry runs cannot publish needs-review comments",
            ));
        }
        (run.publication_scope(input.number), run.run_id)
    };
    let mut notices =
        review_notice::Store::open(input.repository.platform, input.directory, &scope, clock)?;
    let journal = FileJournal::open(input.path, clock)?;
    let mut run =
        DiscoveryRecord::load(&journal)?.ok_or_else(|| fail("invalidState", "Missing run"))?;
    let operation_run_id = run.operation_run_id(input.number)?.to_owned();
    if run.run_id != run_id
        || run.dry_run
        || run.publication_scope(input.number) != scope
        || run.repository_url != input.repository.web_url
        || run.tenant.as_deref() != input.tenant
        || run.state == RunState::Failed
            && (run.error_pull_request_id != Some(input.number)
                || run.error.as_ref().is_none_or(|error| {
                    error.kind != "ambiguousWrite" || !notices.has_attempt(&operation_run_id)
                }))
    {
        return Err(fail(
            "invalidState",
            "Needs-review publication scope changed",
        ));
    }
    if !notices.has_attempt(&operation_run_id) {
        let policy = read_bytes(Path::new(&run.policy.source_path), clock)?
            .ok_or_else(|| fail("invalidState", "Captured significance policy is missing"))?;
        if digest(&policy) != run.policy.sha256 {
            return Err(fail(
                "invalidState",
                "Policy changed; notice publication requires a new assessment",
            ));
        }
        let index = run
            .inventory
            .iter()
            .position(|entry| entry.pull_request_id == input.number)
            .ok_or_else(|| fail("invalidInput", "Unknown needs-review PR"))?;
        assessment_budget_step(&mut run, index, clock, &journal, BudgetPhase::Publish)?;
    }
    let entry = run
        .inventory
        .iter_mut()
        .find(|entry| entry.pull_request_id == input.number)
        .ok_or_else(|| fail("invalidInput", "Unknown needs-review PR"))?;
    let effect_deadline = entry
        .assessment_budget
        .as_ref()
        .map(AssessmentBudget::effect_deadline);
    let (snapshot, evidence) = entry.assessment_inputs()?;
    let assessment = entry
        .assessment
        .as_ref()
        .ok_or_else(|| fail("invalidState", "Missing completed assessment"))?;
    let uncertain = assessment.status(false) == "failed";
    if snapshot != input.snapshot
        || !assessment.needs_review()
        || entry.recovery.stale.is_some()
        || entry.recovery.error.is_some()
        || entry.execution_error.is_some() && !uncertain
    {
        return Err(fail(
            "invalidState",
            "Needs-review publication requires the completed nonqualifying assessment",
        ));
    }
    let Some(Decision::AssessmentRequired { anchor, .. }) = entry.decision() else {
        return Err(fail(
            "invalidState",
            "Missing provider-selected approval baseline",
        ));
    };
    let result = notices.publish(
        api,
        &input.repository.candidate(input.number),
        clock,
        &publication::Input {
            run_id: &operation_run_id,
            assessment_digest: assessment
                .digest
                .as_deref()
                .ok_or_else(|| fail("invalidState", "Missing assessment digest"))?,
            policy_digest: &run.policy.sha256,
            evidence,
            anchor,
            source_commit: &assessment.inventory.new_commit,
            attribution: input.attribution,
            explanation: assessment
                .rationale()
                .ok_or_else(|| fail("invalidState", "Missing significance rationale"))?,
            effect_deadline,
        },
    );
    match result {
        Ok(notice) => {
            entry.review_notice = Some(notice);
            if !uncertain {
                entry.execution_error = None;
            }
            run.error = None;
            run.error_pull_request_id = None;
            finish_state(&mut run);
        }
        Err(error) => {
            if classification(&error) == "budgetExceeded"
                && let Some(budget) = entry.assessment_budget.as_mut()
            {
                budget.before_publication(clock.now().as_millisecond());
            }
            if classification(&error) == "draft" {
                if notices.has_attempt(&operation_run_id) {
                    return Err(fail(
                        "ambiguousWrite",
                        "Draft exclusion cannot discard a possible remote effect",
                    ));
                }
                return run.remove_draft(input.number, &journal);
            }
            let error = if notices.has_unresolved(&operation_run_id)
                || run.state == RunState::Failed
            {
                crate::error::caused_by(
                    "ambiguousWrite",
                    "Needs-review comment may exist; retain the original notify invocation for reconciliation".to_owned(),
                    error.into_std_error(),
                ).into()
            } else {
                error
            };
            let diagnostic = Diagnostic::from_error(&error);
            if matches!(
                diagnostic.kind.as_str(),
                "ambiguousWrite" | "authentication" | "permissionDenied"
            ) {
                run.error = Some(diagnostic);
                run.error_pull_request_id = Some(input.number);
                run.state = RunState::Failed;
            } else {
                if diagnostic.kind == "staleEvidence" {
                    // Preserve uncertainty separately so stale notification evidence remains actionable.
                    entry.recovery.stale = Some(Diagnostic::from_error(&error));
                }
                // Notification errors cannot replace a completed assessment's uncertainty.
                if !uncertain {
                    entry.execution_error = Some(diagnostic);
                }
                finish_state(&mut run);
            }
            run.save(&journal)?;
            return Err(error);
        }
    }
    run.save(&journal)?;
    run.summary(input.number.get().saturating_sub(1), NonZeroUsize::MIN)
}

/// Reads and updates one immutable-snapshot assessment without provider writes.
pub(crate) fn work(
    path: &Path,
    number: NonZeroU32,
    snapshot: &str,
    action: WorkAction,
    process: &dyn Process,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let journal = FileJournal::open(path, clock)?;
    let mut run =
        DiscoveryRecord::load(&journal)?.ok_or_else(|| fail("invalidState", "Missing run"))?;
    if run.state == RunState::Failed {
        return Err(fail("invalidState", "Failed run requires a new invocation"));
    }
    let policy = read_bytes(Path::new(&run.policy.source_path), clock)?
        .ok_or_else(|| fail("invalidState", "Captured significance policy is missing"))?;
    if digest(&policy) != run.policy.sha256 {
        return Err(fail(
            "invalidState",
            "Significance policy changed; start a fresh run",
        ));
    }
    let index = run
        .inventory
        .iter()
        .position(|entry| entry.pull_request_id == number)
        .ok_or_else(|| fail("invalidInput", "Unknown assessment PR"))?;
    let entry = run
        .inventory
        .get(index)
        .ok_or_else(|| fail("invalidState", "Missing assessment PR"))?;
    let (expected, evidence) = entry.assessment_inputs()?;
    if expected != snapshot
        || entry.execution_error.is_some()
        || entry.recovery.stale.is_some()
        || entry.recovery.error.is_some()
    {
        return Err(fail(
            "invalidState",
            "Snapshot changed or assessment preparation failed",
        ));
    }
    let discussion = discussion::render(evidence)?;
    if matches!(action, WorkAction::Prepare)
        && entry.assessment.is_none()
        && !run.assessment_budget_enabled
    {
        return Err(fail(
            "invalidState",
            "This run has no assessment budget; start a fresh bounded assessment",
        ));
    }
    assessment_budget_step(&mut run, index, clock, &journal, BudgetPhase::BeginWork)?;
    if matches!(action, WorkAction::StartWorker) {
        let budget = run
            .inventory
            .get_mut(index)
            .and_then(|entry| entry.assessment_budget.as_mut())
            .ok_or_else(|| fail("invalidState", "Worker reservation requires a bounded run"))?;
        if budget.reserve_worker() != BudgetStatus::Available {
            finish_state(&mut run);
            run.save(&journal)?;
            return Err(fail(
                "budgetExceeded",
                "Assessment worker limit exhausted; retain incomplete work without approval or notice",
            ));
        }
        assessment_budget_step(&mut run, index, clock, &journal, BudgetPhase::FinishWork)?;
        run.save(&journal)?;
        return run.summary(number.get().saturating_sub(1), NonZeroUsize::MIN);
    }
    let entry = run
        .inventory
        .get_mut(index)
        .ok_or_else(|| fail("invalidState", "Missing assessment PR"))?;
    if matches!(action, WorkAction::Prepare) && entry.assessment.is_none() {
        let (old, new) = entry.source_endpoints()?;
        match objects::prepare(
            process,
            &run.repository_url,
            old,
            new,
            run.tenant.as_deref(),
        ) {
            Ok((inventory, patches)) => {
                entry.assessment = Some(Assessment::new_direct(
                    inventory,
                    &patches,
                    snapshot,
                    &run.policy.sha256,
                    &discussion,
                )?);
            }
            Err(error) => {
                entry.execution_error = Some(Diagnostic::from_error(&error));
                assessment_budget_step(&mut run, index, clock, &journal, BudgetPhase::FinishWork)?;
                finish_state(&mut run);
                run.save(&journal)?;
                return run.summary(number.get().saturating_sub(1), NonZeroUsize::MIN);
            }
        }
    }
    let assessment = entry
        .assessment
        .as_mut()
        .ok_or_else(|| fail("invalidState", "Prepare the source inventory first"))?;
    let read_batch = matches!(action, WorkAction::ReadBatch { .. });
    let evidence_read = read_batch
        || matches!(
            action,
            WorkAction::Read { .. } | WorkAction::SourceContext { .. }
        );
    let judgment_batch = matches!(action, WorkAction::JudgeBatch { .. });
    let result = match action {
        WorkAction::Prepare => assessment.summary(0, 10, &run.policy.content, &discussion)?,
        WorkAction::StartWorker => {
            return Err(fail("invalidState", "Worker reservation was not handled"));
        }
        WorkAction::View { after, limit } => assessment.summary(
            after,
            limit.get().min(100),
            &run.policy.content,
            &discussion,
        )?,
        WorkAction::Units {
            reference,
            after,
            limit,
        } => assessment.units_for(reference.as_deref(), after, limit.get().min(100))?,
        WorkAction::Checks { after, limit } => assessment.checks(after, limit.get().min(100))?,
        WorkAction::Note {
            id,
            note,
            previous_digest,
        } => {
            let digest = assessment.note(
                &id,
                note,
                previous_digest.as_deref(),
                &run.policy.content,
                &discussion,
            )?;
            json!({"snapshotDigest":snapshot, "recorded":id, "noteDigest":digest})
        }
        WorkAction::Resolve {
            id,
            judgment,
            previous_digest,
        } => {
            let digest = assessment.resolve(
                &id,
                judgment,
                previous_digest.as_deref(),
                &run.policy.content,
                &discussion,
            )?;
            json!({"snapshotDigest":snapshot, "recorded":id, "resolutionDigest":digest})
        }
        WorkAction::Checkpoint => {
            assessment.checkpoint()?;
            assessment.progress_with_evidence(&run.policy.content, &discussion)?
        }
        WorkAction::Read {
            reference,
            offset,
            limit,
        } => assessment.read(
            &reference,
            offset,
            limit.get(),
            &run.policy.content,
            &discussion,
        )?,
        WorkAction::ReadBatch { request } => {
            assessment.read_batch(request, &run.policy.content, &discussion)?
        }
        WorkAction::JudgeBatch { request } => {
            assessment.record_batch(request, &run.policy.content, &discussion)?
        }
        WorkAction::SourceContext { request, limit } => {
            match assessment.source_context(request, limit.get(), &run.policy.content, &discussion)
            {
                Ok(result) => result,
                Err(error) => {
                    // A failed attempt to acquire needed context is unfinished evidence, never
                    // permission to ignore that request and publish a semantic refusal.
                    if assessment.digest.is_none() {
                        entry.execution_error = Some(Diagnostic::from_error(&error.into()));
                        finish_state(&mut run);
                        run.save(&journal)?;
                        return Err(fail(
                            "evidenceDelivery",
                            "Captured source context acquisition failed; retained assessment is incomplete",
                        ));
                    }
                    return Err(error.into());
                }
            }
        }
        WorkAction::Judge {
            id,
            judgment,
            previous_digest,
        } => {
            let digest = assessment.record(
                &id,
                judgment,
                previous_digest.as_deref(),
                &run.policy.content,
                &discussion,
            )?;
            json!({"snapshotDigest": snapshot, "recorded": id, "judgmentDigest": digest})
        }
        WorkAction::Complete => {
            assessment.finish(&run.policy.content, &discussion)?;
            if assessment.status(run.dry_run) == "failed" {
                entry.execution_error = Some(Diagnostic {
                    kind: "service".to_owned(),
                    message: "Source-change evaluation is uncertain; inspect the recorded judgments and obtain the missing evidence".to_owned(),
                });
            }
            json!({"snapshotDigest": snapshot, "status": assessment.status(run.dry_run),
                "assessmentDigest": assessment.digest, "error": entry.execution_error})
        }
    };
    if let Some(budget) = entry.assessment_budget.as_mut() {
        if evidence_read {
            budget.record_evidence(&result, read_batch)?;
        }
        if judgment_batch {
            budget.record_judgment_batch();
        }
    }
    assessment_budget_step(&mut run, index, clock, &journal, BudgetPhase::FinishWork)?;
    finish_state(&mut run);
    run.save(&journal)?;
    Ok(result)
}

/// Projects enumeration metadata without asking for reviews, discussions, or objects.
fn metadata(api: &mut Api<'_>, repository: &Target) -> Result<Vec<Entry>, AppError> {
    let mut entries = Vec::new();
    for (id, value) in api.open_pull_requests(repository)? {
        let github = repository.platform == Platform::GitHub;
        if boolean(field(&value, if github { "draft" } else { "isDraft" })?)? {
            continue;
        }
        let state = text(field(&value, if github { "state" } else { "status" })?)?;
        if state != if github { "open" } else { "active" } {
            return Err(fail(
                "service",
                "Open PR enumeration returned a non-open pull request",
            ));
        }
        let title = text(field(&value, "title")?)?;
        if title.trim().is_empty() {
            return Err(fail(
                "service",
                "Open PR enumeration returned an empty title",
            ));
        }
        entries.push(Entry {
            pull_request_id: id,
            url: repository.candidate(id).web_url,
            title: title.to_owned(),
            draft: false,
            recency: Recency::pending(),
            inspection: None,
            assessment: None,
            assessment_budget: None,
            execution_error: None,
            publication: None,
            review_notice: None,
            recovery: Recovery::default(),
        });
    }
    entries.sort_by_key(|entry| entry.pull_request_id);
    Ok(entries)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn utc(value: Timestamp) -> String {
    format!("{value:.9}")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        reason = "Tests mutate bounded synthetic records"
    )]

    use std::num::NonZeroU32;
    use std::path::Path;

    use serde_json::{Value, json};

    use super::{PublishInput, digest, previous_notice};
    use crate::target::Target;

    /// Builds only the previous-format fields used to bind notice recovery.
    fn previous_run(target: &Target) -> Value {
        let old = "a".repeat(40);
        let new = "b".repeat(40);
        let evidence = if target.platform == crate::target::Platform::GitHub {
            json!({"platform": "gitHub", "observation": {
                "scope": "history", "pullRequest": {"number": 1, "base": {"repo": {"id": 123}},
                    "head": {"sha": new}},
                "policy": {"requireLastPushApproval": false},
                "reviews": [], "timeline": [], "comments": [], "reviewComments": []
            }})
        } else {
            json!({"platform": "azureDevOps", "observation": {
                "scope": "history", "pullRequest": {"pullRequestId": 1, "repository": {"id": "123"}},
                "sourceCommit": new, "reviewers": [], "iterations": [], "threads": [],
                "policies": [], "evaluations": []
            }})
        };
        let captured = "2026-01-01T00:00:00Z";
        let snapshot = digest(
            &serde_json::to_vec(&json!({"capturedAt": captured, "evidence": evidence})).unwrap(),
        );
        let policy = "Synthetic policy";
        let policy_digest = digest(policy.as_bytes());
        json!({
            "schemaVersion": 14, "runId": "70759fa1-3f53-4f78-81e3-49d5ab9be1d4",
            "repositoryUrl": target.web_url, "repositoryId": "123", "platform": target.platform,
            "accountId": "42", "tenant": null, "dryRun": false,
            "policy": {"sha256": policy_digest, "content": policy},
            "inventory": [{
                "pullRequestId": 1, "url": target.candidate(NonZeroU32::MIN).web_url,
                "title": "Synthetic PR", "draft": false,
                "recovery": {"attemptIds": [], "stale": null, "error": null},
                "inspection": {"capturedAt": captured, "snapshotDigest": snapshot, "evidence": evidence,
                    "decision": {"status": "assessmentRequired", "anchor": {
                        "reviewId": "event", "sourceCommit": old}}},
                "assessment": {"complete": true, "assessmentDigest": "c".repeat(64),
                    "policyDigest": policy_digest, "snapshotDigest": snapshot,
                    "inventory": {"oldCommit": old, "newCommit": new}}
            }]
        })
    }

    #[test]
    fn previous_notice_binds_both_provider_snapshots_without_reading_judgments() {
        for url in [
            "https://github.com/example/widgets",
            "https://dev.azure.com/org/project/_git/repo",
        ] {
            let target = Target::repository(url).unwrap();
            let run = previous_run(&target);
            let snapshot = run["inventory"][0]["inspection"]["snapshotDigest"]
                .as_str()
                .unwrap();
            let input = PublishInput {
                path: Path::new("unused"),
                number: NonZeroU32::MIN,
                snapshot,
                repository: &target,
                attribution: "Attribution",
                directory: Path::new("unused"),
                tenant: None,
            };
            let previous = previous_notice(&run, &input).unwrap();
            assert_eq!(previous.scope.repository_id, "123");
            assert_eq!(previous.operation_run_id, previous.run_id);
            assert_eq!(previous.snapshot, snapshot);

            for (path, replacement) in [
                ("/schemaVersion", json!(15)),
                ("/dryRun", json!(true)),
                ("/repositoryId", json!("other")),
                ("/tenant", json!(12)),
                ("/policy/content", json!("different policy")),
                (
                    "/inventory/0/inspection/evidence/observation/scope",
                    json!("state"),
                ),
                ("/inventory/0/assessment/complete", json!(false)),
                ("/inventory/0/assessment/snapshotDigest", json!("different")),
                (
                    "/inventory/0/assessment/inventory/oldCommit",
                    json!("different"),
                ),
                (
                    "/inventory/0/assessment/inventory/newCommit",
                    json!("different"),
                ),
                ("/inventory/0/assessment/assessmentDigest", json!("bad")),
            ] {
                let mut changed = run.clone();
                *changed.pointer_mut(path).unwrap() = replacement;
                assert!(previous_notice(&changed, &input).is_err(), "{path}");
            }
        }
    }
}
