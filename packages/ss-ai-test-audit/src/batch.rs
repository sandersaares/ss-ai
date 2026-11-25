//! Bounded assertion export and strict semantic-classification ingestion.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use ohno::AppError;
use serde_json::json;
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail};

use crate::model::{
    AssertionSite, Batch, BatchAssertion, Citation, Classification, ClassificationBatch,
    ClassificationJournal, ClassificationKind, EvidenceStatus, IndexEntry,
};
use crate::state::{
    load, output_path, parse_json, reject_source_alias, reject_state_destination, require_version,
    validate_journal, validate_source_snapshot, write_json,
};

/// Matches the skill's bounded agent review unit and cumulative progress reporting.
const MAX_BATCH_SIZE: usize = 25;
/// Keeps one agent-authored result page small enough for bounded local ingestion.
const MAX_CLASSIFICATION_BATCH_BYTES: u64 = 1024 * 1024;
/// Retains nearby source without duplicating complete files in the batch.
const ASSERTION_CONTEXT_RADIUS: usize = 4;
/// Includes the declaration and a small header in each containing-test context.
const TEST_HEADER_LINES: usize = 2;

/// Exports at most one bounded review batch without consuming resumable state.
pub fn next_batch(
    state_directory: &Path,
    output: &Path,
    count: usize,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    if count == 0 || count > MAX_BATCH_SIZE {
        return Err(fail(
            "invalidInput",
            "Assertion batch size must be between 1 and 25",
        ));
    }
    let loaded = load(state_directory, clock)?;
    let completed: BTreeSet<&str> = loaded
        .journal
        .classifications
        .iter()
        .map(|classification| classification.assertion_id.as_str())
        .collect();
    let remaining = loaded
        .inventory
        .assertions
        .iter()
        .filter(|assertion| !completed.contains(assertion.id.as_str()))
        .count();
    let selected = loaded
        .inventory
        .assertions
        .iter()
        .filter(|assertion| !completed.contains(assertion.id.as_str()))
        .take(count)
        .map(|assertion| batch_assertion(assertion, &loaded.inventory.tests, &loaded.index))
        .collect::<Vec<_>>();
    let batch = Batch {
        schema_version: 1,
        audit_id: loaded.state.audit_id.clone(),
        snapshot_sha256: loaded.state.snapshot_sha256.clone(),
        assertions: selected,
        completed: completed.len(),
        remaining,
    };
    let output = output_path(output)?;
    reject_source_alias(&output, &loaded.inventory)?;
    reject_state_destination(&output, state_directory)?;
    write_json(&output, &batch, clock)?;
    Ok(json!({
        "schemaVersion": 1,
        "batchPath": output,
        "auditId": batch.audit_id,
        "snapshotSha256": batch.snapshot_sha256,
        "batchCount": batch.assertions.len(),
        "completed": batch.completed,
        "remaining": batch.remaining
    })
    .to_string())
}

/// Adds exact line context and test ownership to each exported source predicate.
fn batch_assertion(
    assertion: &AssertionSite,
    tests: &[crate::model::TestCase],
    index: &[IndexEntry],
) -> BatchAssertion {
    let test = assertion
        .containing_test_id
        .as_ref()
        .and_then(|id| tests.iter().find(|test| test.id == *id));
    let assertion_context = index
        .iter()
        .filter(|entry| {
            entry.path == assertion.path
                && entry.line >= assertion.line.saturating_sub(ASSERTION_CONTEXT_RADIUS)
                && entry.line <= assertion.end_line.saturating_add(ASSERTION_CONTEXT_RADIUS)
        })
        .cloned();
    let test_context = test
        .into_iter()
        .flat_map(|test| {
            index
                .iter()
                .filter(|entry| {
                    entry.path == test.path
                        && entry.line >= test.line
                        && entry.line <= test.line.saturating_add(TEST_HEADER_LINES)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut context: BTreeMap<(String, usize), IndexEntry> = assertion_context
        .map(|entry| ((entry.path.clone(), entry.line), entry))
        .collect();
    for entry in test_context {
        context.insert((entry.path.clone(), entry.line), entry);
    }
    BatchAssertion {
        assertion: assertion.clone(),
        test_name: test.map(|test| test.name.clone()),
        test_path: test.map(|test| test.path.clone()),
        test_line: test.map(|test| test.line),
        context: context.into_values().collect(),
    }
}

/// Records an idempotent group of validated agent-owned judgments.
pub fn record_batch(
    state_directory: &Path,
    batch_path: &Path,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let loaded = load(state_directory, clock)?;
    let classification_path = state_directory.join("classifications.json");
    let transaction = FileTransaction::open(&classification_path, clock)?;
    let Some(journal_bytes) = transaction.read_bytes()? else {
        return Err(fail("invalidState", "Classification journal is missing"));
    };
    let mut journal: ClassificationJournal = parse_json(&journal_bytes, "classification journal")?;
    require_version(journal.schema_version, "classification journal")?;
    if journal.audit_id != loaded.state.audit_id
        || journal.snapshot_sha256 != loaded.state.snapshot_sha256
    {
        return Err(fail(
            "invalidState",
            "Classification journal belongs to a different audit",
        ));
    }
    validate_journal(&loaded.inventory, &journal, &loaded.index)?;
    let batch_size = fs::metadata(batch_path)?.len();
    if batch_size > MAX_CLASSIFICATION_BATCH_BYTES {
        return Err(fail(
            "invalidInput",
            "Classification batch exceeds the bounded input size",
        ));
    }
    let bytes = fs::read(batch_path).map_err(|error| {
        caused_by(
            "invalidInput",
            format!(
                "Cannot read classification batch '{}'",
                batch_path.display()
            ),
            error,
        )
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CLASSIFICATION_BATCH_BYTES {
        return Err(fail(
            "invalidInput",
            "Classification batch exceeded the bounded input size while being read",
        ));
    }
    let batch: ClassificationBatch = parse_json(&bytes, "classification batch")?;
    require_version(batch.schema_version, "classification batch")?;
    if batch.audit_id != loaded.state.audit_id
        || batch.snapshot_sha256 != loaded.state.snapshot_sha256
    {
        return Err(fail(
            "staleState",
            "Classification batch belongs to a different audit snapshot",
        ));
    }
    if batch.assertions.is_empty() || batch.assertions.len() > MAX_BATCH_SIZE {
        return Err(fail(
            "invalidInput",
            "Classification batch must contain between 1 and 25 assertions",
        ));
    }

    let assertions: BTreeMap<&str, &AssertionSite> = loaded
        .inventory
        .assertions
        .iter()
        .map(|assertion| (assertion.id.as_str(), assertion))
        .collect();
    let tests: BTreeSet<&str> = loaded
        .inventory
        .tests
        .iter()
        .map(|test| test.id.as_str())
        .collect();
    let existing: BTreeMap<String, Classification> = journal
        .classifications
        .iter()
        .map(|classification| (classification.assertion_id.clone(), classification.clone()))
        .collect();
    let mut incoming = BTreeSet::new();
    let mut added = Vec::new();
    let mut unchanged = Vec::new();
    let mut pending = Vec::new();

    for classification in batch.assertions {
        let assertion = assertions
            .get(classification.assertion_id.as_str())
            .copied()
            .ok_or_else(|| {
                fail(
                    "invalidInput",
                    format!(
                        "Batch references unknown assertion '{}'",
                        classification.assertion_id
                    ),
                )
            })?;
        if !incoming.insert(classification.assertion_id.clone()) {
            return Err(fail(
                "invalidInput",
                format!(
                    "Classification batch repeats assertion '{}'",
                    classification.assertion_id
                ),
            ));
        }
        validate_classification(&classification, assertion, &tests, &loaded.index)?;
        if let Some(previous) = existing.get(&classification.assertion_id) {
            if serde_json::to_value(previous)? != serde_json::to_value(&classification)? {
                return Err(fail(
                    "invalidState",
                    format!(
                        "Assertion '{}' already has a different classification",
                        classification.assertion_id
                    ),
                ));
            }
            unchanged.push(classification.assertion_id);
        } else {
            added.push(classification.assertion_id.clone());
            pending.push(classification);
        }
    }
    journal.classifications.extend(pending);
    journal
        .classifications
        .sort_by(|left, right| left.assertion_id.cmp(&right.assertion_id));
    validate_source_snapshot(&loaded.inventory, &loaded.state.repository_root)?;
    transaction.write_bytes(&serde_json::to_vec_pretty(&journal)?)?;
    Ok(json!({
        "schemaVersion": 1,
        "classificationPath": classification_path,
        "added": added,
        "unchanged": unchanged,
        "completed": journal.classifications.len(),
        "remaining": loaded.inventory.assertions.len().saturating_sub(journal.classifications.len())
    })
    .to_string())
}

/// Enforces the evidence and ambiguity contract for each semantic category.
pub(crate) fn validate_classification(
    classification: &Classification,
    assertion: &AssertionSite,
    tests: &BTreeSet<&str>,
    index: &[IndexEntry],
) -> Result<(), AppError> {
    if classification.actual_outcome.trim().is_empty() {
        return Err(fail(
            "invalidInput",
            format!(
                "Assertion '{}' requires the actual outcome it checks",
                assertion.id
            ),
        ));
    }
    if classification.rationale.trim().is_empty() {
        return Err(fail(
            "invalidInput",
            format!("Assertion '{}' requires a rationale", assertion.id),
        ));
    }
    match classification.kind {
        ClassificationKind::Documented => {
            if classification.evidence_status != EvidenceStatus::Confirmed
                || classification.citations.is_empty()
            {
                return Err(fail(
                    "invalidInput",
                    "Documented classifications require confirmed citations",
                ));
            }
        }
        ClassificationKind::Implicit => {
            if classification.evidence_status != EvidenceStatus::Ambiguous {
                return Err(fail(
                    "invalidInput",
                    "Implicit classifications must remain explicitly ambiguous",
                ));
            }
        }
        ClassificationKind::Mixed => {
            if classification.evidence_status != EvidenceStatus::Confirmed
                || classification.citations.is_empty()
                || classification
                    .incidental_reason
                    .as_deref()
                    .is_none_or(str::is_empty)
            {
                return Err(fail(
                    "invalidInput",
                    "Mixed classifications require confirmed promise evidence and the undocumented detail",
                ));
            }
        }
        ClassificationKind::Incidental => {
            if classification
                .incidental_reason
                .as_deref()
                .is_none_or(str::is_empty)
            {
                return Err(fail(
                    "invalidInput",
                    "Incidental classifications require why no public promise applies",
                ));
            }
        }
    }
    for citation in &classification.citations {
        validate_citation(citation, index)?;
    }
    for test in &classification.calling_test_ids {
        if !tests.contains(test.as_str()) {
            return Err(fail(
                "invalidInput",
                format!("Classification references unknown calling test '{test}'"),
            ));
        }
    }
    if assertion.containing_test_id.is_none()
        && assertion.calling_test_ids.is_empty()
        && classification.calling_test_ids.is_empty()
        && classification
            .unlinked_reason
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Err(fail(
            "invalidInput",
            "A shared assertion without calling tests requires an unlinked reason",
        ));
    }
    Ok(())
}

/// Requires every cited excerpt to be contained on the selected indexed line.
fn validate_citation(citation: &Citation, index: &[IndexEntry]) -> Result<(), AppError> {
    if citation.path.is_empty() || citation.line == 0 || citation.excerpt.trim().is_empty() {
        return Err(fail(
            "invalidInput",
            "Citation path, line, and excerpt must be present",
        ));
    }
    let entry = index
        .iter()
        .find(|entry| entry.path == citation.path && entry.line == citation.line)
        .ok_or_else(|| {
            fail(
                "invalidInput",
                "Citation line is not present in the audit index",
            )
        })?;
    if citation.excerpt != entry.excerpt && !entry.excerpt.contains(citation.excerpt.trim()) {
        return Err(fail(
            "invalidInput",
            "Citation excerpt does not match the indexed source line",
        ));
    }
    Ok(())
}
