//! Complete trace, test inventory, and nonjudgmental coverage summaries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde::Serialize;
use serde_json::{json, to_string};
use ss_ai_runtime::{Clock, LockMode, ResourceLock, fail};

use crate::model::{
    AuditSummary, Classification, ClassificationKind, ConsistencyDiagnostic, InventoryCorrection,
    ModuleSummary, ScanDiagnostic, TestCase, TraceRow,
};
use crate::state::{
    LoadedState, load_fresh, output_path, output_path_text, path_is_vacant,
    validate_source_snapshot, write_bytes, write_json,
};

/// Validates exact classification coverage and emits durable audit reports.
///
/// The returned paths identify a complete, immutable report generation.
pub fn report(state_directory: &Path, clock: &dyn Clock) -> Result<String, AppError> {
    let loaded = load_fresh(state_directory, clock)?;
    if loaded.journal.rows().len() != loaded.inventory.assertions.len() {
        return Err(fail(
            "incompleteState",
            format!(
                "Cannot report before classification is complete: {} of {} assertions classified",
                loaded.journal.rows().len(),
                loaded.inventory.assertions.len()
            ),
        ));
    }
    let classifications: BTreeMap<&str, &Classification> = loaded
        .journal
        .rows()
        .iter()
        .map(|classification| (classification.assertion_id.as_str(), classification))
        .collect();
    let mut trace = Vec::new();
    for assertion in &loaded.inventory.assertions {
        let classification = classifications
            .get(assertion.id.as_str())
            .copied()
            .ok_or_else(|| {
                fail(
                    "incompleteState",
                    format!("Assertion '{}' has no classified trace row", assertion.id),
                )
            })?;
        trace.push(TraceRow {
            assertion,
            classification,
        });
    }
    if trace.len() != loaded.inventory.assertions.len() {
        return Err(fail(
            "invalidState",
            "Trace does not contain exactly one row per assertion",
        ));
    }
    let tests = complete_test_inventory(
        &loaded.inventory.tests,
        &trace,
        &loaded.inventory.repository_revision,
    )?;
    let gaps = TestGaps::collect(&tests);
    let consistency_diagnostics = consistency_diagnostics(&trace);
    let summary = build_summary(
        &loaded,
        &trace,
        &tests,
        module_summaries(&trace),
        &gaps,
        consistency_diagnostics.len(),
    );
    let summary_text = render_summary(&summary, &gaps, &loaded.inventory.diagnostics);
    let details_document = json!({
        "schemaVersion": 3,
        "auditId": loaded.state.audit_id,
        "snapshotSha256": loaded.state.snapshot_sha256,
        "contentSha256": loaded.state.content_sha256,
        "indexSha256": loaded.state.index_sha256,
        "toolVersion": loaded.state.tool_version,
        "rubricVersion": loaded.state.rubric_version,
        "contractProfileSha256": loaded.state.contract_profile_sha256,
        "contractProfile": loaded.inventory.contract_profile,
        "repositoryRevision": loaded.inventory.repository_revision,
        "repositoryRoot": loaded.inventory.repository_root,
        "scopePaths": loaded.inventory.scope_paths,
        "assertionTrace": trace,
        "testInventory": tests,
        "diagnostics": loaded.inventory.diagnostics,
        "corrections": loaded.inventory.corrections,
        "consistencyDiagnostics": consistency_diagnostics,
    });
    let details_text = render_details(&DetailRender {
        audit_id: &loaded.state.audit_id,
        repository_root: &loaded.inventory.repository_root,
        revision: &loaded.inventory.repository_revision,
        scope_paths: &loaded.inventory.scope_paths,
        tests: &tests,
        trace: &trace,
        diagnostics: &loaded.inventory.diagnostics,
        corrections: &loaded.inventory.corrections,
        consistency_diagnostics: &consistency_diagnostics,
    })?;
    let output_directory = publish_report(state_directory, clock, |staging| {
        write_json(&staging.join("details.json"), &details_document, clock)?;
        write_bytes(&staging.join("details.md"), details_text.as_bytes(), clock)?;
        // Even a stranded staging directory should not advertise completion when
        // any detail failed. The directory move still commits all artifacts at once.
        write_bytes(&staging.join("summary.md"), summary_text.as_bytes(), clock)?;
        write_json(&staging.join("summary.json"), &summary, clock)?;
        validate_source_snapshot(&loaded.inventory, &loaded.state.repository_root)
    })?;
    Ok(json!({
        "schemaVersion": summary.schema_version,
        "status": summary.status,
        "nextAction": summary.next_action,
        "finalResponseAllowed": summary.final_response_allowed,
        "auditId": summary.audit_id,
        "contentSha256": summary.content_sha256,
        "indexSha256": summary.index_sha256,
        "toolVersion": summary.tool_version,
        "rubricVersion": summary.rubric_version,
        "contractProfileSha256": summary.contract_profile_sha256,
        "testCount": summary.test_count,
        "assertionCount": summary.assertion_count,
        "classificationCounts": summary.classification_counts,
        "noCountedAssertionCount": summary.no_counted_assertion_count,
        "incidentalOnlyCount": summary.incidental_only_count,
        "unlinkedAssertionCount": summary.unlinked_assertion_count,
        "fallibleValueCheckCount": summary.fallible_value_check_count,
        "conditionalAssertionCount": summary.conditional_assertion_count,
        "diagnosticCount": summary.diagnostic_count,
        "correctionCount": summary.correction_count,
        "consistencyDiagnosticCount": summary.consistency_diagnostic_count,
        "summaryJsonPath": output_path_text(&output_directory.join("summary.json"))?,
        "summaryPath": output_path_text(&output_directory.join("summary.md"))?,
        "detailsJsonPath": output_path_text(&output_directory.join("details.json"))?,
        "detailsPath": output_path_text(&output_directory.join("details.md"))?,
        "moduleCount": summary.module_count
    })
    .to_string())
}

/// Computes the metrics shared by the human report, JSON summary and receipt.
fn build_summary(
    loaded: &LoadedState,
    trace: &[TraceRow<'_>],
    tests: &[TestReportRow<'_>],
    modules: Vec<ModuleSummary>,
    gaps: &TestGaps<'_>,
    consistency_diagnostic_count: usize,
) -> AuditSummary {
    let mut languages: BTreeMap<&str, usize> = BTreeMap::new();
    for file in &loaded.inventory.files {
        let count = languages.entry(file.language.as_str()).or_default();
        *count = count.saturating_add(1);
    }
    AuditSummary {
        schema_version: 3,
        status: "complete".to_owned(),
        next_action: "complete".to_owned(),
        final_response_allowed: true,
        audit_id: loaded.state.audit_id.clone(),
        snapshot_sha256: loaded.state.snapshot_sha256.clone(),
        content_sha256: loaded.state.content_sha256.clone(),
        index_sha256: loaded.state.index_sha256.clone(),
        tool_version: loaded.state.tool_version.clone(),
        rubric_version: loaded.state.rubric_version.clone(),
        contract_profile_sha256: loaded.state.contract_profile_sha256.clone(),
        repository_root: loaded.inventory.repository_root.clone(),
        repository_revision: loaded.inventory.repository_revision.clone(),
        scope_paths: loaded.inventory.scope_paths.clone(),
        indexed_file_count: loaded.inventory.files.len(),
        source_languages: languages
            .into_iter()
            .map(|(name, count)| (name.to_owned(), count))
            .collect(),
        test_count: tests.len(),
        assertion_count: trace.len(),
        module_count: modules.len(),
        classification_counts: classification_counts(trace)
            .into_iter()
            .map(|(name, count)| (name.to_owned(), count))
            .collect(),
        no_counted_assertion_count: gaps.no_counted.len(),
        incidental_only_count: gaps.incidental_only.len(),
        unlinked_assertion_count: trace
            .iter()
            .filter(|row| {
                row.assertion.containing_test_id.is_none() && callers(row).next().is_none()
            })
            .count(),
        fallible_value_check_count: trace
            .iter()
            .filter(|row| row.assertion.kind == "fallibleValueCheck")
            .count(),
        conditional_assertion_count: loaded
            .inventory
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == "conditionalAssertion")
            .count(),
        diagnostic_count: loaded.inventory.diagnostics.len(),
        correction_count: loaded.inventory.corrections.len(),
        consistency_diagnostic_count,
        modules,
    }
}

/// Commits a complete report generation by renaming its private staging directory.
fn publish_report(
    state_directory: &Path,
    clock: &dyn Clock,
    write_artifacts: impl FnOnce(&Path) -> Result<(), AppError>,
) -> Result<PathBuf, AppError> {
    let publication = output_path(&state_directory.join("reports").join("publication"))?;
    let directory = publication.parent().ok_or_else(|| {
        fail(
            "invalidState",
            "Report publication requires a parent directory",
        )
    })?;
    let _publication = ResourceLock::open(&publication, LockMode::Exclusive, clock)?;
    let destination = unused_report_path(directory)?;
    output_path_text(&destination)?;
    let staging = directory.join(".staging");
    match fs::symlink_metadata(&staging) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(fail(
                "invalidState",
                "Report staging path must be a directory",
            ));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir_all(&staging)?,
        Err(error) => return Err(error.into()),
    }

    let result = (|| {
        write_artifacts(&staging)?;
        fs::rename(&staging, &destination)?;
        Ok(destination)
    })();
    if result.is_err() {
        // The staging directory is exclusive to this publication. Never remove
        // a published generation or the retained publication lock on failure.
        _ = fs::remove_dir_all(&staging);
    }
    result
}

/// Finds an immutable destination without replacing earlier report results.
fn unused_report_path(directory: &Path) -> Result<PathBuf, AppError> {
    for generation in 1..=usize::MAX {
        let path = directory.join(format!("generation-{generation}"));
        if path_is_vacant(&path)? {
            return Ok(path);
        }
    }
    Err(fail("io", "No unused report generation is available"))
}

/// Combines native and agent attribution for both human and machine views.
fn callers<'a>(row: &TraceRow<'a>) -> impl Iterator<Item = &'a str> {
    row.assertion
        .containing_test_id
        .iter()
        .map(String::as_str)
        .chain(
            row.assertion
                .calling_test_ids
                .iter()
                .chain(&row.classification.calling_test_ids)
                .map(String::as_str),
        )
}

/// Counts each explicit semantic category without turning it into a quality score.
fn classification_counts(trace: &[TraceRow<'_>]) -> BTreeMap<&'static str, usize> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for row in trace {
        let name = match row.classification.kind {
            ClassificationKind::Documented => "Documented",
            ClassificationKind::Implicit => "Implicit",
            ClassificationKind::Mixed => "Mixed",
            ClassificationKind::Incidental => "Incidental",
        };
        let count = counts.entry(name).or_default();
        *count = count.saturating_add(1);
    }
    counts
}

/// Shares complete test attribution and coverage between both report formats.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestReportRow<'a> {
    id: &'a str,
    name: &'a str,
    kind: &'a str,
    language: &'a str,
    path: &'a str,
    line: NonZeroUsize,
    end_line: usize,
    repository_revision: &'a str,
    direct_assertion_ids: BTreeSet<String>,
    shared_assertion_ids: BTreeSet<String>,
    assertion_count: usize,
    incidental_only: bool,
}

/// Retains the rows that explain the two test-level gap counts.
struct TestGaps<'a> {
    no_counted: Vec<&'a TestReportRow<'a>>,
    incidental_only: Vec<&'a TestReportRow<'a>>,
}

impl<'a> TestGaps<'a> {
    /// Selects named gaps once for both the summary counts and Markdown lists.
    fn collect(tests: &'a [TestReportRow<'a>]) -> Self {
        let mut gaps = Self {
            no_counted: Vec::new(),
            incidental_only: Vec::new(),
        };
        for test in tests {
            if test.assertion_count == 0 {
                gaps.no_counted.push(test);
            } else if test.incidental_only {
                gaps.incidental_only.push(test);
            }
        }
        gaps
    }
}

/// Projects final assertion ownership into the complete test inventory.
fn complete_test_inventory<'a>(
    source_tests: &'a [TestCase],
    trace: &[TraceRow<'_>],
    repository_revision: &'a str,
) -> Result<Vec<TestReportRow<'a>>, AppError> {
    let test_ids: BTreeSet<&str> = source_tests.iter().map(|test| test.id.as_str()).collect();
    if test_ids.len() != source_tests.len() {
        return Err(fail(
            "invalidState",
            "Test inventory contains duplicate IDs",
        ));
    }
    let classifications = trace
        .iter()
        .map(|row| (row.assertion.id.as_str(), row.classification.kind))
        .collect::<BTreeMap<_, _>>();
    let mut direct_by_test = BTreeMap::<&str, BTreeSet<&str>>::new();
    let mut shared_by_test = BTreeMap::<&str, BTreeSet<&str>>::new();
    for test in source_tests {
        let direct = direct_by_test.entry(test.id.as_str()).or_default();
        direct.extend(test.direct_assertion_ids.iter().map(String::as_str));
        let shared = shared_by_test.entry(test.id.as_str()).or_default();
        shared.extend(test.shared_assertion_ids.iter().map(String::as_str));
    }
    for row in trace {
        if let Some(test_id) = row.assertion.containing_test_id.as_deref() {
            direct_by_test
                .entry(test_id)
                .or_default()
                .insert(row.assertion.id.as_str());
        }
        for test_id in callers(row) {
            if row.assertion.containing_test_id.as_deref() != Some(test_id) {
                shared_by_test
                    .entry(test_id)
                    .or_default()
                    .insert(row.assertion.id.as_str());
            }
        }
    }
    let mut result = Vec::new();
    for test in source_tests {
        let direct: BTreeSet<String> = direct_by_test
            .remove(test.id.as_str())
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let shared: BTreeSet<String> = shared_by_test
            .remove(test.id.as_str())
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let assertion_ids = direct.iter().chain(shared.iter()).collect::<Vec<_>>();
        let incidental_only = !assertion_ids.is_empty()
            && assertion_ids.iter().all(|id| {
                classifications.get(id.as_str()) == Some(&ClassificationKind::Incidental)
            });
        result.push(TestReportRow {
            id: &test.id,
            name: &test.name,
            kind: &test.kind,
            language: &test.language,
            path: &test.path,
            line: test.line,
            end_line: test.end_line,
            repository_revision,
            assertion_count: direct.len().saturating_add(shared.len()),
            direct_assertion_ids: direct,
            shared_assertion_ids: shared,
            incidental_only,
        });
    }
    if result.len() != source_tests.len() {
        return Err(fail(
            "invalidState",
            "Final test inventory does not represent every inventoried test",
        ));
    }
    Ok(result)
}

/// Calculates descriptive contract-evidence rates for each path module.
fn module_summaries(trace: &[TraceRow<'_>]) -> Vec<ModuleSummary> {
    let mut modules: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for row in trace {
        let module = Path::new(&row.assertion.path).parent().map_or_else(
            || "<root>".to_owned(),
            |path| {
                if path.as_os_str().is_empty() {
                    "<root>".to_owned()
                } else {
                    path.to_string_lossy().replace('\\', "/")
                }
            },
        );
        let value = modules.entry(module).or_default();
        value.0 = value.0.saturating_add(1);
        if matches!(
            row.classification.kind,
            ClassificationKind::Implicit | ClassificationKind::Incidental
        ) {
            value.1 = value.1.saturating_add(1);
        }
        if row.classification.kind == ClassificationKind::Incidental {
            value.2 = value.2.saturating_add(1);
        }
    }
    modules
        .into_iter()
        .map(
            |(module, (total, undocumented, incidental))| ModuleSummary {
                module,
                assertion_count: total,
                no_explicitly_documented_promise_count: undocumented,
                no_public_contract_check_count: incidental,
                no_explicitly_documented_promise_rate: ratio(undocumented, total),
                no_public_contract_check_rate: ratio(incidental, total),
            },
        )
        .collect()
}

/// Expresses a finite assertion-site percentage without dividing by zero.
fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        let numerator = f64::from(u32::try_from(numerator).unwrap_or(u32::MAX));
        let denominator = f64::from(u32::try_from(denominator).unwrap_or(u32::MAX));
        numerator * 100.0 / denominator
    }
}

/// Renders a concise human report with denominators and execution diagnostics.
fn render_summary(
    summary: &AuditSummary,
    gaps: &TestGaps<'_>,
    diagnostics: &[ScanDiagnostic],
) -> String {
    let mut output = String::new();
    let audit_id = &summary.audit_id;
    let repository_root = &summary.repository_root;
    let revision = &summary.repository_revision;
    let scope_paths = &summary.scope_paths;
    let modules = &summary.modules;
    let file_count = summary.indexed_file_count;
    output.push_str("# Test audit\n\n");
    _ = writeln!(output, "- Schema version: {}", summary.schema_version);
    _ = writeln!(output, "- Status: {}", summary.status);
    _ = writeln!(output, "- Next action: {}", summary.next_action);
    _ = writeln!(
        output,
        "- Final response allowed: {}\n",
        summary.final_response_allowed
    );
    _ = writeln!(
        output,
        "Audit: `{audit_id}`  \nSnapshot: `{}`  \nContent digest: `{}`  \nIndex digest: `{}`  \nTool version: `{}`  \nRubric version: `{}`  \nContract profile digest: `{}`  \nRepository: `{}`  \nRevision: `{revision}`  \nScope: `{}`  \nIndexed files: {file_count}\n",
        summary.snapshot_sha256,
        summary.content_sha256,
        summary.index_sha256,
        summary.tool_version,
        summary.rubric_version,
        summary.contract_profile_sha256,
        repository_root,
        if scope_paths.is_empty() {
            "<repository>".to_owned()
        } else {
            scope_paths.join(", ")
        }
    );
    output.push_str(
        "These rates describe assertion evidence. They are not a test-quality score.\n\n",
    );
    output.push_str("## Coverage summary\n\n");
    _ = writeln!(output, "- Tests: {}", summary.test_count);
    _ = writeln!(output, "- Assertions: {}", summary.assertion_count);
    _ = writeln!(output, "- Modules: {}", summary.module_count);
    _ = writeln!(output, "- Indexed files: {}", summary.indexed_file_count);
    let languages = summary
        .source_languages
        .iter()
        .map(|(language, count)| format!("{language}: {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    _ = writeln!(
        output,
        "- Source languages: {}\n",
        if languages.is_empty() {
            "none".to_owned()
        } else {
            languages
        }
    );
    _ = writeln!(
        output,
        "- Tests with no counted assertion: {}",
        summary.no_counted_assertion_count
    );
    _ = writeln!(
        output,
        "- Incidental-only tests: {}",
        summary.incidental_only_count
    );
    _ = writeln!(
        output,
        "- Unlinked assertion sites: {}",
        summary.unlinked_assertion_count
    );
    _ = writeln!(
        output,
        "- Fallible-value candidates: {}",
        summary.fallible_value_check_count
    );
    _ = writeln!(
        output,
        "- Conditional assertion diagnostics: {}",
        summary.conditional_assertion_count
    );
    _ = writeln!(output, "- Scan diagnostics: {}", summary.diagnostic_count);
    _ = writeln!(
        output,
        "- Inventory corrections: {}",
        summary.correction_count
    );
    _ = writeln!(
        output,
        "- Consistency diagnostics: {}\n",
        summary.consistency_diagnostic_count
    );
    output.push_str(
        "No explicitly documented promise counts plausible guarantees without exact wording (`Implicit`) \
         and checks that no public promise requires (`Incidental`). No public-contract check counts \
         only `Incidental` sites. `Mixed` sites have a documented component, so neither numerator includes them.\n\n",
    );
    output.push_str(
        "| Module | Assertions | No explicitly documented promise | No public-contract check |\n",
    );
    output.push_str("| --- | ---: | ---: | ---: |\n");
    let total = modules
        .iter()
        .map(|module| module.assertion_count)
        .sum::<usize>();
    let undocumented = modules
        .iter()
        .map(|module| module.no_explicitly_documented_promise_count)
        .sum::<usize>();
    let incidental = modules
        .iter()
        .map(|module| module.no_public_contract_check_count)
        .sum::<usize>();
    _ = writeln!(
        output,
        "| Total | {total} | {undocumented}/{total} ({:.1}%) | {incidental}/{total} ({:.1}%) |",
        ratio(undocumented, total),
        ratio(incidental, total)
    );
    for module in modules {
        _ = writeln!(
            output,
            "| {} | {} | {}/{} ({:.1}%) | {}/{} ({:.1}%) |",
            escape_table(&module.module),
            module.assertion_count,
            module.no_explicitly_documented_promise_count,
            module.assertion_count,
            module.no_explicitly_documented_promise_rate,
            module.no_public_contract_check_count,
            module.assertion_count,
            module.no_public_contract_check_rate
        );
    }
    output.push_str("\n## Classification totals\n\n");
    for name in ["Documented", "Implicit", "Mixed", "Incidental"] {
        _ = writeln!(
            output,
            "- {name}: {}/{total}",
            summary
                .classification_counts
                .get(name)
                .copied()
                .unwrap_or_default()
        );
    }
    output.push_str("\nDefinitions:\n\n");
    output.push_str(
        "- `No explicitly documented promise = implicit-only + incidental` assertion sites.\n",
    );
    output.push_str("- `No public-contract check = incidental only` assertion sites.\n");
    output.push('\n');

    output.push_str("## Tests with no contractually motivated checks\n\n");
    _ = writeln!(
        output,
        "Incidental-only tests: {}\n",
        gaps.incidental_only.len()
    );
    _ = writeln!(
        output,
        "Tests with no counted assertion: {}\n",
        gaps.no_counted.len()
    );
    for test in &gaps.incidental_only {
        _ = writeln!(
            output,
            "- `{}` (`{}` at line {}) has only incidental checks.\n",
            test.name, test.path, test.line
        );
    }
    for test in &gaps.no_counted {
        _ = writeln!(
            output,
            "- `{}` (`{}` at line {}) has no counted assertion.\n",
            test.name, test.path, test.line
        );
    }
    if diagnostics.is_empty() {
        output.push_str("\n## Diagnostics\n\nNo scan diagnostics.\n");
    } else {
        output.push_str("\n## Diagnostics\n\n");
        for diagnostic in diagnostics {
            _ = writeln!(
                output,
                "- `{}` at `{}`{}: {}\n",
                diagnostic.kind,
                diagnostic.path,
                diagnostic
                    .line
                    .map(|line| format!(":{line}"))
                    .unwrap_or_default(),
                diagnostic.detail
            );
        }
    }
    output
}

/// Supplies the complete evidence set for one human detail rendering.
#[derive(Clone, Copy)]
struct DetailRender<'a> {
    audit_id: &'a str,
    repository_root: &'a str,
    revision: &'a str,
    scope_paths: &'a [String],
    tests: &'a [TestReportRow<'a>],
    trace: &'a [TraceRow<'a>],
    diagnostics: &'a [ScanDiagnostic],
    corrections: &'a [InventoryCorrection],
    consistency_diagnostics: &'a [ConsistencyDiagnostic],
}

/// Renders the complete human-readable companion to `details.json`.
fn render_details(input: &DetailRender<'_>) -> Result<String, AppError> {
    let DetailRender {
        audit_id,
        repository_root,
        revision,
        scope_paths,
        tests,
        trace,
        diagnostics,
        corrections,
        consistency_diagnostics,
    } = *input;
    let mut output = String::new();
    _ = writeln!(
        output,
        "# Test audit details\n\nAudit: `{audit_id}`  \nRepository: `{repository_root}`  \nRevision: `{revision}`  \nScope: `{}`\n",
        if scope_paths.is_empty() {
            "<repository>".to_owned()
        } else {
            scope_paths.join(", ")
        }
    );
    output.push_str("\n## Assertion trace\n\n");
    for row in trace {
        let callers: BTreeSet<_> = callers(row).collect();
        _ = writeln!(
            output,
            "### {} — {:?} — `{}:{}`\n",
            row.assertion.id, row.classification.kind, row.assertion.path, row.assertion.line
        );
        _ = writeln!(
            output,
            "- Actual outcome: {}",
            row.classification.actual_outcome
        );
        _ = writeln!(output, "- Rationale: {}", row.classification.rationale);
        _ = writeln!(
            output,
            "- Evidence status: {:?}",
            row.classification.evidence_status
        );
        _ = writeln!(
            output,
            "- Source: {}",
            row.assertion.expression.replace('\n', " ")
        );
        _ = writeln!(
            output,
            "- Calling tests: {}",
            if callers.is_empty() {
                "none".to_owned()
            } else {
                callers.into_iter().collect::<Vec<_>>().join(", ")
            }
        );
        if let Some(reason) = &row.classification.incidental_reason {
            _ = writeln!(output, "- Incidental reason: {reason}");
        }
        if let Some(reason) = &row.classification.unlinked_reason {
            _ = writeln!(output, "- Unlinked reason: {reason}");
        }
        if let Some(reason) = &row.classification.unresolved_caller_reason {
            _ = writeln!(output, "- Unresolved caller candidate: {reason}");
        }
        if let Some(reason) = &row.classification.review_reason {
            _ = writeln!(output, "- Review reason: {reason}");
        }
        for evidence in &row.assertion.caller_evidence {
            _ = writeln!(
                output,
                "- Native caller evidence ({}): `{}` `{}:{}`",
                evidence.kind, evidence.test_id, evidence.path, evidence.line
            );
        }
        for candidate in &row.assertion.unresolved_caller_candidates {
            _ = writeln!(
                output,
                "- Unresolved caller candidate: `{}` `{}:{}` {}",
                candidate.test_id, candidate.path, candidate.line, candidate.expression
            );
        }
        for citation in &row.classification.citations {
            _ = writeln!(
                output,
                "- Citation: `{}:{}` — {}",
                citation.path, citation.line, citation.excerpt
            );
        }
        output.push('\n');
    }

    output.push_str("## Test inventory\n\n");
    for test in tests {
        _ = writeln!(
            output,
            "- `{}`: {} (`{}:{}`), direct={}, shared={}, incidentalOnly={}\n",
            test.id,
            test.name,
            test.path,
            test.line,
            to_string(&test.direct_assertion_ids)?,
            to_string(&test.shared_assertion_ids)?,
            test.incidental_only
        );
    }

    output.push_str("\n## Diagnostics\n\n");
    if diagnostics.is_empty() {
        output.push_str("No scan diagnostics.\n");
    } else {
        for diagnostic in diagnostics {
            _ = writeln!(
                output,
                "- `{}` at `{}`{}: {}",
                diagnostic.kind,
                diagnostic.path,
                diagnostic
                    .line
                    .map(|line| format!(":{line}"))
                    .unwrap_or_default(),
                diagnostic.detail
            );
        }
    }
    output.push_str("\n## Inventory corrections\n\n");
    if corrections.is_empty() {
        output.push_str("No inventory corrections.\n");
    } else {
        for correction in corrections {
            _ = writeln!(
                output,
                "- `{}` at `{}:{}-{}` ({}, {}): {}",
                correction.assertion_id,
                correction.path,
                correction.line,
                correction.end_line,
                to_string(&correction.candidate_role)?,
                to_string(&correction.candidate_disposition)?,
                correction.reason
            );
        }
    }
    output.push_str("\n## Consistency diagnostics\n\n");
    if consistency_diagnostics.is_empty() {
        output.push_str("No consistency diagnostics.\n");
    } else {
        for diagnostic in consistency_diagnostics {
            _ = writeln!(
                output,
                "- `{}` / {} ({}): {}",
                diagnostic.priority,
                diagnostic.kind,
                diagnostic.assertion_ids.join(", "),
                diagnostic.detail
            );
        }
    }
    Ok(output)
}

/// Flags review patterns without changing semantic judgments or rates.
fn consistency_diagnostics(trace: &[TraceRow<'_>]) -> Vec<ConsistencyDiagnostic> {
    let mut diagnostics = Vec::new();
    if !trace.is_empty()
        && !trace
            .iter()
            .any(|row| row.classification.kind == ClassificationKind::Mixed)
    {
        diagnostics.push(ConsistencyDiagnostic {
            priority: "low".to_owned(),
            kind: "noMixedSites".to_owned(),
            assertion_ids: Vec::new(),
            detail: "No site uses Mixed; confirm compound documented and undocumented constraints were split in the reasoning rather than forced into one label".to_owned(),
        });
    }
    let mut by_rationale: BTreeMap<&str, Vec<&TraceRow<'_>>> = BTreeMap::new();
    let mut by_expression: BTreeMap<&str, Vec<&TraceRow<'_>>> = BTreeMap::new();
    for row in trace {
        by_rationale
            .entry(row.classification.rationale.as_str())
            .or_default()
            .push(row);
        by_expression
            .entry(row.assertion.expression.as_str())
            .or_default()
            .push(row);
    }
    for rows in by_rationale.values() {
        let predicates = rows
            .iter()
            .map(|row| row.assertion.expression.as_str())
            .collect::<BTreeSet<_>>();
        if predicates.len() > 1 {
            diagnostics.push(ConsistencyDiagnostic {
                priority: "medium".to_owned(),
                kind: "sharedRationaleDifferentPredicates".to_owned(),
                assertion_ids: row_ids(rows),
                detail: "One rationale explains different source predicates; verify that its contract claim applies to each".to_owned(),
            });
        }
        let classifications = rows
            .iter()
            .map(|row| row.classification.kind)
            .collect::<BTreeSet<_>>();
        if rows.len() >= 5
            && classifications.len() == 1
            && rows
                .first()
                .is_some_and(|row| row.classification.rationale.chars().count() <= 120)
        {
            diagnostics.push(ConsistencyDiagnostic {
                priority: "medium".to_owned(),
                kind: "repeatedShortRationale".to_owned(),
                assertion_ids: row_ids(rows),
                detail: "This short rationale repeats across many sites; add the discriminating operation, condition, or permitted alternative before relying on it".to_owned(),
            });
        }
    }
    for rows in by_expression.values() {
        let classifications = rows
            .iter()
            .map(|row| row.classification.kind)
            .collect::<BTreeSet<_>>();
        if classifications.len() > 1 {
            diagnostics.push(ConsistencyDiagnostic {
                priority: "high".to_owned(),
                kind: "samePredicateDifferentClassifications".to_owned(),
                assertion_ids: row_ids(rows),
                detail: "The same predicate has different classifications; record the distinct applicability condition".to_owned(),
            });
        }
    }

    let mut by_citation: BTreeMap<(String, NonZeroUsize, NonZeroUsize), Vec<&TraceRow<'_>>> =
        BTreeMap::new();
    for row in trace {
        for citation in &row.classification.citations {
            by_citation
                .entry((citation.path.clone(), citation.line, citation.end_line))
                .or_default()
                .push(row);
        }
    }
    for rows in by_citation.values() {
        let classifications = rows
            .iter()
            .map(|row| row.classification.kind)
            .collect::<BTreeSet<_>>();
        let predicates = rows
            .iter()
            .map(|row| row.assertion.expression.as_str())
            .collect::<BTreeSet<_>>();
        if classifications.len() > 1 {
            diagnostics.push(ConsistencyDiagnostic {
                priority: "high".to_owned(),
                kind: "citationSupportsConflictingClassifications".to_owned(),
                assertion_ids: row_ids(rows),
                detail: "One evidence range supports conflicting classifications; verify subject, conditions, and extracted facts".to_owned(),
            });
        }
        if predicates.len() >= 3 {
            diagnostics.push(ConsistencyDiagnostic {
                priority: "medium".to_owned(),
                kind: "broadCitationReuse".to_owned(),
                assertion_ids: row_ids(rows),
                detail: "One evidence range supports several different predicates; verify each operation-specific relationship".to_owned(),
            });
        }
    }
    diagnostics.sort_by(|left, right| {
        (
            priority_rank(&left.priority),
            &left.kind,
            &left.assertion_ids,
            &left.detail,
        )
            .cmp(&(
                priority_rank(&right.priority),
                &right.kind,
                &right.assertion_ids,
                &right.detail,
            ))
    });
    diagnostics.dedup_by(|left, right| {
        left.kind == right.kind && left.assertion_ids == right.assertion_ids
    });
    diagnostics
}

/// Orders actionable consistency defects before advisory sampling prompts.
fn priority_rank(priority: &str) -> usize {
    match priority {
        "high" => 0,
        "medium" => 1,
        _ => 2,
    }
}

/// Returns stable source-site identities for one consistency warning.
fn row_ids(rows: &[&TraceRow<'_>]) -> Vec<String> {
    rows.iter()
        .map(|row| row.assertion.id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Escapes table delimiters in Markdown without changing the report data.
fn escape_table(value: &str) -> String {
    value.replace('|', "&#124;")
}
