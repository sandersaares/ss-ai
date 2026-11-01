//! Publishes one final review while retaining its durable transaction lock.
#![allow(
    clippy::result_large_err,
    reason = "The application boundary preserves classified diagnostics"
)]
#![expect(
    clippy::indexing_slicing,
    reason = "The transaction constructs the JSON objects and operation indexes itself"
)]

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

use ohno::AppError;
use serde::de::{Error as _, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, from_slice, json, to_vec_pretty};
use ss_ai_runtime::FileTransaction;

use crate::error::fail;
use crate::{Clock, classification};

/// Provider effects retain their review handle in the composition layer.
pub(crate) trait Effects {
    fn start(&mut self) -> Result<Value, AppError>;
    fn add(&mut self, operation: &Value) -> Result<Value, AppError>;
    fn verify_findings(&mut self) -> Result<Value, AppError>;
    fn approve(&mut self) -> Result<Value, AppError>;
    fn complete(&mut self, summary: &str, approve: bool) -> Result<Value, AppError>;
    fn verify(&mut self) -> Result<Value, AppError>;
    fn reopen(&mut self, thread_id: &str) -> Result<(), AppError>;
    fn discard(&mut self) -> Result<(), AppError>;
}

/// Provides an in-process journal boundary for deterministic failure tests.
trait Journal {
    fn read(&mut self) -> Result<Option<Vec<u8>>, AppError>;
    fn write(&mut self, value: &Value) -> Result<(), AppError>;
}

impl Journal for FileTransaction<'_> {
    fn read(&mut self) -> Result<Option<Vec<u8>>, AppError> {
        self.read_bytes()
    }

    fn write(&mut self, value: &Value) -> Result<(), AppError> {
        self.write_bytes(&to_vec_pretty(value)?)
    }
}

/// Refuses to overwrite evidence of a possible prior publication.
pub(crate) fn check_prior(
    journal: &mut FileTransaction<'_>,
    target: &Value,
) -> Result<(), AppError> {
    check_prior_inner(journal, target)
}

/// Runs after caller preflight under the same retained lock used by `check_prior`.
///
/// The caller supplies the path because `FileTransaction` does not expose it.
/// A follow-up failure returns an outcome, not an error: the caller must emit
/// that outcome before exiting nonzero. Other errors leave the journal intact.
#[expect(
    clippy::too_many_arguments,
    reason = "The composition interface keeps the locked journal and transaction inputs explicit"
)]
pub(crate) fn execute(
    journal: &mut FileTransaction<'_>,
    result_path: &Path,
    effects: &mut dyn Effects,
    target: &Value,
    state: &Value,
    plan: &[Value],
    summary: &str,
    approve: bool,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    check_prior_inner(journal, target)?;
    execute_inner(
        journal,
        effects,
        target,
        state,
        plan,
        summary,
        approve,
        &clock.now().to_string(),
        &result_path.to_string_lossy(),
    )
}

/// Compares provider coordinates rather than incidental browser URL spelling.
fn same_target(left: &Value, right: &Value) -> bool {
    ["platform", "organization", "project", "repository"]
    .iter()
    .all(|key| {
        left[*key]
            .as_str()
            .unwrap_or_default()
            .eq_ignore_ascii_case(right[*key].as_str().unwrap_or_default())
    }) && left["pullRequestId"] == right["pullRequestId"]
    // Collection paths may be case-sensitive even when hostnames are not;
    // a conservative mismatch is preferable to replacing another record.
    && left["collectionUrl"].as_str().map(|value| value.trim_end_matches('/'))
        == right["collectionUrl"].as_str().map(|value| value.trim_end_matches('/'))
}

/// Accepts only enough target evidence to distinguish separate pull requests.
fn valid_target(target: &Value) -> bool {
    matches!(target["platform"].as_str(), Some("gitHub" | "azureDevOps"))
        && ["organization", "repository"]
            .iter()
            .all(|key| target[*key].as_str().is_some_and(|s| !s.trim().is_empty()))
        && target["pullRequestId"].as_u64().is_some_and(|id| id > 0)
        && target["webUrl"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
        && (target["platform"] != "azureDevOps"
            || (target["project"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty())
                && target["collectionUrl"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty())))
}

/// Preserves object keys until duplicates have been rejected recursively.
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor)
    }
}

/// Converts complete JSON values without silently dropping repeated properties.
struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object properties")
    }

    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(json!(v)))
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(json!(v)))
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(json!(v)))
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        Ok(UniqueValue(json!(v)))
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(json!(v)))
    }

    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(json!(v)))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        let mut names = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            // PowerShell consumers compare property names without ASCII case.
            if !names.insert(key.to_ascii_lowercase()) {
                return Err(A::Error::custom("Duplicate or ambiguous JSON property"));
            }
            values.insert(key, map.next_value::<UniqueValue>()?.0);
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}

/// Validates the prior schema before allowing any overwrite.
fn check_prior_inner(journal: &mut dyn Journal, target: &Value) -> Result<(), AppError> {
    if !valid_target(target) {
        return Err(fail("invalidInput", "Invalid publication target"));
    }
    let Some(bytes) = journal.read()? else {
        return Ok(());
    };
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let old: UniqueValue = from_slice(bytes).map_err(|error| {
        fail(
            "invalidState",
            format!("Malformed publication record: {error}"),
        )
    })?;
    let old = old.0;
    let operations = old["operations"]
        .as_array()
        .ok_or_else(|| fail("invalidState", "Publication record has no operations"))?;
    if old["schemaVersion"] != 1
        || !valid_target(&old["target"])
        || !matches!(
            old["status"].as_str(),
            Some("inProgress" | "failed" | "succeeded" | "publishedWithFollowUpFailure")
        )
        || !valid_operations(operations)
        || operations.iter().any(|op| {
            !matches!(
                op["operation"].as_str(),
                Some(
                    "startReview"
                        | "newLineThread"
                        | "newFileThread"
                        | "reply"
                        | "verifyFindings"
                        | "approveReview"
                        | "completeReview"
                        | "reopenThread"
                )
            ) || !matches!(
                op["outcome"].as_str(),
                Some("pending" | "succeeded" | "failed" | "ambiguous")
            )
        })
    {
        return Err(fail("invalidState", "Invalid publication record"));
    }
    if !same_target(target, &old["target"]) {
        return Err(fail(
            "invalidState",
            "Publication record names a different pull request",
        ));
    }
    // An initial record or a pending intent is not evidence that no request ran.
    // Only a terminal, definitive start refusal proves that no effect occurred.
    let clean_refusal = old["status"] == "failed"
        && old["inFlight"].is_null()
        && operations
            .first()
            .is_some_and(|op| op["operation"] == "startReview" && op["outcome"] == "failed")
        && operations
            .iter()
            .skip(1)
            .all(|op| op["outcome"] == "pending");
    if !clean_refusal {
        return Err(fail(
            "ambiguousWrite",
            "Prior publication requires reconciliation before replay",
        ));
    }
    Ok(())
}

/// Tracks which journal operations may follow the validated prefix.
#[derive(PartialEq, Eq)]
enum OperationPhase {
    AwaitingStart,
    PublishingFindings,
    FindingsVerified,
    ApprovalRecorded,
    ReviewCompleted,
}

/// Rejects incomplete or reordered schema-1 operation lists before replacement.
fn valid_operations(operations: &[Value]) -> bool {
    let mut phase = OperationPhase::AwaitingStart;
    let mut findings = HashSet::new();
    let mut reopen = HashSet::new();
    for (index, op) in operations.iter().enumerate() {
        if !op.is_object()
            || !["findingId", "threadId", "commentId", "message"]
                .iter()
                .all(|key| op.get(*key).is_some_and(|v| v.is_null() || v.is_string()))
            || !op
                .get("anchor")
                .is_some_and(|v| v.is_null() || v.is_object())
            || !op["wasDowngraded"].is_boolean()
        {
            return false;
        }
        let id = op["findingId"].as_str();
        match op["operation"].as_str() {
            Some("startReview") if index == 0 && id.is_none() => {
                phase = OperationPhase::PublishingFindings;
            }
            Some("newLineThread" | "newFileThread" | "reply")
                if phase == OperationPhase::PublishingFindings =>
            {
                if id.is_none_or(|id| id.is_empty() || !findings.insert(id)) {
                    return false;
                }
            }
            Some("verifyFindings")
                if phase == OperationPhase::PublishingFindings && id.is_none() =>
            {
                phase = OperationPhase::FindingsVerified;
            }
            Some("approveReview") if phase == OperationPhase::FindingsVerified && id.is_none() => {
                phase = OperationPhase::ApprovalRecorded;
            }
            Some("completeReview")
                if matches!(
                    phase,
                    OperationPhase::FindingsVerified | OperationPhase::ApprovalRecorded
                ) && id.is_none() =>
            {
                phase = OperationPhase::ReviewCompleted;
            }
            Some("reopenThread") if phase == OperationPhase::ReviewCompleted => {
                if id.is_none_or(|id| !findings.contains(id) || !reopen.insert(id)) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    phase == OperationPhase::ReviewCompleted
}

/// Builds the schema-1 operation record shared with PowerShell readers.
fn operation(name: &str, finding: Option<&str>, anchor: &Value, downgraded: bool) -> Value {
    json!({"operation": name, "findingId": finding, "outcome": "pending", "anchor": anchor,
        "wasDowngraded": downgraded, "threadId": null, "commentId": null, "message": null})
}

/// Validates the complete plan before any provider effect.
fn validate_plan(plan: &[Value]) -> Result<(), AppError> {
    let mut ids = HashSet::new();
    let mut replies = false;
    for entry in plan {
        let name = entry["operation"].as_str().unwrap_or_default();
        let id = entry["findingId"].as_str().unwrap_or_default();
        let body = entry["body"].as_str().unwrap_or_default();
        if !matches!(name, "newLineThread" | "newFileThread" | "reply")
            || id.trim().is_empty()
            || body.trim().is_empty()
            || !ids.insert(id)
            || entry.get("requiresReopen").is_some_and(|v| !v.is_boolean())
            || entry.get("wasDowngraded").is_some_and(|v| !v.is_boolean())
        {
            return Err(fail("invalidInput", "Malformed publication plan"));
        }
        if name == "reply" {
            replies = true;
            if entry["threadId"].as_str().is_none_or(str::is_empty) {
                return Err(fail("invalidInput", "Reply requires a thread identifier"));
            }
        } else {
            if replies
                || entry["requiresReopen"] == true
                || entry["anchor"]["path"].as_str().is_none_or(str::is_empty)
                || (name == "newLineThread"
                    && entry["anchor"]["endLine"]
                        .as_u64()
                        .is_none_or(|line| line == 0))
                || (name == "newLineThread"
                    && entry["anchor"].get("startLine").is_some_and(|start| {
                        !start.is_null()
                            && start.as_u64().is_none_or(|start| {
                                start == 0
                                    || entry["anchor"]["endLine"]
                                        .as_u64()
                                        .is_none_or(|end| start > end)
                            })
                    }))
            {
                return Err(fail("invalidInput", "Invalid anchor or publication order"));
            }
        }
    }
    Ok(())
}

/// Checks local transaction inputs before any authorized preflight mutation.
pub(crate) fn preflight(target: &Value, state: &Value, plan: &[Value]) -> Result<(), AppError> {
    if !valid_target(target) || !state.is_object() {
        return Err(fail("invalidInput", "Invalid publication target or state"));
    }
    validate_plan(plan)
}

/// Distinguishes proven provider refusals from unknown write outcomes.
fn refusal(error: &AppError, platform: &Value) -> bool {
    let kind = classification(error);
    matches!(kind, "permissionDenied" | "authentication")
        || (platform == "gitHub" && kind == "explicitRateLimited")
        || (platform == "azureDevOps" && kind == "rateLimited")
}

/// Marks storage failures so cleanup cannot follow a failed durable write.
fn save(journal: &mut dyn Journal, result: &Value) -> Result<(), AppError> {
    journal.write(result).map_err(|error| {
        fail(
            "journalFailure",
            format!("Cannot persist publication record: {error}"),
        )
    })
}

/// Persists the in-flight effect before dispatch and its outcome afterward.
fn effect(
    journal: &mut dyn Journal,
    result: &mut Value,
    index: Option<usize>,
    name: &str,
    read_only: bool,
    action: impl FnOnce() -> Result<Value, AppError>,
) -> Result<Value, AppError> {
    let intent = json!({"operation": name, "index": index});
    result["inFlight"] = intent.clone();
    // Schema-1 readers that do not know `inFlight` still refuse replay while
    // the provider call may have reached the service.
    if !read_only && let Some(index) = index {
        result["operations"][index]["outcome"] = json!("ambiguous");
    }
    if let Err(error) = save(journal, result) {
        result["inFlight"] = intent;
        return Err(error);
    }
    match action() {
        Ok(receipt) => {
            if let Some(index) = index {
                result["operations"][index]["outcome"] = json!("succeeded");
                result["operations"][index]["receipt"] = receipt.clone();
                if name == "startReview" {
                    result["reviewId"] = receipt["reviewId"].clone();
                } else if name == "completeReview" {
                    result["reviewUrl"] = receipt["url"].clone();
                } else if matches!(name, "newLineThread" | "newFileThread" | "reply") {
                    result["operations"][index]["threadId"] = receipt["threadId"].clone();
                    result["operations"][index]["commentId"] = receipt["commentId"].clone();
                }
            }
            if name == "verifyFindings" {
                result["preCompletionVerification"] = receipt.clone();
            }
            if name == "verify" {
                result["verification"] = receipt.clone();
            }
            result["inFlight"] = Value::Null;
            if let Err(error) = save(journal, result) {
                result["inFlight"] = intent;
                return Err(error);
            }
            Ok(receipt)
        }
        Err(error) => {
            if let Some(index) = index {
                let vote_may_exist = name == "completeReview"
                    && result["operations"].as_array().is_some_and(|operations| {
                        operations.iter().any(|op| {
                            op["operation"] == "approveReview"
                                && matches!(op["outcome"].as_str(), Some("succeeded" | "ambiguous"))
                        })
                    });
                result["operations"][index]["outcome"] = json!(if !vote_may_exist
                    && (read_only || refusal(&error, &result["platform"]))
                {
                    "failed"
                } else {
                    "ambiguous"
                });
                result["operations"][index]["message"] = json!(error.to_string());
            }
            result["inFlight"] = Value::Null;
            if let Err(error) = save(journal, result) {
                result["inFlight"] = intent;
                return Err(error);
            }
            Err(error)
        }
    }
}

/// Requires both endpoints when the approved line anchor covers a range.
fn line_start_matches(anchor: &Value, comment: &Value) -> bool {
    let Some(end) = anchor["endLine"].as_u64() else {
        return false;
    };
    let start = match anchor.get("startLine") {
        None | Some(Value::Null) => end,
        Some(start) => match start.as_u64() {
            Some(start) if start > 0 && start <= end => start,
            _ => return false,
        },
    };
    // Providers may omit the start for a single-line anchor, but a range
    // cannot be confirmed without evidence of both endpoints.
    match comment.get("startLine") {
        None | Some(Value::Null) => start == end,
        Some(actual) => actual.as_u64() == Some(start),
    }
}

/// Confirms accepted comments against receipts and provider-supported order.
///
/// Review-scoped readback preserves the complete plan order. Independent
/// threads only expose order within each thread; the journal records the
/// sequential dispatch order across them.
fn verify_comments(
    result: &Value,
    plan: &[Value],
    verification: &Value,
    published: bool,
) -> Result<(), AppError> {
    if verification["reviewId"] != result["reviewId"] {
        return Err(fail("service", "Verification names a different review"));
    }
    // Azure threads are independently visible; only comments within a thread
    // have a comparable readback order. The journal retains dispatch order.
    let independent_threads = verification["commentsOrder"] == "unavailable";
    let comments = verification["comments"]
        .as_array()
        .ok_or_else(|| fail("service", "Verification omitted comments"))?;
    let mut next_comment = 0;
    let mut matched = HashSet::new();
    let mut positions = HashMap::new();
    for (index, entry) in plan.iter().enumerate() {
        let record_index = index
            .checked_add(1)
            .expect("A plan index fits after the start operation");
        let record = &result["operations"][record_index];
        let comment_id = record["commentId"].as_str().unwrap_or_default();
        let thread_id = record["threadId"].as_str().unwrap_or_default();
        let found = comments
            .iter()
            .enumerate()
            .skip(if independent_threads { 0 } else { next_comment })
            .find(|(index, comment)| {
                if matched.contains(index)
                    || thread_id.is_empty()
                    || comment["threadId"].as_str() != Some(thread_id)
                {
                    false
                } else if !comment_id.is_empty() {
                    comment["id"].as_str() == Some(comment_id)
                } else {
                    comment["body"] == entry["body"]
                }
            })
            .ok_or_else(|| {
                fail(
                    "service",
                    format!("Missing or reordered finding {}", entry["findingId"]),
                )
            })?;
        // Other comments may appear between findings, but a finding cannot
        // satisfy an earlier plan entry after a later entry has been seen.
        next_comment = found
            .0
            .checked_add(1)
            .expect("A comment index fits within its collection");
        matched.insert(found.0);
        let comment = found.1;
        if independent_threads {
            let position = comment["threadCommentPosition"].as_u64();
            if let Some(previous) = positions.insert(thread_id, position) {
                let ordered = previous
                    .zip(position)
                    .is_some_and(|(previous, position)| previous < position);
                if !ordered {
                    return Err(fail(
                        "service",
                        "Provider thread comment order is missing or does not match",
                    ));
                }
            }
        }
        if comment["body"] != entry["body"] || (published && comment["isPublished"] != true) {
            return Err(fail("service", "Finding body or visibility does not match"));
        }
        let name = entry["operation"].as_str().unwrap_or_default();
        if name != "reply"
            && (comment["path"] != entry["anchor"]["path"]
                || (name == "newFileThread"
                    && (comment["subjectType"] != "FILE" || !comment["line"].is_null()))
                || (name == "newLineThread"
                    && (comment["subjectType"] != "LINE"
                        || comment["line"] != entry["anchor"]["endLine"]
                        || !line_start_matches(&entry["anchor"], comment)
                        || comment["side"].as_str().is_none_or(|side| {
                            side.is_empty()
                                || !side.eq_ignore_ascii_case(
                                    entry["anchor"]["side"].as_str().unwrap_or("right"),
                                )
                        }))))
        {
            return Err(fail("service", "Finding anchor does not match"));
        }
    }
    Ok(())
}

/// Confirms the completed review, verdict, approval and findings against readback.
fn verify_completed(
    result: &Value,
    plan: &[Value],
    summary: &str,
    approve: bool,
    approval_index: Option<usize>,
    verification: &Value,
) -> Result<(), AppError> {
    if verification["isPublished"] != true || verification["body"].as_str() != Some(summary) {
        return Err(fail(
            "service",
            "Published review or summary does not match",
        ));
    }
    if result["platform"] == "gitHub"
        && verification["state"] != if approve { "APPROVED" } else { "COMMENTED" }
    {
        return Err(fail(
            "service",
            "Published GitHub review verdict does not match",
        ));
    }
    if let Some(index) = approval_index {
        let receipt = &result["operations"][index]["receipt"];
        let approval = &verification["approval"];
        let matching_id = receipt["id"]
            .as_str()
            .zip(approval["id"].as_str())
            .is_some_and(|(expected, actual)| {
                !expected.is_empty() && expected.eq_ignore_ascii_case(actual)
            });
        if !matching_id
            || approval["vote"] != 10
            || !approval["isRequired"].is_boolean()
            || approval["isRequired"] != receipt["isRequired"]
        {
            return Err(fail(
                "service",
                "Published Azure DevOps approval does not match",
            ));
        }
    }
    verify_comments(result, plan, verification, true)
}

/// Returns the compact schema-1 outcome consumed by the command layer.
fn outcome(result: &Value, path: &str) -> Value {
    let operations = result["operations"]
        .as_array()
        .expect("Constructed operations");
    let findings = |op: &&Value| {
        matches!(
            op["operation"].as_str(),
            Some("newLineThread" | "newFileThread" | "reply")
        )
    };
    json!({"schemaVersion": 1, "status": result["status"], "message": result["message"],
        "platform": result["platform"], "pullRequestUrl": result["pullRequestUrl"],
        "reviewId": result["reviewId"], "reviewUrl": result["reviewUrl"], "resultPath": path,
        "publishedFindingCount": operations.iter().filter(findings).filter(|op| op["outcome"] == "succeeded").count(),
        "failedOperationCount": operations.iter().filter(|op| op["outcome"] == "failed").count(),
        "ambiguousOperationCount": operations.iter().filter(|op| op["outcome"] == "ambiguous").count(),
        "downgradedAnchorCount": operations.iter().filter(findings).filter(|op| op["wasDowngraded"] == true).count()})
}

/// Executes the ordered transaction; all remote operations have durable intent.
#[expect(
    clippy::too_many_arguments,
    reason = "The in-memory journal boundary mirrors the composition transaction inputs"
)]
fn execute_inner(
    journal: &mut dyn Journal,
    effects: &mut dyn Effects,
    target: &Value,
    state: &Value,
    plan: &[Value],
    summary: &str,
    approve: bool,
    now: &str,
    path: &str,
) -> Result<Value, AppError> {
    preflight(target, state, plan)?;
    let mut operations = vec![operation("startReview", None, &Value::Null, false)];
    for entry in plan {
        operations.push(operation(
            entry["operation"].as_str().unwrap_or_default(),
            entry["findingId"].as_str(),
            &entry["anchor"],
            entry["wasDowngraded"] == true,
        ));
    }
    let verify_index = operations.len();
    operations.push(operation("verifyFindings", None, &Value::Null, false));
    let approval_index = if approve && target["platform"] == "azureDevOps" {
        let index = operations.len();
        operations.push(operation("approveReview", None, &Value::Null, false));
        Some(index)
    } else {
        None
    };
    let complete_index = operations.len();
    operations.push(operation("completeReview", None, &Value::Null, false));
    for entry in plan.iter().filter(|entry| entry["requiresReopen"] == true) {
        operations.push(operation(
            "reopenThread",
            entry["findingId"].as_str(),
            &Value::Null,
            false,
        ));
    }
    let mut result = json!({"schemaVersion": 1, "startedUtc": now, "platform": target["platform"],
        "target": target, "pullRequestUrl": target["webUrl"], "sourceCommit": state["sourceCommit"],
        "mergeBaseCommit": state["mergeBaseCommit"], "status": "inProgress", "reviewId": null,
        "reviewUrl": null, "draftDiscarded": false, "draftDiscardMessage": null, "message": null,
        "operations": operations, "preCompletionVerification": null, "verification": null, "inFlight": null});
    save(journal, &result)?;
    let mut started = false;
    let mut submitted = false;
    let run = (|| -> Result<(), AppError> {
        effect(journal, &mut result, Some(0), "startReview", false, || {
            let receipt = effects.start()?;
            if receipt["reviewId"].as_str().is_none_or(str::is_empty) {
                return Err(fail(
                    "ambiguousWrite",
                    "Start returned no review identifier",
                ));
            }
            Ok(receipt)
        })?;
        started = true;
        for (index, entry) in plan.iter().enumerate() {
            let name = entry["operation"].as_str().unwrap_or_default();
            let record_index = index
                .checked_add(1)
                .expect("A plan index fits after the start operation");
            effect(
                journal,
                &mut result,
                Some(record_index),
                name,
                false,
                || {
                    let receipt = effects.add(entry)?;
                    if receipt["threadId"].as_str().is_none_or(str::is_empty)
                        || (name == "reply" && receipt["threadId"] != entry["threadId"])
                    {
                        return Err(fail(
                            "ambiguousWrite",
                            "Finding returned no matching thread identifier",
                        ));
                    }
                    Ok(receipt)
                },
            )?;
        }
        let verification = effect(
            journal,
            &mut result,
            Some(verify_index),
            "verifyFindings",
            true,
            || effects.verify_findings(),
        )?;
        let checked = if target["platform"] == "gitHub" && verification["state"] != "PENDING" {
            Err(fail("service", "GitHub draft review is not pending"))
        } else {
            verify_comments(&result, plan, &verification, false)
        };
        if let Err(error) = checked {
            result["operations"][verify_index]["outcome"] = json!("failed");
            result["operations"][verify_index]["message"] = json!(error.to_string());
            save(journal, &result)?;
            return Err(error);
        }
        if let Some(index) = approval_index {
            // Legacy readers recognize an ambiguous completion as an obstacle.
            // Keep it ambiguous until summary completion is durably recorded,
            // because the vote is already an independent visible write.
            result["operations"][complete_index]["outcome"] = json!("ambiguous");
            effect(
                journal,
                &mut result,
                Some(index),
                "approveReview",
                false,
                || {
                    let receipt = effects.approve()?;
                    if receipt["id"].as_str().is_none_or(str::is_empty)
                        || receipt["vote"] != 10
                        || !receipt["isRequired"].is_boolean()
                    {
                        return Err(fail(
                            "ambiguousWrite",
                            "Approval returned an incomplete receipt",
                        ));
                    }
                    Ok(receipt)
                },
            )?;
        }
        let review_id = result["reviewId"].clone();
        effect(
            journal,
            &mut result,
            Some(complete_index),
            "completeReview",
            false,
            || {
                let receipt = effects.complete(summary, approve)?;
                if receipt["reviewId"] != review_id
                    || receipt["url"].as_str().is_none_or(str::is_empty)
                {
                    return Err(fail(
                        "ambiguousWrite",
                        "Completion returned an incomplete or mismatched receipt",
                    ));
                }
                Ok(receipt)
            },
        )?;
        submitted = true;
        let verification = effect(journal, &mut result, None, "verify", true, || {
            effects.verify()
        })?;
        verify_completed(
            &result,
            plan,
            summary,
            approve,
            approval_index,
            &verification,
        )?;
        Ok(())
    })();

    if let Err(error) = run {
        // Never dispatch cleanup after a journal failure: its prior intent is the
        // only durable evidence, and another effect would overwrite that evidence.
        if classification(&error) == "journalFailure" || !result["inFlight"].is_null() {
            return Err(error);
        }
        if approval_index.is_some_and(|index| result["operations"][index]["outcome"] != "failed")
            && !submitted
        {
            result["operations"][complete_index]["outcome"] = json!("ambiguous");
        }
        result["status"] = json!("failed");
        result["message"] = json!(error.to_string());
        save(journal, &result)?;
        let completion = &result["operations"][complete_index]["outcome"];
        if started
            && !submitted
            && target["platform"] == "gitHub"
            && completion != "ambiguous"
            && completion != "succeeded"
        {
            result["inFlight"] = json!({"operation": "discard", "index": null});
            save(journal, &result)?;
            match effects.discard() {
                Ok(()) => result["draftDiscarded"] = json!(true),
                Err(discard_error) => {
                    result["draftDiscardMessage"] = json!(discard_error.to_string());
                }
            }
            result["inFlight"] = Value::Null;
            save(journal, &result)?;
        }
        return Err(error);
    }

    let mut failures = Vec::new();
    let follow_start = complete_index
        .checked_add(1)
        .expect("Completion precedes follow-up records");
    for (offset, entry) in plan
        .iter()
        .filter(|entry| entry["requiresReopen"] == true)
        .enumerate()
    {
        let follow_index = follow_start
            .checked_add(offset)
            .expect("Follow-up indices fit within the operation collection");
        let thread = entry["threadId"].as_str().unwrap_or_default();
        result["operations"][follow_index]["threadId"] = json!(thread);
        let reopened = effect(
            journal,
            &mut result,
            Some(follow_index),
            "reopenThread",
            false,
            || effects.reopen(thread).map(|()| Value::Null),
        );
        if let Err(error) = reopened {
            if classification(&error) == "journalFailure" || !result["inFlight"].is_null() {
                return Err(error);
            }
            failures.push(format!("thread '{thread}': {error}"));
        }
    }
    if failures.is_empty() {
        result["status"] = json!("succeeded");
    } else {
        result["status"] = json!("publishedWithFollowUpFailure");
        result["message"] = json!(format!(
            "The review is published and verified; reconcile these threads without publishing again: {}",
            failures.join("; ")
        ));
    }
    save(journal, &result)?;
    Ok(outcome(&result, path))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{Effects, Journal, check_prior_inner, execute_inner};
    use crate::classification;
    use crate::error::fail;
    use ohno::AppError;
    use serde_json::{Value, json, to_string, to_vec};

    /// Models complete durable records; raw bytes take precedence on reads.
    ///
    /// A refused replacement leaves both the stored record and history intact.
    #[derive(Default)]
    struct Memory {
        record: Option<Value>,
        raw: Option<Vec<u8>>,
        history: Vec<Value>,
        writes: usize,
        fail_at: Option<usize>,
        fail_verification_record: bool,
        failure_triggered: bool,
    }

    impl Journal for Memory {
        fn read(&mut self) -> Result<Option<Vec<u8>>, AppError> {
            if let Some(raw) = &self.raw {
                return Ok(Some(raw.clone()));
            }
            Ok(self.record.as_ref().map(|value| to_vec(value).unwrap()))
        }

        fn write(&mut self, value: &Value) -> Result<(), AppError> {
            self.writes = self
                .writes
                .checked_add(1)
                .expect("Test writes fit in usize");
            let failed_verification = self.fail_verification_record
                && value["operations"].as_array().is_some_and(|operations| {
                    operations.iter().any(|operation| {
                        operation["operation"] == "verifyFindings"
                            && operation["outcome"] == "failed"
                    })
                });
            if self.fail_at == Some(self.writes) || failed_verification {
                self.failure_triggered = true;
                return Err(fail("io", "Journal refused replacement"));
            }
            self.record = Some(value.clone());
            self.history.push(value.clone());
            Ok(())
        }
    }

    /// Models provider effects and controlled deviations in their readback.
    #[derive(Default)]
    struct Provider {
        calls: Vec<String>,
        failure: Option<(&'static str, &'static str)>,
        bad_pre: bool,
        bad_post: bool,
        bad_pre_state: bool,
        bad_post_state: bool,
        approved: bool,
        bad_approval: Option<&'static str>,
        two_findings: bool,
    }

    impl Provider {
        fn call(&mut self, name: &str) -> Result<(), AppError> {
            self.calls.push(name.to_owned());
            if let Some((operation, kind)) = self.failure
                && operation == name
            {
                return Err(fail(kind, "Provider failure"));
            }
            Ok(())
        }

        /// Matches `plan()` and effect receipts unless a scenario corrupts evidence.
        fn verification(&self, published: bool) -> Value {
            let state = if !published {
                if self.bad_pre_state {
                    "COMMENTED"
                } else {
                    "PENDING"
                }
            } else if self.bad_post_state {
                if self.approved {
                    "COMMENTED"
                } else {
                    "APPROVED"
                }
            } else if self.approved {
                "APPROVED"
            } else {
                "COMMENTED"
            };
            let mut approval = json!({"id":"reviewer", "vote":10, "isRequired":true});
            match self.bad_approval {
                Some("missing") => approval = Value::Null,
                Some("id") => approval["id"] = json!("other"),
                Some("vote") => approval["vote"] = json!(0),
                Some("required") => approval["isRequired"] = json!(false),
                _ => {}
            }
            let mut verification = json!({"reviewId": "review", "state": state, "isPublished": published, "body": "summary", "approval": approval, "comments": [
                {"id": "comment", "threadId": "thread", "body": if self.bad_pre && !published || self.bad_post && published { "wrong" } else { "finding" },
                 "isPublished": published, "path": "src/file", "subjectType": "LINE",
                 "line": 7, "startLine": null, "side": "RIGHT"}
            ]});
            if self.two_findings {
                let mut second = verification["comments"][0].clone();
                second["id"] = json!("comment2");
                second["threadId"] = json!("thread2");
                verification["comments"]
                    .as_array_mut()
                    .unwrap()
                    .push(second);
                verification["commentsOrder"] = json!("unavailable");
            }
            verification
        }
    }

    impl Effects for Provider {
        fn start(&mut self) -> Result<Value, AppError> {
            self.call("start")?;
            Ok(json!({"reviewId": "review"}))
        }
        fn add(&mut self, _: &Value) -> Result<Value, AppError> {
            let second = self.two_findings && self.calls.iter().any(|call| call == "add");
            self.call("add")?;
            Ok(if second {
                json!({"threadId": "thread2", "commentId": "comment2"})
            } else {
                json!({"threadId": "thread", "commentId": "comment"})
            })
        }
        fn verify_findings(&mut self) -> Result<Value, AppError> {
            self.call("verify_findings")?;
            Ok(self.verification(false))
        }
        fn approve(&mut self) -> Result<Value, AppError> {
            self.call("approve")?;
            Ok(json!({"id":"reviewer", "vote":10, "isRequired":true}))
        }
        fn complete(&mut self, _: &str, approve: bool) -> Result<Value, AppError> {
            self.call("complete")?;
            self.approved = approve;
            Ok(json!({"reviewId": "review", "url": "url"}))
        }
        fn verify(&mut self) -> Result<Value, AppError> {
            self.call("verify")?;
            Ok(self.verification(true))
        }
        fn reopen(&mut self, _: &str) -> Result<(), AppError> {
            self.call("reopen")
        }
        fn discard(&mut self) -> Result<(), AppError> {
            self.call("discard")
        }
    }

    fn target(platform: &str) -> Value {
        json!({"platform": platform, "collectionUrl": "https://example.test/collection", "organization": "org", "project": "project",
            "repository": "repo", "pullRequestId": 1, "webUrl": "url"})
    }

    fn plan() -> Vec<Value> {
        vec![
            json!({"operation": "newLineThread", "findingId": "f", "body": "finding",
            "anchor": {"path": "src/file", "endLine": 7, "side": "right"},
            "requiresReopen": false, "wasDowngraded": false}),
        ]
    }

    fn run(
        journal: &mut Memory,
        effects: &mut Provider,
        platform: &str,
        approve: bool,
    ) -> Result<Value, AppError> {
        execute_inner(
            journal,
            effects,
            &target(platform),
            &json!({"sourceCommit":"s", "mergeBaseCommit":"b"}),
            &plan(),
            "summary",
            approve,
            "2026-01-01T00:00:00Z",
            "publication-result.json",
        )
    }

    #[test]
    fn publishes_in_order_and_checks_both_readbacks() {
        for platform in ["gitHub", "azureDevOps"] {
            let mut journal = Memory::default();
            let mut provider = Provider::default();
            let result = run(&mut journal, &mut provider, platform, true).unwrap();
            assert_eq!(result["status"], "succeeded");
            assert_eq!(result["publishedFindingCount"], 1);
            assert_eq!(result["resultPath"], "publication-result.json");
            let expected = if platform == "gitHub" {
                vec!["start", "add", "verify_findings", "complete", "verify"]
            } else {
                vec![
                    "start",
                    "add",
                    "verify_findings",
                    "approve",
                    "complete",
                    "verify",
                ]
            };
            assert_eq!(provider.calls, expected);
            _ = check_prior_inner(&mut journal, &target(platform)).unwrap_err();
        }
    }

    #[test]
    fn failures_do_not_submit_or_discard_an_ambiguous_submission() {
        for (stage, kind, expected_discard) in [
            ("add", "service", true),
            ("verify_findings", "service", true),
            ("complete", "ambiguousWrite", false),
            ("complete", "permissionDenied", true),
            ("verify", "service", false),
        ] {
            let mut journal = Memory::default();
            let mut provider = Provider {
                failure: Some((stage, kind)),
                ..Provider::default()
            };
            _ = run(&mut journal, &mut provider, "gitHub", false).unwrap_err();
            assert_eq!(
                provider.calls.iter().any(|call| call == "discard"),
                expected_discard,
                "{stage}"
            );
            _ = check_prior_inner(&mut journal, &target("gitHub")).unwrap_err();
        }
    }

    #[test]
    fn approval_vote_remains_visible_when_ado_summary_fails() {
        for kind in ["permissionDenied", "ambiguousWrite"] {
            let mut journal = Memory::default();
            let mut provider = Provider {
                failure: Some(("complete", kind)),
                ..Provider::default()
            };
            _ = run(&mut journal, &mut provider, "azureDevOps", true).unwrap_err();
            let record = journal.record.as_ref().unwrap();
            let ops = record["operations"].as_array().unwrap();
            assert_eq!(
                ops.iter()
                    .find(|op| op["operation"] == "approveReview")
                    .unwrap()["outcome"],
                "succeeded"
            );
            assert_eq!(
                ops.iter()
                    .find(|op| op["operation"] == "completeReview")
                    .unwrap()["outcome"],
                "ambiguous"
            );
            assert!(journal.history.iter().all(|version| {
                let ops = version["operations"].as_array().unwrap();
                !ops.iter()
                    .any(|op| op["operation"] == "approveReview" && op["outcome"] == "succeeded")
                    || ops.iter().any(|op| {
                        op["operation"] == "completeReview"
                            && matches!(op["outcome"].as_str(), Some("ambiguous" | "succeeded"))
                    })
            }));
            _ = check_prior_inner(&mut journal, &target("azureDevOps")).unwrap_err();
        }
    }

    #[test]
    fn legacy_pending_and_corrupt_records_never_authorize_replay() {
        let prior = json!({"schemaVersion": 1, "target": target("gitHub"), "status": "inProgress",
        "operations": [
            super::operation("startReview", None, &Value::Null, false),
            super::operation("verifyFindings", None, &Value::Null, false),
            super::operation("completeReview", None, &Value::Null, false)
        ]});
        for record in [
            prior.clone(),
            json!({"schemaVersion": 1, "target": target("gitHub"), "status":"failed", "operations": []}),
            json!({"schemaVersion": 2, "target": target("gitHub"), "status":"failed", "operations": []}),
        ] {
            let mut journal = Memory {
                record: Some(record),
                ..Memory::default()
            };
            _ = check_prior_inner(&mut journal, &target("gitHub")).unwrap_err();
        }
        let mut prior = prior;
        prior["status"] = json!("failed");
        prior["operations"][0]["outcome"] = json!("failed");
        let mut journal = Memory {
            record: Some(prior),
            ..Memory::default()
        };
        check_prior_inner(&mut journal, &target("gitHub")).unwrap();
    }

    #[test]
    fn journal_operation_order_requires_completed_review_before_follow_up() {
        let start = super::operation("startReview", None, &Value::Null, false);
        let finding = super::operation("reply", Some("f"), &Value::Null, false);
        let verify = super::operation("verifyFindings", None, &Value::Null, false);
        let approve = super::operation("approveReview", None, &Value::Null, false);
        let complete = super::operation("completeReview", None, &Value::Null, false);
        let reopen = super::operation("reopenThread", Some("f"), &Value::Null, false);

        assert!(super::valid_operations(&[
            start.clone(),
            finding.clone(),
            verify.clone(),
            complete.clone(),
            reopen.clone()
        ]));
        assert!(super::valid_operations(&[
            start.clone(),
            finding.clone(),
            verify.clone(),
            approve.clone(),
            complete.clone(),
            reopen.clone()
        ]));
        assert!(!super::valid_operations(&[
            start.clone(),
            finding.clone(),
            verify.clone(),
            reopen,
            complete.clone()
        ]));
        assert!(!super::valid_operations(&[
            start, finding, approve, verify, complete
        ]));
    }

    #[test]
    fn verification_rejects_mismatched_identity_body_visibility_and_anchor() {
        let result = json!({"reviewId": "review", "operations": [
            {"operation": "startReview"},
            {"threadId": "thread", "commentId": "comment"}
        ]});
        let provider = Provider::default();
        let original = provider.verification(true);
        super::verify_comments(&result, &plan(), &original, true).unwrap();
        for (field, wrong) in [
            ("id", json!("other")),
            ("threadId", json!("other")),
            ("threadId", json!("")),
            ("threadId", Value::Null),
            ("body", json!("other")),
            ("isPublished", json!(false)),
            ("path", json!("other")),
            ("subjectType", json!("FILE")),
            ("line", json!(8)),
            ("startLine", json!(3)),
            ("side", json!("left")),
            ("side", json!("")),
            ("side", Value::Null),
        ] {
            let mut bad = original.clone();
            bad["comments"][0][field] = wrong;
            _ = super::verify_comments(&result, &plan(), &bad, true).expect_err(field);
        }
        let mut bad = original;
        bad["reviewId"] = json!("other");
        _ = super::verify_comments(&result, &plan(), &bad, true).unwrap_err();
    }

    #[test]
    fn line_readback_requires_actual_thread_and_side_at_both_stages() {
        let result = json!({"reviewId": "review", "operations": [
            {"operation": "startReview"},
            {"threadId": "thread", "commentId": "comment"}
        ]});
        for published in [false, true] {
            let original = Provider::default().verification(published);
            super::verify_comments(&result, &plan(), &original, published).unwrap();
            for field in ["threadId", "side"] {
                for missing in [Value::Null, json!("")] {
                    let mut bad = original.clone();
                    bad["comments"][0][field] = missing;
                    _ = super::verify_comments(&result, &plan(), &bad, published).unwrap_err();
                }
                let mut absent = original.clone();
                absent["comments"][0].as_object_mut().unwrap().remove(field);
                _ = super::verify_comments(&result, &plan(), &absent, published).unwrap_err();
            }
            let mut left_plan = plan();
            left_plan[0]["anchor"]["side"] = json!("left");
            let mut left = original;
            left["comments"][0]["side"] = json!("LEFT");
            super::verify_comments(&result, &left_plan, &left, published).unwrap();
        }
    }

    #[test]
    fn line_readback_requires_range_start_but_allows_single_line_omission() {
        let result = json!({"reviewId": "review", "operations": [
            {"operation": "startReview"},
            {"threadId": "thread", "commentId": "comment"}
        ]});
        for published in [false, true] {
            let original = Provider::default().verification(published);
            super::verify_comments(&result, &plan(), &original, published).unwrap();
            let mut range = plan();
            range[0]["anchor"]["startLine"] = json!(3);
            _ = super::verify_comments(&result, &range, &original, published).unwrap_err();
            let mut absent = original.clone();
            absent["comments"][0]
                .as_object_mut()
                .unwrap()
                .remove("startLine");
            _ = super::verify_comments(&result, &range, &absent, published).unwrap_err();
            let mut matching = original.clone();
            matching["comments"][0]["startLine"] = json!(3);
            super::verify_comments(&result, &range, &matching, published).unwrap();
            matching["comments"][0]["startLine"] = json!(4);
            _ = super::verify_comments(&result, &range, &matching, published).unwrap_err();
        }
    }

    #[test]
    fn verification_requires_finding_order_before_and_after_completion() {
        let entries = vec![
            json!({"operation":"reply", "findingId":"first", "body":"First title\nBody",
                "threadId":"first-thread"}),
            json!({"operation":"reply", "findingId":"second", "body":"Second title\nBody",
                "threadId":"second-thread"}),
        ];
        let result = json!({"reviewId":"review", "operations":[
            {"operation":"startReview"},
            {"threadId":"first-thread", "commentId":"first-comment"},
            {"threadId":"second-thread", "commentId":"second-comment"}
        ]});
        for published in [false, true] {
            let first = json!({"id":"first-comment", "threadId":"first-thread", "body":"First title\nBody",
                "isPublished":published});
            let second = json!({"id":"second-comment", "threadId":"second-thread", "body":"Second title\nBody",
                "isPublished":published});
            let verification = json!({"reviewId":"review", "comments":[first, second]});
            super::verify_comments(&result, &entries, &verification, published).unwrap();
            let mut unavailable = verification.clone();
            unavailable["commentsOrder"] = json!("unavailable");
            unavailable["comments"].as_array_mut().unwrap().reverse();
            super::verify_comments(&result, &entries, &unavailable, published).unwrap();
            let mut with_extra = verification.clone();
            with_extra["comments"]
                .as_array_mut()
                .unwrap()
                .insert(1, json!({"id":"unrelated", "body":"other"}));
            super::verify_comments(&result, &entries, &with_extra, published).unwrap();
            let mut reordered = verification;
            let mut missing_thread = reordered.clone();
            missing_thread["comments"][0]
                .as_object_mut()
                .unwrap()
                .remove("threadId");
            _ = super::verify_comments(&result, &entries, &missing_thread, published).unwrap_err();
            let mut empty_thread = reordered.clone();
            empty_thread["comments"][0]["threadId"] = json!("");
            _ = super::verify_comments(&result, &entries, &empty_thread, published).unwrap_err();
            reordered["comments"].as_array_mut().unwrap().reverse();
            _ = super::verify_comments(&result, &entries, &reordered, published).unwrap_err();
        }
    }

    #[test]
    fn ado_uses_provider_comment_positions_within_each_thread() {
        let entries = [
            json!({"operation":"reply", "findingId":"first", "body":"first", "threadId":"thread"}),
            json!({"operation":"reply", "findingId":"second", "body":"second", "threadId":"thread"}),
        ];
        let result = json!({"reviewId":"review", "operations":[
            {"operation":"startReview"},
            {"threadId":"thread", "commentId":"first"},
            {"threadId":"thread", "commentId":"second"}
        ]});
        for published in [false, true] {
            let verification = json!({"reviewId":"review", "commentsOrder":"unavailable", "comments":[
                {"id":"first", "threadId":"thread", "body":"first", "isPublished":published, "threadCommentPosition":2},
                {"id":"second", "threadId":"thread", "body":"second", "isPublished":published, "threadCommentPosition":4}
            ]});
            super::verify_comments(&result, &entries, &verification, published).unwrap();
            let mut bad = verification.clone();
            bad["comments"][0]["threadCommentPosition"] = json!(5);
            _ = super::verify_comments(&result, &entries, &bad, published).unwrap_err();
            let mut missing = verification;
            missing["comments"][0]["threadCommentPosition"] = Value::Null;
            _ = super::verify_comments(&result, &entries, &missing, published).unwrap_err();
        }
    }

    #[test]
    fn ado_multiple_threads_are_dispatched_and_verified() {
        let mut entries = plan();
        let mut second = entries[0].clone();
        second["findingId"] = json!("second");
        entries.push(second);
        let mut journal = Memory::default();
        let mut provider = Provider {
            two_findings: true,
            ..Provider::default()
        };
        let outcome = execute_inner(
            &mut journal,
            &mut provider,
            &target("azureDevOps"),
            &json!({}),
            &entries,
            "summary",
            false,
            "2026-01-01T00:00:00Z",
            "publication-result.json",
        )
        .unwrap();
        assert_eq!(outcome["status"], "succeeded");
        assert_eq!(outcome["publishedFindingCount"], 2);
        assert_eq!(
            provider.calls,
            [
                "start",
                "add",
                "add",
                "verify_findings",
                "complete",
                "verify"
            ]
        );
        let records = journal.record.as_ref().unwrap()["operations"]
            .as_array()
            .unwrap();
        assert_eq!(records[1]["threadId"], "thread");
        assert_eq!(records[2]["threadId"], "thread2");
    }

    #[test]
    fn preflight_refuses_invalid_plan_before_any_effect() {
        let valid = plan();
        super::preflight(&target("gitHub"), &json!({}), &valid).unwrap();
        let mut bad = valid.clone();
        bad[0]["body"] = json!("");
        _ = super::preflight(&target("gitHub"), &json!({}), &bad).unwrap_err();
        for invalid_start in [json!(0), json!(8), json!(-1), json!("3")] {
            let mut bad = valid.clone();
            bad[0]["anchor"]["startLine"] = invalid_start;
            _ = super::preflight(&target("gitHub"), &json!({}), &bad).unwrap_err();
        }
        let mut out_of_order = vec![json!({"operation":"reply", "findingId":"reply",
            "body":"reply", "threadId":"thread"})];
        out_of_order.extend(valid);
        _ = super::preflight(&target("gitHub"), &json!({}), &out_of_order).unwrap_err();
        let mut multiple = plan();
        let mut second = plan()[0].clone();
        second["findingId"] = json!("second");
        multiple.push(second);
        super::preflight(&target("azureDevOps"), &json!({}), &multiple).unwrap();
        _ = super::preflight(&json!({}), &json!({}), &plan()).unwrap_err();
    }

    #[test]
    fn duplicate_keys_and_distinct_collection_never_replace_prior_evidence() {
        let mut start = super::operation("startReview", None, &Value::Null, false);
        start["outcome"] = json!("failed");
        let previous = json!({"schemaVersion": 1, "target": target("azureDevOps"), "status": "failed",
            "operations": [start,
                super::operation("verifyFindings", None, &Value::Null, false),
                super::operation("completeReview", None, &Value::Null, false)]});
        let encoded = to_string(&previous).unwrap();
        for duplicate in [
            encoded.replace(
                "\"status\":\"failed\"",
                "\"status\":\"inProgress\",\"status\":\"failed\"",
            ),
            encoded.replace(
                "\"outcome\":\"failed\"",
                "\"outcome\":\"ambiguous\",\"outcome\":\"failed\"",
            ),
            encoded.replace(
                "\"schemaVersion\":1",
                "\"SCHEMAVERSION\":1,\"schemaVersion\":1",
            ),
        ] {
            let mut journal = Memory {
                raw: Some(duplicate.into_bytes()),
                ..Memory::default()
            };
            _ = check_prior_inner(&mut journal, &target("azureDevOps")).unwrap_err();
        }
        let mut other = target("azureDevOps");
        other["collectionUrl"] = json!("https://other.example.test/collection");
        let mut journal = Memory {
            record: Some(previous),
            ..Memory::default()
        };
        _ = check_prior_inner(&mut journal, &other).unwrap_err();
    }

    #[test]
    fn journal_failures_never_authorize_another_effect_or_replay() {
        let mut baseline = Memory::default();
        run(&mut baseline, &mut Provider::default(), "gitHub", false).unwrap();
        // The initial write has no prior record to protect; test every later write.
        for fail_at in 2..=baseline.writes {
            let mut journal = Memory {
                fail_at: Some(fail_at),
                ..Memory::default()
            };
            let mut provider = Provider::default();
            _ = run(&mut journal, &mut provider, "gitHub", false)
                .expect_err(&format!("write {fail_at} must fail"));
            assert!(journal.failure_triggered);
            assert!(
                !provider.calls.iter().any(|call| call == "discard"),
                "{fail_at}"
            );
            _ = check_prior_inner(&mut journal, &target("gitHub"))
                .expect_err(&format!("write {fail_at} must block replay"));
        }
    }

    #[test]
    fn failed_verification_record_write_prevents_draft_cleanup() {
        let mut journal = Memory {
            fail_verification_record: true,
            ..Memory::default()
        };
        let mut provider = Provider {
            bad_pre: true,
            ..Provider::default()
        };
        let error = run(&mut journal, &mut provider, "gitHub", false).unwrap_err();
        assert_eq!(classification(&error), "journalFailure");
        assert!(journal.failure_triggered);
        assert!(
            journal.record.as_ref().unwrap()["operations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|operation| operation["operation"] == "verifyFindings"
                    && operation["outcome"] == "succeeded")
        );
        assert_eq!(provider.calls, ["start", "add", "verify_findings"]);
        _ = check_prior_inner(&mut journal, &target("gitHub")).unwrap_err();
    }

    #[test]
    fn mismatched_readback_fails_before_or_after_completion() {
        for post in [false, true] {
            let mut journal = Memory::default();
            let mut provider = Provider {
                bad_pre: !post,
                bad_post: post,
                ..Provider::default()
            };
            _ = run(&mut journal, &mut provider, "gitHub", false).unwrap_err();
            assert_eq!(provider.calls.iter().any(|call| call == "complete"), post);
            assert_eq!(provider.calls.iter().any(|call| call == "discard"), !post);
        }
    }

    #[test]
    fn github_verdict_requires_pending_draft_and_requested_completed_state() {
        for approve in [false, true] {
            for post in [false, true] {
                let mut journal = Memory::default();
                let mut provider = Provider {
                    bad_pre_state: !post,
                    bad_post_state: post,
                    ..Provider::default()
                };
                _ = run(&mut journal, &mut provider, "gitHub", approve).unwrap_err();
                assert_eq!(provider.calls.iter().any(|call| call == "complete"), post);
                assert_eq!(provider.calls.iter().any(|call| call == "discard"), !post);
                assert_eq!(journal.record.as_ref().unwrap()["status"], "failed");
            }
        }
    }

    #[test]
    fn ado_approval_requires_readback_of_recorded_reviewer_vote() {
        for bad in ["missing", "id", "vote", "required"] {
            let mut journal = Memory::default();
            let mut provider = Provider {
                bad_approval: Some(bad),
                ..Provider::default()
            };
            _ = run(&mut journal, &mut provider, "azureDevOps", true).unwrap_err();
            assert!(provider.calls.iter().any(|call| call == "complete"));
            assert_eq!(journal.record.as_ref().unwrap()["status"], "failed");
            _ = check_prior_inner(&mut journal, &target("azureDevOps")).unwrap_err();
        }
    }

    #[test]
    fn follow_up_failure_is_returned_after_verified_publication() {
        let mut journal = Memory::default();
        let mut provider = Provider {
            failure: Some(("reopen", "service")),
            ..Provider::default()
        };
        let entries = [
            json!({"operation": "reply", "findingId": "reply", "body": "finding",
            "threadId": "thread", "requiresReopen": true}),
        ];
        let outcome = execute_inner(
            &mut journal,
            &mut provider,
            &target("gitHub"),
            &json!({}),
            &entries,
            "summary",
            false,
            "2026-01-01T00:00:00Z",
            "publication-result.json",
        )
        .unwrap();
        assert_eq!(outcome["status"], "publishedWithFollowUpFailure");
        assert_eq!(outcome["ambiguousOperationCount"], 1);
        assert_eq!(
            provider.calls,
            [
                "start",
                "add",
                "verify_findings",
                "complete",
                "verify",
                "reopen"
            ]
        );
        _ = check_prior_inner(&mut journal, &target("gitHub")).unwrap_err();
    }
}
