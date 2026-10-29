//! Exercises local workflow commands through the installed executable boundary.

use std::fs;
use std::process::Command;

use serde_json::Value;
use tempfile::tempdir;

/// Local finding plans must not require provider authentication or executables.
#[test]
fn plan_creation_runs_without_provider_tools() {
    let directory = tempdir().unwrap();
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
