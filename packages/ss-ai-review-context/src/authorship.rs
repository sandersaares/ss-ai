//! Records supplied semantic authorship decisions beside an immutable capture.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, FileTransaction, read_bytes};

use crate::capture::{
    CAPTURE_VERSION, CLASSIFICATION_EVIDENCE, POLICY_VERSION, apply_decisions, array_property,
    comment_input_hash, last_matching, parse_file, property, property_values, selected,
    selected_mut, string_id, validate_capture_keys, validate_capture_structure, validate_decisions,
};
use crate::error::{
    ConflictingClassification, DuplicateClassification, InvalidCaptureComment,
    InvalidClassificationBatch, InvalidClassificationKind, InvalidClassificationShape,
    MissingCapture, MissingCaptureComments, MissingCaptureThreads, MissingClassificationField,
    StaleClassificationSnapshot, TooManyClassifications, TooManyComments, UnknownComment,
    UnknownThread, UnsupportedCaptureVersion,
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
        .ok_or_else(|| MissingCapture::classified(format!("Could not find file: {path_text}")))?;
    let raw = parse_file(&bytes)?;
    validate_capture_keys(&raw)?;
    if raw.get("schemaVersion").and_then(Value::as_i64) != Some(CAPTURE_VERSION) {
        return Err(UnsupportedCaptureVersion::classified(format!(
            "Unsupported capture schema version {}. Capture the pull request again.",
            property(&raw, "schemaVersion")
        ))
        .into());
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
    let (sidecar, applied, unchanged, indeterminate) =
        classify(&raw, &existing, snapshot, classification)?;
    transaction.write_bytes(powershell_json(&sidecar)?.as_bytes())?;
    Ok(json!({
        "contextPath": path_text,
        "applied": applied,
        "unchanged": unchanged,
        "indeterminate": indeterminate,
        "isComplete": indeterminate == 0
    }))
}

/// Validates the complete batch in memory before the caller persists it.
fn classify(
    raw: &Value,
    existing: &Value,
    snapshot: &str,
    classification: &Value,
) -> Result<(Value, usize, usize, usize), AppError> {
    let mut effective = raw.clone();
    apply_decisions(&mut effective, existing)?;
    if snapshot_id(&effective)? != snapshot {
        return Err(StaleClassificationSnapshot::classified().into());
    }

    // Classification changes fields, not identifiers or collection shapes.
    let index = CommentIndex::new(raw);
    let mut decisions = matching_decisions(raw, existing, &index)?;
    let entries = classification
        .as_array()
        .ok_or_else(InvalidClassificationBatch::classified)?;
    let mut seen = HashSet::new();
    let mut applied = 0_usize;
    let mut unchanged = 0_usize;
    for entry in entries {
        let (thread_id, comment_id, kind) = classification_entry(entry)?;
        if !seen.insert((thread_id.to_owned(), comment_id.to_owned())) {
            return Err(DuplicateClassification::classified().into());
        }
        let (thread_array, thread_index, comment_array, comment_index) =
            index.locate(thread_id, comment_id)?;
        let thread = selected(property(&effective, "threads"), thread_array, thread_index);
        let comment = selected(property(thread, "comments"), comment_array, comment_index);
        let current = property(comment, "authorship").as_str().unwrap_or_default();
        if current != "indeterminate" {
            if current == kind {
                unchanged = unchanged
                    .checked_add(1)
                    .ok_or_else(TooManyClassifications::classified)?;
                continue;
            }
            return Err(ConflictingClassification::classified(
                format!(
                    "Comment '{comment_id}' in thread '{thread_id}' is already classified as '{current}' from the evidence the capture collected, so it cannot be reclassified as '{kind}'."
                ),
            ).into());
        }
        let input_hash = comment_input_hash(&effective, thread_id, comment)?;
        let threads = effective
            .get_mut("threads")
            .ok_or_else(MissingCaptureThreads::classified)?;
        let thread = selected_mut(threads, thread_array, thread_index);
        let comments = thread
            .get_mut("comments")
            .ok_or_else(MissingCaptureComments::classified)?;
        let comment = selected_mut(comments, comment_array, comment_index);
        let mut evidence = array_property(comment, "authorshipEvidence");
        evidence.push(Value::String(CLASSIFICATION_EVIDENCE.to_owned()));
        let object = comment
            .as_object_mut()
            .ok_or_else(InvalidCaptureComment::classified)?;
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
            .ok_or_else(TooManyClassifications::classified)?;
    }
    let indeterminate = remaining(&effective)?;
    let sidecar = json!({
        "schemaVersion": 1,
        "policyVersion": POLICY_VERSION,
        "captureHash": document_hash(raw)?,
        "decisions": decisions
    });
    validate_decisions(&sidecar)?;
    Ok((sidecar, applied, unchanged, indeterminate))
}

/// Retains decisions only when the current policy and exact inputs still match.
fn matching_decisions(
    raw: &Value,
    existing: &Value,
    index: &CommentIndex,
) -> Result<Vec<Value>, AppError> {
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
        // Decisions for comments absent from this capture no longer apply.
        let Some((thread_array, thread_index, comment_array, comment_index)) =
            index.find(thread_id, comment_id)
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
        .ok_or_else(InvalidClassificationShape::classified)?;
    let get = |name| {
        object
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                MissingClassificationField::classified(format!("Classification requires {name}"))
            })
    };
    let thread_id = get("threadId")?;
    let comment_id = get("commentId")?;
    let requested_kind = get("kind")?;
    let kind = requested_kind.to_lowercase();
    if kind != "agent" && kind != "human" {
        return Err(InvalidClassificationKind::classified(
            format!(
                "Classification for comment '{comment_id}' in thread '{thread_id}' names authorship '{requested_kind}'. Use 'agent' or 'human'."
            ),
        ).into());
    }
    Ok((thread_id, comment_id, kind))
}

/// Holds coordinates rather than references so decisions can mutate the capture.
pub(crate) struct CommentIndex {
    thread_array: bool,
    threads: HashMap<String, ThreadIndex>,
}

/// Keeps the last occurrence of each comment in the selected thread.
struct ThreadIndex {
    position: usize,
    comment_array: bool,
    comments: HashMap<String, usize>,
}

impl CommentIndex {
    /// Builds the same case-sensitive, last-identifier lookup as the reader.
    pub(crate) fn new(document: &Value) -> Self {
        let mut threads = HashMap::new();
        let thread_values = property(document, "threads");
        let thread_array = thread_values.is_array();
        for (id, position) in Self::identifier_positions(thread_values) {
            let thread = selected(thread_values, thread_array, position);
            threads.insert(
                id,
                ThreadIndex {
                    position,
                    comment_array: property(thread, "comments").is_array(),
                    comments: Self::identifier_positions(property(thread, "comments")),
                },
            );
        }
        Self {
            thread_array,
            threads,
        }
    }

    /// Finds retained coordinates without treating an obsolete decision as an error.
    pub(crate) fn find(
        &self,
        thread_id: &str,
        comment_id: &str,
    ) -> Option<(bool, usize, bool, usize)> {
        let thread = self.threads.get(thread_id)?;
        let comment = thread.comments.get(comment_id)?;
        Some((
            self.thread_array,
            thread.position,
            thread.comment_array,
            *comment,
        ))
    }

    /// Uses the reader's scalar lookup and indexes arrays by their last identifier.
    fn identifier_positions(value: &Value) -> HashMap<String, usize> {
        let mut positions = HashMap::new();
        if let Value::Array(values) = value {
            for (position, value) in values.iter().enumerate() {
                positions.insert(string_id(property(value, "id")), position);
            }
        } else {
            let id = string_id(property(value, "id"));
            if let Some((_, position)) = last_matching(value, &id) {
                positions.insert(id, position);
            }
        }
        positions
    }

    /// Distinguishes a missing thread from a missing comment in that thread.
    fn locate(
        &self,
        thread_id: &str,
        comment_id: &str,
    ) -> Result<(bool, usize, bool, usize), AppError> {
        let thread = self.threads.get(thread_id).ok_or_else(|| {
            UnknownThread::classified(format!("The capture contains no thread '{thread_id}'."))
        })?;
        let comment = thread.comments.get(comment_id).ok_or_else(|| {
            UnknownComment::classified(format!(
                "The capture contains no comment '{comment_id}' in thread '{thread_id}'."
            ))
        })?;
        Ok((
            self.thread_array,
            thread.position,
            thread.comment_array,
            *comment,
        ))
    }
}

/// Counts unsettled comments through the same identifier index used for updates.
fn remaining(document: &Value) -> Result<usize, AppError> {
    let mut threads = HashMap::new();
    for thread in property_values(document, "threads") {
        threads.insert(string_id(property(thread, "id")), thread);
    }
    let mut remaining = 0_usize;
    for thread in threads.into_values() {
        let mut comments = HashMap::new();
        for comment in property_values(thread, "comments") {
            comments.insert(string_id(property(comment, "id")), comment);
        }
        for comment in comments.into_values() {
            if property(comment, "authorship").as_str() == Some("indeterminate") {
                remaining = remaining
                    .checked_add(1)
                    .ok_or_else(TooManyComments::classified)?;
            }
        }
    }
    Ok(remaining)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        reason = "The tests use fixed fixture shapes"
    )]
    use std::error::Error;

    use ohno::AppError;
    use serde_json::{Value, json};
    use ss_ai_runtime::classification;

    use crate::capture::{POLICY_VERSION, apply_decisions, property, selected};
    use crate::error::{
        ConflictingClassification, DuplicateClassification, InvalidClassificationBatch,
        InvalidClassificationKind, InvalidClassificationShape, MissingClassificationField,
        StaleClassificationSnapshot, UnknownComment, UnknownThread,
    };
    use crate::{document_hash, snapshot_id};

    use super::{CommentIndex, classify, remaining};

    /// Confirms the rejected invariant and the application recovery category.
    fn assert_failure<T: Error + 'static>(error: &AppError) {
        assert!(error.find_source::<T>().is_some(), "{error}");
        assert_eq!(classification(error), "invalidInput");
    }

    /// Uses the same capture fixture as the file-boundary classification tests.
    fn fixture() -> (Value, Value, String) {
        let raw: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/ss-review-query-capture.json"
        ))
        .unwrap();
        let existing = json!({
            "schemaVersion": 1,
            "policyVersion": POLICY_VERSION,
            "captureHash": document_hash(&raw).unwrap(),
            "decisions": []
        });
        let snapshot = snapshot_id(&raw).unwrap();
        (raw, existing, snapshot)
    }

    #[test]
    fn classification_rejections_keep_specific_causes() {
        type Check = fn(&AppError);

        let (raw, existing, snapshot) = fixture();
        let raw_before = raw.clone();
        let existing_before = existing.clone();
        assert_failure::<StaleClassificationSnapshot>(
            &classify(&raw, &existing, "stale", &json!([])).unwrap_err(),
        );
        let cases: [(Value, Check); 7] = [
            (json!({}), assert_failure::<InvalidClassificationBatch>),
            (json!([{}]), assert_failure::<InvalidClassificationShape>),
            (
                json!([{"threadId":"","commentId":"C","kind":"agent"}]),
                assert_failure::<MissingClassificationField>,
            ),
            (
                json!([{"threadId":"T","commentId":"C","kind":"robot"}]),
                assert_failure::<InvalidClassificationKind>,
            ),
            (
                json!([{"threadId":"missing","commentId":"C","kind":"agent"}]),
                assert_failure::<UnknownThread>,
            ),
            (
                json!([{"threadId":"T","commentId":"missing","kind":"agent"}]),
                assert_failure::<UnknownComment>,
            ),
            (
                json!([{"threadId":"T","commentId":"C","kind":"agent"},{"threadId":"T","commentId":"C","kind":"agent"}]),
                assert_failure::<DuplicateClassification>,
            ),
        ];
        for (batch, check) in cases {
            check(&classify(&raw, &existing, &snapshot, &batch).unwrap_err());
        }
        assert_eq!(raw, raw_before);
        assert_eq!(existing, existing_before);

        let mut determined = raw;
        determined["threads"][0]["comments"][0]["authorship"] = json!("human");
        let snapshot = snapshot_id(&determined).unwrap();
        let existing = json!({
            "schemaVersion": 1,
            "policyVersion": POLICY_VERSION,
            "captureHash": document_hash(&determined).unwrap(),
            "decisions": []
        });
        assert_failure::<ConflictingClassification>(
            &classify(
                &determined,
                &existing,
                &snapshot,
                &json!([{"threadId":"T","commentId":"C","kind":"agent"}]),
            )
            .unwrap_err(),
        );
    }

    #[test]
    fn index_selects_last_duplicates_and_preserves_scalar_and_case_semantics() {
        let document = json!({"threads": [
            {"id":"T","comments":[{"id":"C","content":"old thread"}]},
            {"id":"T","comments":[{"id":"C","content":"old"},{"id":"C","content":"last"},{"id":"c","content":"lower"}]},
            {"id":"t","comments":{"id":"C","content":"scalar"}}
        ]});
        let index = CommentIndex::new(&document);
        let lookup = |thread, comment| {
            let (ta, ti, ca, ci) = index.locate(thread, comment).unwrap();
            let thread = selected(property(&document, "threads"), ta, ti);
            property(selected(property(thread, "comments"), ca, ci), "content").clone()
        };
        assert_eq!(lookup("T", "C"), json!("last"));
        assert_eq!(lookup("T", "c"), json!("lower"));
        assert_eq!(lookup("t", "C"), json!("scalar"));
        assert_failure::<UnknownThread>(&index.locate("missing", "C").unwrap_err());
        assert_failure::<UnknownComment>(&index.locate("t", "c").unwrap_err());

        let scalar = json!({"threads":{"id":"T","comments":{"id":"C"}}});
        assert_eq!(
            CommentIndex::new(&scalar).locate("T", "C").unwrap(),
            (false, 0, false, 0)
        );
    }

    #[test]
    fn batches_mutate_last_duplicates_and_retain_matching_decisions() {
        let (mut raw, _, _) = fixture();
        let old_thread = raw["threads"][0].clone();
        let mut last_thread = old_thread.clone();
        let first = last_thread["comments"][0].clone();
        let mut second = first.clone();
        second["id"] = json!("c");
        last_thread["comments"] = json!([first.clone(), first, second]);
        raw["threads"] = json!([old_thread, last_thread]);
        let existing = json!({
            "schemaVersion": 1,
            "policyVersion": POLICY_VERSION,
            "captureHash": document_hash(&raw).unwrap(),
            "decisions": []
        });
        let (first, applied, _, remaining) = classify(
            &raw,
            &existing,
            &snapshot_id(&raw).unwrap(),
            &json!([{"threadId":"T","commentId":"C","kind":"agent"}]),
        )
        .unwrap();
        assert_eq!((applied, remaining), (1, 1));
        let mut effective = raw.clone();
        apply_decisions(&mut effective, &first).unwrap();
        assert_eq!(
            effective["threads"][0]["comments"][0]["authorship"],
            "indeterminate"
        );
        assert_eq!(
            effective["threads"][1]["comments"][0]["authorship"],
            "indeterminate"
        );
        assert_eq!(
            effective["threads"][1]["comments"][1]["authorship"],
            "agent"
        );
        let (second, applied, _, remaining) = classify(
            &raw,
            &first,
            &snapshot_id(&effective).unwrap(),
            &json!([{"threadId":"T","commentId":"c","kind":"human"}]),
        )
        .unwrap();
        assert_eq!((applied, remaining), (1, 0));
        assert_eq!(second["decisions"].as_array().unwrap().len(), 2);
        let mut effective = raw.clone();
        apply_decisions(&mut effective, &second).unwrap();
        assert_eq!(
            effective["threads"][1]["comments"][2]["authorship"],
            "human"
        );
    }

    #[test]
    fn progress_counts_last_duplicate_thread_and_comment() {
        let document = json!({"threads": [
            {"id": "1", "comments": [{"id": "old", "authorship": "indeterminate"}]},
            {"id": "1", "comments": [
                {"id": "2", "authorship": "indeterminate"},
                {"id": "2", "authorship": "human"},
                {"id": "3", "authorship": "indeterminate"}
            ]},
            {"id": "other", "comments": {"id": "2", "authorship": "indeterminate"}}
        ]});
        assert_eq!(remaining(&document).unwrap(), 2);
    }
}
