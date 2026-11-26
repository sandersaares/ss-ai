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
    AuditSummary, Classification, ClassificationKind, ModuleSummary, ScanDiagnostic, TestCase,
    TraceRow,
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
    let summary = build_summary(&loaded, &trace, &tests, module_summaries(&trace), &gaps);
    let summary_text = render_summary(&summary, &gaps, &loaded.inventory.diagnostics);
    let details_document = json!({
        "schemaVersion": 1,
        "auditId": loaded.state.audit_id,
        "snapshotSha256": loaded.state.snapshot_sha256,
        "repositoryRevision": loaded.inventory.repository_revision,
        "repositoryRoot": loaded.inventory.repository_root,
        "scopePaths": loaded.inventory.scope_paths,
        "assertionTrace": trace,
        "testInventory": tests,
        "diagnostics": loaded.inventory.diagnostics,
    });
    let details_text = render_details(
        &loaded.state.audit_id,
        &loaded.inventory.repository_root,
        &loaded.inventory.repository_revision,
        &loaded.inventory.scope_paths,
        &tests,
        &trace,
        &loaded.inventory.diagnostics,
    )?;
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
        "testCount": summary.test_count,
        "assertionCount": summary.assertion_count,
        "classificationCounts": summary.classification_counts,
        "noCountedAssertionCount": summary.no_counted_assertion_count,
        "incidentalOnlyCount": summary.incidental_only_count,
        "unlinkedAssertionCount": summary.unlinked_assertion_count,
        "fallibleValueCheckCount": summary.fallible_value_check_count,
        "conditionalAssertionCount": summary.conditional_assertion_count,
        "diagnosticCount": summary.diagnostic_count,
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
) -> AuditSummary {
    let mut languages: BTreeMap<&str, usize> = BTreeMap::new();
    for file in &loaded.inventory.files {
        let count = languages.entry(file.language.as_str()).or_default();
        *count = count.saturating_add(1);
    }
    AuditSummary {
        schema_version: 1,
        status: "complete".to_owned(),
        next_action: "complete".to_owned(),
        final_response_allowed: true,
        audit_id: loaded.state.audit_id.clone(),
        snapshot_sha256: loaded.state.snapshot_sha256.clone(),
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
        .calling_test_ids
        .iter()
        .chain(&row.classification.calling_test_ids)
        .map(String::as_str)
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
    let mut result = Vec::new();
    for test in source_tests {
        let mut direct: BTreeSet<String> = test.direct_assertion_ids.iter().cloned().collect();
        let mut shared: BTreeSet<String> = test.shared_assertion_ids.iter().cloned().collect();
        for row in trace {
            if row.assertion.containing_test_id.as_deref() == Some(test.id.as_str()) {
                direct.insert(row.assertion.id.clone());
            }
            if callers(row).any(|id| id == test.id)
                && row.assertion.containing_test_id.as_deref() != Some(test.id.as_str())
            {
                shared.insert(row.assertion.id.clone());
            }
        }
        let assertion_ids = direct.iter().chain(shared.iter()).collect::<Vec<_>>();
        let incidental_only = !assertion_ids.is_empty()
            && assertion_ids.iter().all(|id| {
                trace
                    .iter()
                    .find(|row| row.assertion.id == **id)
                    .is_some_and(|row| row.classification.kind == ClassificationKind::Incidental)
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
        "Audit: `{audit_id}`  \nSnapshot: `{}`  \nRepository: `{}`  \nRevision: `{revision}`  \nScope: `{}`  \nIndexed files: {file_count}\n",
        summary.snapshot_sha256,
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
    _ = writeln!(output, "- Scan diagnostics: {}\n", summary.diagnostic_count);
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

/// Renders the complete human-readable companion to `details.json`.
fn render_details(
    audit_id: &str,
    repository_root: &str,
    revision: &str,
    scope_paths: &[String],
    tests: &[TestReportRow<'_>],
    trace: &[TraceRow<'_>],
    diagnostics: &[ScanDiagnostic],
) -> Result<String, AppError> {
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
    Ok(output)
}

/// Escapes table delimiters in Markdown without changing the report data.
fn escape_table(value: &str) -> String {
    value.replace('|', "&#124;")
}
