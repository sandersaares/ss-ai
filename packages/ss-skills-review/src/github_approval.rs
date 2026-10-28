use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub(crate) use ss_skills_assessment::commit;

use crate::api::Api;
use crate::approval::Decision;
use crate::clock::timestamp;
use crate::error::fail;
use crate::json::{array, boolean, field, identifier, nullable_field, project, text};
use crate::renewal::{Anchor, Audit};
use crate::target::Target;

/// Complete review, dismissal, and discussion observations remain distinct from authority.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "scope", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum Evidence {
    State {
        #[serde(rename = "pullRequest")]
        pull_request: Value,
    },
    History {
        #[serde(rename = "pullRequest")]
        pull_request: Value,
        reviews: Vec<Value>,
        timeline: Vec<Value>,
        comments: Vec<Value>,
        #[serde(rename = "reviewComments")]
        review_comments: Vec<Value>,
    },
}

impl Evidence {
    /// Notice readback shares PR identity checks without requiring approval standing.
    pub(crate) fn validate_scope(&self, repository: &str, number: u32) -> Result<(), AppError> {
        let pr = match self {
            Self::State { pull_request } | Self::History { pull_request, .. } => pull_request,
        };
        if identifier(field(field(field(pr, "base")?, "repo")?, "id")?)? != repository
            || identifier(field(pr, "number")?)? != number.to_string()
        {
            return Err(fail(
                "service",
                "GitHub approval evidence has another PR identity",
            ));
        }
        Ok(())
    }

    /// Reconstructs source-bound service standing without historical target prerequisites.
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
        if identifier(field(field(pr, "user")?, "id")?)? == account {
            return Ok(ineligible("selfIsAuthor"));
        }
        let Self::History {
            reviews,
            timeline,
            comments,
            review_comments,
            ..
        } = self
        else {
            return Err(fail(
                "service",
                "An active PR requires complete approval evidence",
            ));
        };
        let source = field(pr, "head")?;
        commit(text(field(source, "sha")?)?)?;
        if text(field(source, "ref")?)?.is_empty() {
            return Err(fail("service", "Missing GitHub source branch name"));
        }
        identifier(field(field(source, "repo")?, "id")?)?;
        unique(reviews, "node_id")?;
        unique(reviews, "id")?;
        unique(timeline, "id")?;
        for collection in [comments, review_comments] {
            validate_comments(collection, observed_at)?;
        }
        interpret(reviews, timeline, account, repository, number, observed_at)
    }
}

/// Service-time ordering keeps a later withdrawal from reviving an earlier approval.
struct Event<'a> {
    at: Timestamp,
    kind: EventKind<'a>,
}

/// Only service-authored decision and removal records change approval standing.
enum EventKind<'a> {
    Approved { review: &'a str, source: &'a str },
    Renewal { review: &'a str, audit: Audit },
    Negative,
    UnknownRenewal,
    Dismissed { review: &'a str, automatic: bool },
    Removed,
}

/// A dismissal affects its own review, not a newer independently submitted approval.
enum Standing<'a> {
    None,
    Approval {
        anchor: Anchor,
        review: &'a str,
        dismissed: Option<bool>,
    },
    Negative,
    Removed,
    UnknownRenewal,
}

/// Repeated full observations fence all collected evidence, not only the source SHA.
pub(crate) fn capture(api: &mut Api<'_>, target: &Target) -> Result<Evidence, AppError> {
    let first = observe(api, target)?;
    let second = observe(api, target)?;
    if !equivalent(&first, &second)? {
        return Err(fail(
            "staleEvidence",
            "GitHub approval evidence changed during capture",
        ));
    }
    Ok(first)
}

fn observe(api: &mut Api<'_>, target: &Target) -> Result<Evidence, AppError> {
    let pr = project_pull_request(&api.transport.request(&target.pr_api(), "GET", None)?)?;
    if excluded(&pr)?.is_some() {
        return Ok(Evidence::State { pull_request: pr });
    }
    let reviews = list(api, &format!("{}/reviews", target.pr_api()), "node_id")?;
    let timeline = timeline(api, target)?;
    let comments = list(
        api,
        &format!(
            "{}/issues/{}/comments",
            target.repository_api(),
            target.pull_request_id
        ),
        "id",
    )?;
    let review_comments = list(api, &format!("{}/comments", target.pr_api()), "id")?;
    compact(&Evidence::History {
        pull_request: pr,
        reviews,
        timeline,
        comments,
        review_comments,
    })
}

/// Freshness uses the same focused evidence for new and retained observations.
pub(crate) fn equivalent(left: &Evidence, right: &Evidence) -> Result<bool, AppError> {
    Ok(compact(left)? == compact(right)?)
}

/// Keeps review authority and discussion rather than account profiles or reactions.
fn compact(evidence: &Evidence) -> Result<Evidence, AppError> {
    let mut evidence = evidence.clone();
    let pr = match &mut evidence {
        Evidence::State { pull_request } | Evidence::History { pull_request, .. } => pull_request,
    };
    *pr = project_pull_request(pr)?;
    if let Evidence::History {
        reviews,
        comments,
        review_comments,
        ..
    } = &mut evidence
    {
        for review in reviews {
            *review = project(
                review,
                &[
                    "id",
                    "node_id",
                    "state",
                    "submitted_at",
                    "body",
                    "commit_id",
                    "user",
                ],
            );
            if let Some(user) = review.get_mut("user") {
                *user = project(user, &["id", "node_id", "type"]);
            }
        }
        *comments = project_comments(comments);
        *review_comments = project_comments(review_comments);
    }
    Ok(evidence)
}

/// Discussion context and edit times remain material; likes and profile images do not.
pub(crate) fn project_comments(comments: &[Value]) -> Vec<Value> {
    comments
        .iter()
        .map(|comment| {
            let mut comment = project(
                comment,
                &[
                    "id",
                    "node_id",
                    "body",
                    "created_at",
                    "updated_at",
                    "user",
                    "in_reply_to_id",
                    "path",
                    "diff_hunk",
                    "line",
                    "side",
                    "start_line",
                    "start_side",
                    "original_line",
                    "original_start_line",
                    "position",
                    "original_position",
                    "commit_id",
                    "original_commit_id",
                    "pull_request_review_id",
                    "subject_type",
                ],
            );
            if let Some(user) = comment.get_mut("user") {
                *user = project(user, &["id", "node_id", "type"]);
            }
            comment
        })
        .collect()
}

/// Target-tip and merge bookkeeping changes do not alter source assessment authority.
fn project_pull_request(pr: &Value) -> Result<Value, AppError> {
    let source = field(pr, "head")?;
    let source_repository = nullable_field(source, "repo")?;
    let body = nullable_field(pr, "body")?;
    if !body.is_null() {
        text(body)?;
    }
    let mut base = Map::from_iter([(
        "repo".to_owned(),
        json!({"id": field(field(field(pr, "base")?, "repo")?, "id")?}),
    )]);
    if let Some(target_ref) = pr.pointer("/base/ref") {
        base.insert("ref".to_owned(), target_ref.clone());
    }
    let mut projected = Map::from_iter([
        ("number".to_owned(), field(pr, "number")?.clone()),
        ("state".to_owned(), field(pr, "state")?.clone()),
        ("draft".to_owned(), field(pr, "draft")?.clone()),
        ("body".to_owned(), body.clone()),
        (
            "user".to_owned(),
            json!({ "id": field(field(pr, "user")?, "id")? }),
        ),
        (
            "head".to_owned(),
            json!({ "sha": field(source, "sha")?, "ref": field(source, "ref")?,
            "repo": if source_repository.is_null() { Value::Null }
                else { json!({"id": field(source_repository, "id")?}) } }),
        ),
        ("base".to_owned(), Value::Object(base)),
    ]);
    // The title can convey requirements just as the description can.
    if let Some(title) = pr.get("title") {
        projected.insert("title".to_owned(), title.clone());
    }
    Ok(Value::Object(projected))
}

fn excluded(pr: &Value) -> Result<Option<Decision>, AppError> {
    let state = text(field(pr, "state")?)?;
    if !matches!(state, "open" | "closed") {
        return Err(fail("service", "Unknown GitHub PR state"));
    }
    if state == "closed" {
        return Ok(Some(ineligible("closed")));
    }
    if boolean(field(pr, "draft")?)? {
        return Ok(Some(ineligible("draft")));
    }
    if nullable_field(field(pr, "head")?, "repo")?.is_null() {
        return Err(unresolved("deletedSourceRepository"));
    }
    Ok(None)
}

fn list(api: &mut Api<'_>, path: &str, key: &str) -> Result<Vec<Value>, AppError> {
    let mut values = api.transport.list(path)?;
    unique(&values, key)?;
    values.sort_by_key(|value| value.get(key).map(Value::to_string));
    Ok(values)
}

/// Retains dismissal causes independently of the approved `review.commit_id`.
fn timeline(api: &Api<'_>, target: &Target) -> Result<Vec<Value>, AppError> {
    let mut values = Vec::new();
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    // Use the same finite paging guard as commit capture for advancing malformed responses.
    for _ in 0..1000 {
        let response = api.transport.graphql(
            include_str!("approval-history.graphql"),
            &json!({
                "owner": target.organization, "repo": target.repository,
                "num": target.pull_request_id, "cur": cursor
            }),
        )?;
        let connection = response
            .pointer("/data/repository/pullRequest/timelineItems")
            .ok_or_else(|| fail("service", "Missing GitHub approval-history connection"))?;
        let nodes = array(field(connection, "nodes")?)?;
        values.extend(nodes.iter().cloned());
        unique(&values, "id")?;
        let page = field(connection, "pageInfo")?;
        if !boolean(field(page, "hasNextPage")?)? {
            values.sort_by_key(|value| value.get("id").map(Value::to_string));
            return Ok(values);
        }
        let next = text(field(page, "endCursor")?)?;
        if nodes.is_empty() || next.is_empty() || !cursors.insert(next.to_owned()) {
            return Err(fail(
                "service",
                "GitHub approval history made no paging progress",
            ));
        }
        cursor = Some(next.to_owned());
    }
    Err(fail(
        "service",
        "GitHub approval history exceeded the page safety limit",
    ))
}

/// Joins authoritative service records before interpreting chronological decisions.
fn interpret(
    reviews: &[Value],
    timeline: &[Value],
    account: &str,
    repository: &str,
    number: u32,
    now: Timestamp,
) -> Result<Decision, AppError> {
    let mut review_nodes = HashMap::new();
    let mut dismissals = HashMap::new();
    let mut removals = Vec::new();
    for item in timeline {
        let kind = text(field(item, "__typename")?)?;
        let id = text(field(item, "id")?)?;
        match kind {
            "PullRequestReview" => {
                review_nodes.insert(id, text(field(item, "state")?)?);
            }
            "ReviewDismissedEvent" => {
                let review = text(field(field(item, "review")?, "id")?)?;
                if dismissals.insert(review, item).is_some() {
                    return Err(history_conflict(
                        "repeated review dismissal",
                        review,
                        reviews,
                        timeline,
                        number,
                    ));
                }
            }
            "ReviewRequestRemovedEvent" => removals.push(item),
            _ => return Err(fail("service", "Unexpected GitHub approval-history item")),
        }
    }
    let mut events = Vec::new();
    let mut account_node = None;
    let mut pending = false;
    let mut submitted = HashSet::new();
    for review in reviews {
        let id = text(field(review, "node_id")?)?;
        let state = text(field(review, "state")?)?;
        let user = field(review, "user")?;
        let owned = identifier(field(user, "id")?)? == account;
        if owned {
            let node = field(user, "node_id")?;
            identifier(node)?;
            let node = text(node)?;
            if account_node.is_some_and(|previous| previous != node) {
                return Err(fail("service", "Conflicting GitHub account identifiers"));
            }
            account_node = Some(node);
        }
        if state == "PENDING" {
            pending |= owned;
            continue;
        }
        // GitHub can omit comment-only reviews from timelineItems. A present
        // node must still agree, and decision-bearing reviews require the join.
        let timeline_state = review_nodes.get(id).copied();
        if timeline_state != Some(state) && !(timeline_state.is_none() && state == "COMMENTED") {
            return Err(history_conflict(
                "review states disagree or a required timeline review is absent",
                id,
                reviews,
                timeline,
                number,
            ));
        }
        submitted.insert(id);
        let at = event_time(field(review, "submitted_at")?, now)?;
        let body = text(field(review, "body")?)?;
        let previous = if state == "DISMISSED" {
            let dismissal = dismissals.get(id).ok_or_else(|| {
                history_conflict(
                    "dismissed review lacks dismissal evidence",
                    id,
                    reviews,
                    timeline,
                    number,
                )
            })?;
            let dismissed_at = event_time(field(dismissal, "createdAt")?, now)?;
            if dismissed_at <= at {
                return Err(unresolved("ambiguousDecisionOrdering"));
            }
            let cause = nullable_field(dismissal, "pullRequestCommit")?;
            if !cause.is_null() {
                commit(text(field(field(cause, "commit")?, "oid")?)?)?;
            }
            if owned {
                events.push(Event {
                    at: dismissed_at,
                    kind: EventKind::Dismissed {
                        review: id,
                        automatic: !cause.is_null(),
                    },
                });
            }
            text(field(dismissal, "previousReviewState")?)?
        } else {
            if dismissals.contains_key(id) {
                return Err(history_conflict(
                    "undismissed review has dismissal evidence",
                    id,
                    reviews,
                    timeline,
                    number,
                ));
            }
            state
        };
        let kind = match previous {
            "APPROVED" => {
                let source = text(field(review, "commit_id")?)?;
                commit(source)?;
                Some(match Audit::parse(body) {
                    Ok(None) => EventKind::Approved { review: id, source },
                    Ok(Some(audit))
                        if audit.account_id == account
                            && audit.repository_id == repository
                            && audit.pull_request_id == number
                            && audit.source_commit == source =>
                    {
                        EventKind::Renewal { review: id, audit }
                    }
                    _ => EventKind::UnknownRenewal,
                })
            }
            "CHANGES_REQUESTED" => Some(EventKind::Negative),
            "COMMENTED" => None,
            _ => {
                return Err(fail(
                    "service",
                    "Unrecognized submitted GitHub review state",
                ));
            }
        };
        if owned && let Some(kind) = kind {
            events.push(Event { at, kind });
        }
    }
    if let Some(id) = review_nodes
        .keys()
        .chain(dismissals.keys())
        .find(|id| !submitted.contains(*id))
    {
        return Err(history_conflict(
            "timeline refers to a missing or unsubmitted REST review",
            id,
            reviews,
            timeline,
            number,
        ));
    }
    for removal in removals {
        let at = event_time(field(removal, "createdAt")?, now)?;
        let reviewer = nullable_field(removal, "requestedReviewer")?;
        if reviewer.is_null() {
            return Err(unresolved("missingRemovedReviewerIdentity"));
        }
        let kind = text(field(reviewer, "__typename")?)?;
        if !matches!(kind, "User" | "Team" | "Bot" | "Mannequin") {
            return Err(fail("service", "Unknown GitHub reviewer identity type"));
        }
        if Some(text(field(reviewer, "id")?)?) == account_node {
            events.push(Event {
                at,
                kind: EventKind::Removed,
            });
        }
    }
    if pending {
        return Ok(ineligible("pendingHumanReview"));
    }
    decide_events(events)
}

/// Reports join failures without copying review bodies or other private content.
fn history_conflict(
    reason: &str,
    id: &str,
    reviews: &[Value],
    timeline: &[Value],
    number: u32,
) -> AppError {
    let rest = reviews
        .iter()
        .find(|review| review.get("node_id").and_then(Value::as_str) == Some(id))
        .map(|review| json!({"id": review.get("id"), "state": review.get("state")}));
    let graphql: Vec<_> = timeline
        .iter()
        .filter(|item| {
            item.get("id").and_then(Value::as_str) == Some(id)
                || item.pointer("/review/id").and_then(Value::as_str) == Some(id)
        })
        .map(|item| {
            json!({
                "id": item.get("id"), "type": item.get("__typename"),
                "state": item.get("state"), "previousReviewState": item.get("previousReviewState")
            })
        })
        .collect();
    // Paging descriptions mirror Transport::list and approval-history.graphql;
    // immutable snapshots retain collection contents, not page envelopes.
    fail(
        "service",
        format!(
            "GitHub review/timeline reconciliation failed: {reason}; review node {id:?}; \
             REST pulls/{number}/reviews: {}; GraphQL timelineItems: {}; \
             pagination: REST --paginate --slurp (per_page=100, {} reviews); \
             GraphQL hasNextPage/endCursor (first=100, {} timeline items)",
            json!(rest),
            json!(graphql),
            reviews.len(),
            timeline.len(),
        ),
    )
}

/// Later explicit approvals replace old authority; comment-only reviews do not.
fn decide_events(mut events: Vec<Event<'_>>) -> Result<Decision, AppError> {
    events.sort_by_key(|event| event.at);
    let mut previous = None;
    let mut standing = Standing::None;
    for event in events {
        if previous == Some(event.at) {
            return Err(unresolved("ambiguousDecisionOrdering"));
        }
        previous = Some(event.at);
        match event.kind {
            EventKind::Approved { review, source } => {
                standing = Standing::Approval {
                    anchor: Anchor {
                        review_id: review.to_owned(),
                        source_commit: source.to_owned(),
                    },
                    review,
                    dismissed: None,
                }
            }
            EventKind::Renewal { review, audit } => {
                standing = match standing {
                    Standing::Approval {
                        anchor,
                        dismissed: Some(true),
                        ..
                    } if anchor == audit.anchor => Standing::Approval {
                        anchor,
                        review,
                        dismissed: None,
                    },
                    _ => Standing::UnknownRenewal,
                };
            }
            EventKind::Negative => standing = Standing::Negative,
            EventKind::Removed => standing = Standing::Removed,
            EventKind::UnknownRenewal => standing = Standing::UnknownRenewal,
            EventKind::Dismissed { review, automatic } => {
                if let Standing::Approval {
                    review: current,
                    dismissed,
                    ..
                } = &mut standing
                    && *current == review
                {
                    *dismissed = Some(automatic);
                }
            }
        }
    }
    Ok(match standing {
        Standing::None => Decision::NoPriorApproval,
        Standing::Approval {
            review,
            dismissed: None,
            ..
        } => Decision::AlreadyValid {
            approval_event_id: review.to_owned(),
        },
        Standing::Approval {
            anchor,
            review,
            dismissed: Some(true),
        } => Decision::AssessmentRequired {
            anchor,
            approval_event_id: review.to_owned(),
        },
        Standing::Approval {
            dismissed: Some(false),
            ..
        } => ineligible("approvalDismissed"),
        Standing::Negative => ineligible("negativeDecision"),
        Standing::Removed => ineligible("reviewerRemoved"),
        Standing::UnknownRenewal => return Err(unresolved("unreconciledRenewalEvidence")),
    })
}

/// Discussion capture retains edited content without accepting prose as authorization.
fn validate_comments(comments: &[Value], now: Timestamp) -> Result<(), AppError> {
    unique(comments, "id")?;
    for comment in comments {
        text(field(comment, "body")?)?;
        let created = event_time(field(comment, "created_at")?, now)?;
        let updated = event_time(field(comment, "updated_at")?, now)?;
        if updated < created {
            return Err(fail(
                "service",
                "GitHub comment update precedes its creation",
            ));
        }
    }
    Ok(())
}

fn event_time(value: &Value, now: Timestamp) -> Result<Timestamp, AppError> {
    let at = timestamp(text(value)?)?;
    if at > now {
        return Err(fail("service", "Future GitHub approval evidence"));
    }
    Ok(at)
}

fn unique(values: &[Value], key: &str) -> Result<(), AppError> {
    let mut ids = HashSet::new();
    for value in values {
        if !ids.insert(identifier(field(value, key)?)?) {
            return Err(fail("service", "Repeated GitHub evidence identifier"));
        }
    }
    Ok(())
}

/// Unresolved provider authority is an actionable tooling failure, not completion.
fn unresolved(reason: &str) -> AppError {
    fail(
        "service",
        format!("GitHub approval classification failed: {reason}"),
    )
}

fn ineligible(reason: &str) -> Decision {
    Decision::Ineligible {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        reason = "Tests mutate bounded paths in the documented provider fixture"
    )]

    use ohno::AppError;
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::Evidence;
    use crate::approval::Decision;
    use crate::clock::timestamp;
    use crate::renewal::Anchor;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/github-approval.json")).unwrap()
    }

    fn decide(value: Value) -> Result<Decision, AppError> {
        let fixture = fixture();
        serde_json::from_value::<Evidence>(value)?.decide(
            fixture["accountId"].as_str().unwrap(),
            fixture["repositoryId"].as_str().unwrap(),
            1,
            timestamp(fixture["observedAt"].as_str().unwrap()).unwrap(),
        )
    }

    /// Removes only the dismissal to model the service's still-valid approval state.
    fn valid() -> Value {
        let mut value = fixture()["evidence"].clone();
        value["reviews"][1]["state"] = json!("APPROVED");
        value["timeline"][1]["state"] = json!("APPROVED");
        value["timeline"].as_array_mut().unwrap().remove(2);
        value
    }

    #[test]
    fn focused_evidence_ignores_profiles_and_reactions_but_preserves_material_fields() {
        let original: Evidence = serde_json::from_value(fixture()["evidence"].clone()).unwrap();
        let mut rich = serde_json::to_value(&original).unwrap();
        rich["pullRequest"]["user"]["avatar_url"] = json!("cosmetic-profile");
        rich["reviews"][0]["html_url"] = json!("cosmetic-link");
        rich["reviews"][0]["user"]["login"] = json!("cosmetic-name");
        rich["comments"][0]["reactions"] = json!({"+1": 10});
        rich["comments"][0]["user"] = json!({"id": 99, "login": "cosmetic-name"});
        // Establish the meaningful author before changing its profile.
        let mut baseline = serde_json::to_value(&original).unwrap();
        baseline["comments"][0]["user"] = json!({"id": 99});
        baseline["reviewComments"] = json!([{
            "id": 10, "body": "Inline discussion", "path": "src/lib.rs", "line": 1,
            "side": "RIGHT", "commit_id": "b".repeat(40), "user": {"id": 99},
            // An intermediate commit's excerpt need not exist in either assessed tree.
            "diff_hunk": "@@ -1 +1 @@\n-old\n+intermediate",
            "created_at": "2026-01-08T21:00:00Z", "updated_at": "2026-01-08T21:00:00Z"
        }]);
        rich["reviewComments"] = baseline["reviewComments"].clone();
        rich["reviewComments"][0]["user"]["avatar_url"] = json!("cosmetic-profile");
        let baseline: Evidence = serde_json::from_value(baseline).unwrap();
        let rich: Evidence = serde_json::from_value(rich).unwrap();
        let lean = super::compact(&rich).unwrap();
        assert_eq!(
            serde_json::to_value(&lean).unwrap()["reviewComments"][0]["diff_hunk"],
            serde_json::to_value(&rich).unwrap()["reviewComments"][0]["diff_hunk"]
        );
        assert!(super::equivalent(&baseline, &rich).unwrap());
        assert!(super::equivalent(&rich, &lean).unwrap());
        assert_eq!(super::compact(&lean).unwrap(), lean);
        assert!(!serde_json::to_string(&lean).unwrap().contains("cosmetic-"));
        let saved = serde_json::to_value(&rich).unwrap();
        for (path, replacement) in [
            ("/pullRequest/body", json!("New requirement")),
            ("/pullRequest/base/ref", json!("another-target")),
            ("/pullRequest/head/sha", json!("e".repeat(40))),
            ("/pullRequest/head/repo/id", json!(456)),
            ("/reviews/0/body", json!("New review concern")),
            ("/reviews/1/state", json!("CHANGES_REQUESTED")),
            ("/reviews/1/user/id", json!(99)),
            ("/comments/0/body", json!("New objection")),
            ("/comments/0/user/id", json!(42)),
            ("/comments/0/updated_at", json!("2026-01-09T00:00:00Z")),
            ("/reviewComments/0/path", json!("src/other.rs")),
            ("/reviewComments/0/line", json!(2)),
            (
                "/reviewComments/0/diff_hunk",
                json!("@@ -1 +1 @@\n-old\n+other"),
            ),
            (
                "/timeline/2/pullRequestCommit/commit/oid",
                json!("e".repeat(40)),
            ),
        ] {
            let mut changed = saved.clone();
            *changed.pointer_mut(path).unwrap() = replacement;
            let changed = serde_json::from_value(changed).unwrap();
            assert!(!super::equivalent(&rich, &changed).unwrap(), "{path}");
        }
        assert_eq!(serde_json::to_value(&rich).unwrap(), saved);
    }

    /// Adds a later decision without rewriting the older review or its baseline.
    fn later_review(value: &mut Value, state: &str, body: &str) {
        let mut review = value["reviews"][1].clone();
        review["id"] = json!(4);
        review["node_id"] = json!("review-4");
        review["state"] = json!(state);
        review["body"] = json!(body);
        review["submitted_at"] = json!("2026-01-09T00:00:00Z");
        value["reviews"].as_array_mut().unwrap().push(review);
        if state != "PENDING" {
            value["timeline"].as_array_mut().unwrap().push(json!({
                "__typename": "PullRequestReview", "id": "review-4", "state": state
            }));
        }
    }

    #[test]
    fn ui_approval_needs_only_its_original_source_and_verified_dismissal() {
        let fixture = fixture();
        let evidence = &fixture["evidence"];
        assert_ne!(
            evidence["reviews"][1]["commit_id"],
            evidence["timeline"][2]["pullRequestCommit"]["commit"]["oid"]
        );
        assert_eq!(
            decide(evidence.clone()).unwrap(),
            Decision::AssessmentRequired {
                anchor: Anchor {
                    review_id: "review-2".to_owned(),
                    source_commit: "a".repeat(40)
                },
                approval_event_id: "review-2".to_owned(),
            }
        );
        assert_eq!(
            decide(valid()).unwrap(),
            Decision::AlreadyValid {
                approval_event_id: "review-2".to_owned()
            }
        );
        let mut without_target = evidence.clone();
        without_target["pullRequest"]["base"]
            .as_object_mut()
            .unwrap()
            .remove("sha");
        without_target["pullRequest"]["base"]
            .as_object_mut()
            .unwrap()
            .remove("ref");
        assert_eq!(
            decide(without_target).unwrap().status(),
            "assessmentRequired"
        );
    }

    #[test]
    fn renewals_preserve_the_external_anchor_and_cannot_override_a_withdrawal() {
        let mut value = fixture()["evidence"].clone();
        let audit = json!({
            "schemaVersion": 1, "operationId": Uuid::new_v4().to_string(),
            "repositoryId": "123", "accountId": "42", "pullRequestId": 1,
            "anchor": {"reviewId": "review-2", "sourceCommit": "a".repeat(40)},
            "sourceCommit": "b".repeat(40), "policyDigest": "1".repeat(64),
            "assessmentDigest": "2".repeat(64), "state": "submitted"
        });
        later_review(
            &mut value,
            "APPROVED",
            &format!("Renewal.\n<!-- ss-autoreapprove {audit} -->"),
        );
        value["reviews"][3]["commit_id"] = json!("b".repeat(40));
        for (field, replacement) in [
            ("schemaVersion", json!(2)),
            ("accountId", json!("other")),
            ("repositoryId", json!("other")),
            ("pullRequestId", json!(2)),
            ("sourceCommit", json!("c".repeat(40))),
            (
                "anchor",
                json!({"reviewId":"review-other", "sourceCommit":"a".repeat(40)}),
            ),
        ] {
            let mut mismatched = value.clone();
            let mut invalid = audit.clone();
            invalid[field] = replacement;
            mismatched["reviews"][3]["body"] =
                json!(format!("<!-- ss-autoreapprove {invalid} -->"));
            assert!(
                decide(mismatched)
                    .unwrap_err()
                    .to_string()
                    .contains("unreconciledRenewalEvidence")
            );
        }
        let mut duplicate = value.clone();
        duplicate["reviews"][3]["body"] = json!(format!(
            "<!-- ss-autoreapprove {audit} --><!-- ss-autoreapprove {audit} -->"
        ));
        assert!(
            decide(duplicate)
                .unwrap_err()
                .to_string()
                .contains("unreconciledRenewalEvidence")
        );
        assert_eq!(
            decide(value.clone()).unwrap().approval_event_id(),
            Some("review-4")
        );
        value["reviews"][3]["state"] = json!("DISMISSED");
        value["timeline"][4]["state"] = json!("DISMISSED");
        value["timeline"].as_array_mut().unwrap().push(json!({
            "__typename": "ReviewDismissedEvent", "id": "dismissal-2",
            "createdAt": "2026-01-09T01:00:00Z", "previousReviewState": "APPROVED",
            "review": {"id": "review-4"}, "pullRequestCommit": {"commit": {"oid": "d".repeat(40)}}
        }));
        assert_eq!(
            decide(value.clone()).unwrap(),
            Decision::AssessmentRequired {
                anchor: Anchor {
                    review_id: "review-2".to_owned(),
                    source_commit: "a".repeat(40)
                },
                approval_event_id: "review-4".to_owned(),
            }
        );
        value["timeline"].as_array_mut().unwrap().push(json!({
            "__typename": "ReviewRequestRemovedEvent", "id": "removal",
            "createdAt": "2026-01-08T23:00:00Z", "requestedReviewer": {"__typename": "User", "id": "user-42"}
        }));
        assert!(
            decide(value)
                .unwrap_err()
                .to_string()
                .contains("unreconciledRenewalEvidence")
        );
    }

    #[test]
    fn explicit_decisions_supersede_old_authority_but_comments_do_not() {
        for (state, status, id) in [
            ("APPROVED", "alreadyValid", Some("review-4")),
            ("COMMENTED", "alreadyValid", Some("review-2")),
            ("CHANGES_REQUESTED", "ineligible", None),
            ("PENDING", "ineligible", None),
        ] {
            let mut value = valid();
            later_review(&mut value, state, "An ordinary review.");
            let decision = decide(value).unwrap();
            assert_eq!(decision.status(), status);
            assert_eq!(decision.approval_event_id(), id);
        }
        let mut value = fixture()["evidence"].clone();
        later_review(&mut value, "APPROVED", "A new explicit approval.");
        assert_eq!(decide(value).unwrap().approval_event_id(), Some("review-4"));
    }

    #[test]
    fn omitted_comment_reviews_preserve_standing_for_any_author() {
        let mut no_prior = valid();
        no_prior["reviews"][1]["user"]["id"] = json!(99);
        for baseline in [valid(), fixture()["evidence"].clone(), no_prior] {
            let expected = decide(baseline.clone()).unwrap();
            for owned in [true, false] {
                let mut value = baseline.clone();
                later_review(&mut value, "COMMENTED", "A comment is not a new decision.");
                if !owned {
                    value["reviews"][3]["user"] = json!({"id": 99, "node_id": "user-99"});
                }
                value["timeline"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|item| item["state"] != "COMMENTED");
                assert_eq!(decide(value).unwrap(), expected);
            }
        }
    }

    #[test]
    fn omitted_comments_cannot_hide_negative_pending_or_removed_standing() {
        for (state, expected) in [
            ("CHANGES_REQUESTED", "negativeDecision"),
            ("PENDING", "pendingHumanReview"),
        ] {
            let mut value = valid();
            later_review(&mut value, state, "A later human decision.");
            value["timeline"]
                .as_array_mut()
                .unwrap()
                .retain(|item| item["state"] != "COMMENTED");
            assert_eq!(decide(value).unwrap().reason(), Some(expected));
        }
        let mut value = valid();
        value["timeline"]
            .as_array_mut()
            .unwrap()
            .retain(|item| item["state"] != "COMMENTED");
        value["timeline"].as_array_mut().unwrap().push(json!({
            "__typename": "ReviewRequestRemovedEvent", "id": "removal",
            "createdAt": "2026-01-09T00:00:00Z",
            "requestedReviewer": {"__typename": "User", "id": "user-42"}
        }));
        assert_eq!(decide(value).unwrap().reason(), Some("reviewerRemoved"));
    }

    #[test]
    fn omitted_comments_still_require_valid_records_and_consistent_dismissals() {
        for (path, replacement) in [
            ("/reviews/0/user", Value::Null),
            ("/reviews/0/submitted_at", Value::Null),
            ("/reviews/0/submitted_at", json!("2027-01-01T00:00:00Z")),
            ("/reviews/0/body", Value::Null),
            ("/reviews/0/state", json!("UNKNOWN")),
        ] {
            let mut value = valid();
            value["timeline"].as_array_mut().unwrap().remove(0);
            *value.pointer_mut(path).unwrap() = replacement;
            decide(value).unwrap_err();
        }
        let mut value = valid();
        value["timeline"].as_array_mut().unwrap().remove(0);
        value["timeline"].as_array_mut().unwrap().push(json!({
            "__typename": "ReviewDismissedEvent", "id": "dismissal-comment",
            "createdAt": "2026-01-09T00:00:00Z", "previousReviewState": "COMMENTED",
            "review": {"id": "review-1"}, "pullRequestCommit": null
        }));
        let message = decide(value).unwrap_err().to_string();
        assert!(message.contains("undismissed review has dismissal evidence"));
        assert!(message.contains("dismissal-comment"));
    }

    #[test]
    fn required_review_joins_report_ids_states_endpoints_and_pagination() {
        for state in ["APPROVED", "CHANGES_REQUESTED", "DISMISSED", "UNKNOWN"] {
            let mut value = fixture()["evidence"].clone();
            value["reviews"][1]["state"] = json!(state);
            value["timeline"].as_array_mut().unwrap().remove(1);
            let message = decide(value).unwrap_err().to_string();
            for expected in [
                "review-2",
                "\"id\":2",
                state,
                "REST pulls/1/reviews",
                "GraphQL timelineItems",
                "--paginate --slurp",
                "per_page=100",
                "3 reviews",
                "hasNextPage/endCursor",
                "first=100",
                "3 timeline items",
            ] {
                assert!(message.contains(expected), "{expected}: {message}");
            }
            assert!(!message.contains("Synthetic approving review."));
        }
        let mut value = valid();
        value["timeline"][0]["state"] = json!("APPROVED");
        let message = decide(value).unwrap_err().to_string();
        assert!(message.contains("review-1"));
        assert!(message.contains("COMMENTED"));
        assert!(message.contains("APPROVED"));
    }

    #[test]
    fn timeline_only_and_pending_nodes_do_not_become_missing_comment_exceptions() {
        for state in ["COMMENTED", "APPROVED", "CHANGES_REQUESTED", "PENDING"] {
            let mut value = valid();
            value["timeline"].as_array_mut().unwrap().push(json!({
                "__typename": "PullRequestReview", "id": "orphan", "state": state
            }));
            let message = decide(value).unwrap_err().to_string();
            assert!(message.contains("missing or unsubmitted REST review"));
            assert!(message.contains("orphan"));
            assert!(message.contains(state));
            assert!(message.contains("REST pulls/1/reviews: null"));
        }
        let mut value = valid();
        value["reviews"][1]["state"] = json!("PENDING");
        let message = decide(value).unwrap_err().to_string();
        assert!(message.contains("missing or unsubmitted REST review"));
        assert!(message.contains("PENDING"));
        assert!(message.contains("APPROVED"));
    }

    #[test]
    fn dismissal_join_failures_report_the_related_review_and_events() {
        let mut missing = fixture()["evidence"].clone();
        missing["timeline"].as_array_mut().unwrap().remove(2);
        let message = decide(missing).unwrap_err().to_string();
        assert!(message.contains("dismissed review lacks dismissal evidence"));
        assert!(message.contains("review-2"));
        assert!(message.contains("DISMISSED"));

        let mut orphan = fixture()["evidence"].clone();
        orphan["reviews"].as_array_mut().unwrap().remove(1);
        orphan["timeline"].as_array_mut().unwrap().remove(1);
        let message = decide(orphan).unwrap_err().to_string();
        assert!(message.contains("missing or unsubmitted REST review"));
        assert!(message.contains("review-2"));
        assert!(message.contains("dismissal-1"));
        assert!(message.contains("APPROVED"));

        let mut repeated = fixture()["evidence"].clone();
        let mut dismissal = repeated["timeline"][2].clone();
        dismissal["id"] = json!("dismissal-duplicate");
        repeated["timeline"].as_array_mut().unwrap().push(dismissal);
        let message = decide(repeated).unwrap_err().to_string();
        assert!(message.contains("repeated review dismissal"));
        assert!(message.contains("review-2"));
        assert!(message.contains("dismissal-1"));
        assert!(message.contains("dismissal-duplicate"));
    }

    #[test]
    fn null_causes_removals_and_unknown_renewals_cannot_restore_old_authority() {
        let mut value = fixture()["evidence"].clone();
        value["timeline"][2]["pullRequestCommit"] = Value::Null;
        // Dismissal removes approval authority unless the service proves source-change invalidation.
        assert_eq!(
            decide(value).unwrap(),
            Decision::Ineligible {
                reason: "approvalDismissed".to_owned(),
            }
        );
        let mut value = valid();
        value["timeline"].as_array_mut().unwrap().push(json!({
            "__typename": "ReviewRequestRemovedEvent", "id": "removal-1",
            "createdAt": "2026-01-09T00:00:00Z",
            "requestedReviewer": {"__typename": "User", "id": "user-42"}
        }));
        assert_eq!(decide(value).unwrap().reason(), Some("reviewerRemoved"));
        let mut value = valid();
        later_review(
            &mut value,
            "APPROVED",
            "<!-- ss-autoreapprove malformed -->",
        );
        assert!(
            decide(value)
                .unwrap_err()
                .to_string()
                .contains("unreconciledRenewalEvidence")
        );
    }

    #[test]
    fn dismissal_cannot_fall_back_to_an_older_approval_or_cancel_a_newer_one() {
        let mut value = valid();
        later_review(&mut value, "APPROVED", "A newer independent approval.");
        value["reviews"][3]["state"] = json!("DISMISSED");
        value["timeline"][3]["state"] = json!("DISMISSED");
        value["timeline"].as_array_mut().unwrap().push(json!({
            "__typename": "ReviewDismissedEvent", "id": "dismissal-2",
            "createdAt": "2026-01-09T01:00:00Z", "previousReviewState": "APPROVED",
            "review": {"id": "review-4"}, "pullRequestCommit": null
        }));
        assert_eq!(
            decide(value).unwrap(),
            Decision::Ineligible {
                reason: "approvalDismissed".to_owned(),
            }
        );
        let mut value = fixture()["evidence"].clone();
        later_review(&mut value, "APPROVED", "A newer independent approval.");
        value["timeline"][2]["createdAt"] = json!("2026-01-09T01:00:00Z");
        value["timeline"][2]["pullRequestCommit"] = Value::Null;
        assert_eq!(decide(value).unwrap().approval_event_id(), Some("review-4"));
        let mut value = fixture()["evidence"].clone();
        value["timeline"][2]["previousReviewState"] = json!("CHANGES_REQUESTED");
        value["timeline"][2]["pullRequestCommit"] = Value::Null;
        assert_eq!(decide(value).unwrap().reason(), Some("negativeDecision"));
    }

    #[test]
    fn missing_nullable_fields_do_not_mean_unknown_values() {
        for property in ["pullRequestCommit", "review"] {
            let mut value = fixture()["evidence"].clone();
            value["timeline"][2]
                .as_object_mut()
                .unwrap()
                .remove(property);
            decide(value).unwrap_err();
        }
        let mut value = valid();
        value["pullRequest"]["head"]["repo"] = Value::Null;
        assert!(
            decide(value)
                .unwrap_err()
                .to_string()
                .contains("deletedSourceRepository")
        );
        let mut value = valid();
        value["reviews"][1]["user"]["node_id"] = json!("");
        decide(value).unwrap_err();
    }

    #[test]
    fn incomplete_or_contradictory_service_history_is_not_an_empty_approval_result() {
        let original = fixture()["evidence"].clone();
        for (path, replacement) in [
            ("/reviews/1/node_id", json!("missing-review")),
            ("/reviews/1/commit_id", json!("not-a-commit")),
            ("/reviews/1/user", Value::Null),
            ("/reviews/1/state", json!("APPROVED")),
            ("/timeline/2/previousReviewState", json!("PENDING")),
            ("/timeline/2/review", Value::Null),
            ("/timeline/2/pullRequestCommit", json!({})),
            ("/timeline/2/createdAt", json!("2027-01-01T00:00:00Z")),
            ("/pullRequest/base/repo/id", json!(999)),
            ("/comments/0/updated_at", json!("2020-01-01T00:00:00Z")),
        ] {
            let mut value = original.clone();
            *value.pointer_mut(path).unwrap() = replacement;
            decide(value).unwrap_err();
        }
        let mut value = original;
        let duplicate = value["timeline"][2].clone();
        value["timeline"].as_array_mut().unwrap().push(duplicate);
        decide(value).unwrap_err();
    }

    #[test]
    fn ordering_and_identity_gaps_remain_explicit() {
        let mut value = valid();
        later_review(&mut value, "APPROVED", "Ordinary approval.");
        value["reviews"][3]["submitted_at"] = value["reviews"][1]["submitted_at"].clone();
        assert!(
            decide(value)
                .unwrap_err()
                .to_string()
                .contains("ambiguousDecisionOrdering")
        );
        let mut value = valid();
        value["reviews"][1]["user"]["id"] = json!(900);
        assert_eq!(decide(value).unwrap(), Decision::NoPriorApproval);
        let mut value = valid();
        value["reviews"][1]["state"] = json!("CHANGES_REQUESTED");
        value["timeline"][1]["state"] = json!("CHANGES_REQUESTED");
        assert_eq!(decide(value).unwrap().reason(), Some("negativeDecision"));
    }

    #[test]
    fn bounded_record_mutations_cannot_create_valid_approval_evidence() {
        let original = valid();
        let paths = [
            "/reviews/1/node_id",
            "/reviews/1/user/id",
            "/reviews/1/user/node_id",
            "/reviews/1/commit_id",
            "/timeline/1/state",
            "/pullRequest/base/repo/id",
        ];
        // Fresh seeds vary bounded malformed values without relying on execution duration.
        for _ in 0..8 {
            for byte in Uuid::new_v4().as_bytes() {
                let index = usize::from(*byte);
                let mut value = original.clone();
                *value.pointer_mut(paths[index % paths.len()]).unwrap() =
                    [Value::Null, json!([]), json!(false)][index % 3].clone();
                assert!(!decide(value).is_ok_and(|decision| decision.status() == "alreadyValid"));
            }
        }
    }
}
