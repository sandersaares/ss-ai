//! Exercises one complete local audit from source inventory through final reports.
#![expect(
    clippy::indexing_slicing,
    reason = "The test selects fixed fields from its own generated records."
)]

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, Process, ProcessOutput};
use ss_ai_test_audit::{
    advance, initialize, next_batch, record_batch, report, search, search_many, submit,
};
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

#[cfg(test)]
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
#[expect(
    clippy::cognitive_complexity,
    reason = "The end-to-end test intentionally follows one complete audit contract"
)]
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
        evidence["results"][0]["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["excerpt"] == "/// Returns whether the operation is ready.")
    );
    let batch_path = state.join("batches/one.json");
    let batch_summary = next_batch(&state, &batch_path, 25, &FixedClock).unwrap();
    let batch_summary: Value = serde_json::from_str(&batch_summary).unwrap();
    assert_eq!(batch_summary["batchCount"], 4);
    let display = fs::read_to_string(batch_summary["displayPath"].as_str().unwrap()).unwrap();
    assert!(display.contains("A000001 |"));
    assert!(display.contains("expression:"));
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
    assert_eq!(summary["noCountedAssertionCount"], 1);
    assert!(summary["incidentalOnlyCount"].as_u64().unwrap() > 0);
    assert_eq!(summary["unlinkedAssertionCount"], 0);
    assert_eq!(summary["fallibleValueCheckCount"], 0);
    assert_eq!(summary["conditionalAssertionCount"], 0);
    assert!(summary["diagnosticCount"].as_u64().is_some());
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
    let summary_json: Value =
        serde_json::from_slice(&fs::read(summary["summaryJsonPath"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(summary_json["schemaVersion"], 1);
    assert_eq!(summary_json["auditId"], summary["auditId"]);
    assert_eq!(summary_json["testCount"], summary["testCount"]);
    assert_eq!(summary_json["status"], "complete");
    assert_eq!(summary_json["nextAction"], "complete");
    assert_eq!(summary_json["finalResponseAllowed"], true);
    assert_eq!(summary_json["sourceLanguages"]["rust"], 1);
    assert_eq!(
        summary_json["repositoryRoot"]
            .as_str()
            .unwrap()
            .replace("\\\\?\\", ""),
        repository.to_str().unwrap()
    );
    assert_eq!(summary_json["scopePaths"], json!([".".to_owned()]));
    let details_json: Value =
        serde_json::from_slice(&fs::read(summary["detailsJsonPath"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(details_json["assertionTrace"].as_array().unwrap().len(), 4);
    assert_eq!(details_json["testInventory"].as_array().unwrap().len(), 5);
    let details_text = fs::read_to_string(summary["detailsPath"].as_str().unwrap()).unwrap();
    assert!(details_text.contains("## Assertion trace"));
    assert!(details_text.contains("## Test inventory"));
    assert!(report_text.contains("No explicitly documented promise = implicit-only + incidental"));
    assert!(report_text.contains("No public-contract check = incidental only"));
    assert!(report_text.contains("2/4 (50.0%)"));
    assert!(report_text.contains("## Classification totals"));
    assert!(report_text.contains("## Coverage summary"));
    assert!(report_text.contains("- Status: complete"));
    assert!(report_text.contains("- Schema version: 1"));
    assert!(report_text.contains("- Source languages:"));
    assert!(report_text.contains("- Fallible-value candidates:"));
    assert!(report_text.contains("- Documented: 2/4"));
    assert!(report_text.contains("smoke"));
    assert!(report_text.contains("has only incidental checks"));
}

#[test]
fn continues_through_more_than_four_submit_transitions() {
    let root = TempDir::new().unwrap();
    let repository = root.path().join("repository");
    let state = root.path().join("state");
    fs::create_dir_all(repository.join("src")).unwrap();
    let mut source = String::from("pub fn value() -> i32 { 1 }\n\n");
    for index in 0..126 {
        _ = writeln!(
            source,
            "#[test]\nfn generated_{index:03}() {{ assert_eq!(value(), 1); }}"
        );
    }
    fs::write(repository.join("src/lib.rs"), source).unwrap();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();

    let mut batch_path = state.join("batch-00.json");
    let initial = next_batch(&state, &batch_path, 25, &FixedClock).unwrap();
    let initial: Value = serde_json::from_str(&initial).unwrap();
    assert_eq!(initial["nextAction"], "continue");
    assert_eq!(initial["finalResponseAllowed"], false);

    let expected_remaining = [101, 76, 51, 26, 1, 0];
    for batch_number in 1_usize..=6 {
        let batch: Value = serde_json::from_slice(&fs::read(&batch_path).unwrap()).unwrap();
        let records = batch["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|assertion| {
                json!({
                    "assertionId": assertion["id"],
                    "classification": "Incidental",
                    "evidenceStatus": "Confirmed",
                    "actualOutcome": "The generated test checks a direct source-level value.",
                    "rationale": "No public contract promises this generated test detail.",
                    "citationHandles": [],
                    "incidentalReason": "The check exists only in the continuation fixture.",
                    "callingTestIds": []
                })
            })
            .collect::<Vec<_>>();
        let submission_path = state.join(format!("submission-{batch_number:02}.json"));
        fs::write(
            &submission_path,
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": 1,
                "rows": records
            }))
            .unwrap(),
        )
        .unwrap();
        let next_path = state.join(format!("batch-{batch_number:02}.json"));
        let transition = submit(
            &state,
            &batch_path,
            &submission_path,
            &next_path,
            25,
            &FixedClock,
        )
        .unwrap();
        let transition: Value = serde_json::from_str(&transition).unwrap();
        let remaining = expected_remaining[batch_number.saturating_sub(1)];
        assert_eq!(transition["record"]["remaining"], remaining);
        assert_eq!(transition["finalResponseAllowed"], false);
        if remaining == 0 {
            assert_eq!(transition["nextAction"], "report");
            assert_eq!(transition["nextBatch"]["batchCount"], 0);
        } else {
            assert_eq!(transition["nextAction"], "continue");
            assert_eq!(transition["nextBatch"]["batchCount"], 25.min(remaining));
        }
        batch_path = next_path;
    }

    let complete = report(&state, &FixedClock).unwrap();
    let complete: Value = serde_json::from_str(&complete).unwrap();
    assert_eq!(complete["nextAction"], "complete");
    assert_eq!(complete["finalResponseAllowed"], true);
    assert_eq!(complete["assertionCount"], 126);
}

#[test]
fn rejects_stale_sources_before_accepting_more_judgments() {
    let (_root, repository, state) = fixture();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let batch_path = state.join("batch.json");
    next_batch(&state, &batch_path, 25, &FixedClock).unwrap();
    fs::write(repository.join("src/lib.rs"), "// changed").unwrap();
    let read_only = next_batch(&state, &batch_path, 25, &FixedClock).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&read_only).unwrap()["batchCount"],
        4
    );
    search(&state, "ready", None, 20, &FixedClock).unwrap();
    let classification_path = state.join("stale-classification.json");
    fs::write(&classification_path, b"{}").unwrap();
    let error = record_batch(&state, &classification_path, &FixedClock).unwrap_err();
    assert!(error.to_string().contains("changed after initialization"));
}

#[test]
fn searches_multiple_queries_from_one_snapshot_load() {
    let (_root, repository, state) = fixture();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let evidence = search_many(
        &state,
        &["ready".to_owned(), "shared".to_owned()],
        None,
        20,
        &FixedClock,
    )
    .unwrap();
    let evidence: Value = serde_json::from_str(&evidence).unwrap();
    assert_eq!(evidence["queryCount"], 2);
    assert_eq!(evidence["results"].as_array().unwrap().len(), 2);
}

#[test]
fn advance_records_and_exports_the_next_batch_from_one_state_load() {
    let (_root, repository, state) = fixture();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let batch_path = state.join("batch-one.json");
    next_batch(&state, &batch_path, 1, &FixedClock).unwrap();
    let batch: Value = serde_json::from_slice(&fs::read(&batch_path).unwrap()).unwrap();
    let assertion = &batch["assertions"][0];
    let classification_path = state.join("classification-one.json");
    fs::write(
        &classification_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "auditId": batch["auditId"],
            "snapshotSha256": batch["snapshotSha256"],
            "assertions": [{
                "assertionId": assertion["id"],
                "classification": "Incidental",
                "evidenceStatus": "Confirmed",
                "actualOutcome": "The test checks a source-level detail.",
                "rationale": "No public wording promises this exact check.",
                "citations": [],
                "incidentalReason": "The check is internal test behavior.",
                "callingTestIds": []
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let next_path = state.join("batch-two.json");
    let result = advance(&state, &classification_path, &next_path, 1, &FixedClock).unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["record"]["completed"], 1);
    assert_eq!(result["record"]["remaining"], 3);
    assert_eq!(result["nextBatch"]["batchCount"], 1);
    assert_eq!(result["nextBatch"]["completed"], 1);
    assert_eq!(result["nextBatch"]["remaining"], 3);
    let next: Value = serde_json::from_slice(&fs::read(next_path).unwrap()).unwrap();
    assert_ne!(next["assertions"][0]["id"], assertion["id"]);

    let classification_path = state.join("classification-two.json");
    let records = next["assertions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|assertion| {
            json!({
                "assertionId": assertion["id"],
                "classification": "Incidental",
                "evidenceStatus": "Confirmed",
                "actualOutcome": "The test checks a source-level detail.",
                "rationale": "No public wording promises this exact check.",
                "citations": [],
                "incidentalReason": "The check is internal test behavior.",
                "callingTestIds": []
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        &classification_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "auditId": next["auditId"],
            "snapshotSha256": next["snapshotSha256"],
            "assertions": records
        }))
        .unwrap(),
    )
    .unwrap();
    let batch_three_path = state.join("batch-three.json");
    let batch_three = advance(
        &state,
        &classification_path,
        &batch_three_path,
        25,
        &FixedClock,
    )
    .unwrap();
    let batch_three: Value = serde_json::from_str(&batch_three).unwrap();
    assert_eq!(batch_three["record"]["remaining"], 2);
    let batch_three: Value = serde_json::from_slice(&fs::read(batch_three_path).unwrap()).unwrap();
    assert_eq!(batch_three["assertions"].as_array().unwrap().len(), 2);

    let classification_path = state.join("classification-three.json");
    let records = batch_three["assertions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|assertion| {
            json!({
                "assertionId": assertion["id"],
                "classification": "Incidental",
                "evidenceStatus": "Confirmed",
                "actualOutcome": "The test checks a source-level detail.",
                "rationale": "No public wording promises this exact check.",
                "citations": [],
                "incidentalReason": "The check is internal test behavior.",
                "callingTestIds": []
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        &classification_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "auditId": batch_three["auditId"],
            "snapshotSha256": batch_three["snapshotSha256"],
            "assertions": records
        }))
        .unwrap(),
    )
    .unwrap();
    let terminal_path = state.join("terminal-batch.json");
    let terminal = advance(
        &state,
        &classification_path,
        &terminal_path,
        25,
        &FixedClock,
    )
    .unwrap();
    let terminal: Value = serde_json::from_str(&terminal).unwrap();
    assert_eq!(terminal["record"]["remaining"], 0);
    assert_eq!(terminal["nextBatch"]["batchCount"], 0);
    assert_eq!(terminal["nextAction"], "report");
    assert_eq!(terminal["finalResponseAllowed"], false);
    let report = report(&state, &FixedClock).unwrap();
    let report: Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["status"], "complete");
    assert_eq!(report["nextAction"], "complete");
    assert_eq!(report["finalResponseAllowed"], true);
}

#[test]
fn submit_resolves_multiple_handles_and_explicit_reuse() {
    let (_root, repository, state) = fixture();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let first_batch_path = state.join("compact-batch-one.json");
    next_batch(&state, &first_batch_path, 1, &FixedClock).unwrap();
    let first_batch: Value = serde_json::from_slice(&fs::read(&first_batch_path).unwrap()).unwrap();
    let searches = search_many(
        &state,
        &["ready operation".to_owned(), "shared".to_owned()],
        None,
        20,
        &FixedClock,
    )
    .unwrap();
    let searches: Value = serde_json::from_str(&searches).unwrap();
    let handles = [
        searches["results"][0]["matches"][0]["evidenceHandle"]
            .as_str()
            .unwrap()
            .to_owned(),
        searches["results"][1]["matches"][0]["evidenceHandle"]
            .as_str()
            .unwrap()
            .to_owned(),
    ];
    let first_id = first_batch["assertions"][0]["id"].clone();
    let first_submission_path = state.join("compact-submission-one.json");
    fs::write(
        &first_submission_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "rows": [{
                "assertionId": first_id,
                "classification": "Incidental",
                "evidenceStatus": "Confirmed",
                "actualOutcome": "The predicate checks one exact source-level detail.",
                "rationale": "The public contract does not promise this exact detail.",
                "citationHandles": handles,
                "incidentalReason": "The check is private test mechanics.",
                "callingTestIds": []
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let second_batch_path = state.join("compact-batch-two.json");
    let second = submit(
        &state,
        &first_batch_path,
        &first_submission_path,
        &second_batch_path,
        1,
        &FixedClock,
    )
    .unwrap();
    let second: Value = serde_json::from_str(&second).unwrap();
    assert_eq!(second["record"]["completed"], 1);
    let second_batch: Value =
        serde_json::from_slice(&fs::read(&second_batch_path).unwrap()).unwrap();
    let second_id = second_batch["assertions"][0]["id"].clone();
    let second_submission_path = state.join("compact-submission-two.json");
    fs::write(
        &second_submission_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "rows": [{
                "assertionId": second_id,
                "reuseFrom": first_id,
                "equivalenceConfirmed": true,
                "citationHandles": "Einvalid-reference-000001",
                "callingTestIds": []
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let third_batch_path = state.join("compact-batch-three.json");
    let error = submit(
        &state,
        &second_batch_path,
        &second_submission_path,
        &third_batch_path,
        1,
        &FixedClock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("citationHandles"));
    assert!(error.to_string().contains("Expected an array"));
    assert!(error.to_string().contains(second_id.as_str().unwrap()));

    fs::write(
        &second_submission_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "rows": [{
                "assertionId": second_id,
                "reuseFrom": first_id,
                "equivalenceConfirmed": true,
                "citationHandles": ["Einvalid-reference-000001"],
                "callingTestIds": []
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let error = submit(
        &state,
        &second_batch_path,
        &second_submission_path,
        &third_batch_path,
        1,
        &FixedClock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("citationHandles[0]"));
    assert!(
        error
            .to_string()
            .contains("not bound to this audit snapshot")
    );
    assert!(error.to_string().contains("Einvalid-reference-000001"));
    assert!(error.to_string().contains(second_id.as_str().unwrap()));

    let reuse_handle = searches["results"][0]["matches"][0]["evidenceHandle"]
        .as_str()
        .unwrap();
    fs::write(
        &second_submission_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "rows": [{
                "assertionId": second_id,
                "reuseFrom": first_id,
                "equivalenceConfirmed": true,
                "citationHandles": [reuse_handle],
                "callingTestIds": []
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    submit(
        &state,
        &second_batch_path,
        &second_submission_path,
        &third_batch_path,
        25,
        &FixedClock,
    )
    .unwrap();
    let third_batch: Value = serde_json::from_slice(&fs::read(third_batch_path).unwrap()).unwrap();
    assert_eq!(third_batch["assertions"].as_array().unwrap().len(), 2);
}

#[test]
fn compact_submission_reports_empty_citation_replacement() {
    let (_root, repository, state) = fixture();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let first_batch_path = state.join("first-batch.json");
    next_batch(&state, &first_batch_path, 1, &FixedClock).unwrap();
    let first_batch: Value =
        serde_json::from_slice(&fs::read(first_batch_path.clone()).unwrap()).unwrap();
    let first_id = first_batch["assertions"][0]["id"].clone();
    let handle = serde_json::from_str::<Value>(
        &search(&state, "ready operation", None, 20, &FixedClock).unwrap(),
    )
    .unwrap()["results"][0]["matches"][0]["evidenceHandle"]
        .clone();
    let first_submission_path = state.join("first-submission.json");
    fs::write(
        &first_submission_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "rows": [{
                "assertionId": first_id,
                "classification": "Documented",
                "evidenceStatus": "Confirmed",
                "actualOutcome": "The source check has a public promise.",
                "rationale": "The cited wording promises this behavior.",
                "citationHandles": [handle],
                "callingTestIds": []
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let second_batch_path = state.join("second-batch.json");
    submit(
        &state,
        &first_batch_path,
        &first_submission_path,
        &second_batch_path,
        1,
        &FixedClock,
    )
    .unwrap();
    let second_batch: Value =
        serde_json::from_slice(&fs::read(second_batch_path.clone()).unwrap()).unwrap();
    let submission_path = state.join("empty-replacement.json");
    fs::write(
        &submission_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "rows": [{
                "assertionId": second_batch["assertions"][0]["id"],
                "reuseFrom": first_id,
                "equivalenceConfirmed": true,
                "citationHandles": [],
                "callingTestIds": []
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let error = submit(
        &state,
        &second_batch_path,
        &submission_path,
        &state.join("unwritten.json"),
        1,
        &FixedClock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("Empty replacement"));
    assert!(error.to_string().contains("reuse from"));
    assert!(error.to_string().contains("citationHandles"));
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
    assert!(
        error
            .to_string()
            .contains("does not match the indexed source line")
    );
    assert!(error.to_string().contains("citations[0].excerpt"));
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
fn excludes_production_invariants_and_links_only_bare_test_helpers() {
    let root = TempDir::new().unwrap();
    let repository = root.path().join("repository");
    let state = root.path().join("state");
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(
        repository.join("src/lib.rs"),
        r#"pub fn poll() {
    assert!(false, "production invariant");
    panic!("production failure");
}

fn main() {
    unreachable!("example failure");
}

#[cfg(debug_assertions)]
fn debug_helper() {
    assert_eq!(3, 3);
}

#[cfg(test)]
fn checker() {
    assert_eq!(2, 2);
}

fn exploding() {
    panic!("expected production panic");
}

struct Other;

impl Other {
    fn checker(&self) {}
}

#[cfg(test)]
struct Trigger;

#[cfg(test)]
impl Drop for Trigger {
    fn drop(&mut self) {
        assert!(std::thread::panicking());
        panic!("deliberately triggered");
    }
}

#[test]
fn calls_real_helper() {
    debug_helper();
    checker();
    assert_panics(|| panic!("expected"));
    assert_panics(|| exploding());
}

#[test]
fn calls_same_named_method_only() {
    let other = Other;
    other.checker();
    let trigger = Trigger;
    drop(trigger);
}
"#,
    )
    .unwrap();
    let summary = initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let summary: Value = serde_json::from_str(&summary).unwrap();
    assert_eq!(summary["testCount"], 2);
    assert_eq!(summary["assertionCount"], 5);

    let inventory: Value =
        serde_json::from_slice(&fs::read(state.join("inventory.json")).unwrap()).unwrap();
    let assertions = inventory["assertions"].as_array().unwrap();
    assert!(!assertions.iter().any(|assertion| {
        assertion["expression"]
            .as_str()
            .is_some_and(|expression| expression.contains("production"))
    }));
    let shared = assertions
        .iter()
        .find(|assertion| assertion["helperName"] == "checker")
        .unwrap();
    assert_eq!(shared["callingTestIds"].as_array().unwrap().len(), 1);
    let debug_helper = assertions
        .iter()
        .find(|assertion| assertion["helperName"] == "debug_helper")
        .unwrap();
    assert_eq!(debug_helper["callingTestIds"].as_array().unwrap().len(), 1);
    let destructor = assertions
        .iter()
        .find(|assertion| assertion["helperName"] == "drop")
        .unwrap();
    assert!(destructor["callingTestIds"].as_array().unwrap().is_empty());
    assert!(
        assertions
            .iter()
            .any(|assertion| assertion["kind"] == "panicExpectation")
    );
    assert!(
        !assertions
            .iter()
            .any(|assertion| assertion["helperName"] == "exploding")
    );
}

#[test]
fn inventories_size_checks_compile_constraints_and_ambiguous_expect_calls() {
    let root = TempDir::new().unwrap();
    let repository = root.path().join("repository");
    let state = root.path().join("state");
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(
        repository.join("src/lib.rs"),
        r#"struct Small(u8);

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::Small;
    use static_assertions::assert_eq_size;

    #[cfg(debug_assertions)]
    assert_eq_size!(Small, (u8,));
    #[cfg(not(debug_assertions))]
    assert_eq_size!(Small, u8);

    fn _assert_send<F: Future + Send>(_: F) {}

    fn compile_only_send_check() {
        _assert_send(async {});
    }

    fn ordinary_generic<T: Clone>(value: T) -> T {
        value
    }

    #[test]
    fn checks_presence_but_not_fixture_access() {
        let _value = Some(42).expect("value exists");
        let _pointer = unsafe { Some(42).as_ref() }.expect("pointer fixture");
        let mutex = Mutex::new(Some(42));
        let _guard = mutex.lock().expect("test mutex");
        let _copied = ordinary_generic(42);
    }
}
"#,
    )
    .unwrap();
    initialize(&repository, &[], &state, &NoProcess, &FixedClock).unwrap();
    let inventory: Value =
        serde_json::from_slice(&fs::read(state.join("inventory.json")).unwrap()).unwrap();
    let assertions = inventory["assertions"].as_array().unwrap();
    assert_eq!(
        assertions
            .iter()
            .filter(|assertion| assertion["expression"]
                .as_str()
                .is_some_and(|expression| expression.contains("assert_eq_size!")))
            .count(),
        2
    );
    assert!(assertions.iter().any(|assertion| {
        assertion["kind"] == "compileTimeAssertion"
            && assertion["expression"]
                .as_str()
                .is_some_and(|expression| expression.contains("_assert_send(async"))
    }));
    assert!(assertions.iter().any(|assertion| {
        assertion["kind"] == "fallibleValueCheck"
            && assertion["expression"]
                .as_str()
                .is_some_and(|expression| expression.contains("value exists"))
    }));
    assert!(!assertions.iter().any(|assertion| {
        assertion["expression"].as_str().is_some_and(|expression| {
            expression.contains("test mutex") || expression.contains("pointer fixture")
        })
    }));
    assert!(!assertions.iter().any(|assertion| {
        assertion["expression"]
            .as_str()
            .is_some_and(|expression| expression.contains("ordinary_generic"))
    }));
    let diagnostics = inventory["diagnostics"].as_array().unwrap();
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic["kind"] == "conditionalAssertion")
            .count(),
        2
    );
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("#[cfg(debug_assertions)]"))
    }));
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
