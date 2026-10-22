use std::collections::HashSet;

use jiff::Timestamp;
use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::Api;
use crate::approval::Decision;
use crate::clock::timestamp;
use crate::error::{Failure, fail};
use crate::json::{array, boolean, field, identifier, text};
use crate::renewal::Anchor;
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

impl Evidence {
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
        if identifier(field(field(pr, "repository")?, "id")?)? != repository
            || identifier(field(pr, "pullRequestId")?)? != number.to_string()
        {
            return Err(fail(
                "service",
                "ADO approval evidence has another PR identity",
            ));
        }
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
            }
        }
        decide_history(&events, iterations, &policy, account, current)
    }
}

/// Two complete observations detect changes without claiming an API transaction.
pub(crate) fn capture(api: &mut Api<'_>, target: &Target) -> Result<Evidence, AppError> {
    let first = observe(api, target)?;
    let second = observe(api, target)?;
    if first != second {
        return Err(fail(
            "staleEvidence",
            "ADO approval evidence changed during capture",
        ));
    }
    Ok(first)
}

/// Collects one complete service observation without reading repository content.
fn observe(api: &mut Api<'_>, target: &Target) -> Result<Evidence, AppError> {
    let mut pr = api.transport.request(&target.pr_api(), "GET", None)?;
    // Merge-attempt bookkeeping is not source authority; target names remain for policy routing.
    let metadata = pr
        .as_object_mut()
        .ok_or_else(|| fail("service", "Malformed ADO pull request"))?;
    for key in [
        "lastMergeTargetCommit",
        "lastMergeCommit",
        "mergeStatus",
        "mergeFailureType",
        "mergeFailureMessage",
    ] {
        metadata.remove(key);
    }
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
    let evaluations = policies(api, target, &pr, true)?;
    let policies = policies(api, target, &pr, false)?;
    Ok(Evidence::History {
        pull_request: pr,
        source_commit,
        reviewers,
        iterations,
        threads,
        policies,
        evaluations,
    })
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

/// Configuration continuations and evaluation offsets have different provider contracts.
fn policies(
    api: &mut Api<'_>,
    target: &Target,
    pr: &Value,
    evaluations: bool,
) -> Result<Vec<Value>, AppError> {
    let repository = field(pr, "repository")?;
    let repository_id = identifier(field(repository, "id")?)?;
    let project_id = identifier(field(field(repository, "project")?, "id")?)?;
    let (path, version, key) = if evaluations {
        (
            format!("{}/_apis/policy/evaluations", escape(&project_id)),
            "7.1-preview.1",
            "evaluationId",
        )
    } else {
        (
            format!("{}/_apis/git/policy/configurations", escape(&project_id)),
            "7.1",
            "id",
        )
    };
    let mut values = Vec::new();
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    // Match the existing capture safety bound for endlessly advancing bad pages.
    for _ in 0..1000 {
        let mut query = vec![("$top", "100".to_owned())];
        if evaluations {
            query.extend([
                (
                    "artifactId",
                    format!(
                        "vstfs:///CodeReview/CodeReviewId/{project_id}/{}",
                        target.pull_request_id
                    ),
                ),
                ("includeNotApplicable", "true".to_owned()),
                ("$skip", values.len().to_string()),
            ]);
        } else {
            query.extend([
                ("repositoryId", repository_id.clone()),
                ("refName", text(field(pr, "targetRefName")?)?.to_owned()),
            ]);
            if let Some(cursor) = &cursor {
                query.push(("continuationToken", cursor.clone()));
            }
        }
        let page = api.transport.ado(&path, "GET", None, &query, version)?;
        let items = ado_items(&page.payload)?;
        let empty = items.is_empty();
        values.extend(items);
        unique(&values, key)?;
        if evaluations {
            if page.continuation.is_some() {
                return Err(fail("service", "Unexpected ADO evaluation continuation"));
            }
            if empty {
                values.sort_by_key(|value| value.get(key).map(Value::to_string));
                return Ok(values);
            }
        } else if let Some(next) = page.continuation {
            if empty || !cursors.insert(next.clone()) {
                return Err(fail("service", "ADO policy pagination made no progress"));
            }
            cursor = Some(next);
        } else {
            values.sort_by_key(|value| value.get(key).map(Value::to_string));
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

/// User prose cannot become a vote event, even when it contains system property names.
fn ordered_events(threads: &[Value], now: Timestamp) -> Result<Vec<Event<'_>>, AppError> {
    unique(threads, "id")?;
    let mut events = Vec::new();
    for thread in threads {
        if thread.get("isDeleted") == Some(&Value::Bool(true)) {
            return Err(fail(
                "service",
                "Deleted ADO history cannot prove an approval",
            ));
        }
        let comments = array(field(thread, "comments")?)?;
        let system = comments
            .iter()
            .any(|comment| comment.get("commentType") == Some(&json!("system")));
        if !system {
            continue;
        }
        let kind = property(thread, "CodeReviewThreadType", "System.String")?;
        let at = timestamp(text(field(thread, "publishedDate")?)?)?;
        if at > now {
            return Err(fail("service", "Future ADO system event"));
        }
        events.push(Event {
            id: identifier(field(thread, "id")?)?,
            at,
            value: thread,
            kind: text(kind)?,
        });
    }
    events.sort_by_key(|event| event.at);
    Ok(events)
}

/// Latest explicit decisions and removals supersede every older positive event.
fn decide_history(
    events: &[Event<'_>],
    iterations: &[Value],
    policy: &ApprovalPolicy,
    account: &str,
    current: Option<i64>,
) -> Result<Decision, AppError> {
    let mut latest = None;
    let mut approval = None;
    let mut removed = false;
    let mut relevant_time = None;
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
                    Failure::caused_by("service", "Invalid ADO vote event".to_owned(), error)
                })?;
                vote(&json!(value))?;
                if voter == account {
                    latest = Some(value);
                    // A proxy positive vote is not the account's approval authority.
                    approval = (initiator == account && matches!(value, 5 | 10)).then_some(event);
                    removed = false;
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
                    let count = property(event.value, count, "System.Int32")?
                        .as_u64()
                        .ok_or_else(|| fail("service", "Invalid reviewer-update count"))?;
                    if count > 1 {
                        return Err(fail(
                            "service",
                            format!(
                                "ADO reviewer event {} needs complete bulk identity evidence for {identity} (count {count})",
                                event.id
                            ),
                        ));
                    }
                    if count == 1 && actor(event.value, identity)? == account {
                        relevant = true;
                        approval = None;
                        latest = None;
                        removed = removing;
                    }
                }
                for key in [
                    "CodeReviewReviewersUpdatedNumChanged",
                    "CodeReviewReviewersUpdatedNumDeclined",
                ] {
                    if property(event.value, key, "System.Int32")? != &json!(0) {
                        return Err(fail(
                            "service",
                            format!("ADO reviewer event {} has unsupported {key}", event.id),
                        ));
                    }
                }
                relevant
            }
            // These structured activities do not change reviewer authority.
            "RefUpdate" | "PolicyStatusUpdate" | "AutoCompleteUpdate" | "IsDraftUpdate"
            | "TitleUpdate" | "DescriptionUpdate" | "StatusUpdate" => false,
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
    if let Some(approval) = approval {
        let approved_iteration = iteration_at(iterations, approval.at)?;
        let latest_iteration = iterations
            .iter()
            .max_by_key(|iteration| iteration.get("id").and_then(Value::as_u64))
            .ok_or_else(|| fail("service", "Missing ADO source iteration"))?;
        let advanced = field(approved_iteration, "id")? != field(latest_iteration, "id")?;
        if current != latest {
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
                anchor: Anchor {
                    review_id: approval.id.clone(),
                    source_commit: text(field(
                        field(approved_iteration, "sourceRefCommit")?,
                        "commitId",
                    )?)?
                    .to_owned(),
                },
                approval_event_id: approval.id.clone(),
            });
        }
        return Ok(Decision::AlreadyValid {
            approval_event_id: approval.id.clone(),
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

fn property<'a>(event: &'a Value, name: &str, kind: &str) -> Result<&'a Value, AppError> {
    let property = field(field(event, "properties")?, name)?;
    if text(field(property, "$type")?)? != kind {
        return Err(fail("service", "Unrecognized ADO property wrapper"));
    }
    field(property, "$value")
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
    use crate::clock::timestamp;

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
    fn duplicate_deleted_and_simultaneous_decisions_are_explicit_gaps() {
        let original = evidence(&fixture()["stages"][1]);
        let mut duplicate = original.clone();
        let event = duplicate["threads"][1].clone();
        duplicate["threads"].as_array_mut().unwrap().push(event);
        decide(duplicate).unwrap_err();
        let mut deleted = original.clone();
        deleted["threads"][1]["isDeleted"] = json!(true);
        decide(deleted).unwrap_err();
        let mut simultaneous = original;
        simultaneous["threads"][1]["publishedDate"] =
            simultaneous["threads"][0]["publishedDate"].clone();
        decide(simultaneous).unwrap_err();
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
