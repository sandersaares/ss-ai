//! Exercises local workflow commands through the installed executable boundary.

use std::fs;
use std::process::Command;

use serde_json::Value;
use tempfile::Builder;

/// Local finding plans must not require provider authentication or executables.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn plan_creation_runs_without_provider_tools() {
    let directory = Builder::new()
        .prefix(".workflow-cli-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let path = directory.path().join("finding plan.json");
    let output = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["workflow", "plan-initialize", "--kind", "updates", "--path"])
        .arg(&path)
        .env("PATH", "")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("status"), Some(&Value::from("Created")));
    let plan: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(plan.get("updates"), Some(&serde_json::json!([])));
}

/// Empty plans are valid authoring state but not valid mutation requests.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn empty_mutation_plans_fail_without_changing_the_artifact() {
    let directory = Builder::new()
        .prefix(".workflow-cli-mutation-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let artifact = directory.path().join("reviewer.json");
    let finding = directory.path().join("finding.json");
    fs::write(&finding, r#"{"title":"Problem","description":"What fails","reasoning":"Why","consequence":"Impact","recommendedAction":"Fix","anchor":{"kind":"file","path":"src/lib.rs"}}"#).unwrap();
    let initialize = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["workflow", "initialize-finding", "--output-directory"])
        .arg(directory.path())
        .args(["--reviewer-id", "reviewer", "--focus-area", "correctness"])
        .output()
        .unwrap();
    assert!(
        initialize.status.success(),
        "{}",
        String::from_utf8_lossy(&initialize.stderr)
    );
    let add = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["workflow", "add-finding", "--artifact-path"])
        .arg(&artifact)
        .arg("--finding-path")
        .arg(&finding)
        .args(["--origin", "reviewer"])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let before = fs::read(&artifact).unwrap();
    for (kind, command) in [
        ("updates", "update-finding"),
        ("removals", "remove-finding"),
    ] {
        let plan = directory.path().join(format!("{kind}.json"));
        let initialized = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
            .args(["workflow", "plan-initialize", "--kind", kind, "--path"])
            .arg(&plan)
            .output()
            .unwrap();
        assert!(
            initialized.status.success(),
            "{}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        let contents: Value = serde_json::from_slice(&fs::read(&plan).unwrap()).unwrap();
        assert_eq!(contents.get(kind), Some(&serde_json::json!([])));
        let mut invocation = Command::new(env!("CARGO_BIN_EXE_ss-ai"));
        invocation
            .args(["workflow", command, "--artifact-path"])
            .arg(&artifact)
            .arg("--plan-path")
            .arg(&plan);
        if kind == "removals" {
            invocation.args(["--now-utc", "2024-05-06T07:08:09Z"]);
        }
        let output = invocation.output().unwrap();
        assert!(!output.status.success(), "{kind} accepted an empty plan");
        assert!(
            output.stdout.is_empty(),
            "{kind} emitted a successful result"
        );
        assert!(!output.stderr.is_empty(), "{kind} gave no diagnostic");
        assert_eq!(
            fs::read(&artifact).unwrap(),
            before,
            "{kind} changed the artifact"
        );
    }
}
