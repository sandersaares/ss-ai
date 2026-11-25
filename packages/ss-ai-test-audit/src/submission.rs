//! Resolves compact agent rows into canonical semantic classifications.

use std::collections::{BTreeMap, BTreeSet};

use ohno::AppError;
use serde_json::Value;
use ss_ai_runtime::fail;

use crate::batch::{MAX_BATCH_SIZE, validate_classification};
use crate::model::{
    AssertionSite, Batch, Citation, Classification, ClassificationBatch, CompactSubmission,
    CompactSubmissionRow, IndexEntry,
};
use crate::state::{LoadedState, evidence_index, require_version};

/// Binds one exported batch to the active immutable state token.
pub(crate) fn validate_exported_batch(batch: &Batch, loaded: &LoadedState) -> Result<(), AppError> {
    require_version(batch.schema_version, "exported assertion batch")?;
    if batch.audit_id != loaded.state.audit_id
        || batch.snapshot_sha256 != loaded.state.snapshot_sha256
    {
        return Err(fail(
            "staleState",
            "Exported assertion batch belongs to a different audit snapshot",
        ));
    }
    if batch.assertions.len() > MAX_BATCH_SIZE {
        return Err(fail(
            "invalidInput",
            "Exported assertion batch exceeds the bounded batch size",
        ));
    }
    Ok(())
}

/// Resolves compact rows and explicit reuse into one canonical batch.
pub(crate) fn resolve_submission(
    loaded: &LoadedState,
    batch: &Batch,
    submission: CompactSubmission,
) -> Result<ClassificationBatch, AppError> {
    if submission.rows.len() != batch.assertions.len() {
        return Err(fail(
            "invalidInput",
            format!(
                "Compact submission has {} rows for {} exported assertions",
                submission.rows.len(),
                batch.assertions.len()
            ),
        ));
    }
    let mut rows = BTreeMap::new();
    for row in submission.rows {
        if row.assertion_id.trim().is_empty() {
            return Err(fail(
                "invalidInput",
                "Compact submission row field assertionId must not be empty",
            ));
        }
        if rows.insert(row.assertion_id.clone(), row).is_some() {
            return Err(fail(
                "invalidInput",
                "Compact submission repeats an assertion row",
            ));
        }
    }

    let accepted: BTreeMap<&str, &Classification> = loaded
        .journal
        .rows()
        .iter()
        .map(|classification| (classification.assertion_id.as_str(), classification))
        .collect();
    let assertions: BTreeMap<&str, &AssertionSite> = loaded
        .inventory
        .assertions
        .iter()
        .map(|assertion| (assertion.id.as_str(), assertion))
        .collect();
    let handles = evidence_index(&loaded.state.snapshot_sha256, &loaded.index);
    let tests: BTreeSet<&str> = loaded
        .inventory
        .tests
        .iter()
        .map(|test| test.id.as_str())
        .collect();
    let mut classifications = Vec::new();
    for exported in &batch.assertions {
        let assertion_id = &exported.assertion.id;
        let row = rows.remove(assertion_id).ok_or_else(|| {
            fail(
                "invalidInput",
                format!("Compact submission is missing exported assertion '{assertion_id}'"),
            )
        })?;
        let assertion = assertions
            .get(assertion_id.as_str())
            .copied()
            .ok_or_else(|| {
                fail(
                    "invalidState",
                    format!("Exported assertion '{assertion_id}' is missing from inventory"),
                )
            })?;
        let classification =
            resolve_submission_row(row, assertion, &accepted, &handles, &loaded.index)?;
        validate_classification(&classification, assertion, &tests, &loaded.index)?;
        classifications.push(classification);
    }
    if !rows.is_empty() {
        let unexpected = rows.keys().next().cloned().unwrap_or_default();
        return Err(fail(
            "invalidInput",
            format!("Compact submission row '{unexpected}' is not present in the exported batch"),
        ));
    }
    Ok(ClassificationBatch {
        schema_version: 1,
        audit_id: batch.audit_id.clone(),
        snapshot_sha256: batch.snapshot_sha256.clone(),
        assertions: classifications,
    })
}

/// Resolves one fresh or explicitly reused compact semantic row.
fn resolve_submission_row(
    mut row: CompactSubmissionRow,
    assertion: &AssertionSite,
    accepted: &BTreeMap<&str, &Classification>,
    handles: &BTreeMap<String, usize>,
    index: &[IndexEntry],
) -> Result<Classification, AppError> {
    match row.reuse_from.take() {
        None => resolve_explicit_row(row, assertion, handles, index),
        Some(reuse_from) => resolve_reuse_row(row, &reuse_from, accepted, handles, index),
    }
}

/// Requires the semantic fields to be supplied directly rather than inherited.
fn resolve_explicit_row(
    row: CompactSubmissionRow,
    assertion: &AssertionSite,
    handles: &BTreeMap<String, usize>,
    index: &[IndexEntry],
) -> Result<Classification, AppError> {
    if row.equivalence_confirmed.is_some() {
        return Err(submission_error(
            &row.assertion_id,
            "equivalenceConfirmed",
            "The field applies only when reuseFrom is present",
        ));
    }
    let citation_handles = required_string_array(
        row.citation_handles,
        &row.assertion_id,
        "citationHandles",
        "The field is required, including an explicit empty array for incidental checks",
    )?;
    let calling_test_ids = required_string_array(
        row.calling_test_ids,
        &row.assertion_id,
        "callingTestIds",
        "The field is required; do not omit caller attribution",
    )?;
    Ok(Classification {
        assertion_id: row.assertion_id,
        kind: required_row_value(row.classification, &assertion.id, "classification")?,
        evidence_status: required_row_value(row.evidence_status, &assertion.id, "evidenceStatus")?,
        actual_outcome: required_row_value(row.actual_outcome, &assertion.id, "actualOutcome")?,
        rationale: required_row_value(row.rationale, &assertion.id, "rationale")?,
        citations: resolve_citation_handles(
            &assertion.id,
            "citationHandles",
            &citation_handles,
            handles,
            index,
        )?,
        incidental_reason: row.incidental_reason,
        calling_test_ids,
        unlinked_reason: row.unlinked_reason,
    })
}

/// Copies only the accepted semantic judgment, with new evidence and callers.
fn resolve_reuse_row(
    row: CompactSubmissionRow,
    reuse_from: &str,
    accepted: &BTreeMap<&str, &Classification>,
    handles: &BTreeMap<String, usize>,
    index: &[IndexEntry],
) -> Result<Classification, AppError> {
    if row.equivalence_confirmed != Some(true) {
        return Err(submission_error(
            &row.assertion_id,
            "equivalenceConfirmed",
            format!("Reuse from '{reuse_from}' requires explicit true confirmation"),
        ));
    }
    for (field, present) in [
        ("classification", row.classification.is_some()),
        ("evidenceStatus", row.evidence_status.is_some()),
        ("actualOutcome", row.actual_outcome.is_some()),
        ("rationale", row.rationale.is_some()),
        ("incidentalReason", row.incidental_reason.is_some()),
    ] {
        if present {
            return Err(submission_error(
                &row.assertion_id,
                field,
                format!("The field must be omitted when reuseFrom is '{reuse_from}'"),
            ));
        }
    }
    let citation_handles = required_string_array(
        row.citation_handles,
        &row.assertion_id,
        "citationHandles",
        format!("Reuse from '{reuse_from}' requires an explicit citation replacement"),
    )?;
    if citation_handles.is_empty() {
        return Err(submission_error(
            &row.assertion_id,
            "citationHandles",
            format!(
                "Empty replacement for reuse from '{reuse_from}' affects '{}'; no citation fallback was applied",
                row.assertion_id
            ),
        ));
    }
    let calling_test_ids = required_string_array(
        row.calling_test_ids,
        &row.assertion_id,
        "callingTestIds",
        format!("Reuse from '{reuse_from}' requires explicit caller attribution"),
    )?;
    let source = accepted.get(reuse_from).copied().ok_or_else(|| {
        submission_error(
            &row.assertion_id,
            "reuseFrom",
            format!(
                "Previously assessed assertion '{reuse_from}' is not present in the accepted journal"
            ),
        )
    })?;
    Ok(Classification {
        assertion_id: row.assertion_id.clone(),
        kind: source.kind,
        evidence_status: source.evidence_status,
        actual_outcome: source.actual_outcome.clone(),
        rationale: source.rationale.clone(),
        citations: resolve_citation_handles(
            &row.assertion_id,
            "citationHandles",
            &citation_handles,
            handles,
            index,
        )?,
        incidental_reason: source.incidental_reason.clone(),
        calling_test_ids,
        unlinked_reason: row.unlinked_reason,
    })
}

/// Requires one explicit semantic field and names its compact-row location.
fn required_row_value<T>(value: Option<T>, assertion_id: &str, field: &str) -> Result<T, AppError> {
    value.ok_or_else(|| submission_error(assertion_id, field, "The field is required"))
}

/// Parses an agent-owned string array after preserving field-level error context.
fn required_string_array(
    value: Option<Value>,
    assertion_id: &str,
    field: &str,
    missing_detail: impl Into<String>,
) -> Result<Vec<String>, AppError> {
    let value =
        value.ok_or_else(|| submission_error(assertion_id, field, missing_detail.into()))?;
    let values = value.as_array().ok_or_else(|| {
        submission_error(
            assertion_id,
            field,
            format!("Expected an array of strings, received {value}"),
        )
    })?;
    values
        .iter()
        .enumerate()
        .map(|(offset, value)| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                submission_error(
                    assertion_id,
                    &format!("{field}[{offset}]"),
                    format!("Expected a string, received {value}"),
                )
            })
        })
        .collect()
}

/// Resolves one or more snapshot handles to exact immutable citations.
fn resolve_citation_handles(
    assertion_id: &str,
    field: &str,
    handles: &[String],
    evidence: &BTreeMap<String, usize>,
    index: &[IndexEntry],
) -> Result<Vec<Citation>, AppError> {
    let mut citations = Vec::new();
    for (offset, handle) in handles.iter().enumerate() {
        if handle.trim().is_empty() {
            return Err(submission_error(
                assertion_id,
                &format!("{field}[{offset}]"),
                format!("Evidence reference '{handle}' must not be empty"),
            ));
        }
        let index_offset = evidence.get(handle).copied().ok_or_else(|| {
            submission_error(
                assertion_id,
                &format!("{field}[{offset}]"),
                format!("Evidence reference '{handle}' is not bound to this audit snapshot"),
            )
        })?;
        let entry = index.get(index_offset).ok_or_else(|| {
            fail(
                "invalidState",
                format!(
                    "Assertion '{assertion_id}' evidence reference '{handle}' has no index entry"
                ),
            )
        })?;
        citations.push(Citation {
            path: entry.path.clone(),
            line: entry.line,
            excerpt: entry.excerpt.clone(),
        });
    }
    Ok(citations)
}

/// Names the exact compact input row and field in a recoverable diagnostic.
fn submission_error(assertion_id: &str, field: &str, detail: impl Into<String>) -> AppError {
    fail(
        "invalidInput",
        format!(
            "Assertion '{assertion_id}' field '{field}': {}",
            detail.into()
        ),
    )
}
