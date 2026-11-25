//! Executable contract for the provider-independent test-audit namespace.
#![allow(
    clippy::indexing_slicing,
    reason = "The test indexes JSON fields from its own fixed executable responses."
)]

use std::fs;
use std::process::Command;

use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
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
        "next-batch",
        "record-batch",
        "advance",
        "report",
    ] {
        assert!(stdout.contains(command), "{command}");
    }
    assert!(stdout.contains("test-audit <COMMAND>"));
}

#[test]
fn executable_accepts_multi_query_search_and_advance() {
    let root = TempDir::new().unwrap();
    let repository = root.path().join("repository");
    let state = root.path().join("state");
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(
        repository.join("src/lib.rs"),
        "/// A documented value.\npub fn value() -> i32 { 1 }\n\n#[test]\nfn checks_value() { assert_eq!(value(), 1); }\n",
    )
    .unwrap();

    let initialize = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "initialize", "--repository-path"])
        .arg(&repository)
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
    let search: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["queryCount"], 2);

    let batch_path = state.join("batch-one.json");
    let next = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "next-batch", "--state-directory"])
        .arg(&state)
        .args(["--output-path"])
        .arg(&batch_path)
        .output()
        .unwrap();
    assert!(next.status.success());
    let batch: Value = serde_json::from_slice(&fs::read(&batch_path).unwrap()).unwrap();
    let classification_path = state.join("classification.json");
    fs::write(
        &classification_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "auditId": batch["auditId"],
            "snapshotSha256": batch["snapshotSha256"],
            "assertions": [{
                "assertionId": batch["assertions"][0]["id"],
                "classification": "Documented",
                "evidenceStatus": "Confirmed",
                "actualOutcome": "The test observes the documented function value.",
                "rationale": "The declaration and documentation establish the checked outcome.",
                "citations": [{
                    "path": "src/lib.rs",
                    "line": 1,
                    "excerpt": "/// A documented value."
                }],
                "callingTestIds": []
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let next_path = state.join("batch-two.json");
    let advance = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["test-audit", "advance", "--state-directory"])
        .arg(&state)
        .args(["--batch-path"])
        .arg(&classification_path)
        .args(["--output-path"])
        .arg(&next_path)
        .output()
        .unwrap();
    assert!(
        advance.status.success(),
        "{}",
        String::from_utf8_lossy(&advance.stderr)
    );
    let advance: Value = serde_json::from_slice(&advance.stdout).unwrap();
    assert_eq!(advance["record"]["remaining"], 0);
    assert_eq!(advance["nextBatch"]["batchCount"], 0);
}
