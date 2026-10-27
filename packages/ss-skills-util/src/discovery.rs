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
use crate::clock::{is_recent, timestamp};
use crate::error::{Failure, classification, fail};
use crate::github_approval::Evidence as GitHubEvidence;
use crate::journal::{FileJournal, read_bytes};
use crate::json::{boolean, field, identifier, required_optional, text};
use crate::objects;
use crate::publication::{self, Outcome, Receipt, Scope};
use crate::review_notice::{self, AutomaticApproval, Notice};
use crate::target::{Platform, Target};
use crate::{Clock, Process};

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

/// Every enumerated PR has one recency record, even if it never becomes a candidate.
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
    #[serde(deserialize_with = "required_optional")]
    execution_error: Option<Diagnostic>,
    #[serde(deserialize_with = "required_optional")]
    publication: Option<Receipt>,
    #[serde(deserialize_with = "required_optional")]
    review_notice: Option<Notice>,
    #[serde(deserialize_with = "required_optional")]
    automatic_approval: Option<AutomaticApproval>,
    recovery: Recovery,
}

/// Fresh attempts have distinct host-journal identities; old effects remain reconcilable.
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
        if self.schema_version != 8
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
                        &serde_json::to_string_pretty(evidence)?,
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
                    receipt.validate()?;
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
                if let Some(approval) = &entry.automatic_approval {
                    timestamp(&approval.approved_at)?;
                    if approval.approval_event_id.trim().is_empty()
                        || entry.status(self.dry_run) != "alreadyValid"
                            && entry.status(self.dry_run) != "reapproved"
                    {
                        return Err(fail(
                            "invalidState",
                            "Automatic approval metadata requires valid standing",
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
                if entry.publication.is_none()
                    && entry.automatic_approval
                        != inspection
                            .evidence
                            .as_ref()
                            .map(|evidence| {
                                review_notice::automatic_approval(
                                    evidence,
                                    &self.publication_scope(entry.pull_request_id),
                                    timestamp(&inspection.captured_at)?,
                                )
                            })
                            .transpose()?
                            .flatten()
                {
                    return Err(fail(
                        "invalidState",
                        "Automatic approval summary disagrees with inspection",
                    ));
                }
            } else if entry.assessment.is_some()
                || entry.execution_error.is_some()
                || entry.publication.is_some()
                || entry.review_notice.is_some()
                || entry.automatic_approval.is_some()
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

    fn load(journal: &FileJournal<'_>) -> Result<Option<Self>, AppError> {
        let Some(bytes) = journal.read_bytes()? else {
            return Ok(None);
        };
        let mut document: Value = serde_json::from_slice(&bytes).map_err(|error| {
            Failure::caused_by(
                "invalidState",
                "Malformed discovery record".to_owned(),
                error,
            )
        })?;
        // Schema-7 runs predate bounded refresh and have no consumed recovery attempts.
        if document.get("schemaVersion") == Some(&json!(7)) {
            *document
                .get_mut("schemaVersion")
                .ok_or_else(|| fail("invalidState", "Missing schema version"))? = json!(8);
            let entries = document
                .get_mut("inventory")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| fail("invalidState", "Malformed schema-7 inventory"))?;
            for entry in entries {
                let entry = entry
                    .as_object_mut()
                    .ok_or_else(|| fail("invalidState", "Malformed schema-7 entry"))?;
                if entry.contains_key("recovery") {
                    return Err(fail(
                        "invalidState",
                        "Schema-7 entry cannot contain recovery state",
                    ));
                }
                entry.insert(
                    "recovery".to_owned(),
                    serde_json::to_value(Recovery::default())?,
                );
            }
        }
        let run: Self = serde_json::from_value(document).map_err(|error| {
            Failure::caused_by(
                "invalidState",
                "Malformed discovery record".to_owned(),
                error,
            )
        })?;
        run.validate().map_err(|error| {
            Failure::caused_by(
                "invalidState",
                "Invalid discovery record".to_owned(),
                error.into_std_error(),
            )
        })?;
        Ok(Some(run))
    }

    /// Bounded summaries never expand older PRs into per-PR agent work.
    fn summary(&self, after: u32, limit: NonZeroUsize) -> Value {
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
                json!({
                    "pullRequestId": entry.pull_request_id, "url": entry.url,
                    "title": entry.title,
                    "status": entry.status(self.dry_run),
                    "reason": if entry.draft { Some("draft") } else { decision.and_then(Decision::reason) },
                    "assessmentReason": entry.assessment.as_ref().and_then(Assessment::rationale),
                    "approvalEventId": decision.and_then(Decision::approval_event_id),
                    "capturedAt": entry.inspection.as_ref().map(|inspection| &inspection.captured_at),
                    "snapshotDigest": entry.inspection.as_ref().and_then(|inspection| inspection.snapshot_digest.as_ref()),
                    "assessmentDigest": entry.assessment.as_ref().and_then(|assessment| assessment.digest.as_ref()),
                    "publication": entry.publication,
                    "reviewNotice": entry.review_notice.as_ref().or(match decision {
                        Some(Decision::NeedsReview { notice }) => Some(notice),
                        _ => None,
                    }),
                    "automaticApproval": entry.automatic_approval,
                    "staleRecovery": {
                        "attempts": entry.recovery.attempt_ids.len(),
                        "limit": STALE_RECOVERY_LIMIT,
                        "exhausted": entry.recovery.error.is_some()
                    },
                    "error": entry.recovery.error.as_ref().or(entry.recovery.stale.as_ref()).or(entry.execution_error.as_ref()).or_else(|| entry.inspection.as_ref()?.error.as_ref()).or(entry.recency.error.as_ref())
                })
            })
            .collect::<Vec<_>>();
        let next = if included.len() > limit.get() {
            included
                .get(limit.get().saturating_sub(1))
                .map(|entry| entry.pull_request_id)
        } else {
            None
        };
        json!({
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
                "draft": self.inventory.iter().filter(|entry| entry.draft && entry.recency.state == RecencyState::Recent).count(),
                "pending": self.inventory.iter().filter(|entry| entry.recency.state == RecencyState::Pending).count(),
                "alreadyValid": self.inventory.iter().filter(|entry| entry.status(self.dry_run) == "alreadyValid").count(),
                "noPriorApproval": self.inventory.iter().filter(|entry| matches!(entry.decision(), Some(Decision::NoPriorApproval))).count(),
                "ineligible": self.inventory.iter().filter(|entry| entry.status(self.dry_run) == "ineligible").count(),
                "indeterminate": self.inventory.iter().filter(|entry| entry.status(self.dry_run) == "indeterminate").count()
            },
            "items": page, "nextAfter": next, "error": self.error
        })
    }
}

impl Entry {
    /// The initial attempt keeps its existing journal identity for schema-7 recovery.
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
            return Err(fail("invalidState", "Missing original approval anchor"));
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
            || self.recovery.stale.is_some()
            || self.execution_error.is_some()
            || self.recency.state == RecencyState::Failed
            || self
                .inspection
                .as_ref()
                .is_some_and(|inspection| inspection.error.is_some())
    }

    fn stale(&self) -> bool {
        if self.recovery.error.is_some() {
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
                // Existing schema-8 assessments can retain their pre-attempt digest.
                let legacy = snapshot_digest(evidence, &self.captured_at, None)?;
                if evidence.decide(platform, account, repository, number, observed)? != *decision
                    || *hash != current && *hash != legacy
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
        Failure::caused_by(
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
            schema_version: 8,
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
    Ok(run.summary(0, input.limit))
}

/// Captures candidates, or refreshes clean stale attempts under the publication locks.
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
    // Publication always locks host before run; refreshing must use the same order.
    let scope = if recovery_directory.is_some() {
        let journal = FileJournal::open(path, clock)?;
        let run = DiscoveryRecord::load(&journal)?
            .ok_or_else(|| fail("invalidState", "Discovery record is missing"))?;
        Some(run.publication_scope(number))
    } else {
        None
    };
    let stores = recovery_directory
        .zip(scope.as_ref())
        .map(|(directory, scope)| {
            let publication =
                publication::ProviderStore::open(repository.platform, directory, scope, clock)?;
            let notices = review_notice::Store::open(repository.platform, directory, scope, clock)?;
            publication.ensure_resolved()?;
            notices.ensure_resolved()?;
            Ok::<_, AppError>((publication, notices))
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
    if let Some((publication, notices)) = &stores {
        if scope.as_ref() != Some(&run.publication_scope(number)) {
            return Err(fail("invalidState", "Refresh scope changed"));
        }
        let operation_run_id = entry.operation_run_id(&run.run_id);
        // Even accepted-but-stale effects keep their original assessment and journal.
        if publication.has_effect(operation_run_id)
            || notices.has_attempt(operation_run_id)
            || entry.publication.is_some() && !publication.preflight_only(operation_run_id)
        {
            return Err(fail(
                "ambiguousWrite",
                "An attempted remote effect cannot refresh evidence; reconcile the original publication or notify command",
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
        entry.automatic_approval = None;
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
        let mut scope_error = None;
        let inspection = match observation {
            Ok((evidence, decision, snapshot_digest)) => {
                let automatic = review_notice::automatic_approval(
                    &evidence,
                    &run.publication_scope(number),
                    timestamp(&captured_at)?,
                )?;
                run.inventory
                    .get_mut(index)
                    .ok_or_else(|| fail("invalidState", "Inventory changed"))?
                    .automatic_approval = automatic;
                Inspection {
                    captured_at,
                    evidence: Some(evidence),
                    decision: Some(decision),
                    snapshot_digest: Some(snapshot_digest),
                    error: None,
                }
            }
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
        if run.state != RunState::Failed {
            finish_state(&mut run);
        }
        run.save(&journal)?;
        if let Some(error) = scope_error {
            return Err(error);
        }
    }
    Ok(run.summary(number.get().saturating_sub(1), NonZeroUsize::MIN))
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
    Ok(run.summary(after, limit))
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

/// Every mutation is a whole journal transaction; agents never assemble records.
pub(crate) enum WorkAction {
    Prepare,
    View {
        after: usize,
        limit: NonZeroUsize,
    },
    Read {
        reference: String,
        offset: usize,
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

/// Acquires the host transaction before the run transaction and never repeats uncertain effects.
pub(crate) fn publish(
    api: &mut Api<'_>,
    clock: &dyn Clock,
    input: &PublishInput<'_>,
) -> Result<Value, AppError> {
    if input.attribution.trim().is_empty() {
        return Err(fail("invalidInput", "Publication requires attribution"));
    }
    let (scope, run_id) = {
        let journal = FileJournal::open(input.path, clock)?;
        let run =
            DiscoveryRecord::load(&journal)?.ok_or_else(|| fail("invalidState", "Missing run"))?;
        if run.dry_run {
            return Err(fail(
                "invalidInput",
                "Dry runs cannot publish remote effects",
            ));
        }
        (run.publication_scope(input.number), run.run_id)
    };
    let mut store = publication::ProviderStore::open(
        input.repository.platform,
        input.directory,
        &scope,
        clock,
    )?;
    let notices =
        review_notice::Store::open(input.repository.platform, input.directory, &scope, clock)?;
    notices.ensure_resolved()?;
    let journal = FileJournal::open(input.path, clock)?;
    let mut run =
        DiscoveryRecord::load(&journal)?.ok_or_else(|| fail("invalidState", "Missing run"))?;
    let operation_run_id = run.operation_run_id(input.number)?.to_owned();
    if run.run_id != run_id
        || run.publication_scope(input.number) != scope
        || run.repository_url != input.repository.web_url
        || run.tenant.as_deref() != input.tenant
        || run.dry_run
        || (run.state == RunState::Failed
            && (run.error_pull_request_id != Some(input.number)
                || run
                    .error
                    .as_ref()
                    .is_none_or(|error| error.kind != "ambiguousWrite")
                || !store.has_effect(&operation_run_id)))
    {
        return Err(fail(
            "invalidState",
            "Publication scope changed or this run cannot publish",
        ));
    }
    if store.may_write(&operation_run_id) {
        let policy = read_bytes(Path::new(&run.policy.source_path), clock)?
            .ok_or_else(|| fail("invalidState", "Captured significance policy is missing"))?;
        if digest(&policy) != run.policy.sha256 {
            return Err(fail(
                "invalidState",
                "Policy changed; publication requires a new assessment",
            ));
        }
    }
    let entry = run
        .inventory
        .iter_mut()
        .find(|entry| entry.pull_request_id == input.number)
        .ok_or_else(|| fail("invalidInput", "Unknown publication PR"))?;
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
        return Err(fail("invalidState", "No external approval authority"));
    };
    let target = input.repository.candidate(input.number);
    let result = store.publish(
        api,
        &target,
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
            explanation: assessment.explanation()?,
        },
    );
    // Reporting is part of publication completion, so its failures need the same durable diagnostic.
    let result = result.and_then(|receipt| {
        // A completed invocation reports its persisted observation, not later service standing.
        // The provider has already checked the original immutable assessment before replaying it.
        if matches!(receipt.outcome, Outcome::Reapproved | Outcome::AlreadyValid)
            && entry.publication.as_ref().is_some_and(|saved| {
                saved.operation_id == receipt.operation_id && saved.outcome == receipt.outcome
            })
        {
            return Ok((receipt, entry.automatic_approval.clone()));
        }
        let mut automatic_approval =
            if matches!(receipt.outcome, Outcome::Reapproved | Outcome::AlreadyValid) {
                store.automatic_approval(&operation_run_id)?
            } else {
                None
            };
        if receipt.outcome == Outcome::AlreadyValid && automatic_approval.is_none() {
            // Another run may have renewed approval after this run's inspection.
            // Capture its origin rather than misreporting absent metadata as an external review.
            let fresh = Evidence::capture(api, &target)?;
            if !matches!(
                fresh.decide(
                    run.platform,
                    &run.account_id,
                    &run.repository_id,
                    input.number.get(),
                    clock.now()
                )?,
                Decision::AlreadyValid { .. }
            ) {
                return Err(fail(
                    "staleEvidence",
                    "Approval standing changed before result reporting",
                ));
            }
            automatic_approval = review_notice::automatic_approval(&fresh, &scope, clock.now())?;
        }
        Ok((receipt, automatic_approval))
    });
    match result {
        Ok((receipt, automatic_approval)) => {
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
            entry.automatic_approval = automatic_approval;
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
            let error = if store.has_effect(&operation_run_id) {
                Failure::caused_by("ambiguousWrite",
                    "Publication may have completed; retaining the original intent for reconciliation".to_owned(),
                    error.into_std_error()).into()
            } else {
                error
            };
            let diagnostic = Diagnostic::from_error(&error);
            if matches!(
                diagnostic.kind.as_str(),
                "authentication" | "permissionDenied" | "ambiguousWrite" | "scopeChanged"
            ) {
                run.state = RunState::Failed;
                run.error = Some(diagnostic);
                run.error_pull_request_id = Some(input.number);
            } else {
                entry.execution_error = Some(diagnostic);
                finish_state(&mut run);
            }
            run.save(&journal)?;
            return Err(error);
        }
    }
    run.save(&journal)?;
    Ok(run.summary(input.number.get().saturating_sub(1), NonZeroUsize::MIN))
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
    // Sharing this lock with approval publication prevents opposing writes from overlapping.
    let publication = publication::ProviderStore::open(
        input.repository.platform,
        input.directory,
        &scope,
        clock,
    )?;
    publication.ensure_resolved()?;
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
    }
    let entry = run
        .inventory
        .iter_mut()
        .find(|entry| entry.pull_request_id == input.number)
        .ok_or_else(|| fail("invalidInput", "Unknown needs-review PR"))?;
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
        return Err(fail("invalidState", "Missing original approval authority"));
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
            let error = if notices.has_unresolved(&operation_run_id)
                || run.state == RunState::Failed
            {
                Failure::caused_by(
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
    Ok(run.summary(input.number.get().saturating_sub(1), NonZeroUsize::MIN))
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
    let entry = run
        .inventory
        .iter_mut()
        .find(|entry| entry.pull_request_id == number)
        .ok_or_else(|| fail("invalidInput", "Unknown assessment PR"))?;
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
    let discussion = serde_json::to_string_pretty(evidence)?;
    if matches!(action, WorkAction::Prepare) && entry.assessment.is_none() {
        let (old, new) = entry.source_endpoints()?;
        match objects::prepare(
            process,
            &run.repository_url,
            old,
            new,
            run.tenant.as_deref(),
        ) {
            Ok(inventory) => {
                entry.assessment = Some(Assessment::new(inventory, snapshot, &run.policy.sha256)?);
            }
            Err(error) => {
                entry.execution_error = Some(Diagnostic::from_error(&error));
                finish_state(&mut run);
                run.save(&journal)?;
                return Ok(run.summary(number.get().saturating_sub(1), NonZeroUsize::MIN));
            }
        }
    }
    let assessment = entry
        .assessment
        .as_mut()
        .ok_or_else(|| fail("invalidState", "Prepare the source inventory first"))?;
    let result = match action {
        WorkAction::Prepare => assessment.summary(0, 10)?,
        WorkAction::View { after, limit } => assessment.summary(after, limit.get().min(100))?,
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
        WorkAction::Judge {
            id,
            judgment,
            previous_digest,
        } => {
            let digest = assessment.record(&id, judgment, previous_digest.as_deref())?;
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
    finish_state(&mut run);
    run.save(&journal)?;
    Ok(result)
}

/// Projects enumeration metadata without asking for reviews, discussions, or objects.
fn metadata(api: &mut Api<'_>, repository: &Target) -> Result<Vec<Entry>, AppError> {
    let mut entries = Vec::new();
    for (id, value) in api.open_pull_requests(repository)? {
        let github = repository.platform == Platform::GitHub;
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
            draft: boolean(field(&value, if github { "draft" } else { "isDraft" })?)?,
            recency: Recency::pending(),
            inspection: None,
            assessment: None,
            execution_error: None,
            publication: None,
            review_notice: None,
            automatic_approval: None,
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
