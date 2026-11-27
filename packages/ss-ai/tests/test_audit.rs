//! Executable contract for the provider-independent test-audit namespace.
#![allow(
    clippy::indexing_slicing,
    reason = "The test indexes JSON fields from its own fixed executable responses."
)]

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, from_slice, json, to_vec_pretty};
use tempfile::TempDir;

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn help_lists_the_native_workflow_without_provider_authentication() {
    let output = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .arg("test-audit")
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    for command in [
        "initialize",
        "search",
        "context",
        "evidence",
        "next-batch",
        "record-batch",
        "advance",
        "submit",
        "compare",
        "report",
    ] {
        assert!(stdout.contains(command), "{command}");
    }
    assert!(stdout.contains("test-audit <COMMAND>"));
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn executable_accepts_multi_query_search_and_compact_submit() {
    let root = TempDir::new().unwrap();
    let repository = root.path().join("repository");
    let state = root.path().join("state");
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(
        repository.join("src/lib.rs"),
        "/// A documented value.\npub fn value() -> i32 { 1 }\n\n#[test]\nfn checks_value() { assert_eq!(value(), 1); }\n",
    )
    .unwrap();
    let profile_path = write_contract_profile(root.path());

    let initialize = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "initialize", "--repository-path"])
        .arg(&repository)
        .args(["--contract-profile-path"])
        .arg(&profile_path)
        .args(["--state-directory"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        initialize.status.success(),
        "{}",
        String::from_utf8_lossy(&initialize.stderr)
    );

    let search = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "search", "--state-directory"])
        .arg(&state)
        .args(["--query", "documented", "--query", "checks value"])
        .output()
        .unwrap();
    assert!(
        search.status.success(),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
    let search: Value = from_slice(&search.stdout).unwrap();
    assert_eq!(search["queryCount"], 2);
    let evidence_handle = search["results"][0]["matches"][0]["evidenceHandle"]
        .as_str()
        .unwrap();
    assert_evidence_card(&state, evidence_handle);

    let batch_path = state.join("batch-one.json");
    let next = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "next-batch", "--state-directory"])
        .arg(&state)
        .args(["--output-path"])
        .arg(&batch_path)
        .output()
        .unwrap();
    assert!(next.status.success());
    let next: Value = from_slice(&next.stdout).unwrap();
    let batch_path = next["batchPath"].as_str().unwrap();
    let batch: Value = from_slice(&fs::read(batch_path).unwrap()).unwrap();
    assert_context_card(&state, &batch);
    let evidence_handle = search["results"][0]["matches"][0]["evidenceHandle"].clone();
    let submission_path = state.join("submission.json");
    fs::write(
        &submission_path,
        to_vec_pretty(&json!({
            "schemaVersion": 1,
            "batchFingerprint": next["batchFingerprint"],
            "rows": [{
                "assertionId": batch["assertions"][0]["id"],
                "classification": "Documented",
                "evidenceStatus": "Confirmed",
                "actualOutcome": "The test observes the documented function value.",
                "rationale": "The declaration and documentation establish the checked outcome.",
                "citationHandles": [evidence_handle],
                "callingTestIds": [],
                "decisiveClause": "A documented value.",
                "observationStrength": "exact"
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let next_path = state.join("batch-two.json");
    let submit = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "submit", "--state-directory"])
        .arg(&state)
        .args(["--batch-path"])
        .arg(batch_path)
        .args(["--submission-path"])
        .arg(&submission_path)
        .args(["--output-path"])
        .arg(&next_path)
        .output()
        .unwrap();
    assert!(
        submit.status.success(),
        "{}",
        String::from_utf8_lossy(&submit.stderr)
    );
    let submit: Value = from_slice(&submit.stdout).unwrap();
    assert_eq!(submit["record"]["remaining"], 0);
    assert_eq!(submit["nextBatch"]["batchCount"], 0);
    assert_eq!(submit["nextAction"], "report");
    assert_eq!(submit["finalResponseAllowed"], false);
    assert!(submit["nextBatch"]["displayPath"].is_string());

    let report = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "report", "--state-directory"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let report: Value = from_slice(&report.stdout).unwrap();
    assert_eq!(report["nextAction"], "complete");
    assert_eq!(report["finalResponseAllowed"], true);
    let summary: Value =
        from_slice(&fs::read(report["summaryJsonPath"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(summary["schemaVersion"], 3);
    assert_eq!(summary["testCount"], report["testCount"]);
    assert_eq!(summary["classificationCounts"]["Documented"], 1);
    let summary_text = fs::read_to_string(report["summaryPath"].as_str().unwrap()).unwrap();
    assert!(summary_text.contains("Documented: 1/1"));

    let details: Value =
        from_slice(&fs::read(report["detailsJsonPath"].as_str().unwrap()).unwrap()).unwrap();
    let predicate = "assert_eq!(value(), 1)";
    assert_eq!(details["testInventory"][0]["name"], "checks_value");
    assert_eq!(
        details["assertionTrace"][0]["assertion"]["expression"],
        predicate
    );
    let details_text = fs::read_to_string(report["detailsPath"].as_str().unwrap()).unwrap();
    assert!(details_text.contains("checks_value"));
    assert!(details_text.contains(predicate));
    let containing_test = details["assertionTrace"][0]["assertion"]["containingTestId"]
        .as_str()
        .unwrap();
    assert!(details_text.contains(&format!("- Calling tests: {containing_test}")));
    assert_canonical_report_receipt(&report);
}

/// Verifies complete owner context through the executable boundary.
fn assert_context_card(state: &Path, batch: &Value) {
    let context = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "context", "--state-directory"])
        .arg(state)
        .args(["--assertion-id"])
        .arg(batch["assertions"][0]["id"].as_str().unwrap())
        .output()
        .unwrap();
    assert!(
        context.status.success(),
        "{}",
        String::from_utf8_lossy(&context.stderr)
    );
    let context: Value = from_slice(&context.stdout).unwrap();
    assert!(
        context["owner"]["source"]
            .as_str()
            .unwrap()
            .contains("fn checks_value()")
    );
    assert_eq!(context["assertion"]["id"], batch["assertions"][0]["id"]);
    assert_eq!(context["containingTest"]["name"], "checks_value");
    assert!(
        context["bindingCandidates"].is_array() && context["relatedEvidenceCandidates"].is_array()
    );
}

/// Verifies complete handle-bound evidence through the executable boundary.
fn assert_evidence_card(state: &Path, evidence_handle: &str) {
    let evidence = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "evidence", "--state-directory"])
        .arg(state)
        .args(["--evidence-handle"])
        .arg(evidence_handle)
        .output()
        .unwrap();
    assert!(
        evidence.status.success(),
        "{}",
        String::from_utf8_lossy(&evidence.stderr)
    );
    let evidence: Value = from_slice(&evidence.stdout).unwrap();
    assert_eq!(evidence["evidenceHandle"], evidence_handle);
    assert!(evidence["text"].as_str().unwrap().contains("documented"));
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn executable_accepts_exact_initialization_corrections() {
    let root = TempDir::new().unwrap();
    let repository = root.path().join("repository");
    let state = root.path().join("state");
    fs::create_dir_all(repository.join("tests")).unwrap();
    fs::write(
        repository.join("tests/check.rs"),
        "#[test]\nfn checks() {\n    let holder = callback_fixture;\n    holder.expect(\"fixture still present\");\n}\n",
    )
    .unwrap();
    let correction_path = root.path().join("inventory-corrections.json");
    fs::write(
        &correction_path,
        to_vec_pretty(&json!({
            "schemaVersion": 1,
            "repositoryRevision": "unversioned",
            "corrections": [{
                "path": "tests/check.rs",
                "line": 4,
                "endLine": 4,
                "expression": "holder.expect(\"fixture still present\")",
                "candidateRole": "fixtureSetup",
                "candidateDisposition": "excluded",
                "reason": "This option is populated only by callback-local test state."
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let profile_path = write_contract_profile(root.path());

    let output = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "initialize", "--repository-path"])
        .arg(&repository)
        .args(["--correction-path"])
        .arg(&correction_path)
        .args(["--contract-profile-path"])
        .arg(&profile_path)
        .args(["--state-directory"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["schemaVersion"], 4);
    assert_eq!(receipt["assertionCount"], 0);
    assert_eq!(receipt["correctionCount"], 1);
    let inventory: Value = from_slice(&fs::read(state.join("inventory.json")).unwrap()).unwrap();
    assert_eq!(inventory["corrections"][0]["assertionId"], "A000001");
    assert_eq!(
        inventory["corrections"][0]["fileSha256"],
        inventory["files"][0]["sha256"]
    );
}

/// Supplies the explicit public-source policy required by the CLI.
fn write_contract_profile(root: &Path) -> std::path::PathBuf {
    let path = root.join("contract-profile.json");
    fs::write(
        &path,
        to_vec_pretty(&json!({
            "schemaVersion": 1,
            "name": "library API",
            "interface": "Rust library",
            "audience": "crate users",
            "entryPoints": ["pub items"],
            "internalSurfaces": ["test fixtures"],
            "authorities": [{
                "id": "rustPublicDocs",
                "pathPrefix": "src/",
                "sourceRoles": ["rustItemDoc", "rustPublicExample"],
                "admissible": true,
                "authority": "public item documentation",
                "scope": "documented Rust API behavior"
            }],
            "unresolved": []
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

/// Verifies that the CLI receipt exposes only canonical paired report artifacts.
fn assert_canonical_report_receipt(report: &Value) {
    assert!(report.get("tracePath").is_none());
    assert!(report.get("testInventoryPath").is_none());
    assert!(
        !report["detailsJsonPath"]
            .as_str()
            .map(Path::new)
            .unwrap()
            .parent()
            .unwrap()
            .join("trace.json")
            .exists()
    );
    assert!(
        !report["detailsJsonPath"]
            .as_str()
            .map(Path::new)
            .unwrap()
            .parent()
            .unwrap()
            .join("test-inventory.json")
            .exists()
    );
}
