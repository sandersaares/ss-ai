//! Records supplied semantic authorship decisions beside an immutable capture.

use std::collections::HashSet;
use std::path::Path;

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, FileTransaction, fail, read_bytes};

use crate::capture::{
    CAPTURE_VERSION, CLASSIFICATION_EVIDENCE, POLICY_VERSION, apply_decisions, array_property,
    comment_input_hash, last_matching, parse_file, property, property_values, selected,
    selected_mut, string_id, validate_capture_keys, validate_capture_structure, validate_decisions,
};
use crate::request::{full_path, path_string};
use crate::{document_hash, powershell_json, snapshot_id};

/// Records a supplied classification batch against its exact reading snapshot.
///
/// The capture remains unchanged. The caller supplies decisions rather than
/// delegating authorship judgment to this operation.
///
/// # Errors
///
/// Rejects stale snapshots, malformed evidence or decisions, and file failures.
pub fn write_comment_authorship(
    path: &Path,
    snapshot: &str,
    classification: &Value,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let path = full_path(path)?;
    let path_text = path_string(&path)?;
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(".authorship.json");
    let transaction = FileTransaction::open(Path::new(&sidecar), clock)?;

    let bytes = read_bytes(&path, clock)?
        .ok_or_else(|| fail("invalidInput", format!("Could not find file: {path_text}")))?;
    let raw = parse_file(&bytes)?;
    validate_capture_keys(&raw)?;
    if raw.get("schemaVersion").and_then(Value::as_i64) != Some(CAPTURE_VERSION) {
        return Err(fail(
            "invalidInput",
            format!(
                "Unsupported capture schema version {}. Capture the pull request again.",
                property(&raw, "schemaVersion")
            ),
        ));
    }
    validate_capture_structure(&raw)?;
    let capture_hash = document_hash(&raw)?;
    let existing = match transaction.read_bytes()? {
        Some(bytes) => parse_file(&bytes)?,
        None => json!({
            "schemaVersion": 1,
            "policyVersion": POLICY_VERSION,
            "captureHash": capture_hash,
            "decisions": []
        }),
    };
    let mut effective = raw.clone();
    apply_decisions(&mut effective, &existing)?;
    if snapshot_id(&effective)? != snapshot {
        return Err(fail("invalidInput", "The classification snapshot changed"));
    }

    let mut decisions = matching_decisions(&raw, &existing)?;
    let entries = classification
        .as_array()
        .ok_or_else(|| fail("invalidInput", "Classifications must be a JSON array"))?;
    let mut seen = HashSet::new();
    let mut applied = 0_usize;
    let mut unchanged = 0_usize;
    for entry in entries {
        let (thread_id, comment_id, kind) = classification_entry(entry)?;
        if !seen.insert((thread_id.to_owned(), comment_id.to_owned())) {
            return Err(fail("invalidInput", "Duplicate classification"));
        }
        let (thread_array, thread_index, comment_array, comment_index) =
            locate(&effective, thread_id, comment_id)?;
        let thread = selected(property(&effective, "threads"), thread_array, thread_index);
        let comment = selected(property(thread, "comments"), comment_array, comment_index);
        let current = property(comment, "authorship").as_str().unwrap_or_default();
        if current != "indeterminate" {
            if current == kind {
                unchanged = unchanged
                    .checked_add(1)
                    .ok_or_else(|| fail("invalidInput", "Too many classifications"))?;
                continue;
            }
            return Err(fail(
                "invalidInput",
                format!(
                    "Comment '{comment_id}' in thread '{thread_id}' is already classified as '{current}' from the evidence the capture collected, so it cannot be reclassified as '{kind}'."
                ),
            ));
        }
        let input_hash = comment_input_hash(&effective, thread_id, comment)?;
        let threads = effective
            .get_mut("threads")
            .ok_or_else(|| fail("invalidInput", "Missing capture threads"))?;
        let thread = selected_mut(threads, thread_array, thread_index);
        let comments = thread
            .get_mut("comments")
            .ok_or_else(|| fail("invalidInput", "Missing capture comments"))?;
        let comment = selected_mut(comments, comment_array, comment_index);
        let mut evidence = array_property(comment, "authorshipEvidence");
        evidence.push(Value::String(CLASSIFICATION_EVIDENCE.to_owned()));
        let object = comment
            .as_object_mut()
            .ok_or_else(|| fail("invalidInput", "Capture comment is not an object"))?;
        object.insert("authorship".to_owned(), Value::String(kind.clone()));
        object.insert("authorshipEvidence".to_owned(), Value::Array(evidence));
        decisions.push(json!({
            "threadId": thread_id,
            "commentId": comment_id,
            "kind": kind,
            "inputHash": input_hash,
            "origin": "classified"
        }));
        applied = applied
            .checked_add(1)
            .ok_or_else(|| fail("invalidInput", "Too many classifications"))?;
    }
    let indeterminate = remaining(&effective)?;
    let sidecar = json!({
        "schemaVersion": 1,
        "policyVersion": POLICY_VERSION,
        "captureHash": capture_hash,
        "decisions": decisions
    });
    validate_decisions(&sidecar)?;
    transaction.write_bytes(powershell_json(&sidecar)?.as_bytes())?;
    Ok(json!({
        "contextPath": path_text,
        "applied": applied,
        "unchanged": unchanged,
        "indeterminate": indeterminate,
        "isComplete": indeterminate == 0
    }))
}

/// Retains decisions only when the current policy and exact inputs still match.
fn matching_decisions(raw: &Value, existing: &Value) -> Result<Vec<Value>, AppError> {
    let mut retained = Vec::new();
    if property(existing, "policyVersion").as_str() != Some(POLICY_VERSION) {
        return Ok(retained);
    }
    for entry in property(existing, "decisions")
        .as_array()
        .into_iter()
        .flatten()
    {
        let thread_id = property(entry, "threadId").as_str().unwrap_or_default();
        let comment_id = property(entry, "commentId").as_str().unwrap_or_default();
        let Ok((thread_array, thread_index, comment_array, comment_index)) =
            locate(raw, thread_id, comment_id)
        else {
            continue;
        };
        let thread = selected(property(raw, "threads"), thread_array, thread_index);
        let comment = selected(property(thread, "comments"), comment_array, comment_index);
        if property(comment, "authorship").as_str() == Some("indeterminate")
            && property(entry, "inputHash").as_str()
                == Some(comment_input_hash(raw, thread_id, comment)?.as_str())
        {
            retained.push(entry.clone());
        }
    }
    Ok(retained)
}

/// Checks the exact supplied batch shape before accepting a semantic decision.
fn classification_entry(entry: &Value) -> Result<(&str, &str, String), AppError> {
    let object = entry
        .as_object()
        .filter(|object| object.len() == 3)
        .ok_or_else(|| {
            fail(
                "invalidInput",
                "Classification requires only threadId, commentId and kind",
            )
        })?;
    let get = |name| {
        object
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| fail("invalidInput", format!("Classification requires {name}")))
    };
    let thread_id = get("threadId")?;
    let comment_id = get("commentId")?;
    let requested_kind = get("kind")?;
    let kind = requested_kind.to_lowercase();
    if kind != "agent" && kind != "human" {
        return Err(fail(
            "invalidInput",
            format!(
                "Classification for comment '{comment_id}' in thread '{thread_id}' names authorship '{requested_kind}'. Use 'agent' or 'human'."
            ),
        ));
    }
    Ok((thread_id, comment_id, kind))
}

/// Uses the same last-identifier index semantics as the capture reader.
fn locate(
    document: &Value,
    thread_id: &str,
    comment_id: &str,
) -> Result<(bool, usize, bool, usize), AppError> {
    let (thread_array, thread_index) = last_matching(property(document, "threads"), thread_id)
        .ok_or_else(|| {
            fail(
                "invalidInput",
                format!("The capture contains no thread '{thread_id}'."),
            )
        })?;
    let thread = selected(property(document, "threads"), thread_array, thread_index);
    let (comment_array, comment_index) = last_matching(property(thread, "comments"), comment_id)
        .ok_or_else(|| {
            fail(
                "invalidInput",
                format!("The capture contains no comment '{comment_id}' in thread '{thread_id}'."),
            )
        })?;
    Ok((thread_array, thread_index, comment_array, comment_index))
}

/// Counts unsettled comments through the same identifier index used for updates.
fn remaining(document: &Value) -> Result<usize, AppError> {
    let mut threads = HashSet::new();
    for thread in property_values(document, "threads") {
        threads.insert(string_id(property(thread, "id")));
    }
    let mut remaining = 0_usize;
    for thread_id in threads {
        let (thread_array, thread_index) = last_matching(property(document, "threads"), &thread_id)
            .ok_or_else(|| fail("invalidInput", "Missing capture thread"))?;
        let thread = selected(property(document, "threads"), thread_array, thread_index);
        let mut comments = HashSet::new();
        for comment in property_values(thread, "comments") {
            comments.insert(string_id(property(comment, "id")));
        }
        for comment_id in comments {
            let (comment_array, comment_index) =
                last_matching(property(thread, "comments"), &comment_id)
                    .ok_or_else(|| fail("invalidInput", "Missing capture comment"))?;
            let comment = selected(property(thread, "comments"), comment_array, comment_index);
            if property(comment, "authorship").as_str() == Some("indeterminate") {
                remaining = remaining
                    .checked_add(1)
                    .ok_or_else(|| fail("invalidInput", "Too many comments"))?;
            }
        }
    }
    Ok(remaining)
}
