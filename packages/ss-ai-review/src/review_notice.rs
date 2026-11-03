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
use crate::github_approval;
use crate::github_approval::Evidence as GitHubEvidence;
use crate::journal::FileJournal;
use crate::json::{array, field, identifier, text};
use crate::publication::{Input, Scope, hash};
use crate::renewal::Anchor;
use crate::target::{Platform, Target};

/// Separate syntax identifies account-owned review notices in discussion.
const MARKER: &str = "<!-- ss-autoreapprove-needs-review ";

/// Service-authenticated creation time controls whether an approval resets a notice.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Notice {
    pub(crate) comment_id: String,
    pub(crate) created_at: String,
    pub(crate) rationale: String,
}

/// A provider-derived positive event can supersede an older notice.
pub(crate) struct ApprovalEvent {
    pub(crate) at: Timestamp,
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
    resets: Vec<ApprovalReset>,
}

/// Each captured positive decision has its own durable, nonreplayable reset intent.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApprovalReset {
    review_id: String,
    state: ResetState,
}

/// Readback, not a missing acknowledgement, determines whether reset intent completed.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum ResetState {
    Pending,
    Intent,
    Verified,
    Refused,
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
        if self.schema_version != 1
            || Uuid::parse_str(&self.operation_id).is_err()
            || self.scope.repository_id.trim().is_empty()
            || self.scope.account_id.trim().is_empty()
            || self.scope.pull_request_id == 0
            || self.anchor.review_id.trim().is_empty()
            || self.rationale.trim().is_empty()
        {
            return Err(fail("service", "Invalid needs-review marker"));
        }
        github_approval::commit(&self.anchor.source_commit)?;
        github_approval::commit(&self.source_commit)?;
        for digest in [&self.policy_digest, &self.assessment_digest] {
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(fail("service", "Invalid needs-review marker digest"));
            }
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
                    events.push(ApprovalEvent {
                        at: timestamp(text(field(review, "submitted_at")?)?)?,
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
    let (platform, collection) = match evidence {
        Evidence::GitHub(GitHubEvidence::History { comments, .. }) => (Platform::GitHub, comments),
        Evidence::AzureDevOps(AdoEvidence::History { threads, .. }) => {
            (Platform::AzureDevOps, threads)
        }
        _ => return Ok(Vec::new()),
    };
    parse_notices(collection, platform, scope, now, external, operation)
}

/// Inspection and operation-specific readback share the same provenance checks.
fn parse_notices(
    collection: &[Value],
    platform: Platform,
    scope: &Scope,
    now: Timestamp,
    external: Option<Timestamp>,
    operation: Option<&str>,
) -> Result<Vec<(Marker, Notice, String)>, AppError> {
    let ado = platform == Platform::AzureDevOps;
    let comments = if ado {
        let mut comments = Vec::new();
        for thread in collection {
            // Deleted ADO threads may retain only a tombstone, without comments.
            if thread.get("isDeleted") == Some(&Value::Bool(true)) {
                continue;
            }
            for comment in array(field(thread, "comments")?)? {
                comments.push(comment);
            }
        }
        comments
    } else {
        collection.iter().collect()
    };
    let mut found = Vec::new();
    for comment in comments {
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

/// Exact bytes and an authenticated author identify the intended prior comment.
fn matching_notice(
    attempt: &Attempt,
    platform: Platform,
    scope: &Scope,
    comments: &[Value],
    now: Timestamp,
) -> Result<Notice, AppError> {
    if attempt.state == State::Refused {
        return Err(fail(
            "invalidState",
            "Needs-review comment was refused; start a fresh run",
        ));
    }
    let found = parse_notices(
        comments,
        platform,
        scope,
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
    Ok(notice.clone())
}

/// Notice effects need fenced discussion, not source refs or current approval authority.
fn capture_comments(
    api: &mut Api<'_>,
    target: &Target,
    scope: &Scope,
) -> Result<Vec<Value>, AppError> {
    let first = observe_comments(api, target, scope)?;
    let second = observe_comments(api, target, scope)?;
    if first != second {
        return Err(fail(
            "staleEvidence",
            "Needs-review discussion changed during capture",
        ));
    }
    Ok(first)
}

/// Every observation validates service PR identity before reading its complete discussion.
fn observe_comments(
    api: &mut Api<'_>,
    target: &Target,
    scope: &Scope,
) -> Result<Vec<Value>, AppError> {
    let pull_request = api.transport.request(&target.pr_api(), "GET", None)?;
    let (evidence, path) = match target.platform {
        Platform::GitHub => (
            Evidence::GitHub(GitHubEvidence::State { pull_request }),
            format!(
                "{}/issues/{}/comments",
                target.repository_api(),
                target.pull_request_id
            ),
        ),
        Platform::AzureDevOps => (
            Evidence::AzureDevOps(AdoEvidence::State { pull_request }),
            format!("{}/threads", target.pr_api()),
        ),
    };
    evidence.validate_scope(&scope.repository_id, scope.pull_request_id)?;
    let mut comments = api.transport.list(&path)?;
    let mut ids = HashSet::new();
    for comment in &comments {
        if !ids.insert(identifier(field(comment, "id")?)?) {
            return Err(fail(
                "service",
                "Duplicate needs-review discussion identity",
            ));
        }
    }
    // Service list ordering does not change the captured discussion.
    comments.sort_by_key(|comment| comment.get("id").map(Value::to_string));
    match target.platform {
        Platform::AzureDevOps => ado_approval::project_threads(&comments),
        Platform::GitHub => Ok(github_approval::project_comments(&comments)),
    }
}

/// A later provider approval resets older notices for the same account.
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
    let approval = approvals(evidence, &scope, now)?
        .into_iter()
        .map(|event| event.at)
        .max();
    let notices = notices(evidence, &scope, now, approval, None)?;
    let mut active = None;
    for (_, notice, _) in notices {
        let at = timestamp(&notice.created_at)?;
        if approval.is_none_or(|approval| approval <= at)
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

/// Bind resets to positive decisions captured before the notice, never future approvals.
fn planned_resets(evidence: &Evidence, scope: &Scope) -> Result<Vec<ApprovalReset>, AppError> {
    let (items, github) = match evidence {
        Evidence::GitHub(GitHubEvidence::History { reviews, .. }) => (reviews, true),
        Evidence::AzureDevOps(AdoEvidence::History { reviewers, .. }) => (reviewers, false),
        _ => {
            return Err(fail(
                "invalidState",
                "Approval reset requires complete captured history",
            ));
        }
    };
    let mut resets = Vec::new();
    for item in items {
        let account = if github { field(item, "user")? } else { item };
        if identifier(field(account, "id")?)? != scope.account_id {
            continue;
        }
        let positive = if github {
            text(field(item, "state")?)? == "APPROVED"
        } else {
            matches!(field(item, "vote")?.as_i64(), Some(5 | 10))
        };
        if positive {
            resets.push(ApprovalReset {
                review_id: identifier(field(item, "id")?)?,
                state: ResetState::Pending,
            });
        }
    }
    Ok(resets)
}

/// Recheck ownership and standing without requiring current source refs or approval eligibility.
fn reset_required(
    api: &mut Api<'_>,
    target: &Target,
    scope: &Scope,
    review_id: &str,
    original: &Evidence,
    now: Timestamp,
) -> Result<bool, AppError> {
    if api.identity()?.id.as_deref() != Some(&scope.account_id) {
        return Err(fail("authentication", "Approval reset account changed"));
    }
    let pr = api.transport.request(&target.pr_api(), "GET", None)?;
    let github = target.platform == Platform::GitHub;
    let evidence = if github {
        Evidence::GitHub(GitHubEvidence::State {
            pull_request: pr.clone(),
        })
    } else {
        Evidence::AzureDevOps(AdoEvidence::State {
            pull_request: pr.clone(),
        })
    };
    evidence.validate_scope(&scope.repository_id, scope.pull_request_id)?;
    // Closed and draft PR recovery verifies prior effects without initiating new ones.
    let active = if github {
        text(field(&pr, "state")?)? == "open" && field(&pr, "draft")? == &Value::Bool(false)
    } else {
        text(field(&pr, "status")?)? == "active" && field(&pr, "isDraft")? == &Value::Bool(false)
    };
    if !active {
        return Ok(false);
    }
    let items = api.transport.list(&format!(
        "{}/{}",
        target.pr_api(),
        if github { "reviews" } else { "reviewers" }
    ))?;
    let mut item = None;
    for candidate in &items {
        if identifier(field(candidate, "id")?)? == review_id {
            if item.is_some() {
                return Err(fail("service", "Duplicate approval reset identity"));
            }
            item = Some(candidate);
        }
    }
    let Some(item) = item else {
        if github {
            return Err(fail(
                "service",
                "Captured approval disappeared during reset readback",
            ));
        }
        // A removed ADO reviewer has no approval to clear.
        return Ok(false);
    };
    let account = if github { field(item, "user")? } else { item };
    if identifier(field(account, "id")?)? != scope.account_id {
        return Err(fail(
            "service",
            "Approval reset review belongs to another account",
        ));
    }
    if github {
        // Review IDs are immutable: dismissing this old review cannot dismiss a new review.
        return Ok(text(field(item, "state")?)? == "APPROVED");
    }
    let vote = field(item, "vote")?
        .as_i64()
        .ok_or_else(|| fail("service", "Invalid reviewer vote during approval reset"))?;
    if !matches!(vote, -10 | -5 | 0 | 5 | 10) {
        return Err(fail(
            "service",
            "Unknown reviewer vote during approval reset",
        ));
    }
    if vote <= 0 || item.get("hasDeclined") == Some(&Value::Bool(true)) {
        // Do not replace withdrawals, negative votes, or declined reviews.
        return Ok(false);
    }
    let mut current = original.clone();
    let Evidence::AzureDevOps(AdoEvidence::History { threads, .. }) = &mut current else {
        return Err(fail(
            "invalidState",
            "ADO reset lacks captured approval history",
        ));
    };
    *threads = capture_comments(api, target, scope)?;
    let approval_times = |evidence: &Evidence| -> Result<Vec<Timestamp>, AppError> {
        Ok(approvals(evidence, scope, now)?
            .into_iter()
            .map(|event| event.at)
            .collect())
    };
    // A later independent approval belongs to the human, even if its vote strength is unchanged.
    let current = approval_times(&current)?;
    let original = approval_times(original)?;
    let unchanged = current == original;
    if !unchanged && current.iter().max() <= original.iter().max() {
        return Err(fail(
            "service",
            "Approval history changed without a provably later approval; reset requires manual attention",
        ));
    }
    if api.identity()?.id.as_deref() != Some(&scope.account_id) {
        return Err(fail(
            "authentication",
            "Approval reset account changed during capture",
        ));
    }
    Ok(unchanged)
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
            Some(bytes) => {
                let document: Value = serde_json::from_slice(&bytes)?;
                if document.get("schemaVersion") != Some(&json!(2)) {
                    return Err(fail(
                        "invalidState",
                        "Unsupported needs-review journal schema",
                    ));
                }
                serde_json::from_value::<Record>(document)?
            }
            None => Record {
                schema_version: 2,
                scope: scope.clone(),
                attempts: Vec::new(),
            },
        };
        if record.schema_version != 2 || record.scope != *scope {
            return Err(fail("invalidState", "Invalid needs-review journal scope"));
        }

        let mut runs = HashSet::new();
        let mut operations = HashSet::new();
        for attempt in &record.attempts {
            attempt.marker.validate()?;
            let mut resets = HashSet::new();
            if attempt.marker.scope != *scope
                || Uuid::parse_str(&attempt.run_id).is_err()
                || !runs.insert(&attempt.run_id)
                || !operations.insert(&attempt.marker.operation_id)
                || Marker::parse(&attempt.body)?.as_ref() != Some(&attempt.marker)
                || attempt.resets.iter().any(|reset| {
                    reset.review_id.trim().is_empty()
                        || !resets.insert(&reset.review_id)
                        || platform == Platform::AzureDevOps && reset.review_id != scope.account_id
                        || platform == Platform::GitHub && reset.review_id.parse::<u64>().is_err()
                        || attempt.state == State::Verified && reset.state != ResetState::Verified
                })
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
                "An unresolved needs-review notification blocks publication; reconcile its original notify invocation",
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

    /// Posts a decision notice once, then clears only the captured account-owned approvals.
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
            let expected = planned_resets(input.evidence, &self.record.scope)?;
            let attempt = self
                .record
                .attempts
                .get(index)
                .ok_or_else(|| fail("invalidState", "Missing notice attempt"))?;
            if marker.assessment_digest != input.assessment_digest
                || marker.policy_digest != input.policy_digest
                || marker.anchor != *input.anchor
                || marker.source_commit != input.source_commit
                || !attempt
                    .resets
                    .iter()
                    .map(|reset| &reset.review_id)
                    .eq(expected.iter().map(|reset| &reset.review_id))
            {
                return Err(fail(
                    "invalidState",
                    "Notice recovery requires the original immutable assessment",
                ));
            }
            return self.readback(api, target, clock, index, input);
        }
        // New notices require full preflight evidence; readback cannot authorize a new effect.
        let fresh = Evidence::capture(api, target)?;
        if api.identity()?.id.as_deref() != Some(&self.record.scope.account_id) {
            return Err(fail(
                "authentication",
                "Needs-review publication account changed during capture",
            ));
        }
        let now = clock.now();
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
        if matches!(&decision, Decision::Ineligible { reason } if reason == "draft") {
            return Err(fail("draft", "Draft PR excluded before notice publication"));
        }
        if !input.evidence.equivalent(&fresh)? {
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
            "{}\n\n## Automatic reapproval withheld\n\n### Reasons\n\n{}\n\n{MARKER}{document} -->",
            input.attribution, input.explanation
        );
        if Marker::parse(&body)?.as_ref() != Some(&marker) {
            return Err(fail(
                "invalidInput",
                "Notice explanation or attribution conflicts with its reserved marker",
            ));
        }
        if let Some(deadline) = input.effect_deadline {
            deadline.check(clock)?;
        }
        self.record.attempts.push(Attempt {
            run_id: input.run_id.to_owned(),
            marker,
            body: body.clone(),
            state: State::Intent,
            resets: planned_resets(&fresh, &self.record.scope)?,
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
        let index = self.record.attempts.len().saturating_sub(1);
        self.readback(api, target, clock, index, input)
    }

    /// Reconciles comments and reset intents without replaying either uncertain write.
    fn readback(
        &mut self,
        api: &mut Api<'_>,
        target: &Target,
        clock: &dyn Clock,
        index: usize,
        input: &Input<'_>,
    ) -> Result<Notice, AppError> {
        let comments = capture_comments(api, target, &self.record.scope).map_err(|error| fail("ambiguousWrite", format!("Needs-review comment may exist; retain the original run for reconciliation: {error}")))?;
        if api.identity()?.id.as_deref() != Some(&self.record.scope.account_id) {
            return Err(fail(
                "ambiguousWrite",
                "Needs-review comment may exist but authenticated account changed",
            ));
        }
        let attempt = self
            .record
            .attempts
            .get(index)
            .ok_or_else(|| fail("invalidState", "Missing needs-review intent"))?;
        let notice = matching_notice(
            attempt,
            target.platform,
            &self.record.scope,
            &comments,
            clock.now(),
        )?;
        // A completed operation cannot clear approvals from a later independent review.
        if attempt.state == State::Verified {
            return Ok(notice);
        }
        self.reset_approvals(api, target, clock, index, input)?;
        self.save_state(index, State::Verified)?;
        Ok(notice)
    }

    /// Only pending resets may write; intent and refusals are never automatically resent.
    fn reset_approvals(
        &mut self,
        api: &mut Api<'_>,
        target: &Target,
        clock: &dyn Clock,
        index: usize,
        input: &Input<'_>,
    ) -> Result<(), AppError> {
        let reset_count = self
            .record
            .attempts
            .get(index)
            .ok_or_else(|| fail("invalidState", "Missing notice intent"))?
            .resets
            .len();
        for reset_index in 0..reset_count {
            let reset = self
                .record
                .attempts
                .get(index)
                .and_then(|attempt| attempt.resets.get(reset_index))
                .ok_or_else(|| fail("invalidState", "Missing approval reset"))?;
            let state = reset.state;
            let review_id = reset.review_id.clone();
            if state == ResetState::Verified {
                continue;
            }
            if !reset_required(
                api,
                target,
                &self.record.scope,
                &review_id,
                input.evidence,
                clock.now(),
            )? {
                self.save_reset_state(index, reset_index, ResetState::Verified)?;
                continue;
            }
            if state == ResetState::Refused {
                return Err(fail(
                    "service",
                    "Needs-review comment exists, but approval reset was refused; clear the approval manually",
                ));
            }
            if state == ResetState::Intent {
                return Err(fail(
                    "ambiguousWrite",
                    "Approval reset remains unverified; the request was not replayed",
                ));
            }
            if let Some(deadline) = input.effect_deadline {
                deadline.check(clock)?;
            }
            if api.identity()?.id.as_deref() != Some(&self.record.scope.account_id) {
                return Err(fail(
                    "authentication",
                    "Approval reset account changed before submission",
                ));
            }
            self.save_reset_state(index, reset_index, ResetState::Intent)?;
            let result = match target.platform {
                Platform::GitHub => api.transport.github_dismiss_once(
                    &format!("{}/reviews/{review_id}/dismissals", target.pr_api()),
                    &json!({"message": format!("{}\n\n{}", input.attribution, input.explanation)}),
                ),
                Platform::AzureDevOps => api.transport.ado_write_once(
                    &format!("{}/reviewers/{review_id}", target.pr_api()),
                    "PUT",
                    &json!({"id": review_id, "vote": 0}),
                ),
            };
            if let Err(error) = result
                && classification(&error) != "ambiguousWrite"
            {
                self.save_reset_state(index, reset_index, ResetState::Refused)?;
                return Err(error);
            }
            // Acknowledgements alone do not prove that the account's old approval is gone.
            if reset_required(
                api,
                target,
                &self.record.scope,
                &review_id,
                input.evidence,
                clock.now(),
            )? {
                return Err(fail(
                    "ambiguousWrite",
                    "Needs-review comment exists, but approval reset could not be verified",
                ));
            }
            self.save_reset_state(index, reset_index, ResetState::Verified)?;
        }
        Ok(())
    }

    /// Failed saves must not turn a pending or uncertain reset into a replayable request.
    fn save_reset_state(
        &mut self,
        index: usize,
        reset_index: usize,
        state: ResetState,
    ) -> Result<(), AppError> {
        let reset = self
            .record
            .attempts
            .get_mut(index)
            .and_then(|attempt| attempt.resets.get_mut(reset_index))
            .ok_or_else(|| fail("invalidState", "Missing approval reset"))?;
        let previous = replace(&mut reset.state, state);
        if let Err(error) = self.save() {
            self.record
                .attempts
                .get_mut(index)
                .and_then(|attempt| attempt.resets.get_mut(reset_index))
                .ok_or_else(|| fail("invalidState", "Missing approval reset"))?
                .state = previous;
            return Err(error);
        }
        Ok(())
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

    use super::{Attempt, Marker, State, matching_notice, notices, planned_resets};
    use crate::ado_approval::Evidence as AdoEvidence;
    use crate::approval::{Decision, Evidence};
    use crate::clock::timestamp;
    use crate::publication::Scope;
    use crate::renewal::Anchor;
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

    /// Only account-owned positive decisions are reset targets, even when several remain.
    #[test]
    fn reset_plans_exclude_other_accounts_and_nonpositive_decisions() {
        let scope = marker().scope;
        let mut history = fixture();
        history["reviews"][0]["state"] = json!("APPROVED");
        history["reviews"][1]["state"] = json!("APPROVED");
        let mut older = history["reviews"][1].clone();
        older["id"] = json!(4);
        older["node_id"] = json!("older-review");
        history["reviews"].as_array_mut().unwrap().push(older);
        let evidence = serde_json::from_value::<Evidence>(
            json!({"platform": "gitHub", "observation": history}),
        )
        .unwrap();
        let plan = planned_resets(&evidence, &scope).unwrap();
        assert_eq!(
            plan.iter()
                .map(|reset| reset.review_id.as_str())
                .collect::<Vec<_>>(),
            ["2", "4"]
        );

        let mut history: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/ado-no-approval-policy.json"
        ))
        .unwrap();
        let account = history["stages"][2]["reviewers"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let scope = Scope {
            account_id: account.clone(),
            ..scope
        };
        for vote in [-10, -5, 0, 5, 10] {
            history["stages"][2]["reviewers"][0]["vote"] = json!(vote);
            let stage = &history["stages"][2];
            let evidence = Evidence::AzureDevOps(AdoEvidence::History {
                pull_request: stage["pullRequest"].clone(),
                source_commit: stage["sourceCommit"].as_str().unwrap().to_owned(),
                reviewers: stage["reviewers"].as_array().unwrap().clone(),
                iterations: stage["iterations"].as_array().unwrap().clone(),
                threads: stage["threads"].as_array().unwrap().clone(),
                policies: stage["policies"].as_array().unwrap().clone(),
                evaluations: stage["evaluations"].as_array().unwrap().clone(),
            });
            let plan = planned_resets(&evidence, &scope).unwrap();
            assert_eq!(plan.len(), usize::from(vote > 0));
            if vote > 0 {
                assert_eq!(plan[0].review_id, account);
            }
        }
    }

    #[test]
    fn readback_requires_one_exact_owned_comment_on_each_provider() {
        let marker = marker();
        let body = format!(
            "Notice\n\n<!-- ss-autoreapprove-needs-review {} -->",
            serde_json::to_string(&marker).unwrap()
        );
        let attempt = Attempt {
            run_id: Uuid::new_v4().to_string(),
            marker: marker.clone(),
            body: body.clone(),
            state: State::Intent,
            resets: Vec::new(),
        };
        let now = timestamp("2026-01-10T00:00:00Z").unwrap();
        for platform in [Platform::GitHub, Platform::AzureDevOps] {
            let comment = if platform == Platform::GitHub {
                json!({"id": 60, "user": {"id": 42}, "created_at": "2026-01-09T00:00:00Z",
                    "updated_at": "2026-01-09T00:00:00Z", "body": body})
            } else {
                json!({"id": 1, "comments": [{"id": 60, "commentType": "text",
                    "author": {"id": "42"}, "publishedDate": "2026-01-09T00:00:00Z",
                    "lastContentUpdatedDate": "2026-01-09T00:00:00Z", "content": body}]})
            };
            let notice = matching_notice(
                &attempt,
                platform,
                &marker.scope,
                std::slice::from_ref(&comment),
                now,
            )
            .unwrap();
            assert_eq!(notice.comment_id, "60");
            let error = matching_notice(&attempt, platform, &marker.scope, &[], now).unwrap_err();
            assert_eq!(crate::classification(&error), "ambiguousWrite");
            let error = matching_notice(
                &attempt,
                platform,
                &marker.scope,
                &[comment.clone(), comment.clone()],
                now,
            )
            .unwrap_err();
            assert_eq!(crate::classification(&error), "ambiguousWrite");
            let mut changed = comment.clone();
            if platform == Platform::GitHub {
                changed["user"]["id"] = json!(99);
            } else {
                changed["comments"][0]["author"]["id"] = json!("99");
            }
            matching_notice(&attempt, platform, &marker.scope, &[changed], now).unwrap_err();
            let mut changed = comment;
            if platform == Platform::GitHub {
                changed["body"] = json!(format!("Changed {body}"));
            } else {
                changed["comments"][0]["content"] = json!(format!("Changed {body}"));
            }
            matching_notice(&attempt, platform, &marker.scope, &[changed], now).unwrap_err();
        }
    }

    fn add_notice(evidence: &mut Value, at: &str, id: u32) {
        evidence["comments"].as_array_mut().unwrap().push(json!({
            "id": id, "user": {"id": 42}, "created_at": at, "updated_at": at,
            "body": format!("[Copilot speaking]\n\n<!-- ss-autoreapprove-needs-review {} -->", serde_json::to_string(&marker()).unwrap())
        }));
    }

    /// Approval body content does not change the provider's approval event.
    fn add_approval(evidence: &mut Value, at: &str, owned: bool, automatic: bool) {
        evidence["reviews"].as_array_mut().unwrap().push(json!({
            "id": 50, "node_id": "new-approval",
            "user": if owned { json!({"id": 42, "node_id": "user-42"}) } else { json!({"id": 99, "node_id": "user-99"}) },
            "state": "APPROVED", "submitted_at": at, "commit_id": "b".repeat(40),
            "body": if automatic { "Automatic approval" } else { "Approval" }
        }));
        evidence["timeline"].as_array_mut().unwrap().push(json!({
            "__typename": "PullRequestReview", "id": "new-approval", "state": "APPROVED"
        }));
    }

    #[test]
    fn only_strictly_later_owned_approvals_expire_notices() {
        for (at, owned, automatic, expected) in [
            ("2026-01-08T23:00:00Z", true, false, "needsReview"),
            ("2026-01-09T00:00:00Z", true, false, "needsReview"),
            ("2026-01-09T01:00:00Z", false, false, "needsReview"),
            ("2026-01-09T01:00:00Z", true, true, "alreadyValid"),
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

    /// Provider approval events, not body markers, determine notice expiry.
    #[test]
    fn approval_body_markers_do_not_change_provider_authority() {
        let mut evidence = fixture();
        evidence["reviews"][1]["body"] = json!("<!-- ss-autoreapprove malformed -->");
        assert_eq!(decide(&evidence).unwrap().status(), "assessmentRequired");
        add_approval(&mut evidence, "2026-01-09T01:00:00Z", true, false);
        assert_eq!(decide(&evidence).unwrap().status(), "alreadyValid");
        add_notice(&mut evidence, "2026-01-09T00:00:00Z", 60);
        assert_eq!(decide(&evidence).unwrap().status(), "alreadyValid");
        evidence["reviews"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap()["body"] = json!("<!-- ss-autoreapprove malformed -->");
        assert_eq!(decide(&evidence).unwrap().status(), "alreadyValid");
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
