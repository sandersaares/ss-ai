//! Projects audit decisions into a capture-bound, publishable finding set.

use std::fs;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{evidence_hash, read_capture};
use ss_ai_runtime::{Clock, FileTransaction, Process, fail, read_bytes};

use crate::anchors::{canonical_path, resolve_anchor_in_diff};
use crate::audit::{last_decision, read_decisions};
use crate::findings::{read_artifact, validate_artifact};
use crate::workflow_plan::parse_json;

/// Builds and atomically publishes the current approved finding set.
///
/// The capture and repository must either both be supplied or both be absent.
///
/// # Errors
///
/// Rejects unresolved decisions, invalid inputs, and failed comparisons.
#[expect(
    clippy::module_name_repetitions,
    reason = "The entrypoint is named consistently with the workflow command"
)]
pub fn build_approved(
    findings_path: &Path,
    decisions_path: &Path,
    output_path: &Path,
    context_path: Option<&Path>,
    repository_path: Option<&Path>,
    clock: &dyn Clock,
    process: &dyn Process,
) -> Result<Value, AppError> {
    if context_path.is_some() && repository_path.is_none() {
        return Err(invalid("A refreshed capture also requires -RepositoryPath"));
    }
    if repository_path.is_some() && context_path.is_none() {
        return Err(invalid(
            "Checking the reviewed diff also requires -ContextPath",
        ));
    }
    let output_path = absolute(output_path)?;
    for input in [Some(findings_path), Some(decisions_path), context_path]
        .into_iter()
        .flatten()
    {
        reject_same_file(&output_path, input)?;
    }
    if let Some(context_path) = context_path {
        let mut sidecar = context_path.as_os_str().to_os_string();
        sidecar.push(".authorship.json");
        reject_same_file(&output_path, Path::new(&sidecar))?;
    }
    let artifact = read_artifact(findings_path, clock)?;
    if artifact.get("kind").and_then(Value::as_str) != Some("consolidated") {
        return Err(invalid("Approved findings require a consolidated artifact"));
    }
    // The audit module owns decision validation and last-decision semantics.
    let decisions = read_decisions(decisions_path, clock)?;
    let mut undecided = Vec::new();
    for finding in array(&artifact, "findings")? {
        let id = required_text(finding, "id")?;
        if matches!(last_decision(&decisions, id)?, None | Some("needs-audit")) {
            undecided.push(format!("'{id}'"));
        }
    }
    if !undecided.is_empty() {
        let last = undecided
            .pop()
            .ok_or_else(|| invalid("Missing finding identifier"))?;
        let names = if undecided.is_empty() {
            last
        } else {
            format!("{} or {last}", undecided.join(", "))
        };
        return Err(invalid(format!(
            "The audit recorded no decision for {names}. Every finding must be decided."
        )));
    }
    let context = context_path
        .map(|path| read_capture(path, clock))
        .transpose()?;
    let comparison = if let (Some(context), Some(repository)) = (&context, repository_path) {
        let pr = context
            .get("pullRequest")
            .ok_or_else(|| invalid("Missing pull request"))?;
        Some(ss_ai_git::compare(
            process,
            repository,
            required_text(pr, "reviewedMergeBaseCommit")?,
            required_text(pr, "reviewedSourceCommit")?,
        )?)
    } else {
        None
    };
    let mut approved = Vec::new();
    let mut unanchored = Vec::new();
    let (mut rejected, mut dropped, mut manual, mut automatic) =
        (0_usize, 0_usize, 0_usize, 0_usize);
    for finding in array(&artifact, "findings")? {
        let id = required_text(finding, "id")?;
        let decision = last_decision(&decisions, id)?
            .ok_or_else(|| invalid(format!("Finding '{id}' is undecided")))?;
        match decision {
            "needs-audit" => return Err(invalid(format!("Finding '{id}' needs audit"))),
            "rejected" => {
                rejected = rejected.saturating_add(1);
                continue;
            }
            "dropped" => {
                dropped = dropped.saturating_add(1);
                continue;
            }
            "approved" | "auto-approved" => {}
            _ => return Err(invalid("Invalid decision")),
        }
        if let Some(context) = &context {
            check_follow_up(context, finding)
                .map_err(|error| invalid(format!("Finding '{id}' cannot be published: {error}")))?;
        }
        // Follow-ups publish into the validated existing thread, not a changed-file anchor.
        // See skills/ss-review/docs/design.md, "Review publication".
        if finding.get("followUp").is_none_or(Value::is_null)
            && let Some(diff) = &comparison
        {
            let anchor = finding
                .get("anchor")
                .ok_or_else(|| invalid("Missing anchor"))?;
            if let Err(error) = resolve_anchor_in_diff(anchor, diff) {
                let path = required_text(anchor, "path")?;
                let path = canonical_path(path).unwrap_or_else(|_| path.to_owned());
                unanchored
                    .push(json!({"findingId": id, "path": path, "reason": error.to_string()}));
                continue;
            }
        }
        if decision == "approved" {
            manual = manual.saturating_add(1);
        } else {
            automatic = automatic.saturating_add(1);
        }
        // Preserve the approved source record, including the original anchor.
        approved.push(finding.clone());
    }
    let pr = context.as_ref().and_then(|value| value.get("pullRequest"));
    let source = pr
        .and_then(|value| value.get("reviewedSourceCommit"))
        .cloned()
        .unwrap_or(Value::Null);
    let base = pr
        .and_then(|value| value.get("reviewedMergeBaseCommit"))
        .cloned()
        .unwrap_or(Value::Null);
    let hash = context.as_ref().map(evidence_hash).transpose()?;
    let ids: Vec<_> = approved
        .iter()
        .filter_map(|finding| finding.get("id").cloned())
        .collect();
    let document = json!({"schemaVersion": 2,
        "target": context.as_ref().and_then(|value| value.get("target")).cloned().unwrap_or(Value::Null),
        "reviewedSourceCommit": source, "reviewedMergeBaseCommit": base,
        "reviewedContextHash": hash, "findings": approved});
    let transaction = FileTransaction::open(&output_path, clock)?;
    transaction.write_bytes(&serde_json::to_vec_pretty(&document)?)?;
    Ok(
        json!({"outputPath": output_path, "approved": ids.len(), "manuallyApproved": manual,
        "autoApproved": automatic, "rejected": rejected, "dropped": dropped,
        "unanchored": unanchored.len(), "unanchoredFinding": unanchored, "findingId": ids,
        "reviewedSourceCommit": source, "reviewedMergeBaseCommit": base}),
    )
}

/// Checks that an approved set is bound to the exact publication capture.
///
/// # Errors
///
/// Rejects malformed records and changed evidence.
#[expect(
    clippy::module_name_repetitions,
    reason = "The entrypoint is named consistently with the workflow command"
)]
pub fn check_approved(
    path: &Path,
    context_path: Option<&Path>,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let bytes = read_bytes(path, clock)?.ok_or_else(|| invalid("Approved set is missing"))?;
    let document = parse_json(&bytes)?;
    let context = context_path
        .map(|path| read_capture(path, clock))
        .transpose()?;
    check_approved_value(&document, context.as_ref())
}

/// Checks the exact approved and capture snapshots passed by a publisher.
///
/// # Errors
///
/// Rejects unreadable or malformed requests and stale approved sets.
pub fn check_approved_request_path(path: &Path, clock: &dyn Clock) -> Result<(), AppError> {
    let bytes = read_bytes(path, clock)?.ok_or_else(|| invalid("Freshness request is missing"))?;
    let request = parse_json(&bytes)?;
    let object = request
        .as_object()
        .ok_or_else(|| invalid("Invalid freshness request"))?;
    if object.len() != 2 || !object.contains_key("approved") || !object.contains_key("context") {
        return Err(invalid("Freshness request requires approved and context"));
    }
    let approved = object
        .get("approved")
        .ok_or_else(|| invalid("Missing approved set"))?;
    let context = object.get("context").filter(|value| !value.is_null());
    check_approved_value(approved, context)
}

/// Applies the same freshness rules to file reads and already-read snapshots.
fn check_approved_value(document: &Value, context: Option<&Value>) -> Result<(), AppError> {
    let object = document
        .as_object()
        .ok_or_else(|| invalid("Invalid approved set"))?;
    let fields = [
        "schemaVersion",
        "target",
        "reviewedSourceCommit",
        "reviewedMergeBaseCommit",
        "reviewedContextHash",
        "findings",
    ];
    if object.len() != fields.len()
        || fields.iter().any(|field| !object.contains_key(*field))
        || document.get("schemaVersion").and_then(Value::as_u64) != Some(2)
    {
        return Err(invalid("Invalid approved set schema"));
    }
    validate_artifact(
        &json!({"schemaVersion": 1, "kind": "consolidated", "findings": array(document, "findings")?}),
    )?;
    match context {
        None => {
            if fields[1..5]
                .iter()
                .any(|field| !document.get(*field).is_some_and(Value::is_null))
            {
                return Err(invalid("Capture-bound approved set requires its capture"));
            }
        }
        Some(context) => {
            if context.get("schemaVersion").and_then(Value::as_u64) != Some(3) {
                return Err(invalid("Unsupported capture schema version"));
            }
            let hash = evidence_hash(context)?;
            let pr = context
                .get("pullRequest")
                .ok_or_else(|| invalid("Missing pull request"))?;
            required_text(pr, "reviewedSourceCommit")?;
            required_text(pr, "reviewedMergeBaseCommit")?;
            if document.get("reviewedContextHash").and_then(Value::as_str) != Some(&hash)
                || document.get("target").unwrap_or(&Value::Null)
                    != context.get("target").unwrap_or(&Value::Null)
                || document.get("reviewedSourceCommit") != pr.get("reviewedSourceCommit")
                || document.get("reviewedMergeBaseCommit") != pr.get("reviewedMergeBaseCommit")
            {
                return Err(invalid("Approved set is not bound to this capture"));
            }
        }
    }
    Ok(())
}

/// Confirms a follow-up addresses the opening comment of a reply-capable thread.
fn check_follow_up(context: &Value, finding: &Value) -> Result<(), AppError> {
    let Some(follow_up) = finding.get("followUp").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let thread_id = required_text(follow_up, "threadId")?;
    let comment_id = required_text(follow_up, "commentId")?;
    let threads = match context.get("threads") {
        Some(Value::Array(threads)) => threads.as_slice(),
        Some(thread @ Value::Object(_)) => std::slice::from_ref(thread),
        _ => return Err(invalid("Missing or invalid threads array")),
    };
    let thread = threads
        .iter()
        .find(|thread| identifier_matches(thread.get("id"), thread_id))
        .ok_or_else(|| {
            invalid(format!(
                "Follow-up thread '{thread_id}' is absent from the capture"
            ))
        })?;
    if !identifier_matches(thread.get("openingCommentId"), comment_id)
        || thread.get("kind").and_then(Value::as_str) != Some("text")
    {
        return Err(invalid(
            "Follow-up does not name a reply-capable opening comment",
        ));
    }
    match context.get("platform").and_then(Value::as_str) {
        Some("gitHub") if !thread.get("context").is_none_or(Value::is_null) => Ok(()),
        Some("azureDevOps") => Ok(()),
        _ => Err(invalid("Follow-up thread cannot accept replies")),
    }
}

/// Provider identifiers may be strings or integral Azure identifiers.
fn identifier_matches(value: Option<&Value>, expected: &str) -> bool {
    match value {
        Some(Value::String(value)) => value == expected,
        Some(Value::Number(value)) if value.as_i64().is_some() || value.as_u64().is_some() => {
            value.to_string() == expected
        }
        _ => false,
    }
}

/// Avoids replacing the inputs used to derive the result, including path aliases.
fn reject_same_file(output: &Path, input: &Path) -> Result<(), AppError> {
    let input = absolute(input)?;
    let identity = |path: &Path| -> Option<PathBuf> {
        if let Ok(path) = fs::canonicalize(path) {
            return Some(path);
        }
        Some(
            fs::canonicalize(path.parent()?)
                .ok()?
                .join(path.file_name()?),
        )
    };
    if output == input
        || identity(output)
            .zip(identity(&input))
            .is_some_and(|(a, b)| a == b)
    {
        return Err(invalid("Output must not overwrite an input"));
    }
    Ok(())
}

/// Resolves paths without requiring their destination to exist.
fn absolute(path: &Path) -> Result<PathBuf, AppError> {
    Ok(std::path::absolute(path)
        .map_err(|error| ss_ai_runtime::caused_by("io", "Cannot resolve path", error))?)
}

/// Requires a nonempty string at a record boundary.
fn required_text<'a>(value: &'a Value, key: &str) -> Result<&'a str, AppError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid(format!("Missing or invalid {key}")))
}

/// Requires an explicit array instead of treating absent input as complete.
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, AppError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("Missing or invalid {key} array")))
}

/// Marks invalid input separately from I/O failures.
fn invalid(message: impl Into<String>) -> AppError {
    fail("invalidInput", message.into())
}
