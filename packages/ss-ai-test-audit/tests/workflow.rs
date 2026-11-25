//! Exercises one complete local audit from source inventory through final reports.
#![expect(
    clippy::indexing_slicing,
    reason = "The test selects fixed fields from its own generated records."
)]

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, Process, ProcessOutput};
use ss_ai_test_audit::{initialize, next_batch, record_batch, report, search};
use tempfile::TempDir;

/// Supplies a deterministic initialization time.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2026-11-25T01:02:03Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Audit workflow tests must not wait")
    }
}

/// Rejects process use for a deliberately unversioned fixture.
struct NoProcess;

impl Process for NoProcess {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        panic!("Unversioned test fixtures must not launch processes")
    }
}

/// Supplies deterministic Git identity for a nested-repository-path contract.
struct GitProcess {
    root: String,
}

impl Process for GitProcess {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        panic!("The test audit must use the isolated Git boundary")
    }

    fn isolated_git(&self, _: &Path, arguments: &[String]) -> io::Result<ProcessOutput> {
        let stdout = if arguments.first().map(String::as_str) == Some("rev-parse")
            && arguments.get(1).map(String::as_str) == Some("--show-toplevel")
        {
            format!("{}\n", self.root).into_bytes()
        } else if arguments.first().map(String::as_str) == Some("rev-parse")
            && arguments.get(1).map(String::as_str) == Some("HEAD")
        {
            b"0123456789abcdef0123456789abcdef01234567\n".to_vec()
        } else {
            panic!("Unexpected Git arguments")
        };
        Ok(ProcessOutput::new(true, stdout, Vec::new()))
    }
}

/// Creates the fixture and returns its repository and separate state directories.
fn fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = TempDir::new().unwrap();
    let repository = root.path().join("repository");
    let state = root.path().join("state");
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(
        repository.join("src/lib.rs"),
        r#"//! Example crate.

/// Returns whether the operation is ready.
///
/// ```
/// assert!(ready());
/// ```
pub fn ready() -> bool {
    true
}

fn shared(value: usize) {
    assert_eq!(value, 2);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented() {
        assert!(ready());
    }

    #[test]
    fn shared_check() {
        shared(2);
    }

    #[test]
    #[should_panic]
    fn panic_expected() {
        panic!("expected");
    }

    #[test]
    fn smoke() {
        let _ = Some(1).unwrap();
        let _ = ready();
    }
}
"#,
    )
    .unwrap();
    (root, repository, state)
}

#[test]
fn audits_parser_sites_with_resumable_evidence_batches() {
    let (_root, repository, state) = fixture();
    let summary = initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let summary: Value = serde_json::from_str(&summary).unwrap();
    assert_eq!(summary["testCount"], 5);
    assert_eq!(summary["assertionCount"], 4);
    assert_eq!(summary["revisionKind"], "unversioned");

    let evidence = search(&state, "ready operation", None, 20, &FixedClock).unwrap();
    let evidence: Value = serde_json::from_str(&evidence).unwrap();
    assert!(
        evidence["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["excerpt"] == "/// Returns whether the operation is ready.")
    );

    let batch_path = state.join("batches/one.json");
    let batch_summary = next_batch(&state, &batch_path, 25, &FixedClock).unwrap();
    let batch_summary: Value = serde_json::from_str(&batch_summary).unwrap();
    assert_eq!(batch_summary["batchCount"], 4);
    let batch: Value = serde_json::from_slice(&fs::read(batch_path).unwrap()).unwrap();
    let assertions = batch["assertions"].as_array().unwrap();
    assert_eq!(
        assertions
            .iter()
            .filter(|assertion| assertion["kind"] == "panicExpectation")
            .count(),
        1
    );
    assert!(
        assertions
            .iter()
            .find(|assertion| assertion["helperName"] == "shared")
            .unwrap()["callingTestIds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| !id.as_str().unwrap().is_empty())
    );

    let audit_id = batch["auditId"].clone();
    let snapshot = batch["snapshotSha256"].clone();
    let records = assertions
        .iter()
        .map(|assertion| {
            let is_documented = assertion["expression"]
                .as_str()
                .unwrap()
                .contains("assert!(ready())");
            if is_documented {
                json!({
                    "assertionId": assertion["id"],
                    "classification": "Documented",
                    "evidenceStatus": "Confirmed",
                    "actualOutcome": "The call reports readiness through the public function.",
                    "rationale": "The public API and documentation promise the checked outcome.",
                    "citations": [{
                        "path": "src/lib.rs",
                        "line": 3,
                        "excerpt": "/// Returns whether the operation is ready."
                    }],
                    "callingTestIds": []
                })
            } else {
                json!({
                    "assertionId": assertion["id"],
                    "classification": "Incidental",
                    "evidenceStatus": "Confirmed",
                    "actualOutcome": "The predicate verifies test-local behavior.",
                    "rationale": "No public contract promises this internal detail or test setup.",
                    "citations": [],
                    "incidentalReason": "The exact helper input or induced panic is not a public promise.",
                    "callingTestIds": []
                })
            }
        })
        .collect::<Vec<_>>();
    let classification_path = state.join("batches/classified.json");
    fs::create_dir_all(classification_path.parent().unwrap()).unwrap();
    fs::write(
        &classification_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "auditId": audit_id,
            "snapshotSha256": snapshot,
            "assertions": records
        }))
        .unwrap(),
    )
    .unwrap();

    let recorded = record_batch(&state, &classification_path, &FixedClock).unwrap();
    let recorded: Value = serde_json::from_str(&recorded).unwrap();
    assert_eq!(recorded["completed"], 4);
    assert_eq!(recorded["remaining"], 0);
    let replay = record_batch(&state, &classification_path, &FixedClock).unwrap();
    let replay: Value = serde_json::from_str(&replay).unwrap();
    assert_eq!(replay["unchanged"].as_array().unwrap().len(), 4);

    let summary = report(&state, &FixedClock).unwrap();
    let summary: Value = serde_json::from_str(&summary).unwrap();
    assert_eq!(summary["status"], "complete");
    assert_eq!(summary["testCount"], 5);
    assert_eq!(summary["assertionCount"], 4);
    assert_eq!(summary["classificationCounts"]["Documented"], 2);
    assert_eq!(summary["classificationCounts"]["Incidental"], 2);
    let trace: Value = serde_json::from_slice(
        &fs::read(Path::new(summary["tracePath"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(trace["rows"].as_array().unwrap().len(), 4);
    let test_inventory: Value = serde_json::from_slice(
        &fs::read(Path::new(summary["testInventoryPath"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(test_inventory["tests"].as_array().unwrap().len(), 5);
    let report_text = fs::read_to_string(summary["summaryPath"].as_str().unwrap()).unwrap();
    assert!(report_text.contains("No explicitly documented promise = implicit-only + incidental"));
    assert!(report_text.contains("No public-contract check = incidental only"));
    assert!(report_text.contains("2/4 (50.0%)"));
    assert!(report_text.contains("## Classification totals"));
    assert!(report_text.contains("- Documented: 2/4"));
    assert!(report_text.contains("smoke"));
    assert!(report_text.contains("has only incidental checks"));
}

#[test]
fn rejects_stale_sources_before_accepting_more_judgments() {
    let (_root, repository, state) = fixture();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let batch_path = state.join("batch.json");
    next_batch(&state, &batch_path, 25, &FixedClock).unwrap();
    fs::write(repository.join("src/lib.rs"), "// changed").unwrap();
    let error = next_batch(&state, &batch_path, 25, &FixedClock).unwrap_err();
    assert!(error.to_string().contains("changed after initialization"));
}

#[test]
fn rejects_batch_output_that_aliases_an_audited_file() {
    let (_root, repository, state) = fixture();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let source = repository.join("src/lib.rs");
    let original = fs::read(&source).unwrap();
    let error = next_batch(&state, &source, 25, &FixedClock).unwrap_err();
    assert!(error.to_string().contains("aliases audited source"));
    assert_eq!(fs::read(source).unwrap(), original);
    let inventory = state.join("inventory.json");
    let original_inventory = fs::read(&inventory).unwrap();
    let error = next_batch(&state, &inventory, 25, &FixedClock).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot replace audit state file")
    );
    assert_eq!(fs::read(inventory).unwrap(), original_inventory);
}

#[test]
fn rejects_unbound_citations_and_partial_reports_without_mutation() {
    let (_root, repository, state) = fixture();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let batch_path = state.join("batch.json");
    next_batch(&state, &batch_path, 25, &FixedClock).unwrap();
    let batch: Value = serde_json::from_slice(&fs::read(&batch_path).unwrap()).unwrap();
    let first = &batch["assertions"][0];
    let classification = json!({
        "schemaVersion": 1,
        "auditId": batch["auditId"],
        "snapshotSha256": batch["snapshotSha256"],
        "assertions": [{
            "assertionId": first["id"],
            "classification": "Documented",
            "evidenceStatus": "Confirmed",
            "actualOutcome": "A readiness value is checked.",
            "rationale": "The public documentation states this behavior.",
            "citations": [{
                "path": "src/lib.rs",
                "line": 3,
                "excerpt": "not the source line"
            }],
            "callingTestIds": []
        }]
    });
    let classification_path = state.join("classification.json");
    fs::write(
        &classification_path,
        serde_json::to_vec_pretty(&classification).unwrap(),
    )
    .unwrap();
    let error = record_batch(&state, &classification_path, &FixedClock).unwrap_err();
    assert!(error.to_string().contains("excerpt does not match"));
    let journal: Value =
        serde_json::from_slice(&fs::read(state.join("classifications.json")).unwrap()).unwrap();
    assert_eq!(journal["classifications"].as_array().unwrap().len(), 0);
    let error = report(&state, &FixedClock).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("before classification is complete")
    );
}

#[test]
fn inventories_narrow_common_language_profiles() {
    let root = TempDir::new().unwrap();
    let repository = root.path().join("repository");
    let state = root.path().join("state");
    fs::create_dir_all(&repository).unwrap();
    fs::write(
        repository.join("checks.py"),
        "def test_value():\n    assert 2 == 2\n",
    )
    .unwrap();
    fs::write(
        repository.join("checks.test.js"),
        "test('value', () => {\n  expect(2).toBe(2);\n});\n",
    )
    .unwrap();
    fs::write(
        repository.join("Checks.cs"),
        "class Checks {\n[Fact]\npublic void Value() {\nAssert.Equal(2, 2);\n}\n}\n",
    )
    .unwrap();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let inventory: Value =
        serde_json::from_slice(&fs::read(state.join("inventory.json")).unwrap()).unwrap();
    assert_eq!(inventory["tests"].as_array().unwrap().len(), 3);
    assert_eq!(inventory["assertions"].as_array().unwrap().len(), 3);
    assert!(
        inventory["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["kind"] == "heuristicLanguage")
    );
}

#[test]
fn accepts_a_path_inside_a_git_working_tree() {
    let root = TempDir::new().unwrap();
    let repository = root.path().join("repository");
    let nested = repository.join("packages/example");
    let state = root.path().join("state");
    fs::create_dir_all(repository.join(".git")).unwrap();
    fs::create_dir_all(nested.join("src")).unwrap();
    fs::write(nested.join("src/lib.rs"), "pub fn value() -> usize { 1 }\n").unwrap();
    let process = GitProcess {
        root: repository.to_string_lossy().into_owned(),
    };
    let summary = initialize(&nested, &[".".into()], &state, &process, &FixedClock).unwrap();
    let summary: Value = serde_json::from_str(&summary).unwrap();
    assert_eq!(summary["revisionKind"], "git");
    assert_eq!(
        summary["repositoryRevision"],
        "0123456789abcdef0123456789abcdef01234567"
    );
}
