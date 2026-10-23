//! Persists account-owned review requirements without granting approval authority.

use std::collections::HashSet;
use std::mem::replace;
use std::path::Path;

use jiff::Timestamp;
use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::Clock;
use crate::ado_approval::{self, Evidence as AdoEvidence};
use crate::api::Api;
use crate::approval::{Decision, Evidence};
use crate::clock::timestamp;
use crate::error::{classification, fail};
use crate::github_approval::Evidence as GitHubEvidence;
use crate::journal::FileJournal;
use crate::json::{array, field, identifier, text};
use crate::publication::{Input, Scope, hash};
use crate::renewal::{Anchor, Audit, State as AuditState};
use crate::target::{Platform, Target};

/// Separate syntax prevents a discussion notice from becoming a renewal audit.
const MARKER: &str = "<!-- ss-autoreapprove-needs-review ";

/// Service-authenticated creation time controls whether an external approval resets a notice.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Notice {
    pub(crate) comment_id: String,
    pub(crate) created_at: String,
    pub(crate) rationale: String,
}

/// Summary metadata distinguishes automatic approvals from independent reviews.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AutomaticApproval {
    pub(crate) approval_event_id: String,
    pub(crate) approved_at: String,
}

/// Provider-derived positive events retain their external-versus-automatic origin.
pub(crate) struct ApprovalEvent {
    pub(crate) id: String,
    pub(crate) at: Timestamp,
    pub(crate) automatic: bool,
}

/// The marker binds the refusal to its authenticated scope and completed assessment.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Marker {
    schema_version: u32,
    operation_id: String,
    scope: Scope,
    anchor: Anchor,
    source_commit: String,
    policy_digest: String,
    assessment_digest: String,
    rationale: String,
}

/// Retained intent prevents interrupted or overlapping runs from posting duplicate notices.
pub(crate) struct Store<'a> {
    journal: FileJournal<'a>,
    record: Record,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    schema_version: u32,
    scope: Scope,
    attempts: Vec<Attempt>,
}

/// Only a definitive refusal permits a new write; uncertain effects are read back.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Attempt {
    run_id: String,
    marker: Marker,
    body: String,
    state: State,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum State {
    Intent,
    Verified,
    Refused,
}

impl Marker {
    fn parse(body: &str) -> Result<Option<Self>, AppError> {
        let Some((_, remainder)) = body.split_once(MARKER) else {
            return Ok(None);
        };
        let (document, suffix) = remainder
            .split_once("-->")
            .ok_or_else(|| fail("service", "Unterminated needs-review marker"))?;
        if suffix.contains(MARKER) || body.matches("<!-- ss-autoreapprove").count() != 1 {
            return Err(fail("service", "Duplicate needs-review markers"));
        }
        let marker: Self = serde_json::from_str(document.trim())?;
        marker.validate()?;
        Ok(Some(marker))
    }

    fn validate(&self) -> Result<(), AppError> {
        // Reuse the renewal identity and digest contract without treating this as an approval.
        Audit {
            schema_version: self.schema_version,
            operation_id: self.operation_id.clone(),
            repository_id: self.scope.repository_id.clone(),
            account_id: self.scope.account_id.clone(),
            pull_request_id: self.scope.pull_request_id,
            anchor: self.anchor.clone(),
            source_commit: self.source_commit.clone(),
            policy_digest: self.policy_digest.clone(),
            assessment_digest: self.assessment_digest.clone(),
            state: AuditState::Submitted,
        }
        .validate()?;
        if self.rationale.trim().is_empty() {
            return Err(fail("service", "Needs-review marker has no rationale"));
        }
        Ok(())
    }
}

/// Native provider history, not marker prose or timestamps, establishes positive events.
fn approvals(
    evidence: &Evidence,
    scope: &Scope,
    now: Timestamp,
) -> Result<Vec<ApprovalEvent>, AppError> {
    match evidence {
        Evidence::GitHub(GitHubEvidence::History {
            reviews, timeline, ..
        }) => {
            let mut events = Vec::new();
            for review in reviews {
                if identifier(field(field(review, "user")?, "id")?)? != scope.account_id {
                    continue;
                }
                let id = text(field(review, "node_id")?)?;
                let state = text(field(review, "state")?)?;
                let approved = state == "APPROVED"
                    || state == "DISMISSED"
                        && timeline.iter().any(|event| {
                            event.pointer("/review/id").and_then(Value::as_str) == Some(id)
                                && event.get("previousReviewState").and_then(Value::as_str)
                                    == Some("APPROVED")
                        });
                if approved {
                    // Only interpretable audits supply reset metadata; the provider validates standing.
                    let Ok(audit) = Audit::parse(text(field(review, "body")?)?) else {
                        continue;
                    };
                    events.push(ApprovalEvent {
                        id: id.to_owned(),
                        at: timestamp(text(field(review, "submitted_at")?)?)?,
                        automatic: audit.is_some(),
                    });
                }
            }
            Ok(events)
        }
        Evidence::AzureDevOps(evidence) => evidence.approval_events(
            &scope.account_id,
            &scope.repository_id,
            scope.pull_request_id,
            now,
        ),
        Evidence::GitHub(GitHubEvidence::State { .. }) => Ok(Vec::new()),
    }
}

/// Reads only account-owned standalone comments; copied markers cannot affect another account.
fn notices(
    evidence: &Evidence,
    scope: &Scope,
    now: Timestamp,
    external: Option<Timestamp>,
    operation: Option<&str>,
) -> Result<Vec<(Marker, Notice, String)>, AppError> {
    let comments: Vec<(&Value, bool)> = match evidence {
        Evidence::GitHub(GitHubEvidence::History { comments, .. }) => {
            comments.iter().map(|comment| (comment, false)).collect()
        }
        Evidence::AzureDevOps(AdoEvidence::History { threads, .. }) => {
            let mut comments = Vec::new();
            for thread in threads {
                // Deleted ADO threads may retain only a tombstone, without comments.
                if thread.get("isDeleted") == Some(&Value::Bool(true)) {
                    continue;
                }
                for comment in array(field(thread, "comments")?)? {
                    comments.push((comment, true));
                }
            }
            comments
        }
        _ => return Ok(Vec::new()),
    };
    let mut found = Vec::new();
    for (comment, ado) in comments {
        // Only surviving text comments supply provenance; removed payloads may be absent.
        if ado
            && (comment.get("isDeleted") == Some(&Value::Bool(true))
                || comment.get("commentType").and_then(Value::as_str) != Some("text"))
        {
            continue;
        }
        let body = text(field(comment, if ado { "content" } else { "body" })?)?;
        if !body.contains(MARKER) {
            continue;
        }
        let author = if ado { "author" } else { "user" };
        if identifier(field(field(comment, author)?, "id")?)? != scope.account_id {
            continue;
        }
        // Readback validates only this operation, not unrelated expired or edited notices.
        // Full marker, exact-body, and uniqueness checks still establish the owned effect.
        if operation.is_some_and(|operation| !body.contains(operation)) {
            continue;
        }
        let created = text(field(
            comment,
            if ado { "publishedDate" } else { "created_at" },
        )?)?;
        let at = timestamp(created)?;
        if at > now {
            return Err(fail("service", "Future needs-review notice"));
        }
        // A fresh independent review supersedes the old notice entirely, including later edits.
        if external.is_some_and(|external| external > at) {
            continue;
        }
        let Some(marker) = Marker::parse(body)? else {
            continue;
        };
        if marker.scope != *scope || comment.get("isDeleted") == Some(&json!(true)) {
            return Err(fail(
                "service",
                "Invalid owned needs-review notice scope or deletion",
            ));
        }
        let updated = text(field(
            comment,
            if ado {
                "lastContentUpdatedDate"
            } else {
                "updated_at"
            },
        )?)?;
        if timestamp(updated)? != at {
            return Err(fail("service", "Edited needs-review notice"));
        }
        found.push((
            marker.clone(),
            Notice {
                comment_id: identifier(field(comment, "id")?)?,
                created_at: created.to_owned(),
                rationale: marker.rationale,
            },
            body.to_owned(),
        ));
    }
    Ok(found)
}

/// A later external approval resets every older notice, but a renewal never does.
pub(crate) fn apply(
    evidence: &Evidence,
    decision: Decision,
    account: &str,
    repository: &str,
    number: u32,
    now: Timestamp,
) -> Result<Decision, AppError> {
    let scope = Scope {
        account_id: account.to_owned(),
        repository_id: repository.to_owned(),
        pull_request_id: number,
    };
    let external = approvals(evidence, &scope, now)?
        .into_iter()
        .filter(|event| !event.automatic)
        .map(|event| event.at)
        .max();
    let notices = notices(evidence, &scope, now, external, None)?;
    let mut active = None;
    for (_, notice, _) in notices {
        let at = timestamp(&notice.created_at)?;
        if external.is_none_or(|external| external <= at)
            && active.as_ref().is_none_or(|(latest, _)| *latest < at)
        {
            active = Some((at, notice));
        }
    }
    // Closed/draft/own PRs remain service exclusions; all other active notices still need review.
    let excluded = matches!(&decision, Decision::Ineligible { reason } if matches!(reason.as_str(), "closed" | "draft" | "selfIsAuthor"));
    if !excluded && let Some((_, notice)) = active {
        return Ok(Decision::NeedsReview { notice });
    }
    Ok(decision)
}

/// Resolves the timestamp only for the current standing automatic approval.
pub(crate) fn automatic_approval(
    evidence: &Evidence,
    scope: &Scope,
    now: Timestamp,
) -> Result<Option<AutomaticApproval>, AppError> {
    let platform = match evidence {
        Evidence::GitHub(_) => Platform::GitHub,
        Evidence::AzureDevOps(_) => Platform::AzureDevOps,
    };
    let decision = evidence.decide(
        platform,
        &scope.account_id,
        &scope.repository_id,
        scope.pull_request_id,
        now,
    )?;
    let Decision::AlreadyValid { approval_event_id } = decision else {
        return Ok(None);
    };
    Ok(approvals(evidence, scope, now)?
        .into_iter()
        .find(|event| event.id == approval_event_id && event.automatic)
        .map(|event| AutomaticApproval {
            approval_event_id: event.id,
            approved_at: event.at.to_string(),
        }))
}

impl<'a> Store<'a> {
    /// Callers hold the shared publication lock before opening this sibling journal.
    pub(crate) fn open(
        platform: Platform,
        directory: &Path,
        scope: &Scope,
        clock: &'a dyn Clock,
    ) -> Result<Self, AppError> {
        let path = directory
            .join("review-notices")
            .join(format!("{}.json", hash(&(platform, scope))?));
        let journal = FileJournal::open(&path, clock)?;
        let record = match journal.read_bytes()? {
            Some(bytes) => serde_json::from_slice::<Record>(&bytes)?,
            None => Record {
                schema_version: 1,
                scope: scope.clone(),
                attempts: Vec::new(),
            },
        };
        if record.schema_version != 1 || record.scope != *scope {
            return Err(fail("invalidState", "Invalid needs-review journal scope"));
        }

        let mut runs = HashSet::new();
        let mut operations = HashSet::new();
        for attempt in &record.attempts {
            attempt.marker.validate()?;
            if attempt.marker.scope != *scope
                || Uuid::parse_str(&attempt.run_id).is_err()
                || !runs.insert(&attempt.run_id)
                || !operations.insert(&attempt.marker.operation_id)
                || Marker::parse(&attempt.body)?.as_ref() != Some(&attempt.marker)
            {
                return Err(fail("invalidState", "Invalid needs-review journal attempt"));
            }
        }
        Ok(Self { journal, record })
    }

    pub(crate) fn ensure_resolved(&self) -> Result<(), AppError> {
        if self
            .record
            .attempts
            .iter()
            .any(|attempt| attempt.state == State::Intent)
        {
            return Err(fail(
                "ambiguousWrite",
                "An unresolved needs-review comment blocks publication; reconcile its original notify invocation",
            ));
        }
        Ok(())
    }

    /// Recovery stays bound to the original PR and run, never another run's failed scope.
    pub(crate) fn has_attempt(&self, run: &str) -> bool {
        self.record
            .attempts
            .iter()
            .any(|attempt| attempt.run_id == run)
    }

    /// Intent remains uncertain until readback establishes the owned comment or a refusal.
    pub(crate) fn has_unresolved(&self, run: &str) -> bool {
        self.record
            .attempts
            .iter()
            .any(|attempt| attempt.run_id == run && attempt.state == State::Intent)
    }

    /// Refreshes immutable evidence before a single comment POST and reconciles intent read-only.
    pub(crate) fn publish(
        &mut self,
        api: &mut Api<'_>,
        target: &Target,
        clock: &dyn Clock,
        input: &Input<'_>,
    ) -> Result<Notice, AppError> {
        if api.identity()?.id.as_deref() != Some(&self.record.scope.account_id) {
            return Err(fail(
                "authentication",
                "Needs-review publication account changed",
            ));
        }
        let fresh = Evidence::capture(api, target)?;
        if api.identity()?.id.as_deref() != Some(&self.record.scope.account_id) {
            return Err(fail(
                "authentication",
                "Needs-review publication account changed during capture",
            ));
        }
        let now = clock.now();
        if let Some(index) = self
            .record
            .attempts
            .iter()
            .position(|attempt| attempt.run_id == input.run_id)
        {
            let marker = &self
                .record
                .attempts
                .get(index)
                .ok_or_else(|| fail("invalidState", "Missing notice attempt"))?
                .marker;
            if marker.assessment_digest != input.assessment_digest
                || marker.policy_digest != input.policy_digest
                || marker.anchor != *input.anchor
                || marker.source_commit != input.source_commit
            {
                return Err(fail(
                    "invalidState",
                    "Notice recovery requires the original immutable assessment",
                ));
            }
            return self.reconcile(index, &fresh, now);
        }
        // Existing effects need authenticated readback, not renewed approval eligibility.
        let decision = fresh.decide(
            target.platform,
            &self.record.scope.account_id,
            &self.record.scope.repository_id,
            self.record.scope.pull_request_id,
            now,
        )?;
        self.ensure_resolved()?;
        if let Decision::NeedsReview { notice } = decision {
            return Ok(notice);
        }
        let unchanged = match (input.evidence, &fresh) {
            (Evidence::AzureDevOps(left), Evidence::AzureDevOps(right)) => {
                ado_approval::equivalent(left, right)?
            }
            _ => input.evidence == &fresh,
        };
        if !unchanged {
            return Err(fail(
                "staleEvidence",
                "Source or decision evidence changed before needs-review publication",
            ));
        }
        let marker = Marker {
            schema_version: 1,
            operation_id: Uuid::new_v4().to_string(),
            scope: self.record.scope.clone(),
            anchor: input.anchor.clone(),
            source_commit: input.source_commit.to_owned(),
            policy_digest: input.policy_digest.to_owned(),
            assessment_digest: input.assessment_digest.to_owned(),
            rationale: input.explanation.to_owned(),
        };
        let document = serde_json::to_string(&marker)?.replace("-->", "\\u002d\\u002d>");
        let body = format!(
            "{}\n\nThis pull request cannot be automatically reapproved and needs a separate new review.\n\n{}\n\n{MARKER}{document} -->",
            input.attribution, input.explanation
        );
        if Marker::parse(&body)?.as_ref() != Some(&marker) {
            return Err(fail(
                "invalidInput",
                "Notice explanation or attribution conflicts with its reserved marker",
            ));
        }
        self.record.attempts.push(Attempt {
            run_id: input.run_id.to_owned(),
            marker,
            body: body.clone(),
            state: State::Intent,
        });
        if let Err(error) = self.save() {
            // No effect starts before intent is durable, so a failed save leaves no owned attempt.
            let _ = self.record.attempts.pop();
            return Err(error);
        }
        let response = match target.platform {
            Platform::GitHub => api.transport.github_write_once(&format!("{}/issues/{}/comments", target.repository_api(), target.pull_request_id), &json!({"body": body})),
            Platform::AzureDevOps => api.transport.ado_write_once(&format!("{}/threads", target.pr_api()), "POST", &json!({"status": 4, "comments": [{"parentCommentId": 0, "commentType": 1, "content": body}]})),
        };
        if let Err(error) = response
            && classification(&error) != "ambiguousWrite"
        {
            self.save_state(self.record.attempts.len().saturating_sub(1), State::Refused)?;
            return Err(error);
        }
        // A lost acknowledgement does not permit another POST, even if readback fails.
        let fresh = Evidence::capture(api, target).map_err(|error| fail("ambiguousWrite", format!("Needs-review comment may exist; retain the original run for reconciliation: {error}")))?;
        if api.identity()?.id.as_deref() != Some(&self.record.scope.account_id) {
            return Err(fail(
                "ambiguousWrite",
                "Needs-review comment may exist but authenticated account changed",
            ));
        }
        let index = self.record.attempts.len().saturating_sub(1);
        self.reconcile(index, &fresh, clock.now())
    }

    fn reconcile(
        &mut self,
        index: usize,
        evidence: &Evidence,
        now: Timestamp,
    ) -> Result<Notice, AppError> {
        evidence.validate_scope(
            &self.record.scope.repository_id,
            self.record.scope.pull_request_id,
        )?;
        let attempt = self
            .record
            .attempts
            .get(index)
            .ok_or_else(|| fail("invalidState", "Missing needs-review intent"))?;
        if attempt.state == State::Refused {
            return Err(fail(
                "invalidState",
                "Needs-review comment was refused; start a fresh run",
            ));
        }
        let found = notices(
            evidence,
            &self.record.scope,
            now,
            None,
            Some(&attempt.marker.operation_id),
        )?;
        let matching = found
            .iter()
            .filter(|(marker, _, body)| {
                marker.operation_id == attempt.marker.operation_id && body == &attempt.body
            })
            .collect::<Vec<_>>();
        let [(_, notice, _)] = matching.as_slice() else {
            return Err(fail(
                "ambiguousWrite",
                "No unique owned needs-review comment; publication was not replayed",
            ));
        };
        let notice = notice.clone();
        self.save_state(index, State::Verified)?;
        Ok(notice)
    }

    /// Recovery checks retain the last durable state when a terminal save fails.
    fn save_state(&mut self, index: usize, state: State) -> Result<(), AppError> {
        let previous = replace(
            &mut self
                .record
                .attempts
                .get_mut(index)
                .ok_or_else(|| fail("invalidState", "Missing notice intent"))?
                .state,
            state,
        );
        let result = self.save();
        if result.is_err() {
            self.record
                .attempts
                .get_mut(index)
                .ok_or_else(|| fail("invalidState", "Missing notice intent"))?
                .state = previous;
        }
        result
    }

    fn save(&self) -> Result<(), AppError> {
        self.journal
            .write_bytes(&serde_json::to_vec_pretty(&self.record)?)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        reason = "Tests mutate bounded synthetic fixture paths"
    )]

    use ohno::AppError;
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{Marker, notices};
    use crate::ado_approval::Evidence as AdoEvidence;
    use crate::approval::{Decision, Evidence};
    use crate::clock::timestamp;
    use crate::publication::Scope;
    use crate::renewal::{Anchor, Audit, State};
    use crate::target::Platform;

    /// Supplies the same complete provider history used by approval authority tests.
    fn fixture() -> Value {
        serde_json::from_str::<Value>(include_str!("../tests/fixtures/github-approval.json"))
            .unwrap()["evidence"]
            .clone()
    }

    fn decide(evidence: &Value) -> Result<Decision, AppError> {
        serde_json::from_value::<Evidence>(json!({"platform": "gitHub", "observation": evidence}))?
            .decide(
                Platform::GitHub,
                "42",
                "123",
                1,
                timestamp("2026-01-10T00:00:00Z")?,
            )
    }

    /// Synthetic marker identities use the fixture's external anchor and current source.
    fn marker() -> Marker {
        Marker {
            schema_version: 1,
            operation_id: Uuid::new_v4().to_string(),
            scope: Scope {
                repository_id: "123".into(),
                account_id: "42".into(),
                pull_request_id: 1,
            },
            anchor: Anchor {
                review_id: "review-2".into(),
                source_commit: "a".repeat(40),
            },
            source_commit: "b".repeat(40),
            policy_digest: "c".repeat(64),
            assessment_digest: "d".repeat(64),
            rationale: "Significant design change requires a new review.".into(),
        }
    }

    fn add_notice(evidence: &mut Value, at: &str, id: u32) {
        evidence["comments"].as_array_mut().unwrap().push(json!({
            "id": id, "user": {"id": 42}, "created_at": at, "updated_at": at,
            "body": format!("[Copilot speaking]\n\n<!-- ss-autoreapprove-needs-review {} -->", serde_json::to_string(&marker()).unwrap())
        }));
    }

    /// Positive events vary only their service author, submission time, and renewal origin.
    fn add_approval(evidence: &mut Value, at: &str, owned: bool, automatic: bool) {
        let audit = Audit {
            schema_version: 1,
            operation_id: Uuid::new_v4().to_string(),
            repository_id: "123".into(),
            account_id: "42".into(),
            pull_request_id: 1,
            anchor: marker().anchor,
            source_commit: "b".repeat(40),
            policy_digest: "c".repeat(64),
            assessment_digest: "d".repeat(64),
            state: State::Submitted,
        };
        evidence["reviews"].as_array_mut().unwrap().push(json!({
            "id": 50, "node_id": "new-approval",
            "user": if owned { json!({"id": 42, "node_id": "user-42"}) } else { json!({"id": 99, "node_id": "user-99"}) },
            "state": "APPROVED", "submitted_at": at, "commit_id": "b".repeat(40),
            "body": if automatic { format!("<!-- ss-autoreapprove {} -->", serde_json::to_string(&audit).unwrap()) } else { "External approval".into() }
        }));
        evidence["timeline"].as_array_mut().unwrap().push(json!({
            "__typename": "PullRequestReview", "id": "new-approval", "state": "APPROVED"
        }));
    }

    #[test]
    fn only_strictly_later_owned_external_approvals_expire_notices() {
        for (at, owned, automatic, expected) in [
            ("2026-01-08T23:00:00Z", true, false, "needsReview"),
            ("2026-01-09T00:00:00Z", true, false, "needsReview"),
            ("2026-01-09T01:00:00Z", false, false, "needsReview"),
            ("2026-01-09T01:00:00Z", true, true, "needsReview"),
            ("2026-01-09T01:00:00Z", true, false, "alreadyValid"),
        ] {
            let mut evidence = fixture();
            add_notice(&mut evidence, "2026-01-09T00:00:00Z", 60);
            add_approval(&mut evidence, at, owned, automatic);
            assert_eq!(
                decide(&evidence).unwrap().status(),
                expected,
                "{at}: {owned}: {automatic}"
            );
        }
    }

    #[test]
    fn external_approval_does_not_expire_a_newer_notice() {
        let mut evidence = fixture();
        add_notice(&mut evidence, "2026-01-09T00:00:00Z", 60);
        add_approval(&mut evidence, "2026-01-09T01:00:00Z", true, false);
        add_notice(&mut evidence, "2026-01-09T02:00:00Z", 61);
        let Decision::NeedsReview { notice } = decide(&evidence).unwrap() else {
            panic!("Newer notice must remain active");
        };
        assert_eq!(notice.comment_id, "61");
    }

    #[test]
    fn foreign_markers_do_not_block_and_malformed_owned_markers_fail() {
        let mut evidence = fixture();
        add_notice(&mut evidence, "2026-01-09T00:00:00Z", 60);
        evidence["comments"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap()["user"]["id"] = json!(99);
        assert_eq!(decide(&evidence).unwrap().status(), "assessmentRequired");
        for mutation in ["edited", "malformed", "duplicate", "scope", "future"] {
            let mut evidence = fixture();
            add_notice(&mut evidence, "2026-01-09T00:00:00Z", 60);
            let comment = evidence["comments"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap();
            match mutation {
                "edited" => comment["updated_at"] = json!("2026-01-09T00:01:00Z"),
                "malformed" => {
                    comment["body"] = json!("<!-- ss-autoreapprove-needs-review {} -->");
                }
                "duplicate" => {
                    comment["body"] = json!(format!(
                        "{}\n{}",
                        comment["body"].as_str().unwrap(),
                        comment["body"].as_str().unwrap()
                    ));
                }
                "scope" => {
                    let mut marker = marker();
                    marker.scope.pull_request_id = 2;
                    comment["body"] = json!(format!(
                        "<!-- ss-autoreapprove-needs-review {} -->",
                        serde_json::to_string(&marker).unwrap()
                    ));
                }
                "future" => {
                    comment["created_at"] = json!("2027-01-01T00:00:00Z");
                    comment["updated_at"] = comment["created_at"].clone();
                }
                _ => unreachable!(),
            }
            decide(&evidence).unwrap_err();
        }
    }

    #[test]
    fn expired_notices_do_not_regain_authority_through_edits() {
        let mut evidence = fixture();
        add_notice(&mut evidence, "2026-01-09T00:00:00Z", 60);
        add_approval(&mut evidence, "2026-01-09T01:00:00Z", true, false);
        let comment = evidence["comments"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap();
        comment["updated_at"] = json!("2026-01-09T02:00:00Z");
        assert_eq!(decide(&evidence).unwrap().status(), "alreadyValid");
    }

    /// Superseded invalid audits cannot block a later independent approval or expire notices.
    #[test]
    fn later_external_approval_supersedes_malformed_audit_without_granting_authority() {
        let mut evidence = fixture();
        evidence["reviews"][1]["body"] = json!("<!-- ss-autoreapprove malformed -->");
        decide(&evidence).unwrap_err();
        add_approval(&mut evidence, "2026-01-09T01:00:00Z", true, false);
        assert_eq!(decide(&evidence).unwrap().status(), "alreadyValid");
        add_notice(&mut evidence, "2026-01-09T00:00:00Z", 60);
        assert_eq!(decide(&evidence).unwrap().status(), "alreadyValid");
        evidence["reviews"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap()["body"] = json!("<!-- ss-autoreapprove malformed -->");
        decide(&evidence).unwrap_err();
    }

    /// Deleted discussion must neither break scanning nor satisfy notice readback.
    #[test]
    fn ado_notice_scanning_ignores_deleted_payloads_but_validates_live_comments() {
        let marker = marker();
        let now = timestamp("2026-01-10T00:00:00Z").unwrap();
        let comment = json!({
            "id": 60, "commentType": "text", "author": {"id": marker.scope.account_id},
            "publishedDate": "2026-01-09T00:00:00Z",
            "lastContentUpdatedDate": "2026-01-09T00:00:00Z",
            "content": format!("<!-- ss-autoreapprove-needs-review {} -->", serde_json::to_string(&marker).unwrap())
        });
        let mut deleted_comment = comment.clone();
        deleted_comment["isDeleted"] = json!(true);
        for deleted in [
            json!({"id": "deleted", "isDeleted": true}),
            json!({
                "id": "deleted", "isDeleted": true,
                "comments": [{"commentType": "text", "content": null}]
            }),
            json!({
                "id": "deleted-comment",
                "comments": [{"isDeleted": true, "commentType": "text"}]
            }),
            json!({"id": "deleted-notice", "comments": [deleted_comment]}),
            json!({"id": "deleted-thread", "isDeleted": true, "comments": [comment]}),
        ] {
            // The scanner consumes discussion only; its caller validates approval history.
            let mut evidence = Evidence::AzureDevOps(AdoEvidence::History {
                pull_request: Value::Null,
                source_commit: String::new(),
                reviewers: Vec::new(),
                iterations: Vec::new(),
                policies: Vec::new(),
                evaluations: Vec::new(),
                threads: vec![deleted, json!({"id": "live-notice", "comments": [comment]})],
            });
            let found = notices(&evidence, &marker.scope, now, None, None).unwrap();
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].1.comment_id, "60");
            assert_eq!(found[0].1.rationale, marker.rationale);

            let Evidence::AzureDevOps(AdoEvidence::History { threads, .. }) = &mut evidence else {
                unreachable!()
            };
            threads.last_mut().unwrap()["comments"][0]["content"] = Value::Null;
            notices(&evidence, &marker.scope, now, None, None).unwrap_err();
        }
    }
}
