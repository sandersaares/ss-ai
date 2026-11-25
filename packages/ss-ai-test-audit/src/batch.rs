//! Bounded assertion export and strict semantic-classification ingestion.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use ohno::AppError;
use serde_json::json;
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail};

use crate::model::{
    AssertionSite, Batch, BatchAssertion, Citation, Classification, ClassificationBatch,
    ClassificationJournal, ClassificationKind, CompactSubmission, EvidenceStatus, IndexEntry,
};
use crate::state::{
    LoadedState, load, load_fresh, output_path, parse_json, reject_source_alias,
    reject_state_destination, require_version, validate_journal, validate_source_snapshot,
    write_bytes, write_json,
};
use crate::submission::{resolve_submission, validate_exported_batch};

/// Matches the skill's bounded agent review unit and cumulative progress reporting.
pub(crate) const MAX_BATCH_SIZE: usize = 25;
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
    let batch = build_batch(&loaded, count);
    let output = output_path(output)?;
    let display = batch_display_path(&output)?;
    preflight_batch_output(state_directory, &output, &display, &loaded.inventory)?;
    export_batch(&batch, &output, &display, clock)?;
    Ok(batch_summary(&batch, &output, &display).to_string())
}

/// Builds the next bounded assertion batch from one already validated state load.
fn build_batch(loaded: &LoadedState, count: usize) -> Batch {
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
    Batch {
        schema_version: 1,
        audit_id: loaded.state.audit_id.clone(),
        snapshot_sha256: loaded.state.snapshot_sha256.clone(),
        assertions: selected,
        completed: completed.len(),
        remaining,
    }
}

/// Summarizes one exported batch without repeating its assertion payload.
fn batch_summary(batch: &Batch, output: &Path, display: &Path) -> serde_json::Value {
    let next_action = if batch.assertions.is_empty() {
        "report"
    } else {
        "continue"
    };
    json!({
        "schemaVersion": 1,
        "batchPath": output,
        "displayPath": display,
        "auditId": batch.audit_id,
        "snapshotSha256": batch.snapshot_sha256,
        "batchCount": batch.assertions.len(),
        "completed": batch.completed,
        "remaining": batch.remaining,
        "nextAction": next_action,
        "finalResponseAllowed": false
    })
}

/// Publishes the machine record and its compact human-readable projection.
fn export_batch(
    batch: &Batch,
    output: &Path,
    display: &Path,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    write_json(output, batch, clock)?;
    write_bytes(display, compact_batch_display(batch).as_bytes(), clock)?;
    Ok(())
}

/// Derives the compact companion without allowing it to alias the machine record.
fn batch_display_path(output: &Path) -> Result<std::path::PathBuf, AppError> {
    let name = output
        .file_name()
        .ok_or_else(|| fail("invalidInput", "Batch output requires a file name"))?;
    Ok(output.with_file_name(format!("{}.txt", name.to_string_lossy())))
}

/// Protects state and source files before a journal transition is committed.
fn preflight_batch_output(
    state_directory: &Path,
    output: &Path,
    display: &Path,
    inventory: &crate::model::Inventory,
) -> Result<(), AppError> {
    for path in [output, display] {
        reject_source_alias(path, inventory)?;
        reject_state_destination(path, state_directory)?;
    }
    Ok(())
}

/// Renders only fields needed to judge one bounded assertion batch.
fn compact_batch_display(batch: &Batch) -> String {
    let mut output = format!(
        "Audit {} snapshot {} completed {} remaining {}\n",
        batch.audit_id, batch.snapshot_sha256, batch.completed, batch.remaining
    );
    for assertion in &batch.assertions {
        let test = assertion
            .test_name
            .as_deref()
            .map_or_else(|| "-".to_owned(), str::to_owned);
        let callers = assertion.assertion.calling_test_ids.join(",");
        _ = writeln!(
            output,
            "{} | {} | {}:{}-{} | test={} | helper={} | callers={}",
            assertion.assertion.id,
            assertion.assertion.kind,
            assertion.assertion.path,
            assertion.assertion.line,
            assertion.assertion.end_line,
            test,
            assertion.assertion.helper_name.as_deref().unwrap_or("-"),
            callers
        );
        _ = writeln!(
            output,
            "  expression: {}",
            assertion.assertion.expression.replace('\n', " ")
        );
        for entry in &assertion.context {
            _ = writeln!(
                output,
                "  context {}:{}: {}",
                entry.path, entry.line, entry.excerpt
            );
        }
    }
    output
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
    let loaded = load_fresh(state_directory, clock)?;
    let (_, summary) = record_loaded(state_directory, batch_path, clock, &loaded)?;
    Ok(summary.to_string())
}

/// Records one classification batch and exports the next batch in the same process.
pub fn advance(
    state_directory: &Path,
    batch_path: &Path,
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
    let loaded = load_fresh(state_directory, clock)?;
    let classification = read_classification_batch_limited(batch_path)?;
    advance_classification(
        state_directory,
        output,
        count,
        clock,
        loaded,
        classification,
    )
}

/// Resolves compact agent rows against the exported batch and accepted journal.
pub fn submit(
    state_directory: &Path,
    batch_path: &Path,
    submission_path: &Path,
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
    let loaded = load_fresh(state_directory, clock)?;
    let batch: Batch = parse_json(
        &read_bounded_json(batch_path, "exported assertion batch")?,
        "exported assertion batch",
    )?;
    validate_exported_batch(&batch, &loaded)?;
    let submission: CompactSubmission = parse_json(
        &read_bounded_json(submission_path, "compact submission")?,
        "compact submission",
    )?;
    require_version(submission.schema_version, "compact submission")?;
    let classification = resolve_submission(&loaded, &batch, submission)?;
    advance_classification(
        state_directory,
        output,
        count,
        clock,
        loaded,
        classification,
    )
}

/// Applies the shared record-and-export transaction to one canonical batch.
fn advance_classification(
    state_directory: &Path,
    output: &Path,
    count: usize,
    clock: &dyn Clock,
    loaded: LoadedState,
    classification: ClassificationBatch,
) -> Result<String, AppError> {
    let output = output_path(output)?;
    let display = batch_display_path(&output)?;
    preflight_batch_output(state_directory, &output, &display, &loaded.inventory)?;
    let (journal, recorded) =
        record_classification(state_directory, classification, clock, &loaded)?;
    let mut advanced = loaded;
    advanced.journal = journal;
    let batch = build_batch(&advanced, count);
    export_batch(&batch, &output, &display, clock)?;
    let next_action = if recorded
        .get("remaining")
        .and_then(serde_json::Value::as_u64)
        == Some(0)
    {
        "report"
    } else {
        "continue"
    };
    Ok(json!({
        "schemaVersion": 1,
        "record": recorded,
        "nextBatch": batch_summary(&batch, &output, &display),
        "nextAction": next_action,
        "finalResponseAllowed": false
    })
    .to_string())
}

/// Records one strict classification batch from an already fresh state load.
fn record_loaded(
    state_directory: &Path,
    batch_path: &Path,
    clock: &dyn Clock,
    loaded: &LoadedState,
) -> Result<(ClassificationJournal, serde_json::Value), AppError> {
    let batch = read_classification_batch_limited(batch_path)?;
    record_classification(state_directory, batch, clock, loaded)
}

/// Reads and bounds one canonical low-level classification batch.
fn read_classification_batch_limited(path: &Path) -> Result<ClassificationBatch, AppError> {
    parse_json(
        &read_bounded_json(path, "classification batch")?,
        "classification batch",
    )
}

/// Reads one agent input with a stable safety ceiling and actionable path.
fn read_bounded_json(path: &Path, role: &str) -> Result<Vec<u8>, AppError> {
    let batch_size = fs::metadata(path)
        .map_err(|error| {
            caused_by(
                "invalidInput",
                format!("Cannot inspect {role} '{}'", path.display()),
                error,
            )
        })?
        .len();
    if batch_size > MAX_CLASSIFICATION_BATCH_BYTES {
        return Err(fail(
            "invalidInput",
            format!("{role} '{}' exceeds the bounded input size", path.display()),
        ));
    }
    let bytes = fs::read(path).map_err(|error| {
        caused_by(
            "invalidInput",
            format!("Cannot read {role} '{}'", path.display()),
            error,
        )
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CLASSIFICATION_BATCH_BYTES {
        return Err(fail(
            "invalidInput",
            format!(
                "{role} '{}' exceeded the bounded input size while being read",
                path.display()
            ),
        ));
    }
    Ok(bytes)
}

/// Validates and atomically records one already parsed classification batch.
fn record_classification(
    state_directory: &Path,
    batch: ClassificationBatch,
    clock: &dyn Clock,
    loaded: &LoadedState,
) -> Result<(ClassificationJournal, serde_json::Value), AppError> {
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
    let summary = json!({
        "schemaVersion": 1,
        "classificationPath": classification_path,
        "added": added,
        "unchanged": unchanged,
        "completed": journal.classifications.len(),
        "remaining": loaded.inventory.assertions.len().saturating_sub(journal.classifications.len())
    });
    Ok((journal, summary))
}

/// Names the exact compact input row and field in a recoverable diagnostic.
fn row_error(assertion_id: &str, field: &str, detail: impl Into<String>) -> AppError {
    fail(
        "invalidInput",
        format!(
            "Assertion '{assertion_id}' field '{field}': {}",
            detail.into()
        ),
    )
}

/// Enforces the evidence and ambiguity contract for each semantic category.
pub(crate) fn validate_classification(
    classification: &Classification,
    assertion: &AssertionSite,
    tests: &BTreeSet<&str>,
    index: &[IndexEntry],
) -> Result<(), AppError> {
    if classification.actual_outcome.trim().is_empty() {
        return Err(row_error(
            &assertion.id,
            "actualOutcome",
            "The field must describe the outcome checked by the assertion",
        ));
    }
    if classification.rationale.trim().is_empty() {
        return Err(row_error(
            &assertion.id,
            "rationale",
            "The field must explain the contract relationship",
        ));
    }
    match classification.kind {
        ClassificationKind::Documented => {
            if classification.evidence_status != EvidenceStatus::Confirmed {
                return Err(row_error(
                    &assertion.id,
                    "evidenceStatus",
                    "Documented classifications require Confirmed evidence",
                ));
            }
            if classification.citations.is_empty() {
                return Err(row_error(
                    &assertion.id,
                    "citationHandles/citations",
                    "Documented classifications require at least one exact evidence reference",
                ));
            }
        }
        ClassificationKind::Implicit => {
            if classification.evidence_status != EvidenceStatus::Ambiguous {
                return Err(row_error(
                    &assertion.id,
                    "evidenceStatus",
                    "Implicit classifications require Ambiguous evidence",
                ));
            }
        }
        ClassificationKind::Mixed => {
            if classification.evidence_status != EvidenceStatus::Confirmed {
                return Err(row_error(
                    &assertion.id,
                    "evidenceStatus",
                    "Mixed classifications require Confirmed evidence",
                ));
            }
            if classification.citations.is_empty() {
                return Err(row_error(
                    &assertion.id,
                    "citationHandles/citations",
                    "Mixed classifications require documented-component evidence",
                ));
            }
            if classification
                .incidental_reason
                .as_deref()
                .is_none_or(str::is_empty)
            {
                return Err(row_error(
                    &assertion.id,
                    "incidentalReason",
                    "Mixed classifications require the undocumented detail",
                ));
            }
        }
        ClassificationKind::Incidental => {
            if classification
                .incidental_reason
                .as_deref()
                .is_none_or(str::is_empty)
            {
                return Err(row_error(
                    &assertion.id,
                    "incidentalReason",
                    "Incidental classifications require why no public promise applies",
                ));
            }
        }
    }
    for (position, citation) in classification.citations.iter().enumerate() {
        validate_citation(&assertion.id, position, citation, index)?;
    }
    for (position, test) in classification.calling_test_ids.iter().enumerate() {
        if !tests.contains(test.as_str()) {
            return Err(row_error(
                &assertion.id,
                &format!("callingTestIds[{position}]"),
                format!("Unknown calling test '{test}'"),
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
        return Err(row_error(
            &assertion.id,
            "callingTestIds/unlinkedReason",
            "A shared assertion without calling tests requires an explicit unlinked reason",
        ));
    }
    Ok(())
}

/// Requires every cited excerpt to be contained on the selected indexed line.
fn validate_citation(
    assertion_id: &str,
    position: usize,
    citation: &Citation,
    index: &[IndexEntry],
) -> Result<(), AppError> {
    let field = format!("citations[{position}]");
    if citation.path.trim().is_empty() {
        return Err(row_error(
            assertion_id,
            &format!("{field}.path"),
            "Citation path must not be empty",
        ));
    }
    if citation.line == 0 {
        return Err(row_error(
            assertion_id,
            &format!("{field}.line"),
            "Citation line must be positive",
        ));
    }
    if citation.excerpt.trim().is_empty() {
        return Err(row_error(
            assertion_id,
            &format!("{field}.excerpt"),
            format!(
                "Citation excerpt for evidence reference '{}:{}' must not be empty",
                citation.path, citation.line
            ),
        ));
    }
    let entry = index
        .iter()
        .find(|entry| entry.path == citation.path && entry.line == citation.line)
        .ok_or_else(|| {
            row_error(
                assertion_id,
                &field,
                format!(
                    "Evidence reference '{}:{}' is not present in the audit index",
                    citation.path, citation.line
                ),
            )
        })?;
    if citation.excerpt != entry.excerpt && !entry.excerpt.contains(citation.excerpt.trim()) {
        return Err(row_error(
            assertion_id,
            &format!("{field}.excerpt"),
            format!(
                "Evidence reference '{}:{}' does not match the indexed source line",
                citation.path, citation.line
            ),
        ));
    }
    Ok(())
}
