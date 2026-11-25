//! Complete trace, test inventory, and nonjudgmental coverage summaries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use ohno::AppError;
use serde_json::json;
use ss_ai_runtime::{Clock, fail};

use crate::model::{Classification, ClassificationKind, ModuleSummary, TestCase, TraceRow};
use crate::state::{load_fresh, validate_source_snapshot, write_bytes, write_json};

/// Validates exact classification coverage and emits durable audit reports.
pub fn report(state_directory: &Path, clock: &dyn Clock) -> Result<String, AppError> {
    let loaded = load_fresh(state_directory, clock)?;
    if loaded.journal.classifications.len() != loaded.inventory.assertions.len() {
        return Err(fail(
            "incompleteState",
            format!(
                "Cannot report before classification is complete: {} of {} assertions classified",
                loaded.journal.classifications.len(),
                loaded.inventory.assertions.len()
            ),
        ));
    }
    let classifications: BTreeMap<&str, &Classification> = loaded
        .journal
        .classifications
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
            assertion: assertion.clone(),
            classification: classification.clone(),
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
    let modules = module_summaries(&trace);
    let classification_counts = classification_counts(&trace);
    let no_counted_assertion_count = tests
        .iter()
        .filter(|test| {
            test.get("assertionCount")
                .and_then(serde_json::Value::as_u64)
                == Some(0)
        })
        .count();
    let incidental_only_count = tests
        .iter()
        .filter(|test| {
            test.get("incidentalOnly")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        })
        .count();
    let unlinked_assertion_count = trace
        .iter()
        .filter(|row| {
            row.assertion.containing_test_id.is_none()
                && row.assertion.calling_test_ids.is_empty()
                && row.classification.calling_test_ids.is_empty()
        })
        .count();
    let trace_document = json!({
        "schemaVersion": 1,
        "auditId": loaded.state.audit_id,
        "snapshotSha256": loaded.state.snapshot_sha256,
        "repositoryRevision": loaded.inventory.repository_revision,
        "classificationCounts": classification_counts,
        "modules": modules,
        "rows": trace,
    });
    let test_document = json!({
        "schemaVersion": 1,
        "auditId": loaded.state.audit_id,
        "snapshotSha256": loaded.state.snapshot_sha256,
        "tests": tests,
    });
    let report_text = render_report(
        &loaded.state.audit_id,
        &loaded.inventory.repository_revision,
        loaded.inventory.files.len(),
        &modules,
        &classification_counts,
        &tests,
        &loaded.inventory.diagnostics,
    );
    validate_source_snapshot(&loaded.inventory, &loaded.state.repository_root)?;

    let output_directory = state_directory.join("reports");
    fs::create_dir_all(&output_directory)?;
    let trace_path = output_directory.join("trace.json");
    let inventory_path = output_directory.join("test-inventory.json");
    let summary_path = output_directory.join("summary.md");
    write_json(&trace_path, &trace_document, clock)?;
    write_json(&inventory_path, &test_document, clock)?;
    write_bytes(&summary_path, report_text.as_bytes(), clock)?;
    Ok(json!({
        "schemaVersion": 1,
        "status": "complete",
        "auditId": loaded.state.audit_id,
        "testCount": tests.len(),
        "assertionCount": trace.len(),
        "classificationCounts": classification_counts,
        "noCountedAssertionCount": no_counted_assertion_count,
        "incidentalOnlyCount": incidental_only_count,
        "unlinkedAssertionCount": unlinked_assertion_count,
        "diagnosticCount": loaded.inventory.diagnostics.len(),
        "tracePath": trace_path,
        "testInventoryPath": inventory_path,
        "summaryPath": summary_path,
        "moduleCount": modules.len()
    })
    .to_string())
}

/// Counts each explicit semantic category without turning it into a quality score.
fn classification_counts(trace: &[TraceRow]) -> BTreeMap<&'static str, usize> {
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

/// Projects final assertion ownership into the complete test inventory.
fn complete_test_inventory(
    source_tests: &[TestCase],
    trace: &[TraceRow],
    repository_revision: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
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
            if (row
                .assertion
                .calling_test_ids
                .iter()
                .any(|id| id == &test.id)
                || row
                    .classification
                    .calling_test_ids
                    .iter()
                    .any(|id| id == &test.id))
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
        result.push(json!({
            "id": test.id,
            "name": test.name,
            "kind": test.kind,
            "language": test.language,
            "path": test.path,
            "line": test.line,
            "endLine": test.end_line,
            "repositoryRevision": repository_revision,
            "directAssertionIds": direct,
            "sharedAssertionIds": shared,
            "assertionCount": direct.len().saturating_add(shared.len()),
            "incidentalOnly": incidental_only
        }));
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
fn module_summaries(trace: &[TraceRow]) -> Vec<ModuleSummary> {
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
fn render_report(
    audit_id: &str,
    revision: &str,
    file_count: usize,
    modules: &[ModuleSummary],
    classification_counts: &BTreeMap<&str, usize>,
    tests: &[serde_json::Value],
    diagnostics: &[crate::model::ScanDiagnostic],
) -> String {
    let mut output = String::new();
    output.push_str("# Test audit\n\n");
    _ = writeln!(
        output,
        "Audit: `{audit_id}`  \nRevision: `{revision}`  \nIndexed files: {file_count}\n"
    );
    output.push_str(
        "These rates describe assertion evidence. They are not a test-quality score.\n\n",
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
        "| Total | {total} | {undocumented}/{total} ({:.1}%) | {incidental}/{total} ({:.1}%) |\n",
        ratio(undocumented, total),
        ratio(incidental, total)
    );
    for module in modules {
        _ = writeln!(
            output,
            "| {} | {} | {}/{} ({:.1}%) | {}/{} ({:.1}%) |\n",
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
            classification_counts.get(name).copied().unwrap_or_default()
        );
    }
    output.push_str("\nDefinitions:\n\n");
    output.push_str(
        "- `No explicitly documented promise = implicit-only + incidental` assertion sites.\n",
    );
    output.push_str("- `No public-contract check = incidental only` assertion sites.\n");
    output.push_str("- Mixed sites have a documented component and enter neither numerator.\n\n");

    let no_counted: Vec<&serde_json::Value> = tests
        .iter()
        .filter(|test| {
            test.get("assertionCount")
                .and_then(serde_json::Value::as_u64)
                == Some(0)
        })
        .collect();
    let incidental_only: Vec<&serde_json::Value> = tests
        .iter()
        .filter(|test| {
            test.get("incidentalOnly")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        })
        .collect();
    output.push_str("## Tests with no contractually motivated checks\n\n");
    _ = writeln!(output, "Incidental-only tests: {}\n", incidental_only.len());
    _ = writeln!(
        output,
        "Tests with no counted assertion: {}\n",
        no_counted.len()
    );
    for test in incidental_only {
        _ = writeln!(
            output,
            "- `{}` (`{}` at line {}) has only incidental checks.\n",
            test["name"].as_str().unwrap_or("unknown"),
            test["path"].as_str().unwrap_or("unknown"),
            test["line"].as_u64().unwrap_or_default()
        );
    }
    for test in no_counted {
        _ = writeln!(
            output,
            "- `{}` (`{}` at line {}) has no counted assertion.\n",
            test["name"].as_str().unwrap_or("unknown"),
            test["path"].as_str().unwrap_or("unknown"),
            test["line"].as_u64().unwrap_or_default()
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

/// Escapes table content without changing persisted paths or scores.
fn escape_table(value: &str) -> String {
    value.replace('|', "&#124;")
}
