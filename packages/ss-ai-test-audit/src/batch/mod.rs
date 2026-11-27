//! Bounded assertion export and strict semantic-classification ingestion.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{File, remove_file};
use std::io::Read;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde_json::{Value, json, to_value, to_vec_pretty};
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail};

use crate::model::{
    AssertionSite, AuditState, Batch, BatchAssertion, BatchContextEntry, Citation, Classification,
    ClassificationBatch, ClassificationJournal, ClassificationKind, CompactSubmission,
    EvidenceStatus, IndexEntry, Inventory, ScanDiagnostic, TestCase,
};
use crate::scan::digest;
use crate::state::{
    LoadedState, create_output_parent, load, load_fresh, output_path_text, parse_json,
    path_is_vacant, reject_input_alias, reject_source_alias, reject_state_destination,
    require_version, resolve_output_path, validate_journal, validate_source_snapshot,
};
use crate::submission::{resolve_submission, validate_exported_batch};

/// Matches the skill's bounded agent review unit and cumulative progress reporting.
pub(crate) const MAX_BATCH_SIZE: usize = 25;
/// Bounds a complete JSON page at the reader's input ceiling, even when
/// escaping and repeated context inflate the serialized assertions.
const MAX_BATCH_BYTES: usize = 1024 * 1024;
/// Preserves useful predicate text while giving each preview only a fraction
/// of the page budget, leaving room for escaped text, metadata and context.
const MAX_EXPRESSION_BYTES: usize = 16 * 1024;
/// Retains nearby source without duplicating complete files in the batch.
const ASSERTION_CONTEXT_RADIUS: usize = 4;
/// Includes the declaration and a small header in each containing-test context.
const TEST_HEADER_LINES: usize = 2;

/// Exports at most one bounded review batch without consuming resumable state.
///
/// The returned `batchPath` and `displayPath` identify one immutable export.
pub fn next_batch(
    state_directory: &Path,
    output: &Path,
    count: usize,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let count = batch_count(count)?;
    let loaded = load(state_directory, clock)?;
    let batch = build_batch(&loaded, count);
    let output = preflight_batch_output(state_directory, output, &loaded.inventory, &[])?;
    Ok(export_batch(batch, &output, clock)?.to_string())
}

/// Builds the next bounded assertion batch from one already validated state load.
fn build_batch(loaded: &LoadedState, count: NonZeroUsize) -> Batch {
    let completed: BTreeSet<&str> = loaded
        .journal
        .rows()
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
        .take(count.get())
        .map(|assertion| {
            batch_assertion(
                assertion,
                &loaded.inventory.tests,
                &loaded.index,
                &loaded.inventory.diagnostics,
            )
        })
        .collect::<Vec<_>>();
    Batch {
        schema_version: crate::model::AUDIT_STATE_SCHEMA_VERSION,
        audit_id: loaded.state.audit_id.clone(),
        snapshot_sha256: loaded.state.snapshot_sha256.clone(),
        content_sha256: loaded.state.content_sha256.clone(),
        index_sha256: loaded.state.index_sha256.clone(),
        tool_version: loaded.state.tool_version.clone(),
        rubric_version: loaded.state.rubric_version.clone(),
        assertions: selected,
        completed: completed.len(),
        remaining,
    }
}

/// Summarizes one exported batch without repeating its assertion payload.
fn batch_summary(batch: &Batch, output: &str, display: &str, fingerprint: &str) -> Value {
    json!({
        "schemaVersion": 1,
        "batchPath": output,
        "displayPath": display,
        "auditId": batch.audit_id,
        "snapshotSha256": batch.snapshot_sha256,
        "contentSha256": batch.content_sha256,
        "indexSha256": batch.index_sha256,
        "toolVersion": batch.tool_version,
        "rubricVersion": batch.rubric_version,
        "batchFingerprint": fingerprint,
        "batchCount": batch.assertions.len(),
        "completed": batch.completed,
        "remaining": batch.remaining,
        "nextAction": next_action(batch.remaining),
        "directive": batch_directive(batch.remaining),
        "finalResponseAllowed": false
    })
}

/// Publishes an immutable pair and uses its machine record as the commit point.
fn export_batch(mut batch: Batch, output: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    create_output_parent(output)?;
    let bytes = bounded_export(&mut batch)?;
    let fingerprint = digest(&bytes);
    let display_text = compact_batch_display(&batch, &fingerprint);
    for generation in 0..=usize::MAX {
        let output = if generation == 0 {
            output.to_owned()
        } else {
            append_suffix(output, &format!(".{generation}.json"))
        };
        let display = append_suffix(&output, ".txt");
        let output_text = output_path_text(&output)?;
        let display_path = output_path_text(&display)?;
        // Hold both leases through publication. Always taking the machine lock
        // before its suffixed companion also orders overlapping output names.
        let machine = FileTransaction::open(&output, clock)?;
        let companion = FileTransaction::open(&display, clock)?;
        if !path_is_vacant(&output)? || !path_is_vacant(&display)? {
            continue;
        }
        companion.write_bytes(display_text.as_bytes())?;
        // Readers discover the pair through its machine record or the command result.
        if let Err(error) = machine.write_bytes(&bytes) {
            _ = remove_file(&display);
            return Err(error);
        }
        return Ok(batch_summary(
            &batch,
            output_text,
            display_path,
            &fingerprint,
        ));
    }
    Err(fail("io", "No unused batch output generation is available"))
}

/// Adds a suffix without losing information about the caller's filename.
fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

/// Fits the largest initial page into the exact size accepted by the reader.
fn bounded_export(batch: &mut Batch) -> Result<Vec<u8>, AppError> {
    loop {
        let bytes = to_vec_pretty(batch)?;
        if bytes.len() <= MAX_BATCH_BYTES {
            return Ok(bytes);
        }
        if batch.assertions.len() <= 1 {
            return Err(fail(
                "invalidInput",
                "Assertion batch exceeds the bounded input size",
            ));
        }
        _ = batch.assertions.pop();
    }
}

/// Protects state and source files before a journal transition is committed.
fn preflight_batch_output(
    state_directory: &Path,
    output: &Path,
    inventory: &Inventory,
    inputs: &[&Path],
) -> Result<PathBuf, AppError> {
    let resolved = resolve_output_path(output)?;
    let display = append_suffix(&resolved, ".txt");
    let requested_display = append_suffix(output, ".txt");
    for (requested, path) in [(output, resolved.as_path()), (&requested_display, &display)] {
        output_path_text(path)?;
        reject_input_alias(path, inputs)?;
        reject_source_alias(path, inventory)?;
        reject_state_destination(requested, path, state_directory)?;
    }
    Ok(resolved)
}

/// Renders only fields needed to judge one bounded assertion batch.
fn compact_batch_display(batch: &Batch, fingerprint: &str) -> String {
    let mut output = format!(
        "Audit {} snapshot {} completed {} remaining {}\nbatchFingerprint: {fingerprint}\n",
        batch.audit_id, batch.snapshot_sha256, batch.completed, batch.remaining,
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
        for evidence in &assertion.assertion.caller_evidence {
            _ = writeln!(
                output,
                "  native caller {}: {} {}:{}-{} {}",
                evidence.kind,
                evidence.test_id,
                evidence.path,
                evidence.line,
                evidence.end_line,
                evidence.expression.replace('\n', " ")
            );
        }
        for candidate in &assertion.assertion.unresolved_caller_candidates {
            _ = writeln!(
                output,
                "  unresolved caller candidate: {} {}:{}-{} {}",
                candidate.test_id,
                candidate.path,
                candidate.line,
                candidate.end_line,
                candidate.expression.replace('\n', " ")
            );
        }
        for entry in &assertion.context {
            _ = writeln!(
                output,
                "  context {}:{}-{} [{}] owner={}: {}",
                entry.path,
                entry.line,
                entry.end_line,
                entry.source_role,
                entry.owner,
                entry.excerpt
            );
        }
        for diagnostic in &assertion.diagnostics {
            _ = writeln!(
                output,
                "  diagnostic {} {}:{} {}",
                diagnostic.kind,
                diagnostic.path,
                diagnostic
                    .line
                    .map(|line| line.to_string())
                    .unwrap_or_default(),
                diagnostic.detail
            );
        }
    }
    output
}

/// Adds exact line context and test ownership to each exported source predicate.
fn batch_assertion(
    assertion: &AssertionSite,
    tests: &[TestCase],
    index: &[IndexEntry],
    diagnostics: &[ScanDiagnostic],
) -> BatchAssertion {
    let test = assertion
        .containing_test_id
        .as_ref()
        .and_then(|id| tests.iter().find(|test| test.id == *id));
    // Keep both boundaries rather than duplicating the full body of multiline predicates.
    let near = |line: usize, anchor: usize| line.abs_diff(anchor) <= ASSERTION_CONTEXT_RADIUS;
    let context = index
        .iter()
        .filter(|entry| {
            (entry.path == assertion.path
                && (near(entry.line.get(), assertion.line.get())
                    || near(entry.line.get(), assertion.end_line)))
                || test.is_some_and(|test| {
                    entry.path == test.path
                        && (test.line..=test.line.saturating_add(TEST_HEADER_LINES))
                            .contains(&entry.line)
                })
        })
        .map(|entry| BatchContextEntry {
            path: entry.path.clone(),
            line: entry.line,
            end_line: entry.end_line,
            language: entry.language.clone(),
            kind: entry.kind.clone(),
            owner: entry.owner.clone(),
            source_role: entry.source_role.clone(),
            excerpt: entry.excerpt.clone(),
            preview_truncated: entry.preview_truncated,
            repository_revision: entry.repository_revision.clone(),
            file_sha256: entry.file_sha256.clone(),
        })
        .collect();
    let relevant_diagnostics = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.path == assertion.path
                && diagnostic
                    .line
                    .is_some_and(|line| (assertion.line.get()..=assertion.end_line).contains(&line))
        })
        .cloned()
        .collect();
    let mut assertion = assertion.clone();
    if assertion.expression.len() > MAX_EXPRESSION_BYTES {
        let end = assertion
            .expression
            .floor_char_boundary(MAX_EXPRESSION_BYTES);
        assertion.expression.truncate(end);
        assertion
            .expression
            .push_str(" … [see source for full expression]");
    }
    BatchAssertion {
        assertion,
        test_name: test.map(|test| test.name.clone()),
        test_path: test.map(|test| test.path.clone()),
        test_line: test.map(|test| test.line),
        context,
        diagnostics: relevant_diagnostics,
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
    let count = batch_count(count)?;
    let loaded = load_fresh(state_directory, clock)?;
    let classification = read_classification_batch_limited(batch_path)?;
    advance_classification(
        state_directory,
        output,
        count,
        clock,
        loaded,
        classification,
        &[batch_path],
    )
}

/// Resolves compact agent rows against the exported batch and accepted journal.
///
/// Submit the fingerprint returned with the batch and retain its exact JSON bytes.
pub fn submit(
    state_directory: &Path,
    batch_path: &Path,
    submission_path: &Path,
    output: &Path,
    count: usize,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let count = batch_count(count)?;
    let loaded = load_fresh(state_directory, clock)?;
    let batch_bytes = read_bounded_json(batch_path, "exported assertion batch")?;
    let batch: Batch = parse_json(&batch_bytes, "exported assertion batch")?;
    validate_exported_batch(&batch, &loaded)?;
    let submission: CompactSubmission = parse_json(
        &read_bounded_json(submission_path, "compact submission")?,
        "compact submission",
    )?;
    require_version(submission.schema_version, "compact submission")?;
    if submission.batch_fingerprint != digest(&batch_bytes) {
        return Err(fail(
            "staleState",
            "Compact submission batchFingerprint does not match the exported assertion batch",
        ));
    }
    let classification = resolve_submission(&loaded, &batch, submission)?;
    advance_classification(
        state_directory,
        output,
        count,
        clock,
        loaded,
        classification,
        &[batch_path, submission_path],
    )
}

/// Applies the shared record-and-export sequence to one canonical batch.
///
/// The journal write commits the classifications even if export later fails.
fn advance_classification(
    state_directory: &Path,
    output: &Path,
    count: NonZeroUsize,
    clock: &dyn Clock,
    loaded: LoadedState,
    classification: ClassificationBatch,
    inputs: &[&Path],
) -> Result<String, AppError> {
    let output = preflight_batch_output(state_directory, output, &loaded.inventory, inputs)?;
    create_output_parent(&output)?;
    let (journal, recorded) =
        record_classification(state_directory, classification, clock, &loaded)?;
    let mut advanced = loaded;
    advanced.journal = journal;
    let batch = build_batch(&advanced, count);
    let next_action = next_action(batch.remaining);
    let next = export_batch(batch, &output, clock)?;
    Ok(json!({
        "schemaVersion": 1,
        "record": recorded,
        "nextBatch": next,
        "nextAction": next_action,
        "directive": transition_directive(next_action),
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
) -> Result<(ValidatedJournal, Value), AppError> {
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
    let inspect_error = |error| {
        caused_by(
            "invalidInput",
            format!("Cannot inspect {role} '{}'", path.display()),
            error,
        )
    };
    let read_error = |error| {
        caused_by(
            "invalidInput",
            format!("Cannot read {role} '{}'", path.display()),
            error,
        )
    };
    let not_regular = || {
        fail(
            "invalidInput",
            format!("{role} '{}' must be a regular file", path.display()),
        )
    };
    // Do not open a known nonregular file: opening a FIFO could block.
    if !path.metadata().map_err(inspect_error)?.is_file() {
        return Err(not_regular());
    }
    let file = File::open(path).map_err(read_error)?;
    if !file.metadata().map_err(inspect_error)?.is_file() {
        return Err(not_regular());
    }
    let mut bytes = Vec::new();
    // Detect overflow rather than accepting a valid JSON prefix of a larger input.
    file.take(u64::try_from(MAX_BATCH_BYTES)?.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(read_error)?;
    if bytes.len() > MAX_BATCH_BYTES {
        return Err(fail(
            "invalidInput",
            format!("{role} '{}' exceeds the bounded input size", path.display()),
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
) -> Result<(ValidatedJournal, Value), AppError> {
    let classification_path = resolve_output_path(&state_directory.join("classifications.json"))?;
    let classification_path_text = output_path_text(&classification_path)?;
    let transaction = FileTransaction::open(&classification_path, clock)?;
    let Some(journal_bytes) = transaction.read_bytes()? else {
        return Err(fail("invalidState", "Classification journal is missing"));
    };
    let mut journal = ValidatedJournal::from_persisted(
        parse_json(&journal_bytes, "classification journal")?,
        &loaded.state,
        &loaded.inventory,
        &loaded.index,
    )?;
    let (added, unchanged) = accept_batch(
        &mut journal,
        batch,
        &loaded.state,
        &loaded.inventory,
        &loaded.index,
    )?;
    validate_source_snapshot(&loaded.inventory, &loaded.state.repository_root)?;
    transaction.write_bytes(&to_vec_pretty(&journal.0)?)?;
    let remaining = loaded
        .inventory
        .assertions
        .len()
        .saturating_sub(journal.rows().len());
    let summary = json!({
        "schemaVersion": 1,
        "classificationPath": classification_path_text,
        "added": added,
        "unchanged": unchanged,
        "completed": journal.rows().len(),
        "remaining": remaining,
        "nextAction": next_action(remaining),
        "directive": record_directive(remaining),
        "finalResponseAllowed": false
    });
    Ok((journal, summary))
}

/// Retains only journals checked against the exact inventory and evidence index.
pub(crate) struct ValidatedJournal(ClassificationJournal);

impl ValidatedJournal {
    /// Validates persisted rows before any command uses their accepted judgments.
    pub(crate) fn from_persisted(
        journal: ClassificationJournal,
        state: &AuditState,
        inventory: &Inventory,
        index: &[IndexEntry],
    ) -> Result<Self, AppError> {
        crate::state::require_audit_version(journal.schema_version, "classification journal")?;
        if journal.audit_id != state.audit_id {
            return Err(fail(
                "invalidState",
                "Classification journal belongs to a different audit",
            ));
        }
        if journal.snapshot_sha256 != state.snapshot_sha256 {
            return Err(fail(
                "invalidState",
                "Classification journal does not match the snapshot digest",
            ));
        }
        if journal.content_sha256 != state.content_sha256
            || journal.index_sha256 != state.index_sha256
            || journal.tool_version != state.tool_version
            || journal.rubric_version != state.rubric_version
        {
            return Err(fail(
                "invalidState",
                "Classification journal does not match the audit provenance",
            ));
        }
        validate_journal(inventory, &journal, index)?;
        Ok(Self(journal))
    }

    /// Exposes accepted judgments without allowing unchecked editing.
    pub(crate) fn rows(&self) -> &[Classification] {
        &self.0.classifications
    }
}

/// Applies an entire validated batch, or leaves the existing judgments unchanged.
fn accept_batch(
    journal: &mut ValidatedJournal,
    batch: ClassificationBatch,
    state: &AuditState,
    inventory: &Inventory,
    index: &[IndexEntry],
) -> Result<(Vec<String>, Vec<String>), AppError> {
    require_version(batch.schema_version, "classification batch")?;
    if batch.audit_id != state.audit_id {
        return Err(fail(
            "staleState",
            "Classification batch belongs to a different audit",
        ));
    }
    if batch.snapshot_sha256 != state.snapshot_sha256 {
        return Err(fail(
            "staleState",
            "Classification batch does not match the snapshot digest",
        ));
    }
    if batch.assertions.is_empty() || batch.assertions.len() > MAX_BATCH_SIZE {
        return Err(fail(
            "invalidInput",
            "Classification batch must contain between 1 and 25 assertions",
        ));
    }

    let assertions: BTreeMap<&str, &AssertionSite> = inventory
        .assertions
        .iter()
        .map(|assertion| (assertion.id.as_str(), assertion))
        .collect();
    let tests: BTreeSet<&str> = inventory
        .tests
        .iter()
        .map(|test| test.id.as_str())
        .collect();
    let existing: BTreeMap<&str, &Classification> = journal
        .rows()
        .iter()
        .map(|classification| (classification.assertion_id.as_str(), classification))
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
        validate_classification(&classification, assertion, &tests, index)?;
        if let Some(previous) = existing.get(classification.assertion_id.as_str()) {
            if to_value(previous)? != to_value(&classification)? {
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
    journal.0.classifications.extend(pending);
    journal
        .0
        .classifications
        .sort_by(|left, right| left.assertion_id.cmp(&right.assertion_id));
    Ok((added, unchanged))
}

/// Checks the externally supplied export maximum once.
fn batch_count(count: usize) -> Result<NonZeroUsize, AppError> {
    NonZeroUsize::new(count)
        .filter(|count| count.get() <= MAX_BATCH_SIZE)
        .ok_or_else(|| {
            fail(
                "invalidInput",
                "Assertion batch size must be between 1 and 25",
            )
        })
}

/// Directs progress to classification or reporting, neither of which is final.
fn next_action(remaining: usize) -> &'static str {
    if remaining == 0 { "report" } else { "continue" }
}

/// Names the next operation for an exported batch without implying completion.
fn batch_directive(remaining: usize) -> &'static str {
    if remaining == 0 {
        "Run the final report now; do not claim completion from this export."
    } else {
        "INTERIM: read displayPath and classify the returned assertions now; do not end the turn."
    }
}

/// Carries the same hard gate through a combined transition receipt.
fn transition_directive(next_action: &str) -> &'static str {
    if next_action == "report" {
        "Run the final report now; do not claim completion from this transition."
    } else {
        "INTERIM: read nextBatch.displayPath and continue classification now; do not end the turn."
    }
}

/// Directs a record-only recovery path without manufacturing a batch receipt.
fn record_directive(remaining: usize) -> &'static str {
    if remaining == 0 {
        "Run the final report now; do not claim completion from this record."
    } else {
        "INTERIM: export and classify the next batch now; do not end the turn."
    }
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
        }
        ClassificationKind::Incidental => {}
    }
    let incidental_reason = match classification.kind {
        ClassificationKind::Mixed => Some("Mixed classifications require the undocumented detail"),
        ClassificationKind::Incidental => {
            Some("Incidental classifications require why no public promise applies")
        }
        _ => None,
    };
    if reason_missing_or_blank(
        classification.incidental_reason.as_deref(),
        incidental_reason.is_some(),
    ) {
        return Err(row_error(
            &assertion.id,
            "incidentalReason",
            incidental_reason.unwrap_or("An incidental reason must not be blank"),
        ));
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
    let unlinked = assertion.containing_test_id.is_none()
        && assertion.calling_test_ids.is_empty()
        && classification.calling_test_ids.is_empty();
    if reason_missing_or_blank(classification.unlinked_reason.as_deref(), unlinked) {
        return Err(row_error(
            &assertion.id,
            "callingTestIds/unlinkedReason",
            if unlinked {
                "A shared assertion without calling tests requires an explicit unlinked reason"
            } else {
                "An unlinked reason must not be blank"
            },
        ));
    }
    if !unlinked && classification.unlinked_reason.is_some() {
        return Err(row_error(
            &assertion.id,
            "unlinkedReason",
            "The field must be omitted when native or agent-confirmed callers exist",
        ));
    }
    if classification
        .unresolved_caller_reason
        .as_deref()
        .is_some_and(|reason| reason.trim().is_empty())
    {
        return Err(row_error(
            &assertion.id,
            "unresolvedCallerReason",
            "An unresolved caller reason must not be blank when supplied",
        ));
    }
    if classification
        .review_reason
        .as_deref()
        .is_some_and(|reason| reason.trim().is_empty())
    {
        return Err(row_error(
            &assertion.id,
            "reviewReason",
            "A review reason must not be blank when supplied",
        ));
    }
    Ok(())
}

/// Treats whitespace-only explanations as absent, but rejects them even if optional.
fn reason_missing_or_blank(reason: Option<&str>, required: bool) -> bool {
    reason.map_or(required, |reason| reason.trim().is_empty())
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
    if citation.end_line != entry.end_line
        || citation.owner != entry.owner
        || citation.source_role != entry.source_role
        || (citation.excerpt != entry.text && !entry.text.contains(citation.excerpt.trim()))
    {
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

#[cfg(test)]
mod fuzz;
