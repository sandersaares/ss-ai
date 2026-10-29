use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::api::Api;
use crate::approval::{Decision, Evidence as ProviderEvidence};
use crate::clock::timestamp;
use crate::error::fail;
use crate::json::{array, boolean, field, identifier, project, text};
use crate::renewal::{Anchor, Audit};
use crate::review_notice::ApprovalEvent;
use crate::target::{Target, escape};
use crate::transport::ado_items;

/// Immutable service evidence binds reviewer authority to source and policy state.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "scope", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum Evidence {
    /// Current PR metadata alone can exclude a closed or draft PR.
    State {
        #[serde(rename = "pullRequest")]
        pull_request: Value,
    },
    /// Complete, twice-observed lists distinguish history from current vote state.
    History {
        #[serde(rename = "pullRequest")]
        pull_request: Value,
        #[serde(rename = "sourceCommit")]
        source_commit: String,
        reviewers: Vec<Value>,
        iterations: Vec<Value>,
        threads: Vec<Value>,
        policies: Vec<Value>,
        evaluations: Vec<Value>,
    },
}

/// A relevant service event, ordered by service time rather than commit dates.
struct Event<'a> {
    id: String,
    at: Timestamp,
    value: &'a Value,
    kind: &'a str,
}

/// Combines applicable reviewer policies without confusing merge checks with votes.
#[derive(Default)]
struct ApprovalPolicy {
    reset_on_push: bool,
    reset_rejections: bool,
    fresh_iteration: bool,
}

/// Joins a separate authenticated audit to ADO's source-iteration vote event.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Renewal {
    pub(crate) audit: Audit,
    pub(crate) vote_event_id: String,
    pub(crate) iteration_id: u64,
}

impl Evidence {
    /// Exposes authenticated positive event times for notice expiry and result reporting.
    pub(crate) fn approval_events(
        &self,
        account: &str,
        repository: &str,
        number: u32,
        now: Timestamp,
    ) -> Result<Vec<ApprovalEvent>, AppError> {
        let Self::History {
            threads,
            iterations,
            ..
        } = self
        else {
            return Ok(Vec::new());
        };
        let events = ordered_events(threads, now)?;
        let renewals = renewals(
            threads, &events, iterations, account, repository, number, now,
        )?;
        let mut approvals = Vec::new();
        for event in events {
            if event.kind == "VoteUpdate"
                && actor(event.value, "CodeReviewVotedByIdentity")? == account
                && actor(event.value, "CodeReviewVotedByInitiatorIdentity")? == account
                && matches!(
                    text(property(
                        event.value,
                        "CodeReviewVoteResult",
                        "System.String"
                    )?)?,
                    "5" | "10"
                )
            {
                approvals.push(ApprovalEvent {
                    automatic: renewals.contains_key(&event.id),
                    id: event.id,
                    at: event.at,
                });
            }
        }
        Ok(approvals)
    }

    /// Notice readback shares PR identity checks without requiring approval standing.
    pub(crate) fn validate_scope(&self, repository: &str, number: u32) -> Result<(), AppError> {
        let pr = match self {
            Self::State { pull_request } | Self::History { pull_request, .. } => pull_request,
        };
        if identifier(field(field(pr, "repository")?, "id")?)? != repository
            || identifier(field(pr, "pullRequestId")?)? != number.to_string()
        {
            return Err(fail(
                "service",
                "ADO approval evidence has another PR identity",
            ));
        }
        Ok(())
    }

    /// Binds stored evidence to the enclosing run and recomputes its conclusion.
    pub(crate) fn decide(
        &self,
        account: &str,
        repository: &str,
        number: u32,
        observed_at: Timestamp,
    ) -> Result<Decision, AppError> {
        let pr = match self {
            Self::State { pull_request } | Self::History { pull_request, .. } => pull_request,
        };
        self.validate_scope(repository, number)?;
        if let Some(decision) = excluded(pr)? {
            return Ok(decision);
        }
        let Self::History {
            source_commit,
            reviewers,
            iterations,
            threads,
            policies,
            evaluations,
            ..
        } = self
        else {
            return Err(fail(
                "service",
                "An active PR requires complete approval evidence",
            ));
        };
        commit(source_commit)?;
        validate_iterations(iterations, source_commit, observed_at)?;
        let events = ordered_events(threads, observed_at)?;
        let policy = approval_policy(policies, evaluations)?;
        let mut current = None;
        let mut reviewer_ids = HashSet::new();
        for reviewer in reviewers {
            let id = identifier(field(reviewer, "id")?)?;
            if !reviewer_ids.insert(id.clone()) {
                return Err(fail("service", "Duplicate ADO reviewer"));
            }
            let vote = vote(field(reviewer, "vote")?)?;
            if id == account {
                if reviewer
                    .get("isContainer")
                    .is_some_and(|value| value != false)
                {
                    return Err(fail("service", "Authenticated ADO reviewer is a group"));
                }
                // An individual's votedFor entries are group contributions, not proxy votes.
                if let Some(groups) = reviewer.get("votedFor") {
                    array(groups)?;
                }
                current = Some(vote);
                for flag in ["isFlagged", "hasDeclined"] {
                    if let Some(value) = reviewer.get(flag)
                        && boolean(value)?
                    {
                        return Ok(Decision::Ineligible {
                            reason: "pendingHumanReview".to_owned(),
                        });
                    }
                }
            }
        }
        for thread in threads {
            if !is_deleted(thread)
                && thread.get("status").and_then(Value::as_str) == Some("pending")
                && array(field(thread, "comments")?)?.iter().any(|comment| {
                    !is_deleted(comment)
                        && comment.pointer("/author/id").and_then(Value::as_str) == Some(account)
                })
            {
                return Ok(Decision::Ineligible {
                    reason: "pendingHumanReview".to_owned(),
                });
            }
        }
        // Deleted audit attribution cannot turn an automatic vote into an external
        // anchor. Retained marker bytes only veto authority (docs/reapproval.md).
        let deleted_audit = threads.iter().any(|thread| {
            thread
                .get("comments")
                .and_then(Value::as_array)
                .is_some_and(|comments| {
                    comments.iter().any(|comment| {
                        (is_deleted(thread) || is_deleted(comment))
                            && comment.pointer("/author/id").and_then(Value::as_str)
                                == Some(account)
                            && comment
                                .get("content")
                                .and_then(Value::as_str)
                                .is_some_and(|body| body.contains("<!-- ss-autoreapprove"))
                    })
                })
        });
        let mut renewals = renewals(
            threads,
            &events,
            iterations,
            account,
            repository,
            number,
            observed_at,
        )?;
        if deleted_audit {
            // Lost attribution prevents interpreting a renewal chain. Still replay
            // surviving explicit decisions below, but never grant approval authority.
            renewals.clear();
        }
        let deleted_history = threads.iter().any(|thread| {
            is_deleted(thread)
                || thread
                    .get("comments")
                    .and_then(Value::as_array)
                    .is_some_and(|comments| comments.iter().any(is_deleted))
        });
        let decision = decide_history(
            &events,
            iterations,
            &policy,
            account,
            current,
            &renewals,
            deleted_history,
        )?;
        if deleted_audit && !matches!(decision, Decision::Ineligible { .. }) {
            return Ok(Decision::Ineligible {
                reason: "approvalEvidenceDeleted".to_owned(),
            });
        }
        Ok(decision)
    }
}

/// Two complete observations detect changes without claiming an API transaction.
pub(crate) fn capture(api: &mut Api<'_>, target: &Target) -> Result<Evidence, AppError> {
    let first = observe(api, target)?;
    let second = observe(api, target)?;
    if !equivalent(&first, &second)? {
        return Err(fail(
            "staleEvidence",
            "ADO approval evidence changed during capture",
        ));
    }
    Ok(first)
}

/// Collects one complete service observation without reading repository content.
fn observe(api: &mut Api<'_>, target: &Target) -> Result<Evidence, AppError> {
    let pr = project_pull_request(&api.transport.request(&target.pr_api(), "GET", None)?);
    if excluded(&pr)?.is_some() {
        return Ok(Evidence::State { pull_request: pr });
    }
    let source_commit = read_ref(api, target, text(field(&pr, "sourceRefName")?)?)?;
    let reviewers = unique_list(api, &format!("{}/reviewers", target.pr_api()), "id")?;
    let iterations = unique_list(api, &format!("{}/iterations", target.pr_api()), "id")?
        .iter()
        .map(|iteration| -> Result<Value, AppError> {
            Ok(json!({"id": field(iteration, "id")?, "createdDate": field(iteration, "createdDate")?,
                "sourceRefCommit": {"commitId": field(field(iteration, "sourceRefCommit")?, "commitId")?}}))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let threads = unique_list(api, &format!("{}/threads", target.pr_api()), "id")?;
    // Validate notification structure and service time before discarding routine activity.
    ordered_events(&threads, api.transport.clock.now())?;
    let evaluations = evaluations(api, target, &pr)?;
    let policies = policies(api, &pr)?;
    compact(&Evidence::History {
        pull_request: pr,
        source_commit,
        reviewers,
        iterations,
        threads,
        policies,
        evaluations,
    })
}

/// Shared freshness checks distinguish authority from provider bookkeeping.
pub(crate) fn equivalent(left: &Evidence, right: &Evidence) -> Result<bool, AppError> {
    Ok(comparison(left)? == comparison(right)?)
}

/// Keeps source, authority, and meaningful discussion, including from older snapshots.
fn compact(evidence: &Evidence) -> Result<Evidence, AppError> {
    let mut evidence = evidence.clone();
    let pr = match &mut evidence {
        Evidence::State { pull_request } | Evidence::History { pull_request, .. } => pull_request,
    };
    *pr = project_pull_request(pr);
    if let Evidence::History {
        reviewers,
        iterations,
        threads,
        policies,
        evaluations,
        ..
    } = &mut evidence
    {
        for iteration in iterations {
            *iteration = project(iteration, &["id", "createdDate", "sourceRefCommit"]);
            if let Some(commit) = iteration.get_mut("sourceRefCommit") {
                *commit = project(commit, &["commitId"]);
            }
        }
        for reviewer in reviewers {
            // Group contributions do not turn an individual's vote into a proxy vote.
            if let Some(groups) = reviewer.get("votedFor") {
                array(groups)?;
            }
            *reviewer = project(
                reviewer,
                &[
                    "id",
                    "vote",
                    "isContainer",
                    "isFlagged",
                    "hasDeclined",
                    "inactive",
                    "isDeletedInOrigin",
                ],
            );
        }
        *threads = project_threads(threads)?;
        // Validate every returned policy before excluding known merge-only checks.
        // Unknown vote semantics must remain failures, not disappear through projection.
        approval_policy(policies, evaluations)?;
        policies.retain(|policy| {
            policy.pointer("/type/id").and_then(Value::as_str)
                == Some("fa4e907d-c16b-4a4c-9dfa-4906e5d171dd")
                && policy.get("isEnabled") == Some(&Value::Bool(true))
        });
        for policy in policies.iter_mut() {
            *policy = project_policy(policy);
        }
        evaluations.retain(|evaluation| {
            evaluation
                .pointer("/configuration/type/id")
                .and_then(Value::as_str)
                == Some("fa4e907d-c16b-4a4c-9dfa-4906e5d171dd")
                && policies
                    .iter()
                    .any(|policy| policy.get("id") == evaluation.pointer("/configuration/id"))
        });
        for evaluation in evaluations {
            *evaluation = project(evaluation, &["evaluationId", "configuration", "status"]);
            if let Some(configuration) = evaluation.get_mut("configuration") {
                // The independent configuration read owns enablement and deletion.
                // Evaluation replicas contribute only policy identity, settings and applicability.
                *configuration =
                    project(&project_policy(configuration), &["id", "type", "settings"]);
            }
        }
    }
    Ok(evidence)
}

/// Stable IDs and PR content matter; repository size and merge jobs do not.
fn project_pull_request(pr: &Value) -> Value {
    let mut pr = project(
        pr,
        &[
            "pullRequestId",
            "status",
            "isDraft",
            "title",
            "description",
            "sourceRefName",
            "targetRefName",
            "forkSource",
            "repository",
            "createdBy",
        ],
    );
    if let Some(repository) = pr.get_mut("repository") {
        *repository = project_repository(repository);
    }
    if let Some(source) = pr.get_mut("forkSource") {
        *source = project(source, &["name", "repository"]);
        if let Some(repository) = source.get_mut("repository") {
            *repository = project_repository(repository);
        }
    }
    if let Some(author) = pr.get_mut("createdBy") {
        *author = project_identity(author);
    }
    pr
}

/// Fork and target references share stable scope rather than repository statistics.
fn project_repository(repository: &Value) -> Value {
    let mut repository = project(repository, &["id", "project", "isDisabled", "isFork"]);
    if let Some(project_ref) = repository.get_mut("project") {
        *project_ref = project(project_ref, &["id", "state", "visibility"]);
    }
    repository
}

/// Identity presentation is not attribution, group membership, or account standing.
fn project_identity(identity: &Value) -> Value {
    project(
        identity,
        &["id", "isContainer", "inactive", "isDeletedInOrigin"],
    )
}

/// Policy names, creator profiles and revision counters do not define vote semantics.
fn project_policy(policy: &Value) -> Value {
    let mut policy = project(
        policy,
        &["id", "isEnabled", "isDeleted", "type", "settings"],
    );
    if let Some(kind) = policy.get_mut("type") {
        *kind = project(kind, &["id"]);
    }
    policy
}

/// Shared discussion projection also protects notice readback from cosmetic changes.
pub(crate) fn project_threads(threads: &[Value]) -> Result<Vec<Value>, AppError> {
    let mut projected = Vec::new();
    for thread in threads {
        if routine_thread(thread)? {
            continue;
        }
        let mut thread = project(
            thread,
            &[
                "id",
                "status",
                "isDeleted",
                "publishedDate",
                "threadContext",
                "pullRequestThreadContext",
                "properties",
                "identities",
                "comments",
            ],
        );
        if let Some(Value::Object(identities)) = thread.get_mut("identities") {
            for identity in identities.values_mut() {
                *identity = project_identity(identity);
            }
        }
        if let Some(properties) = thread.get_mut("properties") {
            *properties = project(
                properties,
                &[
                    "CodeReviewThreadType",
                    "CodeReviewVoteResult",
                    "CodeReviewVotedByIdentity",
                    "CodeReviewVotedByInitiatorIdentity",
                    "CodeReviewReviewersUpdatedNumAdded",
                    "CodeReviewReviewersUpdatedNumRemoved",
                    "CodeReviewReviewersUpdatedNumChanged",
                    "CodeReviewReviewersUpdatedNumDeclined",
                    "CodeReviewReviewersUpdatedAddedIdentity",
                    "CodeReviewReviewersUpdatedRemovedIdentity",
                    "CodeReviewReviewersUpdatedChangedIdentity",
                    "CodeReviewReviewersUpdatedChangedDecline",
                    "CodeReviewReviewersUpdatedByIdentity",
                ],
            );
        }
        if let Some(Value::Array(comments)) = thread.get_mut("comments") {
            for comment in comments {
                let service_text = comment.get("commentType").and_then(Value::as_str)
                    == Some("system")
                    && !comment
                        .get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|body| body.contains("<!-- ss-autoreapprove"));
                let updated = comment
                    .get("lastContentUpdatedDate")
                    .or_else(|| comment.get("lastUpdatedDate"))
                    .cloned();
                *comment = project(
                    comment,
                    &[
                        "id",
                        "parentCommentId",
                        "author",
                        "content",
                        "publishedDate",
                        "isDeleted",
                        "commentType",
                    ],
                );
                if service_text {
                    // Structured properties, not generated prose or its edit clock,
                    // establish service vote/reviewer authority.
                    if let Some(object) = comment.as_object_mut() {
                        object.remove("content");
                    }
                } else if let Some(updated) = updated {
                    // The content-specific clock distinguishes edits from likes. Older
                    // service shapes expose only the general update clock.
                    comment["lastContentUpdatedDate"] = updated;
                }
                if let Some(author) = comment.get_mut("author") {
                    *author = project_identity(author);
                }
            }
        }
        projected.push(thread);
    }
    Ok(projected)
}

/// Routine service activity is redundant with current source, PR state and policy reads.
fn routine_thread(thread: &Value) -> Result<bool, AppError> {
    if is_deleted(thread) || thread.get("status").and_then(Value::as_str) == Some("pending") {
        return Ok(false);
    }
    let comments = array(field(thread, "comments")?)?;
    if comments.is_empty()
        || comments.iter().any(|comment| {
            is_deleted(comment)
                || comment.get("commentType").and_then(Value::as_str) != Some("system")
                || comment
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains("<!-- ss-autoreapprove"))
        })
    {
        return Ok(false);
    }
    // Capture validates raw event semantics first. Discussion-only reconciliation
    // needs no vote interpretation: unfamiliar or malformed history stays retained.
    let kind = match thread.get("properties") {
        None | Some(Value::Null) => {
            return Ok(thread.get("identities").is_none_or(|identities| {
                identities.is_null() || identities.as_object().is_some_and(Map::is_empty)
            }));
        }
        Some(properties) => properties.get("CodeReviewThreadType"),
    };
    Ok(kind.is_some_and(|kind| {
        kind.get("$type").and_then(Value::as_str) == Some("System.String")
            && matches!(
                kind.get("$value").and_then(Value::as_str),
                Some(
                    "RefUpdate"
                        | "PolicyStatusUpdate"
                        | "AutoCompleteUpdate"
                        | "IsDraftUpdate"
                        | "TitleUpdate"
                        | "DescriptionUpdate"
                        | "StatusUpdate"
                        | "MergeAttempt"
                )
            )
    }))
}

/// Compares effective applicability rather than transient evaluation attempts.
fn comparison(evidence: &Evidence) -> Result<Evidence, AppError> {
    let mut evidence = compact(evidence)?;
    if let Evidence::History { evaluations, .. } = &mut evidence {
        for evaluation in evaluations.iter_mut() {
            let configuration = field(evaluation, "configuration")?;
            // compact validates and retains only enabled reviewer-policy evaluations.
            let applicability = text(field(evaluation, "status")?)? != "notApplicable";
            *evaluation = json!({"configuration": configuration, "applicable": applicability});
        }
        evaluations.sort_by_key(|evaluation| {
            evaluation
                .pointer("/configuration/id")
                .map(Value::to_string)
        });
    }
    Ok(evidence)
}

/// Unsupported source-repository semantics cannot manufacture an approval baseline.
fn excluded(pr: &Value) -> Result<Option<Decision>, AppError> {
    let status = text(field(pr, "status")?)?;
    let draft = boolean(field(pr, "isDraft")?)?;
    if !matches!(status, "active" | "completed" | "abandoned") {
        return Err(fail("service", "Unknown ADO PR state"));
    }
    let reason = if status != "active" {
        Some("closed")
    } else if draft {
        Some("draft")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Ok(Some(Decision::Ineligible {
            reason: reason.to_owned(),
        }));
    }
    if pr.get("forkSource").is_some_and(|value| !value.is_null()) {
        return Err(fail(
            "service",
            "ADO fork source requires source-repository approval evidence",
        ));
    }
    Ok(None)
}

/// Ref reads use exact names, not a prefix match or a potentially stale merge attempt.
fn read_ref(api: &mut Api<'_>, target: &Target, name: &str) -> Result<String, AppError> {
    let filter = name
        .strip_prefix("refs/heads/")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| fail("service", "Expected an ADO branch ref"))?;
    let page = api.transport.ado(
        &format!("{}/refs", target.repository_api()),
        "GET",
        None,
        &[("filter", format!("heads/{filter}"))],
        "7.1",
    )?;
    if page.continuation.is_some() {
        return Err(fail("service", "Incomplete ADO ref evidence"));
    }
    let mut found = None;
    for value in ado_items(&page.payload)? {
        if text(field(&value, "name")?)? == name {
            if found.is_some() {
                return Err(fail("service", "Duplicate exact ADO ref"));
            }
            let id = text(field(&value, "objectId")?)?;
            commit(id)?;
            found = Some(id.to_owned());
        }
    }
    found.ok_or_else(|| fail("service", "Missing exact ADO branch ref"))
}

fn unique_list(api: &mut Api<'_>, path: &str, key: &str) -> Result<Vec<Value>, AppError> {
    let mut values = api.transport.list(path)?;
    unique(&values, key)?;
    // Provider ordering is not part of snapshot identity.
    values.sort_by_key(|value| value.get(key).map(Value::to_string));
    Ok(values)
}

/// Reads all evaluations without deriving offsets from filtered service records.
fn evaluations(api: &mut Api<'_>, target: &Target, pr: &Value) -> Result<Vec<Value>, AppError> {
    let project_id = identifier(field(field(field(pr, "repository")?, "project")?, "id")?)?;
    // The endpoint returns all evaluations when optional $top/$skip are omitted.
    // Offset paging can omit underlying records, so returned counts are not offsets.
    // Ref: docs/reapproval.md, "Azure DevOps evidence and policy interpretation".
    let page = api.transport.ado(
        &format!("{}/_apis/policy/evaluations", escape(&project_id)),
        "GET",
        None,
        &[
            (
                "artifactId",
                format!(
                    "vstfs:///CodeReview/CodeReviewId/{project_id}/{}",
                    target.pull_request_id
                ),
            ),
            ("includeNotApplicable", "true".to_owned()),
        ],
        "7.1-preview.1",
    )?;
    if page.continuation.is_some() {
        return Err(fail("service", "Unexpected ADO evaluation continuation"));
    }
    let mut values = ado_items(&page.payload)?;
    unique(&values, "evaluationId")?;
    // Provider ordering is not part of snapshot identity.
    values.sort_by_key(|value| value.get("evaluationId").map(Value::to_string));
    Ok(values)
}

/// Configuration enumeration follows opaque continuations, not evaluation offsets.
fn policies(api: &mut Api<'_>, pr: &Value) -> Result<Vec<Value>, AppError> {
    let repository = field(pr, "repository")?;
    let repository_id = identifier(field(repository, "id")?)?;
    let project_id = identifier(field(field(repository, "project")?, "id")?)?;
    let path = format!("{}/_apis/git/policy/configurations", escape(&project_id));
    let mut values = Vec::new();
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    // Match the existing capture safety bound for endlessly advancing bad pages.
    for _ in 0..1000 {
        // Match the bounded configuration page size used by the provider capture.
        let mut query = vec![
            ("$top", "100".to_owned()),
            ("repositoryId", repository_id.clone()),
            ("refName", text(field(pr, "targetRefName")?)?.to_owned()),
        ];
        if let Some(cursor) = &cursor {
            query.push(("continuationToken", cursor.clone()));
        }
        let page = api.transport.ado(&path, "GET", None, &query, "7.1")?;
        let items = ado_items(&page.payload)?;
        let empty = items.is_empty();
        values.extend(items);
        unique(&values, "id")?;
        if let Some(next) = page.continuation {
            if empty || !cursors.insert(next.clone()) {
                return Err(fail("service", "ADO policy pagination made no progress"));
            }
            cursor = Some(next);
        } else {
            values.sort_by_key(|value| value.get("id").map(Value::to_string));
            return Ok(values);
        }
    }
    Err(fail(
        "service",
        "ADO policies exceeded the page safety limit",
    ))
}

/// Evaluations identify applicable branch policies; merge checks do not cast votes.
fn approval_policy(policies: &[Value], evaluations: &[Value]) -> Result<ApprovalPolicy, AppError> {
    unique(policies, "id")?;
    unique(evaluations, "evaluationId")?;
    let mut result = ApprovalPolicy::default();
    let mut evaluated = HashSet::new();
    for evaluation in evaluations {
        let configuration = field(evaluation, "configuration")?;
        let kind = text(field(field(configuration, "type")?, "id")?)?;
        if kind == "fa4e907d-c16b-4a4c-9dfa-4906e5d171dd" {
            let id = identifier(field(configuration, "id")?)?;
            if !evaluated.insert(id.clone()) {
                return Err(fail(
                    "service",
                    format!("Duplicate ADO reviewer-policy evaluation {id}"),
                ));
            }
            let policy = policies
                .iter()
                .find(|policy| policy.get("id") == configuration.get("id"))
                .ok_or_else(|| fail("service", format!("Missing ADO policy configuration {id}")))?;
            if field(policy, "settings")? != field(configuration, "settings")? {
                return Err(fail(
                    "staleEvidence",
                    format!("ADO policy {id} evaluation settings differ from configuration"),
                ));
            }
            let status = text(field(evaluation, "status")?)?;
            if !matches!(
                status,
                "queued" | "running" | "approved" | "rejected" | "notApplicable" | "broken"
            ) {
                return Err(fail(
                    "service",
                    format!("Unrecognized ADO evaluation status {status} for reviewer policy {id}"),
                ));
            }
            if boolean(field(policy, "isEnabled")?)? && status != "notApplicable" {
                read_reviewer_policy(policy, &mut result)?;
            }
        } else if !is_non_approval_policy(kind) {
            return Err(fail(
                "service",
                format!("Unrecognized ADO evaluation policy type {kind}"),
            ));
        }
    }
    for policy in policies {
        if !boolean(field(policy, "isEnabled")?)? {
            continue;
        }
        let kind = text(field(field(policy, "type")?, "id")?)?;
        match kind {
            "fa4e907d-c16b-4a4c-9dfa-4906e5d171dd" => {
                let id = identifier(field(policy, "id")?)?;
                if !evaluated.contains(&id) {
                    return Err(fail(
                        "service",
                        format!("Missing ADO reviewer-policy evaluation for configuration {id}"),
                    ));
                }
            }
            "7ed39669-655c-494e-b4a0-a08b4da0fcce" => {
                let settings = field(policy, "settings")?;
                validate_settings(
                    settings,
                    &[
                        "scope",
                        "enforceConsistentCase",
                        "rejectDotGit",
                        "optimizedByDefault",
                        "breadcrumbDays",
                        "allowedForkTargets",
                        "gvfsOnly",
                        "gvfsExemptUsers",
                        "gvfsAllowedVersionRanges",
                        "detectRenameFalsePositivesByDefault",
                        "strictVoteMode",
                        "inheritPullRequestCreationMode",
                        "repoPullRequestAsDraftByDefault",
                        "repoPullRequestAutoCompleteByDefault",
                        "revertEnabled",
                        "copilotReviewerEnabled",
                        "copilotReviewEffortLevel",
                        "copilotReviewMcpEnabled",
                    ],
                    &identifier(field(policy, "id")?)?,
                )?;
                if settings
                    .get("strictVoteMode")
                    .is_some_and(|value| !value.is_null() && value != false)
                {
                    return Err(fail(
                        "service",
                        "ADO repository setting strictVoteMode requires vote semantics evidence",
                    ));
                }
            }
            _ if is_non_approval_policy(kind) => {}
            _ => {
                return Err(fail(
                    "service",
                    format!("Unrecognized enabled ADO policy type {kind}"),
                ));
            }
        }
    }
    Ok(result)
}

/// These provider policy identifiers govern merge checks, not individual vote freshness.
fn is_non_approval_policy(kind: &str) -> bool {
    matches!(
        kind,
        "e67ae10f-cf9a-40bc-8e66-6b3a8216956e" // Secrets scanning.
            | "7ed39669-655c-494e-b4a0-a08b4da0fcce" // Repository settings.
            | "fd2167ab-b0be-447a-8ec8-39368250530e" // Required reviewers.
            | "0609b952-1397-4640-95ec-e00a01b2c241" // Build validation.
            | "cbdc66da-9728-4af8-aada-9a5a32e4a226" // Status checks.
            | "c6a1889d-b943-4856-b76f-9e46bb6b0df2" // Comment resolution.
            | "fa4e907d-c16b-4a4c-9dfa-4916e5d171ab" // Merge strategy.
            | "67ed70bd-2a6b-4006-af44-be590463f46d" // Proof of presence.
    )
}

/// Folds documented reset and iteration rules across all applicable reviewer policies.
fn read_reviewer_policy(policy: &Value, result: &mut ApprovalPolicy) -> Result<(), AppError> {
    let id = identifier(field(policy, "id")?)?;
    let settings = field(policy, "settings")?;
    validate_settings(
        settings,
        &[
            "scope",
            "minimumApproverCount",
            "creatorVoteCounts",
            "allowDownvotes",
            "blockLastPusherVote",
            "resetOnSourcePush",
            "resetRejectionsOnSourcePush",
            "requireVoteOnLastIteration",
            "requireVoteOnEachIteration",
        ],
        &id,
    )?;
    if field(settings, "minimumApproverCount")?.as_u64().is_none() {
        return Err(fail(
            "service",
            format!("Invalid minimumApproverCount in ADO policy {id}"),
        ));
    }
    let flag = |name: &str| -> Result<bool, AppError> {
        // Optional flags are opt-in in the provider's policy settings contract.
        settings.get(name).map_or(Ok(false), |value| {
            value.as_bool().ok_or_else(|| {
                fail(
                    "service",
                    format!("Invalid boolean setting {name} in ADO policy {id}"),
                )
            })
        })
    };
    result.reset_on_push |= flag("resetOnSourcePush")?;
    result.reset_rejections |= flag("resetRejectionsOnSourcePush")?;
    let last_iteration = flag("requireVoteOnLastIteration")?;
    let each_iteration = flag("requireVoteOnEachIteration")?;
    result.fresh_iteration |= last_iteration || each_iteration;
    for name in ["creatorVoteCounts", "allowDownvotes", "blockLastPusherVote"] {
        flag(name)?;
    }
    Ok(())
}

/// Unknown settings need a diagnostic naming the configuration, not a terminal skip.
fn validate_settings(settings: &Value, names: &[&str], id: &str) -> Result<(), AppError> {
    let settings = settings.as_object().ok_or_else(|| {
        fail(
            "service",
            format!("Malformed ADO policy settings in configuration {id}"),
        )
    })?;
    for name in settings.keys() {
        if !names.contains(&name.as_str()) {
            return Err(fail(
                "service",
                format!("Unrecognized ADO policy setting {name} in configuration {id}"),
            ));
        }
    }
    Ok(())
}

/// History must contain the complete ordered iteration prefix, even for old approvals.
fn validate_iterations(iterations: &[Value], source: &str, now: Timestamp) -> Result<(), AppError> {
    unique(iterations, "id")?;
    if iterations.is_empty() {
        return Err(fail("service", "Missing ADO iteration history"));
    }
    let mut ordered = Vec::new();
    for iteration in iterations {
        let id = field(iteration, "id")?
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(|| fail("service", "Invalid ADO iteration identifier"))?;
        let at = timestamp(text(field(iteration, "createdDate")?)?)?;
        if at > now {
            return Err(fail("service", "Future ADO iteration evidence"));
        }
        commit(text(field(
            field(iteration, "sourceRefCommit")?,
            "commitId",
        )?)?)?;
        ordered.push((
            id,
            at,
            text(field(field(iteration, "sourceRefCommit")?, "commitId")?)?,
        ));
    }
    ordered.sort_unstable();
    if ordered
        .last()
        .is_none_or(|(_, _, latest)| *latest != source)
    {
        return Err(fail(
            "service",
            "Current source is missing from ADO iteration history",
        ));
    }
    let mut expected = 1_u64;
    let mut previous = None;
    for (id, at, _) in ordered {
        if id != expected || previous.is_some_and(|previous| previous >= at) {
            return Err(fail(
                "service",
                "Incomplete or ambiguous ADO iteration ordering",
            ));
        }
        expected = expected
            .checked_add(1)
            .ok_or_else(|| fail("service", "Iteration overflow"))?;
        previous = Some(at);
    }
    Ok(())
}

/// Deleted records cannot supply authority or require their removed payloads.
fn is_deleted(value: &Value) -> bool {
    value.get("isDeleted") == Some(&Value::Bool(true))
}

/// Only surviving system comments make structured thread properties authoritative.
fn has_system_comment(thread: &Value) -> Result<bool, AppError> {
    if is_deleted(thread) {
        return Ok(false);
    }
    Ok(array(field(thread, "comments")?)?.iter().any(|comment| {
        !is_deleted(comment) && comment.get("commentType") == Some(&json!("system"))
    }))
}

/// User prose and deleted comments cannot turn a thread into a vote event.
fn ordered_events(threads: &[Value], now: Timestamp) -> Result<Vec<Event<'_>>, AppError> {
    unique(threads, "id")?;
    let mut events = Vec::new();
    for thread in threads {
        if !has_system_comment(thread)? {
            continue;
        }
        let at = timestamp(text(field(thread, "publishedDate")?)?)?;
        if at > now {
            return Err(fail("service", "Future ADO system event"));
        }
        let Some(kind) = system_event_kind(thread)? else {
            continue;
        };
        events.push(Event {
            id: identifier(field(thread, "id")?)?,
            at,
            value: thread,
            kind,
        });
    }
    events.sort_by_key(|event| event.at);
    Ok(events)
}

/// Separates untyped notifications from incomplete structured authority evidence.
fn system_event_kind(thread: &Value) -> Result<Option<&str>, AppError> {
    let id = identifier(field(thread, "id")?)?;
    if thread.get("properties").is_none_or(Value::is_null) {
        // Untyped notifications carry no vote/reset authority. Retained identity
        // mappings instead indicate incomplete structured evidence (docs/reapproval.md).
        if thread.get("identities").is_some_and(|identities| {
            !identities.is_null() && !identities.as_object().is_some_and(Map::is_empty)
        }) {
            return Err(fail(
                "service",
                format!(
                    "ADO system thread {id} has identity evidence but lacks properties.CodeReviewThreadType; \
                     cannot establish vote/reviewer authority"
                ),
            ));
        }
        return Ok(None);
    }
    text(property(thread, "CodeReviewThreadType", "System.String")?)
        .map(Some)
        .map_err(|error| {
            crate::error::caused_by(
                "service",
                format!(
                    "ADO system thread {id} requires a string properties.CodeReviewThreadType to classify authority"
                ),
                error,
            )
            .into()
        })
}

/// Latest explicit decisions and participation changes supersede older authority.
fn decide_history(
    events: &[Event<'_>],
    iterations: &[Value],
    policy: &ApprovalPolicy,
    account: &str,
    current: Option<i64>,
    renewals: &HashMap<String, Renewal>,
    deleted_history: bool,
) -> Result<Decision, AppError> {
    let mut latest = None;
    let mut approval = None;
    let mut removed = false;
    let mut declined = None;
    let mut relevant_time = None;
    let mut anchor = None;
    for event in events {
        let relevant = match event.kind {
            "VoteUpdate" => {
                let voter = actor(event.value, "CodeReviewVotedByIdentity")?;
                let initiator = actor(event.value, "CodeReviewVotedByInitiatorIdentity")?;
                let value = text(property(
                    event.value,
                    "CodeReviewVoteResult",
                    "System.String",
                )?)?;
                let value = value.parse::<i64>().map_err(|error| {
                    crate::error::caused_by("service", "Invalid ADO vote event".to_owned(), error)
                })?;
                vote(&json!(value))?;
                if voter == account {
                    let mut applicable = initiator == account && matches!(value, 5 | 10);
                    if let Some(renewal) = renewals.get(&event.id) {
                        if deleted_history
                            && (approval.is_none()
                                || !events
                                    .iter()
                                    .any(|event| event.id == renewal.audit.anchor.review_id))
                        {
                            // A renewal cannot replace its deleted external anchor.
                            applicable = false;
                            anchor = None;
                        } else {
                            let previous: &Event<'_> = approval.ok_or_else(|| {
                                fail("service", "ADO renewal has no preceding approval authority")
                            })?;
                            if initiator != account
                                || !matches!(value, 5 | 10)
                                || anchor.as_ref() != Some(&renewal.audit.anchor)
                                || latest != Some(value)
                                || field(iteration_at(iterations, previous.at)?, "id")?
                                    == field(iteration_at(iterations, event.at)?, "id")?
                            {
                                return Err(fail(
                                    "service",
                                    "ADO renewal does not preserve invalidated external authority",
                                ));
                            }
                        }
                    } else {
                        anchor = (initiator == account && matches!(value, 5 | 10))
                            .then(|| -> Result<Anchor, AppError> {
                                Ok(Anchor {
                                    review_id: event.id.clone(),
                                    source_commit: text(field(
                                        field(
                                            iteration_at(iterations, event.at)?,
                                            "sourceRefCommit",
                                        )?,
                                        "commitId",
                                    )?)?
                                    .to_owned(),
                                })
                            })
                            .transpose()?;
                    }
                    latest = Some(value);
                    // A proxy positive vote is not the account's approval authority.
                    approval = applicable.then_some(event);
                    removed = false;
                    if initiator == account || declined == Some(false) {
                        // A direct decision resumes review after an earlier decline.
                        // Proxy votes cannot clear the account's participation veto.
                        declined = None;
                    }
                    true
                } else {
                    false
                }
            }
            "ReviewersUpdate" => {
                let mut relevant = false;
                for (count, identity, removing) in [
                    (
                        "CodeReviewReviewersUpdatedNumAdded",
                        "CodeReviewReviewersUpdatedAddedIdentity",
                        false,
                    ),
                    (
                        "CodeReviewReviewersUpdatedNumRemoved",
                        "CodeReviewReviewersUpdatedRemovedIdentity",
                        true,
                    ),
                ] {
                    if updated_reviewer(event, count, identity)? == Some(account) {
                        relevant = true;
                        approval = None;
                        anchor = None;
                        latest = None;
                        removed = removing;
                        declined = None;
                    }
                }
                if let Some(reviewer) = updated_reviewer(
                    event,
                    "CodeReviewReviewersUpdatedNumDeclined",
                    "CodeReviewReviewersUpdatedChangedIdentity",
                )? {
                    let value = match text(property(
                        event.value,
                        "CodeReviewReviewersUpdatedChangedDecline",
                        "System.String",
                    )?)? {
                        "True" => true,
                        "False" => false,
                        _ => {
                            return Err(fail(
                                "service",
                                format!(
                                    "ADO reviewer event {} has invalid decline state",
                                    event.id
                                ),
                            ));
                        }
                    };
                    if reviewer == account {
                        relevant = true;
                        // Neither declining nor undoing a decline approves a source.
                        // A later direct vote must establish its own external anchor.
                        approval = None;
                        anchor = None;
                        declined = Some(value);
                    }
                }
                let key = "CodeReviewReviewersUpdatedNumChanged";
                if property(event.value, key, "System.Int32")? != &json!(0) {
                    return Err(fail(
                        "service",
                        format!("ADO reviewer event {} has unsupported {key}", event.id),
                    ));
                }
                relevant
            }
            // These structured activities do not change reviewer authority.
            "RefUpdate" | "PolicyStatusUpdate" | "AutoCompleteUpdate" | "IsDraftUpdate"
            | "TitleUpdate" | "DescriptionUpdate" | "StatusUpdate" | "MergeAttempt" => false,
            _ => {
                return Err(fail(
                    "service",
                    format!(
                        "Unrecognized ADO system history type {} in thread {}",
                        event.kind, event.id
                    ),
                ));
            }
        };
        if relevant {
            if relevant_time == Some(event.at) {
                return Err(fail(
                    "service",
                    format!("Ambiguous ADO decision ordering at thread {}", event.id),
                ));
            }
            relevant_time = Some(event.at);
        }
    }
    if removed || current.is_none() && approval.is_some() {
        return Ok(Decision::Ineligible {
            reason: "reviewerRemoved".to_owned(),
        });
    }
    if declined == Some(true) {
        return Ok(Decision::Ineligible {
            reason: "pendingHumanReview".to_owned(),
        });
    }
    if current.is_some_and(|value| value < 0) || latest.is_some_and(|value| value < 0) {
        return Ok(Decision::Ineligible {
            reason: "negativeDecision".to_owned(),
        });
    }
    if current.is_none() && latest.is_none() || current == Some(0) && latest.is_none() {
        return Ok(Decision::NoPriorApproval);
    }
    if latest == Some(0) {
        return Ok(Decision::Ineligible {
            reason: "approvalWithdrawn".to_owned(),
        });
    }
    if declined == Some(false) && current == latest {
        // The service may retain a positive vote while participation changes.
        // Clearing the decline does not restore its superseded approval event.
        return Ok(Decision::NoPriorApproval);
    }
    if latest.is_none() && deleted_history {
        // A current positive vote alone supplies neither an external approval
        // event nor an approved source. Missing deleted authority is not work.
        return Ok(Decision::NoPriorApproval);
    }
    if let Some(approval) = approval {
        let approved_iteration = iteration_at(iterations, approval.at)?;
        let latest_iteration = iterations
            .iter()
            .max_by_key(|iteration| iteration.get("id").and_then(Value::as_u64))
            .ok_or_else(|| fail("service", "Missing ADO source iteration"))?;
        let advanced = field(approved_iteration, "id")? != field(latest_iteration, "id")?;
        if current != latest {
            if deleted_history {
                return Ok(Decision::Ineligible {
                    reason: "approvalEvidenceDeleted".to_owned(),
                });
            }
            // Current settings cannot prove which policy applied to a past push.
            // Missing reset evidence must not turn a possible withdrawal into authority.
            return Err(fail(
                "service",
                format!(
                    "ADO current vote {current:?} does not match history {latest:?}; \
                 missing authoritative vote-reset evidence after approval thread {}; \
                 resetOnSourcePush {}; resetRejectionsOnSourcePush {}",
                    approval.id, policy.reset_on_push, policy.reset_rejections
                ),
            ));
        }
        if advanced && policy.fresh_iteration {
            return Ok(Decision::AssessmentRequired {
                anchor: anchor
                    .ok_or_else(|| fail("service", "ADO approval lost its external anchor"))?,
                approval_event_id: approval.id.clone(),
            });
        }
        return Ok(Decision::AlreadyValid {
            approval_event_id: approval.id.clone(),
        });
    }
    if deleted_history {
        return Ok(Decision::Ineligible {
            reason: "approvalEvidenceDeleted".to_owned(),
        });
    }
    if current == latest {
        return Ok(Decision::Ineligible {
            reason: "approvalByOtherActor".to_owned(),
        });
    }
    Err(fail(
        "service",
        format!("ADO current vote {current:?} does not match history {latest:?}"),
    ))
}

/// Resolves single-reviewer transitions without guessing bulk account attribution.
fn updated_reviewer<'a>(
    event: &Event<'a>,
    count: &str,
    identity: &str,
) -> Result<Option<&'a str>, AppError> {
    let count = property(event.value, count, "System.Int32")?
        .as_u64()
        .ok_or_else(|| fail("service", "Invalid reviewer-update count"))?;
    match count {
        0 => Ok(None),
        1 => actor(event.value, identity).map(Some),
        _ => Err(fail(
            "service",
            format!(
                "ADO reviewer event {} needs complete bulk identity evidence for {identity} (count {count})",
                event.id
            ),
        )),
    }
}

/// Producer and publication validation use the same latest source-iteration binding.
pub(crate) fn source_iteration(evidence: &ProviderEvidence) -> Result<(u64, &str), AppError> {
    let ProviderEvidence::AzureDevOps(Evidence::History {
        iterations,
        source_commit,
        ..
    }) = evidence
    else {
        return Err(fail(
            "invalidState",
            "ADO publication requires complete source evidence",
        ));
    };
    let iteration = iterations
        .iter()
        .max_by_key(|item| item.get("id").and_then(Value::as_u64))
        .ok_or_else(|| fail("service", "Missing ADO source iteration"))?;
    Ok((
        field(iteration, "id")?
            .as_u64()
            .ok_or_else(|| fail("service", "Invalid ADO iteration"))?,
        source_commit,
    ))
}

/// The transaction preserves the account's existing positive vote strength.
pub(crate) fn current_vote(evidence: &ProviderEvidence, account: &str) -> Result<i64, AppError> {
    let ProviderEvidence::AzureDevOps(Evidence::History { reviewers, .. }) = evidence else {
        return Err(fail("invalidState", "Missing ADO reviewer evidence"));
    };
    let reviewer = reviewers
        .iter()
        .find(|reviewer| reviewer.get("id").and_then(Value::as_str) == Some(account))
        .ok_or_else(|| fail("service", "ADO reviewer is absent"))?;
    vote(field(reviewer, "vote")?)
}

/// Mutable reviewer readback alone cannot identify an accepted reapproval.
pub(crate) fn new_vote(
    before: &ProviderEvidence,
    after: &ProviderEvidence,
    account: &str,
    expected_vote: i64,
) -> Result<String, AppError> {
    let ProviderEvidence::AzureDevOps(Evidence::History { threads: old, .. }) = before else {
        return Err(fail("invalidState", "Missing ADO preflight history"));
    };
    let ProviderEvidence::AzureDevOps(Evidence::History { threads, .. }) = after else {
        return Err(fail("staleEvidence", "Missing ADO readback history"));
    };
    let old_ids = old
        .iter()
        .map(|thread| identifier(field(thread, "id")?))
        .collect::<Result<HashSet<_>, _>>()?;
    let mut matches = Vec::new();
    for thread in threads {
        let id = identifier(field(thread, "id")?)?;
        if old_ids.contains(&id)
            || !has_system_comment(thread)?
            || system_event_kind(thread)? != Some("VoteUpdate")
        {
            continue;
        }
        if actor(thread, "CodeReviewVotedByIdentity")? == account
            && actor(thread, "CodeReviewVotedByInitiatorIdentity")? == account
            && text(property(thread, "CodeReviewVoteResult", "System.String")?)?
                == expected_vote.to_string()
        {
            matches.push(id);
        }
    }
    let [id] = matches.as_slice() else {
        return Err(fail(
            "ambiguousWrite",
            "Expected exactly one new account-owned positive ADO vote event",
        ));
    };
    Ok(id.clone())
}

/// Exact service-authored immutable audit bytes bind a later read to its own effect.
pub(crate) fn matching_audit(
    thread: &Value,
    body: &str,
    account: &str,
) -> Result<String, AppError> {
    let mut comments = array(field(thread, "comments")?)?
        .iter()
        .filter(|comment| !is_deleted(comment));
    let comment = comments
        .next()
        .ok_or_else(|| fail("service", "ADO renewal audit has missing discussion"))?;
    if comments.next().is_some() {
        return Err(fail(
            "service",
            "ADO renewal audit has additional discussion",
        ));
    }
    if thread.get("isDeleted") == Some(&json!(true))
        || comment.get("isDeleted") == Some(&json!(true))
        || text(field(comment, "commentType")?)? != "text"
        || text(field(field(comment, "author")?, "id")?)? != account
        || text(field(comment, "content")?)? != body
    {
        return Err(fail(
            "service",
            "ADO renewal audit author, content or deletion evidence changed",
        ));
    }
    let published = timestamp(text(field(comment, "publishedDate")?)?)?;
    if let Some(updated) = comment
        .get("lastContentUpdatedDate")
        .or_else(|| comment.get("lastUpdatedDate"))
        && timestamp(text(updated)?)? != published
    {
        return Err(fail(
            "service",
            "Edited ADO renewal audit cannot establish provenance",
        ));
    }
    identifier(field(thread, "id")?)
}

/// Audit prose supplies attribution only after joining service author, event and iteration.
fn renewals(
    threads: &[Value],
    events: &[Event<'_>],
    iterations: &[Value],
    account: &str,
    repository: &str,
    number: u32,
    now: Timestamp,
) -> Result<HashMap<String, Renewal>, AppError> {
    let mut result = HashMap::new();
    let mut operations = HashSet::new();
    for thread in threads {
        if is_deleted(thread) {
            continue;
        }
        for comment in array(field(thread, "comments")?)? {
            if is_deleted(comment)
                || comment.pointer("/author/id").and_then(Value::as_str) != Some(account)
            {
                continue;
            }
            let Some(body) = comment.get("content").and_then(Value::as_str) else {
                continue;
            };
            let Some((_, remainder)) = body.split_once("<!-- ss-autoreapprove") else {
                continue;
            };
            // Needs-review notices carry no vote authority and have their own strict parser.
            if remainder.starts_with("-needs-review ") {
                continue;
            }
            let remainder = remainder
                .strip_prefix("-ado ")
                .ok_or_else(|| fail("service", "Unrecognized ADO renewal audit marker"))?;
            let (document, suffix) = remainder
                .split_once("-->")
                .ok_or_else(|| fail("service", "Unterminated ADO renewal marker"))?;
            let renewal: Renewal = serde_json::from_str(document.trim())?;
            renewal.audit.validate()?;
            matching_audit(thread, body, account)?;
            let audit_at = timestamp(text(field(comment, "publishedDate")?)?)?;
            if suffix.contains("<!-- ss-autoreapprove")
                || renewal.audit.account_id != account
                || renewal.audit.repository_id != repository
                || renewal.audit.pull_request_id != number
                || audit_at > now
                || !operations.insert(renewal.audit.operation_id.clone())
            {
                return Err(fail(
                    "service",
                    "ADO renewal audit has conflicting scope, source, event or ordering",
                ));
            }
            let event = events
                .iter()
                .find(|event| event.id == renewal.vote_event_id);
            if event.is_none() {
                let mut deleted_vote = false;
                for thread in threads {
                    if identifier(field(thread, "id")?)? == renewal.vote_event_id {
                        deleted_vote = is_deleted(thread)
                            || (array(field(thread, "comments")?)?.iter().any(is_deleted)
                                && !has_system_comment(thread)?);
                    }
                }
                if deleted_vote {
                    // A surviving audit is attribution, not authority for its deleted vote.
                    continue;
                }
            }
            let event = event.ok_or_else(|| {
                fail(
                    "service",
                    "ADO renewal audit refers to a missing vote event",
                )
            })?;
            let iteration = iteration_at(iterations, event.at)?;
            if event.kind != "VoteUpdate"
                || actor(event.value, "CodeReviewVotedByIdentity")? != account
                || actor(event.value, "CodeReviewVotedByInitiatorIdentity")? != account
                || field(iteration, "id")?.as_u64() != Some(renewal.iteration_id)
                || text(field(field(iteration, "sourceRefCommit")?, "commitId")?)?
                    != renewal.audit.source_commit
                || audit_at < event.at
                || result
                    .insert(renewal.vote_event_id.clone(), renewal)
                    .is_some()
            {
                return Err(fail(
                    "service",
                    "ADO renewal audit has conflicting scope, source, event or ordering",
                ));
            }
        }
    }
    Ok(result)
}

/// Approval time selects the immutable source iteration, never a commit timestamp.
fn iteration_at(iterations: &[Value], at: Timestamp) -> Result<&Value, AppError> {
    let mut selected = None;
    let mut selected_at = None;
    for iteration in iterations {
        let created = timestamp(text(field(iteration, "createdDate")?)?)?;
        if created == at {
            return Err(fail(
                "service",
                "ADO approval coincides with a source iteration; source ordering is ambiguous",
            ));
        }
        if created < at && selected_at.is_none_or(|previous| previous < created) {
            selected = Some(iteration);
            selected_at = Some(created);
        }
    }
    selected.ok_or_else(|| {
        fail(
            "service",
            "ADO approval predates complete source iteration history",
        )
    })
}

/// Keeps required typed evidence strict and identifies the incomplete event.
fn property<'a>(event: &'a Value, name: &str, kind: &str) -> Result<&'a Value, AppError> {
    let read = || {
        let property = field(field(event, "properties")?, name)?;
        if text(field(property, "$type")?)? != kind {
            return Err(fail("service", "Unrecognized ADO property wrapper"));
        }
        field(property, "$value")
    };
    read().map_err(|error| {
        crate::error::caused_by(
            "service",
            format!(
                "ADO system thread {} requires properties.{name} with {kind} authority evidence",
                event.get("id").unwrap_or(&Value::Null)
            ),
            error,
        )
        .into()
    })
}

fn actor<'a>(event: &'a Value, name: &str) -> Result<&'a str, AppError> {
    let key = text(property(event, name, "System.String")?)?;
    text(field(field(field(event, "identities")?, key)?, "id")?)
}

fn vote(value: &Value) -> Result<i64, AppError> {
    value
        .as_i64()
        .filter(|value| matches!(value, -10 | -5 | 0 | 5 | 10))
        .ok_or_else(|| fail("service", "Invalid ADO vote value"))
}

fn unique(values: &[Value], key: &str) -> Result<(), AppError> {
    let mut ids = HashSet::new();
    for value in values {
        if !ids.insert(identifier(field(value, key)?)?) {
            return Err(fail("service", "Repeated ADO evidence identifier"));
        }
    }
    Ok(())
}

fn commit(value: &str) -> Result<(), AppError> {
    // ADO Services commit identifiers use SHA-1, as in its documented Git contract.
    if value.len() != 40
        || !value
            .bytes()
            .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
    {
        return Err(fail("service", "Invalid ADO commit identifier"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        reason = "Tests mutate bounded paths in the verified service fixture"
    )]

    use ohno::AppError;
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{Decision, Evidence};
    use crate::approval::Evidence as ProviderEvidence;
    use crate::clock::timestamp;
    use crate::renewal::Anchor;

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../tests/fixtures/ado-no-approval-policy.json"
        ))
        .unwrap()
    }

    fn evidence(stage: &Value) -> Value {
        json!({
            "scope": "history", "pullRequest": stage["pullRequest"],
            "sourceCommit": stage["sourceCommit"],
            "reviewers": stage["reviewers"], "iterations": stage["iterations"],
            "threads": stage["threads"], "policies": stage["policies"],
            "evaluations": stage["evaluations"]
        })
    }

    fn decide(value: Value) -> Result<Decision, AppError> {
        let fixture = fixture();
        serde_json::from_value::<Evidence>(value)?.decide(
            fixture["accountId"].as_str().unwrap(),
            fixture["stages"][0]["pullRequest"]["repository"]["id"]
                .as_str()
                .unwrap(),
            1,
            timestamp("2026-01-02T00:00:00Z").unwrap(),
        )
    }

    /// Synthetic policies exercise documented semantics without copying private captures.
    fn reviewer_policy(value: &mut Value, settings: Value) {
        let id = value["policies"]
            .as_array()
            .unwrap()
            .len()
            .checked_add(1)
            .unwrap();
        let Value::Object(settings) = settings else {
            panic!("Test policy settings must be an object")
        };
        let mut policy = json!({
            "id": id, "isEnabled": true, "isBlocking": true,
            "type": {"id": "fa4e907d-c16b-4a4c-9dfa-4906e5d171dd"},
            "settings": {"minimumApproverCount": 1}
        });
        policy["settings"].as_object_mut().unwrap().extend(settings);
        value["policies"]
            .as_array_mut()
            .unwrap()
            .push(policy.clone());
        value["evaluations"].as_array_mut().unwrap().push(json!({
            "evaluationId": format!("evaluation-{id}"),
            "configuration": policy, "status": "queued", "context": {}
        }));
    }

    /// A synthetic renewal joins sanitized service vote fields to a documented audit thread.
    fn renewed() -> Value {
        let mut value = evidence(&fixture()["stages"][2]);
        reviewer_policy(&mut value, json!({"requireVoteOnLastIteration": true}));
        let Decision::AssessmentRequired { anchor, .. } = decide(value.clone()).unwrap() else {
            panic!("Fixture must require an assessment");
        };
        let mut event = value["threads"][1].clone();
        event["id"] = json!(20);
        event["publishedDate"] = json!("2026-01-01T01:00:00Z");
        value["threads"].as_array_mut().unwrap().push(event);
        let last = value["iterations"].as_array().unwrap().last().unwrap();
        let marker = json!({"audit": {"schemaVersion": 1, "operationId": Uuid::new_v4().to_string(),
            "repositoryId": value["pullRequest"]["repository"]["id"], "accountId": fixture()["accountId"],
            "pullRequestId": 1, "anchor": anchor, "sourceCommit": value["sourceCommit"],
            // Arbitrary valid digests exercise provenance, not semantic assessment.
            "policyDigest": "a".repeat(64), "assessmentDigest": "b".repeat(64), "state": "submitted"},
            "voteEventId": "20", "iterationId": last["id"]});
        append_audit(&mut value, &marker);
        value
    }

    fn append_audit(value: &mut Value, marker: &Value) {
        value["threads"].as_array_mut().unwrap().push(json!({
            "id": 30, "status": "closed", "isDeleted": false,
            "comments": [{"id": 1, "parentCommentId": 0, "commentType": "text",
                "author": {"id": fixture()["accountId"]},
                "content": format!("[Copilot speaking]\n<!-- ss-autoreapprove-ado {marker} -->"),
                "publishedDate": "2026-01-01T01:05:00Z", "lastUpdatedDate": "2026-01-01T01:05:00Z",
                "isDeleted": false}]
        }));
    }

    #[test]
    fn renewed_votes_retain_the_original_anchor_across_pushes_and_policy_changes() {
        let mut value = renewed();
        assert_eq!(
            decide(value.clone()).unwrap(),
            Decision::AlreadyValid {
                approval_event_id: "20".to_owned()
            }
        );
        let original = evidence(&fixture()["stages"][2]);
        value["iterations"].as_array_mut().unwrap().push(json!({
            "id": 3, "createdDate": "2026-01-01T02:00:00Z",
            "sourceRefCommit": {"commitId": "d".repeat(40)}
        }));
        value["sourceCommit"] = json!("d".repeat(40));
        let Decision::AssessmentRequired {
            anchor,
            approval_event_id,
        } = decide(value.clone()).unwrap()
        else {
            panic!("A later iteration needs cumulative assessment");
        };
        assert_eq!(anchor.review_id, "2");
        assert_eq!(
            anchor.source_commit,
            original["iterations"][0]["sourceRefCommit"]["commitId"]
                .as_str()
                .unwrap()
        );
        assert_eq!(approval_event_id, "20");
        // Current policy changes do not rewrite the provenance of an older renewal.
        value["evaluations"] = json!([]);
        value["policies"] = json!([]);
        assert_eq!(decide(value).unwrap().status(), "alreadyValid");
    }

    #[test]
    fn malformed_or_conflicting_audits_never_supply_a_new_external_anchor() {
        for (path, replacement) in [
            ("/audit/anchor/reviewId", json!("another-approval")),
            ("/audit/anchor/sourceCommit", json!("d".repeat(40))),
            ("/audit/sourceCommit", json!("d".repeat(40))),
            ("/audit/accountId", json!("another-account")),
            ("/audit/repositoryId", json!("another-repository")),
            ("/audit/pullRequestId", json!(2)),
            ("/audit/policyDigest", json!("invalid")),
            ("/voteEventId", json!("2")),
            ("/iterationId", json!(1)),
        ] {
            let mut value = renewed();
            let thread = value["threads"].as_array_mut().unwrap().pop().unwrap();
            let body = thread["comments"][0]["content"].as_str().unwrap();
            let document = body
                .split_once("-ado ")
                .unwrap()
                .1
                .split_once("-->")
                .unwrap()
                .0;
            let mut marker: Value = serde_json::from_str(document).unwrap();
            *marker.pointer_mut(path).unwrap() = replacement;
            append_audit(&mut value, &marker);
            decide(value).unwrap_err();
        }
        for (path, replacement) in [
            ("/comments/0/lastUpdatedDate", json!("2026-01-01T02:00:00Z")),
            (
                "/comments/0/content",
                json!("<!-- ss-autoreapprove-ado malformed -->"),
            ),
        ] {
            let mut value = renewed();
            *value["threads"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()
                .pointer_mut(path)
                .unwrap_or_else(|| panic!("Missing test field {path}")) = replacement;
            decide(value).unwrap_err();
        }
        let mut duplicated = renewed();
        let mut thread = duplicated["threads"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        thread["id"] = json!(31);
        duplicated["threads"].as_array_mut().unwrap().push(thread);
        decide(duplicated).unwrap_err();
    }

    #[test]
    fn project_bookkeeping_is_ignored_without_mutating_captured_evidence() {
        let mut original = evidence(&fixture()["stages"][2]);
        // Arbitrary project-wide versions differ without changing this PR's authority.
        original["pullRequest"]["repository"]["project"]["revision"] = json!(7);
        original["pullRequest"]["repository"]["project"]["lastUpdateTime"] =
            json!("2026-01-01T00:00:00Z");
        for scope in ["history", "state"] {
            let mut original = original.clone();
            if scope == "state" {
                original = json!({"scope": scope, "pullRequest": original["pullRequest"]});
            }
            let captured: Evidence = serde_json::from_value(original.clone()).unwrap();
            for key in ["revision", "lastUpdateTime"] {
                for replacement in [Some(json!("changed")), None] {
                    let mut changed = original.clone();
                    let project = changed["pullRequest"]["repository"]["project"]
                        .as_object_mut()
                        .unwrap();
                    if let Some(replacement) = replacement {
                        project.insert(key.to_owned(), replacement);
                    } else {
                        project.remove(key);
                    }
                    let changed: Evidence = serde_json::from_value(changed).unwrap();
                    assert!(
                        super::equivalent(&captured, &changed).unwrap(),
                        "{scope}: {key}"
                    );
                    assert!(
                        super::equivalent(&changed, &captured).unwrap(),
                        "{scope}: {key}"
                    );
                    assert_eq!(serde_json::to_value(&captured).unwrap(), original);
                }
            }
        }
    }

    #[test]
    fn focused_evidence_omits_cosmetics_and_merge_checks_without_rewriting_history() {
        let original: Evidence = serde_json::from_value(renewed()).unwrap();
        let mut rich = serde_json::to_value(&original).unwrap();
        rich["pullRequest"]["mergeId"] = json!("cosmetic-job");
        rich["pullRequest"]["completionQueueTime"] = json!("cosmetic-queue");
        rich["pullRequest"]["repository"]["size"] = json!(999);
        rich["pullRequest"]["repository"]["isInMaintenance"] = json!(true);
        rich["pullRequest"]["repository"]["project"]["name"] = json!("cosmetic-project");
        rich["reviewers"][0]["displayName"] = json!("cosmetic-reviewer");
        rich["reviewers"][0]["votedFor"] = json!([{"id": "group", "vote": 10}]);
        rich["iterations"][0]["author"] = json!({"displayName": "cosmetic-author"});
        rich["iterations"][0]["sourceRefCommit"]["url"] = json!("cosmetic-link");
        rich["threads"][1]["comments"][0]["usersLiked"] = json!([{"id": "other"}]);
        rich["threads"][1]["comments"][0]["content"] = json!("cosmetic-system-text");
        rich["threads"][1]["comments"][0]["lastContentUpdatedDate"] = json!("cosmetic-system-time");
        rich["threads"][1]["comments"][0]["author"] = json!({"displayName": "cosmetic-author"});
        for identity in rich["threads"][1]["identities"]
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            identity["displayName"] = json!("cosmetic-actor");
            identity["url"] = json!("cosmetic-link");
        }
        // Omitted author IDs in system comments do not establish vote authority;
        // the structured actor mapping does.
        let mut baseline = serde_json::to_value(&original).unwrap();
        baseline["threads"][1]["comments"][0]["author"] = json!({});
        let baseline: Evidence = serde_json::from_value(baseline).unwrap();
        for policy in rich["policies"].as_array_mut().unwrap() {
            policy["revision"] = json!(99);
            policy["type"]["displayName"] = json!("cosmetic-policy");
        }
        let evaluation = rich["evaluations"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap();
        evaluation["evaluationId"] = json!("another-attempt");
        evaluation["configuration"]["revision"] = json!(99);
        evaluation["configuration"]["isEnabled"] = json!(false);
        evaluation["configuration"]["isDeleted"] = json!(true);
        evaluation["configuration"]["type"]["displayName"] = json!("cosmetic-policy");
        rich["policies"].as_array_mut().unwrap().push(json!({
            "id": 90, "isEnabled": true, "type": {"id": "0609b952-1397-4640-95ec-e00a01b2c241"},
            "settings": {"buildDefinitionId": 10}
        }));
        rich["threads"].as_array_mut().unwrap().push(json!({
            "id": 91, "publishedDate": "2026-01-01T01:10:00Z", "status": "closed",
            "properties": {"CodeReviewThreadType": {"$type": "System.String", "$value": "MergeAttempt"}},
            "comments": [{"commentType": "system", "content": "cosmetic-job"}]
        }));
        let saved = rich.clone();
        let rich: Evidence = serde_json::from_value(rich).unwrap();
        let lean = super::compact(&rich).unwrap();
        assert!(super::equivalent(&baseline, &rich).unwrap());
        assert!(super::equivalent(&rich, &lean).unwrap());
        assert!(super::equivalent(&lean, &rich).unwrap());
        assert_eq!(super::compact(&lean).unwrap(), lean);
        assert_eq!(serde_json::to_value(&rich).unwrap(), saved);
        assert!(!serde_json::to_string(&lean).unwrap().contains("cosmetic-"));
        assert_eq!(
            decide(serde_json::to_value(lean).unwrap()).unwrap(),
            decide(saved).unwrap()
        );
    }

    #[test]
    fn routine_notifications_keep_human_replies_pending_state_and_deleted_history() {
        let routine = json!({
            "id": 91, "publishedDate": "2026-01-01T01:10:00Z", "status": "closed",
            "properties": {"CodeReviewThreadType": {"$type": "System.String", "$value": "PolicyStatusUpdate"}},
            "comments": [{"commentType": "system", "content": "Policy progress"}]
        });
        assert!(
            super::project_threads(std::slice::from_ref(&routine))
                .unwrap()
                .is_empty()
        );
        for extra in ["humanReply", "pending", "deleted"] {
            let mut material = routine.clone();
            match extra {
                "humanReply" => material["comments"].as_array_mut().unwrap().push(json!({
                    "id": 2, "commentType": "text", "content": "New objection", "author": {"id": "human"}
                })),
                "pending" => material["status"] = json!("pending"),
                "deleted" => material["isDeleted"] = json!(true),
                _ => unreachable!(),
            }
            assert_eq!(
                super::project_threads(&[material]).unwrap().len(),
                1,
                "{extra}"
            );
        }
        let mut unsupported = routine;
        unsupported["properties"]["CodeReviewThreadType"]["$value"] = json!("UnknownVoteReset");
        let projected = super::project_threads(&[unsupported]).unwrap();
        assert_eq!(
            projected[0]["properties"]["CodeReviewThreadType"]["$value"],
            "UnknownVoteReset"
        );
    }

    #[test]
    fn audit_content_clock_ignores_reaction_updates_but_not_content_edits() {
        let mut value = renewed();
        let expected = decide(value.clone()).unwrap();
        let audit = value["threads"].as_array_mut().unwrap().last_mut().unwrap();
        audit["comments"][0]["lastContentUpdatedDate"] = json!("2026-01-01T01:05:00Z");
        audit["comments"][0]["lastUpdatedDate"] = json!("2026-01-01T02:00:00Z");
        assert_eq!(decide(value.clone()).unwrap(), expected);
        let audit = value["threads"].as_array_mut().unwrap().last_mut().unwrap();
        audit["comments"][0]["lastContentUpdatedDate"] = json!("2026-01-01T02:00:00Z");
        decide(value).unwrap_err();
    }

    #[test]
    fn project_bookkeeping_exclusion_preserves_substantive_evidence_guards() {
        let mut original = renewed();
        // Synthetic PR content and scope exercise guards alongside unrelated metadata churn.
        original["pullRequest"]["repository"]["project"]["revision"] = json!(7);
        original["pullRequest"]["repository"]["project"]["lastUpdateTime"] =
            json!("2026-01-01T00:00:00Z");
        original["pullRequest"]["repository"]["project"]["name"] = json!("fixture-project");
        original["pullRequest"]["repository"]["project"]["state"] = json!("wellFormed");
        original["pullRequest"]["repository"]["revision"] = json!(7);
        original["pullRequest"]["revision"] = json!(7);
        original["pullRequest"]["title"] = json!("Fixture PR");
        original["pullRequest"]["description"] = json!("Fixture discussion");
        original["evaluations"][0]["configuration"]["revision"] = json!(1);
        original["threads"][1]["comments"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id": 2, "commentType": "text", "content": "Fixture discussion"
            }));
        for (path, replacement) in [
            (
                "/pullRequest/repository/project/id",
                json!("another-project"),
            ),
            ("/pullRequest/repository/project/state", json!("deleting")),
            ("/pullRequest/repository/id", json!("another-repository")),
            ("/pullRequest/pullRequestId", json!(2)),
            (
                "/pullRequest/sourceRefName",
                json!("refs/heads/another-source"),
            ),
            (
                "/pullRequest/targetRefName",
                json!("refs/heads/another-target"),
            ),
            ("/pullRequest/status", json!("completed")),
            ("/pullRequest/isDraft", json!(true)),
            ("/pullRequest/title", json!("Changed title")),
            ("/pullRequest/description", json!("New human objection")),
            ("/sourceCommit", json!("e".repeat(40))),
            (
                "/iterations/0/sourceRefCommit/commitId",
                json!("e".repeat(40)),
            ),
            ("/reviewers/0/vote", json!(0)),
            ("/reviewers/0/id", json!("another-reviewer")),
            ("/reviewers/0/isFlagged", json!(true)),
            ("/reviewers/0/hasDeclined", json!(true)),
            ("/threads/1/comments/1/content", json!("Changed discussion")),
            ("/evaluations/0/status", json!("notApplicable")),
        ] {
            let mut changed = original.clone();
            changed["pullRequest"]["repository"]["project"]["revision"] = json!(8);
            changed["pullRequest"]["repository"]["project"]["lastUpdateTime"] =
                json!("2026-01-01T01:00:00Z");
            *changed
                .pointer_mut(path)
                .unwrap_or_else(|| panic!("Missing test field {path}")) = replacement;
            let captured: Evidence = serde_json::from_value(original.clone()).unwrap();
            let changed: Evidence = serde_json::from_value(changed).unwrap();
            assert!(!super::equivalent(&captured, &changed).unwrap(), "{path}");
        }
    }

    #[test]
    fn evaluation_progress_does_not_hide_configuration_applicability_or_discussion_changes() {
        let original = renewed();
        let original: Evidence = serde_json::from_value(original).unwrap();
        let mut progress = original.clone();
        let Evidence::History { evaluations, .. } = &mut progress else {
            unreachable!()
        };
        evaluations.last_mut().unwrap()["status"] = json!("approved");
        evaluations.last_mut().unwrap()["context"] = json!({"changed": true});
        assert!(super::equivalent(&original, &progress).unwrap());

        let mut not_applicable = progress.clone();
        let Evidence::History { evaluations, .. } = &mut not_applicable else {
            unreachable!()
        };
        evaluations.last_mut().unwrap()["status"] = json!("notApplicable");
        assert!(!super::equivalent(&original, &not_applicable).unwrap());

        let mut changed_policy = progress.clone();
        let Evidence::History { evaluations, .. } = &mut changed_policy else {
            unreachable!()
        };
        evaluations.last_mut().unwrap()["configuration"]["settings"]["minimumApproverCount"] =
            json!(2);
        super::equivalent(&original, &changed_policy).unwrap_err();
        let Evidence::History { policies, .. } = &mut changed_policy else {
            unreachable!()
        };
        policies.last_mut().unwrap()["settings"]["minimumApproverCount"] = json!(2);
        assert!(!super::equivalent(&original, &changed_policy).unwrap());

        let Evidence::History { threads, .. } = &mut progress else {
            unreachable!()
        };
        threads.last_mut().unwrap()["comments"][0]["content"] = json!("New human objection");
        assert!(!super::equivalent(&original, &progress).unwrap());
    }

    #[test]
    fn later_human_decisions_supersede_a_renewal_without_moving_its_anchor_implicitly() {
        for (vote, status) in [
            (0, "ineligible"),
            (-5, "ineligible"),
            (-10, "ineligible"),
            (10, "alreadyValid"),
        ] {
            let mut value = renewed();
            let mut event = value["threads"][1].clone();
            event["id"] = json!(40);
            event["publishedDate"] = json!("2026-01-01T02:00:00Z");
            event["properties"]["CodeReviewVoteResult"]["$value"] = json!(vote.to_string());
            value["threads"].as_array_mut().unwrap().push(event);
            value["reviewers"][0]["vote"] = json!(vote);
            let decision = decide(value).unwrap();
            assert_eq!(decision.status(), status);
            if vote == 10 {
                assert_eq!(decision.approval_event_id(), Some("40"));
            }
        }
    }

    #[test]
    fn ordinary_policy_combinations_classify_retained_and_iteration_bound_votes() {
        for (settings, expected) in [
            (json!({}), "alreadyValid"),
            (json!({"resetOnSourcePush": false}), "alreadyValid"),
            (
                json!({"requireVoteOnLastIteration": true}),
                "assessmentRequired",
            ),
            (
                json!({"requireVoteOnEachIteration": true}),
                "assessmentRequired",
            ),
            (
                json!({"requireVoteOnLastIteration": true, "requireVoteOnEachIteration": true}),
                "assessmentRequired",
            ),
        ] {
            let mut value = evidence(&fixture()["stages"][2]);
            reviewer_policy(&mut value, settings);
            reviewer_policy(&mut value, json!({"resetOnSourcePush": false}));
            value["reviewers"][0]["votedFor"] = json!([{"id": "required-group", "vote": 10}]);
            for (index, kind) in [
                "fd2167ab-b0be-447a-8ec8-39368250530e",
                "0609b952-1397-4640-95ec-e00a01b2c241",
                "cbdc66da-9728-4af8-aada-9a5a32e4a226",
                "c6a1889d-b943-4856-b76f-9e46bb6b0df2",
                "fa4e907d-c16b-4a4c-9dfa-4916e5d171ab",
                "67ed70bd-2a6b-4006-af44-be590463f46d",
            ]
            .iter()
            .enumerate()
            {
                let policy = json!({"id": format!("merge-{index}"), "isEnabled": true, "type": {"id": kind}});
                value["policies"]
                    .as_array_mut()
                    .unwrap()
                    .push(policy.clone());
                value["evaluations"].as_array_mut().unwrap().push(json!({
                    "evaluationId": format!("merge-{index}"), "configuration": policy,
                    "status": "rejected"
                }));
            }
            let decision = decide(value.clone()).unwrap();
            assert_eq!(decision.status(), expected);
            if let Decision::AssessmentRequired {
                anchor,
                approval_event_id,
            } = decision
            {
                assert_eq!(approval_event_id, "2");
                assert_eq!(anchor.review_id, "2");
                assert_eq!(
                    anchor.source_commit,
                    value["iterations"][0]["sourceRefCommit"]["commitId"]
                );
            }
            // An explicit reapproval after the latest iteration is fresh in every mode.
            value["threads"][1]["publishedDate"] = json!("2026-01-01T01:00:00Z");
            assert_eq!(decide(value).unwrap().status(), "alreadyValid");
        }
    }

    #[test]
    fn reset_settings_cannot_replace_cause_evidence_or_override_human_decisions() {
        let mut value = evidence(&fixture()["stages"][2]);
        reviewer_policy(
            &mut value,
            json!({"resetOnSourcePush": true, "resetRejectionsOnSourcePush": true}),
        );
        value["reviewers"][0]["vote"] = json!(0);
        assert!(
            decide(value.clone())
                .unwrap_err()
                .to_string()
                .contains("missing authoritative vote-reset evidence")
        );
        value["threads"][1]["properties"]["CodeReviewVoteResult"]["$value"] = json!("0");
        assert_eq!(
            decide(value.clone()).unwrap(),
            Decision::Ineligible {
                reason: "approvalWithdrawn".to_owned()
            }
        );
        value["threads"][1]["properties"]["CodeReviewVoteResult"]["$value"] = json!("-10");
        assert_eq!(
            decide(value).unwrap(),
            Decision::Ineligible {
                reason: "negativeDecision".to_owned()
            }
        );

        let mut no_push = evidence(&fixture()["stages"][1]);
        reviewer_policy(&mut no_push, json!({"resetOnSourcePush": true}));
        no_push["reviewers"][0]["vote"] = json!(0);
        decide(no_push).unwrap_err();
        let mut no_reset = evidence(&fixture()["stages"][2]);
        no_reset["reviewers"][0]["vote"] = json!(0);
        decide(no_reset).unwrap_err();
    }

    #[test]
    fn policy_evidence_gaps_name_the_missing_configuration_or_setting() {
        let mut value = evidence(&fixture()["stages"][2]);
        reviewer_policy(&mut value, json!({"futureResetMode": true}));
        assert!(
            decide(value.clone())
                .unwrap_err()
                .to_string()
                .contains("futureResetMode")
        );
        value["evaluations"] = json!([]);
        assert!(
            decide(value)
                .unwrap_err()
                .to_string()
                .contains("Missing ADO reviewer-policy evaluation")
        );
        let mut value = evidence(&fixture()["stages"][2]);
        reviewer_policy(&mut value, json!({"requireVoteOnLastIteration": true}));
        value["evaluations"][0]["status"] = json!("notApplicable");
        assert_eq!(decide(value).unwrap().status(), "alreadyValid");
        let mut value = evidence(&fixture()["stages"][2]);
        reviewer_policy(
            &mut value,
            json!({"requireVoteOnLastIteration": true, "requireVoteOnEachIteration": "not-a-boolean"}),
        );
        assert!(
            decide(value)
                .unwrap_err()
                .to_string()
                .contains("requireVoteOnEachIteration")
        );
        let mut value = evidence(&fixture()["stages"][2]);
        reviewer_policy(&mut value, json!({}));
        value["evaluations"][0]["status"] = json!("future-status");
        assert!(
            decide(value)
                .unwrap_err()
                .to_string()
                .contains("future-status")
        );
    }

    #[test]
    fn routine_activities_and_other_accounts_do_not_change_the_accounts_authority() {
        let mut value = evidence(&fixture()["stages"][2]);
        for (index, kind) in ["PolicyStatusUpdate", "AutoCompleteUpdate", "IsDraftUpdate"]
            .iter()
            .enumerate()
        {
            let mut event = value["threads"][1].clone();
            event["id"] = json!(format!("activity-{index}"));
            event["properties"]["CodeReviewThreadType"]["$value"] = json!(kind);
            value["threads"].as_array_mut().unwrap().push(event);
        }
        assert_eq!(decide(value.clone()).unwrap().status(), "alreadyValid");
        value["reviewers"] = json!([]);
        value["threads"][1]["identities"]["1"]["id"] = json!("other-account");
        assert_eq!(decide(value).unwrap(), Decision::NoPriorApproval);
    }

    /// Synthetic notifications model absent metadata without copying service prose.
    fn untyped_notification(properties: Option<Value>) -> Value {
        let mut thread = json!({
            "id": "routine-notification",
            "publishedDate": "2026-01-01T12:00:00Z",
            "isDeleted": false,
            "status": "closed",
            "comments": [{
                "commentType": "system",
                "content": "No source change or vote reset. VoteUpdate CodeReviewVoteResult 10."
            }]
        });
        if let Some(properties) = properties {
            thread["properties"] = properties;
        }
        thread
    }

    #[test]
    fn null_and_omitted_notification_properties_preserve_exact_classification() {
        let fixture = fixture();
        for properties in [Some(Value::Null), None] {
            for stage in fixture["stages"].as_array().unwrap() {
                let mut value = evidence(stage);
                let expected = decide(value.clone()).unwrap();
                value["threads"]
                    .as_array_mut()
                    .unwrap()
                    .push(untyped_notification(properties.clone()));
                assert_eq!(decide(value).unwrap(), expected);
            }
            let mut value = evidence(&fixture["stages"][2]);
            reviewer_policy(&mut value, json!({"requireVoteOnLastIteration": true}));
            let expected = decide(value.clone()).unwrap();
            assert_eq!(expected.status(), "assessmentRequired");
            value["threads"]
                .as_array_mut()
                .unwrap()
                .push(untyped_notification(properties));
            assert_eq!(decide(value).unwrap(), expected);
        }
    }

    #[test]
    fn untyped_notifications_do_not_override_later_human_decisions() {
        for properties in [Some(Value::Null), None] {
            for (vote, reason) in [
                (0, "approvalWithdrawn"),
                (-5, "negativeDecision"),
                (-10, "negativeDecision"),
            ] {
                let mut value = evidence(&fixture()["stages"][2]);
                let mut decision = value["threads"][1].clone();
                decision["id"] = json!("later-human-decision");
                decision["publishedDate"] = json!("2026-01-01T18:00:00Z");
                decision["properties"]["CodeReviewVoteResult"]["$value"] = json!(vote.to_string());
                value["reviewers"][0]["vote"] = json!(vote);
                let threads = value["threads"].as_array_mut().unwrap();
                threads.push(decision);
                // Input order must not replace chronological decision ordering.
                threads.push(untyped_notification(properties.clone()));
                assert_eq!(
                    decide(value).unwrap(),
                    Decision::Ineligible {
                        reason: reason.to_owned()
                    }
                );
            }
        }
    }

    #[test]
    fn untyped_notifications_cannot_supply_missing_vote_or_reset_evidence() {
        for properties in [Some(Value::Null), None] {
            let mut value = evidence(&fixture()["stages"][2]);
            reviewer_policy(&mut value, json!({"resetOnSourcePush": true}));
            value["threads"]
                .as_array_mut()
                .unwrap()
                .push(untyped_notification(properties));
            value["reviewers"][0]["vote"] = json!(0);
            assert!(
                decide(value.clone())
                    .unwrap_err()
                    .to_string()
                    .contains("missing authoritative vote-reset evidence")
            );
            value["reviewers"][0]["vote"] = json!(10);
            value["threads"].as_array_mut().unwrap().remove(1);
            assert!(
                decide(value)
                    .unwrap_err()
                    .to_string()
                    .contains("does not match history")
            );
        }
    }

    #[test]
    fn incomplete_structured_events_identify_thread_and_required_authority() {
        let original = evidence(&fixture()["stages"][2]);
        for properties in [
            Value::Null,
            json!({}),
            json!([]),
            json!(false),
            json!({"CodeReviewThreadType": null}),
            json!({"CodeReviewThreadType": {"$type": "System.Int32", "$value": 10}}),
            json!({"CodeReviewThreadType": {"$type": "System.String", "$value": 10}}),
            json!({"CodeReviewVoteResult": {"$type": "System.String", "$value": "10"}}),
        ] {
            let mut value = original.clone();
            value["threads"][1]["properties"] = properties;
            let error = decide(value).unwrap_err().to_string();
            assert!(error.contains("thread 2"), "{error}");
            assert!(error.contains("CodeReviewThreadType"), "{error}");
        }
        let mut value = original.clone();
        value["threads"][1]
            .as_object_mut()
            .unwrap()
            .remove("properties");
        let error = decide(value).unwrap_err().to_string();
        assert!(error.contains("thread 2"), "{error}");
        assert!(error.contains("CodeReviewThreadType"), "{error}");

        for name in [
            "CodeReviewVoteResult",
            "CodeReviewVotedByIdentity",
            "CodeReviewVotedByInitiatorIdentity",
        ] {
            let mut value = original.clone();
            value["threads"][1]["properties"]
                .as_object_mut()
                .unwrap()
                .remove(name);
            let error = decide(value).unwrap_err().to_string();
            assert!(error.contains("thread 2"), "{error}");
            assert!(error.contains(name), "{error}");
        }
    }

    #[test]
    fn untyped_notifications_still_require_complete_nonfuture_surviving_history() {
        for (key, replacement) in [
            ("publishedDate", Value::Null),
            ("publishedDate", json!("2027-01-01T00:00:00Z")),
            ("identities", json!({"1": {"id": "unresolved-actor"}})),
            ("identities", json!([])),
        ] {
            let mut value = evidence(&fixture()["stages"][2]);
            let mut notification = untyped_notification(Some(Value::Null));
            notification[key] = replacement;
            value["threads"].as_array_mut().unwrap().push(notification);
            decide(value).unwrap_err();
        }
        for identities in [Value::Null, json!({})] {
            let mut value = evidence(&fixture()["stages"][2]);
            let mut notification = untyped_notification(Some(Value::Null));
            notification["identities"] = identities;
            value["threads"].as_array_mut().unwrap().push(notification);
            assert_eq!(decide(value).unwrap().status(), "alreadyValid");
        }
    }

    #[test]
    fn verified_stages_retain_the_same_approval_event_until_reviewer_removal() {
        let fixture = fixture();
        assert_eq!(
            decide(evidence(&fixture["stages"][0])).unwrap(),
            Decision::NoPriorApproval
        );
        for index in [1, 2, 3] {
            assert_eq!(
                decide(evidence(&fixture["stages"][index])).unwrap(),
                Decision::AlreadyValid {
                    approval_event_id: "2".to_owned()
                }
            );
        }
        assert_eq!(
            decide(evidence(&fixture["stages"][4])).unwrap(),
            Decision::Ineligible {
                reason: "reviewerRemoved".to_owned()
            }
        );
    }

    /// Replays the captured decline shape without retaining private service content.
    fn decline_event() -> Value {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/ado-reviewer-decline.json"))
                .unwrap();
        fixture["thread"].clone()
    }

    #[test]
    fn later_direct_approval_supersedes_a_historical_decline() {
        let mut value = evidence(&fixture()["stages"][1]);
        let mut approval = value["threads"][1].clone();
        approval["id"] = json!(91);
        approval["publishedDate"] = json!("2026-01-01T00:29:00Z");
        value["threads"] = json!([value["threads"][0], decline_event(), approval]);
        value["reviewers"][0]["votedFor"] = json!([{"id": "group", "vote": 10}]);
        let expected = Decision::AlreadyValid {
            approval_event_id: "91".to_owned(),
        };
        assert_eq!(decide(value.clone()).unwrap(), expected);
        let captured: Evidence = serde_json::from_value(value.clone()).unwrap();
        let compact = super::compact(&captured).unwrap();
        assert_eq!(
            decide(serde_json::to_value(compact).unwrap()).unwrap(),
            expected
        );
        value["threads"].as_array_mut().unwrap().reverse();
        assert_eq!(decide(value).unwrap(), expected);
    }

    #[test]
    fn decline_invalidates_old_authority_even_if_current_flags_are_clear() {
        for current_vote in [0, 10] {
            for flag in [false, true] {
                let mut value = evidence(&fixture()["stages"][2]);
                value["threads"]
                    .as_array_mut()
                    .unwrap()
                    .push(decline_event());
                value["reviewers"][0]["vote"] = json!(current_vote);
                value["reviewers"][0]["hasDeclined"] = json!(flag);
                reviewer_policy(&mut value, json!({"requireVoteOnEachIteration": true}));
                assert_eq!(
                    decide(value).unwrap(),
                    Decision::Ineligible {
                        reason: "pendingHumanReview".to_owned()
                    }
                );
            }
        }
    }

    #[test]
    fn undoing_a_decline_cannot_restore_an_old_approval() {
        for current_vote in [0, -5, -10, 5, 10] {
            let mut value = evidence(&fixture()["stages"][2]);
            value["threads"][1]["properties"]["CodeReviewVoteResult"]["$value"] =
                json!(current_vote.to_string());
            let mut undo = decline_event();
            undo["id"] = json!(91);
            undo["publishedDate"] = json!("2026-01-01T00:29:00Z");
            undo["properties"]["CodeReviewReviewersUpdatedChangedDecline"]["$value"] =
                json!("False");
            value["threads"]
                .as_array_mut()
                .unwrap()
                .extend([decline_event(), undo]);
            value["reviewers"][0]["vote"] = json!(current_vote);
            let expected = match current_vote {
                0 => Decision::Ineligible {
                    reason: "approvalWithdrawn".to_owned(),
                },
                -5 | -10 => Decision::Ineligible {
                    reason: "negativeDecision".to_owned(),
                },
                _ => Decision::NoPriorApproval,
            };
            assert_eq!(decide(value).unwrap(), expected);
        }
        let mut value = evidence(&fixture()["stages"][1]);
        let mut undo = decline_event();
        undo["properties"]["CodeReviewReviewersUpdatedChangedDecline"]["$value"] = json!("False");
        value["threads"].as_array_mut().unwrap().push(undo);
        value["reviewers"][0]["vote"] = json!(0);
        assert!(
            decide(value)
                .unwrap_err()
                .to_string()
                .contains("does not match history")
        );
    }

    #[test]
    fn votes_after_decline_keep_their_own_decision_and_actor_semantics() {
        for (vote, reason) in [
            (0, "approvalWithdrawn"),
            (-5, "negativeDecision"),
            (-10, "negativeDecision"),
            (5, "alreadyValid"),
            (10, "alreadyValid"),
        ] {
            for direct in [true, false] {
                let mut value = evidence(&fixture()["stages"][1]);
                let mut event = value["threads"][1].clone();
                event["id"] = json!(91);
                event["publishedDate"] = json!("2026-01-01T00:29:00Z");
                event["properties"]["CodeReviewVoteResult"]["$value"] = json!(vote.to_string());
                if !direct {
                    event["identities"]["2"] = json!({"id": "proxy"});
                    event["properties"]["CodeReviewVotedByInitiatorIdentity"]["$value"] =
                        json!("2");
                }
                value["threads"]
                    .as_array_mut()
                    .unwrap()
                    .extend([decline_event(), event]);
                value["reviewers"][0]["vote"] = json!(vote);
                let expected = if !direct {
                    Decision::Ineligible {
                        reason: "pendingHumanReview".to_owned(),
                    }
                } else if vote > 0 {
                    Decision::AlreadyValid {
                        approval_event_id: "91".to_owned(),
                    }
                } else {
                    Decision::Ineligible {
                        reason: reason.to_owned(),
                    }
                };
                assert_eq!(
                    decide(value).unwrap(),
                    expected,
                    "vote {vote}, direct {direct}"
                );
            }
        }
    }

    #[test]
    fn post_decline_approval_uses_its_own_source_anchor_and_current_vetoes() {
        let mut value = evidence(&fixture()["stages"][3]);
        let mut approval = value["threads"][1].clone();
        approval["id"] = json!(91);
        // This approval follows the decline and the middle source iteration.
        approval["publishedDate"] = json!("2026-01-01T00:29:00Z");
        value["threads"]
            .as_array_mut()
            .unwrap()
            .extend([decline_event(), approval]);
        reviewer_policy(&mut value, json!({"requireVoteOnEachIteration": true}));
        assert_eq!(
            decide(value.clone()).unwrap(),
            Decision::AssessmentRequired {
                anchor: Anchor {
                    review_id: "91".to_owned(),
                    source_commit: value["iterations"][1]["sourceRefCommit"]["commitId"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                },
                approval_event_id: "91".to_owned(),
            }
        );
        for flag in ["hasDeclined", "isFlagged"] {
            let mut pending = value.clone();
            pending["reviewers"][0][flag] = json!(true);
            assert_eq!(
                decide(pending).unwrap(),
                Decision::Ineligible {
                    reason: "pendingHumanReview".to_owned()
                }
            );
        }
        value["threads"].as_array_mut().unwrap().push(json!({
            "id": 92, "status": "pending",
            "comments": [{"author": {"id": fixture()["accountId"]}, "commentType": "text"}]
        }));
        assert_eq!(
            decide(value).unwrap(),
            Decision::Ineligible {
                reason: "pendingHumanReview".to_owned()
            }
        );
    }

    #[test]
    fn decline_attribution_and_state_are_material_evidence() {
        let mut value = evidence(&fixture()["stages"][2]);
        value["threads"]
            .as_array_mut()
            .unwrap()
            .push(decline_event());
        let original: Evidence = serde_json::from_value(value.clone()).unwrap();
        let compact = super::compact(&original).unwrap();
        assert!(super::equivalent(&original, &compact).unwrap());
        assert_eq!(super::compact(&compact).unwrap(), compact);
        assert_eq!(
            decide(serde_json::to_value(compact).unwrap()).unwrap(),
            decide(value.clone()).unwrap()
        );
        for (path, replacement) in [
            (
                "/properties/CodeReviewReviewersUpdatedChangedDecline/$value",
                json!("False"),
            ),
            (
                "/properties/CodeReviewReviewersUpdatedChangedIdentity/$value",
                json!("2"),
            ),
            ("/identities/1/id", json!("another-account")),
        ] {
            let mut changed = value.clone();
            *changed["threads"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()
                .pointer_mut(path)
                .unwrap() = replacement;
            let changed: Evidence = serde_json::from_value(changed).unwrap();
            assert!(!super::equivalent(&original, &changed).unwrap(), "{path}");
        }
        let mut other = value.clone();
        other["threads"].as_array_mut().unwrap().last_mut().unwrap()["identities"]["1"]["id"] =
            json!("another-account");
        assert_eq!(
            decide(other).unwrap(),
            Decision::AlreadyValid {
                approval_event_id: "2".to_owned()
            }
        );
        for comment_only in [false, true] {
            let mut deleted = value.clone();
            let event = deleted["threads"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap();
            if comment_only {
                event["comments"][0]["isDeleted"] = json!(true);
            } else {
                event["isDeleted"] = json!(true);
            }
            assert_eq!(
                decide(deleted).unwrap(),
                Decision::AlreadyValid {
                    approval_event_id: "2".to_owned()
                }
            );
        }
    }

    #[test]
    fn malformed_decline_evidence_and_tied_decisions_remain_explicit_failures() {
        for (path, replacement) in [
            (
                "/properties/CodeReviewReviewersUpdatedNumDeclined/$type",
                json!("System.String"),
            ),
            (
                "/properties/CodeReviewReviewersUpdatedNumDeclined/$value",
                json!("1"),
            ),
            (
                "/properties/CodeReviewReviewersUpdatedNumDeclined/$value",
                json!(-1),
            ),
            (
                "/properties/CodeReviewReviewersUpdatedNumDeclined/$value",
                json!(2),
            ),
            (
                "/properties/CodeReviewReviewersUpdatedChangedDecline/$type",
                json!("System.Boolean"),
            ),
            (
                "/properties/CodeReviewReviewersUpdatedChangedDecline/$value",
                json!(true),
            ),
            (
                "/properties/CodeReviewReviewersUpdatedChangedDecline/$value",
                json!("unknown"),
            ),
            (
                "/properties/CodeReviewReviewersUpdatedChangedIdentity/$value",
                json!("missing"),
            ),
            (
                "/properties/CodeReviewReviewersUpdatedChangedIdentity",
                Value::Null,
            ),
            (
                "/properties/CodeReviewReviewersUpdatedChangedDecline",
                Value::Null,
            ),
            ("/identities/1/id", Value::Null),
        ] {
            let mut value = evidence(&fixture()["stages"][1]);
            let mut decline = decline_event();
            *decline.pointer_mut(path).unwrap() = replacement;
            value["threads"].as_array_mut().unwrap().push(decline);
            decide(value).expect_err(path);
        }
        let mut value = evidence(&fixture()["stages"][1]);
        value["threads"][1]["publishedDate"] = decline_event()["publishedDate"].clone();
        value["threads"]
            .as_array_mut()
            .unwrap()
            .push(decline_event());
        assert!(
            decide(value)
                .unwrap_err()
                .to_string()
                .contains("Ambiguous ADO decision ordering")
        );
    }

    #[test]
    fn decline_prevents_a_renewal_from_reusing_earlier_external_authority() {
        let mut value = renewed();
        value["threads"]
            .as_array_mut()
            .unwrap()
            .push(decline_event());
        assert!(
            decide(value)
                .unwrap_err()
                .to_string()
                .contains("no preceding approval authority")
        );
    }

    #[test]
    fn documented_vote_strengths_do_not_invent_automatic_reset_causality() {
        for (vote, status) in [
            (5, "alreadyValid"),
            (10, "alreadyValid"),
            (-10, "ineligible"),
            (-5, "ineligible"),
            (0, "ineligible"),
        ] {
            let mut value = evidence(&fixture()["stages"][1]);
            value["reviewers"][0]["vote"] = json!(vote);
            value["threads"][1]["properties"]["CodeReviewVoteResult"]["$value"] =
                json!(vote.to_string());
            assert_eq!(decide(value).unwrap().status(), status);
        }
    }

    #[test]
    fn verified_non_approval_evaluations_do_not_hide_an_existing_vote() {
        let original = evidence(&fixture()["stages"][2]);
        let mut changed = original;
        changed["evaluations"] = json!([{
            "evaluationId": "verified-evaluation",
            "configuration": {"type": {"id": "e67ae10f-cf9a-40bc-8e66-6b3a8216956e"}}
        }]);
        assert_eq!(decide(changed).unwrap().status(), "alreadyValid");
    }

    #[test]
    fn unverified_policy_and_history_forms_cannot_report_an_established_approval() {
        let original = evidence(&fixture()["stages"][2]);
        for (path, replacement) in [
            ("/policies/5/settings/strictVoteMode", json!(true)),
            ("/policies/3/type/id", json!("unknown-policy")),
            (
                "/evaluations",
                json!([{"evaluationId": "unknown-evaluation"}]),
            ),
            (
                "/threads/1/properties/CodeReviewVotedByIdentity/$value",
                json!("missing"),
            ),
            (
                "/threads/1/properties/CodeReviewVoteResult/$type",
                json!("System.Int32"),
            ),
            (
                "/threads/1/properties/CodeReviewVoteResult/$value",
                json!(10),
            ),
            ("/threads/1/identities/1/id", json!("another-account")),
            ("/threads/1/comments/0/commentType", json!("text")),
            ("/iterations", json!([])),
            ("/iterations/1/id", json!(3)),
            ("/sourceCommit", json!("e".repeat(40))),
            ("/threads/1/publishedDate", json!("2027-01-01T00:00:00Z")),
            ("/reviewers/0/vote", json!(0)),
            ("/reviewers/0/vote", json!(99)),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(path).unwrap() = replacement;
            let result = decide(changed);
            assert!(
                !result
                    .as_ref()
                    .is_ok_and(|decision| decision.status() == "alreadyValid"),
                "{path}"
            );
        }
        let mut unknown_setting = original;
        unknown_setting["policies"][5]["settings"]["futureVoteMode"] = json!(true);
        assert!(
            decide(unknown_setting)
                .unwrap_err()
                .to_string()
                .contains("futureVoteMode")
        );
    }

    #[test]
    fn duplicate_and_simultaneous_decisions_are_explicit_gaps() {
        let original = evidence(&fixture()["stages"][1]);
        let mut duplicate = original.clone();
        let event = duplicate["threads"][1].clone();
        duplicate["threads"].as_array_mut().unwrap().push(event);
        decide(duplicate).unwrap_err();
        let mut simultaneous = original;
        simultaneous["threads"][1]["publishedDate"] =
            simultaneous["threads"][0]["publishedDate"].clone();
        decide(simultaneous).unwrap_err();
    }

    #[test]
    fn deleted_threads_and_comments_leave_surviving_authority_unchanged() {
        for stage in fixture()["stages"].as_array().unwrap() {
            let mut original = evidence(stage);
            reviewer_policy(&mut original, json!({"requireVoteOnLastIteration": true}));
            let expected = decide(original.clone()).unwrap();
            for deleted in [
                // Tombstones need not retain the payload required of live history.
                json!({"id": "deleted-thread", "isDeleted": true}),
                json!({"id": "deleted-thread", "isDeleted": true, "status": "pending",
                    "comments": [{"author": {"id": fixture()["accountId"]},
                        "commentType": "system"}]}),
                json!({"id": "deleted-comment", "status": "pending",
                    "comments": [{"isDeleted": true, "commentType": "system",
                        "author": {"id": fixture()["accountId"]}}]}),
            ] {
                let mut value = original.clone();
                value["threads"].as_array_mut().unwrap().push(deleted);
                assert_eq!(decide(value).unwrap(), expected);
            }
        }
    }

    #[test]
    fn deleted_approvals_never_supply_an_anchor_from_current_votes_or_prose() {
        for comment_only in [false, true] {
            for current in [
                Value::Null,
                json!(0),
                json!(5),
                json!(10),
                json!(-5),
                json!(-10),
            ] {
                let mut value = evidence(&fixture()["stages"][2]);
                if comment_only {
                    value["threads"][1]["comments"][0]["isDeleted"] = json!(true);
                } else {
                    value["threads"][1] = json!({"id": 2, "isDeleted": true});
                }
                value["reviewers"] = if current.is_null() {
                    json!([])
                } else {
                    json!([{"id": fixture()["accountId"], "vote": current}])
                };
                let expected = if current.as_i64().is_some_and(|vote| vote < 0) {
                    Decision::Ineligible {
                        reason: "negativeDecision".to_owned(),
                    }
                } else {
                    Decision::NoPriorApproval
                };
                assert_eq!(decide(value).unwrap(), expected);
            }
        }
    }

    #[test]
    fn deleted_history_does_not_override_surviving_decisions_or_pending_reviews() {
        for (vote, reason) in [
            (0, "approvalWithdrawn"),
            (-5, "negativeDecision"),
            (-10, "negativeDecision"),
        ] {
            let mut value = evidence(&fixture()["stages"][2]);
            let mut event = value["threads"][1].clone();
            event["id"] = json!("later-decision");
            event["publishedDate"] = json!("2026-01-01T18:00:00Z");
            event["properties"]["CodeReviewVoteResult"]["$value"] = json!(vote.to_string());
            value["threads"]
                .as_array_mut()
                .unwrap()
                .extend([json!({"id": "deleted", "isDeleted": true}), event]);
            value["reviewers"][0]["vote"] = json!(vote);
            assert_eq!(
                decide(value).unwrap(),
                Decision::Ineligible {
                    reason: reason.to_owned()
                }
            );
        }
        let mut value = evidence(&fixture()["stages"][2]);
        value["threads"].as_array_mut().unwrap().push(json!({
            "id": "pending", "status": "pending", "comments": [
                {"isDeleted": true},
                {"author": {"id": fixture()["accountId"]}}
            ]
        }));
        assert_eq!(
            decide(value).unwrap(),
            Decision::Ineligible {
                reason: "pendingHumanReview".to_owned()
            }
        );
        for flag in ["isFlagged", "hasDeclined"] {
            let mut value = evidence(&fixture()["stages"][2]);
            value["threads"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id": "deleted", "isDeleted": true}));
            value["reviewers"][0][flag] = json!(true);
            assert_eq!(
                decide(value).unwrap(),
                Decision::Ineligible {
                    reason: "pendingHumanReview".to_owned()
                }
            );
        }
        let mut value = evidence(&fixture()["stages"][2]);
        value["threads"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "deleted", "isDeleted": true}));
        value["reviewers"][0]["vote"] = json!(0);
        assert_eq!(
            decide(value).unwrap(),
            Decision::Ineligible {
                reason: "approvalEvidenceDeleted".to_owned()
            }
        );
    }

    #[test]
    fn deleted_renewal_provenance_cannot_manufacture_external_authority() {
        for path in ["/isDeleted", "/comments/0/isDeleted"] {
            let mut value = renewed();
            *value["threads"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()
                .pointer_mut(path)
                .unwrap() = json!(true);
            assert_eq!(
                decide(value).unwrap(),
                Decision::Ineligible {
                    reason: "approvalEvidenceDeleted".to_owned()
                }
            );
            let mut value = renewed();
            *value["threads"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()
                .pointer_mut(path)
                .unwrap() = json!(true);
            let mut withdrawal = value["threads"][1].clone();
            withdrawal["id"] = json!(40);
            withdrawal["publishedDate"] = json!("2026-01-01T02:00:00Z");
            withdrawal["properties"]["CodeReviewVoteResult"]["$value"] = json!("0");
            value["threads"].as_array_mut().unwrap().push(withdrawal);
            value["reviewers"][0]["vote"] = json!(0);
            assert_eq!(
                decide(value).unwrap(),
                Decision::Ineligible {
                    reason: "approvalWithdrawn".to_owned()
                }
            );
        }
        for comment_only in [false, true] {
            let mut value = renewed();
            if comment_only {
                value["threads"][1]["comments"][0]["isDeleted"] = json!(true);
            } else {
                value["threads"][1]["isDeleted"] = json!(true);
            }
            assert_eq!(
                decide(value).unwrap(),
                Decision::Ineligible {
                    reason: "approvalEvidenceDeleted".to_owned()
                }
            );

            let mut value = renewed();
            let event = value["threads"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|thread| thread["id"] == 20)
                .unwrap();
            if comment_only {
                event["comments"][0]["isDeleted"] = json!(true);
            } else {
                *event = json!({"id": 20, "isDeleted": true});
            }
            assert_eq!(
                decide(value).unwrap(),
                Decision::AssessmentRequired {
                    anchor: Anchor {
                        review_id: "2".to_owned(),
                        source_commit:
                            fixture()["stages"][2]["iterations"][0]["sourceRefCommit"]["commitId"]
                                .as_str()
                                .unwrap()
                                .to_owned(),
                    },
                    approval_event_id: "2".to_owned(),
                }
            );
        }
    }

    #[test]
    fn publication_readback_requires_a_surviving_new_vote() {
        let before = ProviderEvidence::AzureDevOps(
            serde_json::from_value(evidence(&fixture()["stages"][2])).unwrap(),
        );
        for comment_only in [false, true] {
            let mut value = renewed();
            let event = value["threads"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|thread| thread["id"] == 20)
                .unwrap();
            let mut surviving = event.clone();
            if comment_only {
                event["comments"][0]["isDeleted"] = json!(true);
            } else {
                *event = json!({"id": 20, "isDeleted": true});
            }
            let after =
                ProviderEvidence::AzureDevOps(serde_json::from_value(value.clone()).unwrap());
            let account = fixture()["accountId"].as_str().unwrap().to_owned();
            assert!(
                super::new_vote(&before, &after, &account, 10)
                    .unwrap_err()
                    .to_string()
                    .contains("exactly one new")
            );
            surviving["id"] = json!(21);
            value["threads"].as_array_mut().unwrap().push(surviving);
            let after = ProviderEvidence::AzureDevOps(serde_json::from_value(value).unwrap());
            assert_eq!(
                super::new_vote(&before, &after, &account, 10).unwrap(),
                "21"
            );
        }
    }

    #[test]
    fn deleted_replies_do_not_hide_surviving_system_votes_or_renewal_audits() {
        let mut value = renewed();
        for thread in value["threads"].as_array_mut().unwrap() {
            thread["comments"]
                .as_array_mut()
                .unwrap()
                .push(json!({"isDeleted": true}));
        }
        assert_eq!(
            decide(value).unwrap(),
            Decision::AlreadyValid {
                approval_event_id: "20".to_owned()
            }
        );
    }

    #[test]
    fn deleted_audit_in_a_renewal_chain_is_ineligible_not_a_new_anchor_or_failure() {
        let mut value = renewed();
        let body = value["threads"].as_array().unwrap().last().unwrap()["comments"][0]["content"]
            .as_str()
            .unwrap();
        let document = body
            .split_once("-ado ")
            .unwrap()
            .1
            .split_once("-->")
            .unwrap()
            .0;
        let mut marker: Value = serde_json::from_str(document).unwrap();
        // A later source and renewal exercise lost attribution inside a chain.
        value["sourceCommit"] = json!("d".repeat(40));
        let source = value["sourceCommit"].clone();
        value["iterations"].as_array_mut().unwrap().push(json!({
            "id": 3, "createdDate": "2026-01-01T02:00:00Z",
            "sourceRefCommit": {"commitId": source}
        }));
        let mut event = value["threads"][1].clone();
        event["id"] = json!(21);
        event["publishedDate"] = json!("2026-01-01T03:00:00Z");
        value["threads"].as_array_mut().unwrap().push(event);
        marker["audit"]["operationId"] = json!(Uuid::new_v4().to_string());
        marker["audit"]["sourceCommit"] = value["sourceCommit"].clone();
        marker["voteEventId"] = json!("21");
        marker["iterationId"] = json!(3);
        append_audit(&mut value, &marker);
        let thread = value["threads"].as_array_mut().unwrap().last_mut().unwrap();
        thread["id"] = json!(31);
        thread["comments"][0]["publishedDate"] = json!("2026-01-01T03:05:00Z");
        thread["comments"][0]["lastUpdatedDate"] = json!("2026-01-01T03:05:00Z");
        assert_eq!(
            decide(value.clone()).unwrap(),
            Decision::AlreadyValid {
                approval_event_id: "21".to_owned()
            }
        );
        value["threads"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|thread| thread["id"] == 30)
            .unwrap()["isDeleted"] = json!(true);
        assert_eq!(
            decide(value).unwrap(),
            Decision::Ineligible {
                reason: "approvalEvidenceDeleted".to_owned()
            }
        );
    }

    #[test]
    fn deleted_records_do_not_hide_malformed_surviving_evidence() {
        for path in [
            "/threads/1/properties/CodeReviewVoteResult",
            "/policies/3/type/id",
            "/iterations/1/sourceRefCommit/commitId",
        ] {
            let mut value = evidence(&fixture()["stages"][2]);
            value["threads"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id": "deleted", "isDeleted": true}));
            *value.pointer_mut(path).unwrap() = Value::Null;
            decide(value).unwrap_err();
        }
        let original: Evidence = serde_json::from_value(evidence(&fixture()["stages"][2])).unwrap();
        let mut changed = original.clone();
        let Evidence::History { threads, .. } = &mut changed else {
            unreachable!()
        };
        threads[1]["isDeleted"] = json!(true);
        assert!(!super::equivalent(&original, &changed).unwrap());
    }

    #[test]
    fn bounded_schema_mutations_never_turn_missing_evidence_into_validity() {
        let original = evidence(&fixture()["stages"][2]);
        let paths = [
            "/sourceCommit",
            "/iterations/0/sourceRefCommit/commitId",
            "/threads/1/identities/1/id",
            "/threads/1/properties/CodeReviewVoteResult/$type",
            "/threads/1/properties/CodeReviewVotedByIdentity/$value",
            "/reviewers/0/vote",
        ];
        // Fresh seeds extend ordinary parser coverage; the fixture bounds every mutation.
        for _ in 0..8 {
            for byte in Uuid::new_v4().as_bytes() {
                let index = usize::from(*byte);
                let mut changed = original.clone();
                let replacement = match index % 3 {
                    0 => Value::Null,
                    1 => json!(false),
                    _ => json!([]),
                };
                *changed.pointer_mut(paths[index % paths.len()]).unwrap() = replacement;
                assert!(!decide(changed).is_ok_and(|decision| decision.status() == "alreadyValid"));
            }
        }
    }
}
