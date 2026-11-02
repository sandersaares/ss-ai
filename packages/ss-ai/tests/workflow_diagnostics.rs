//! Exercises finding diagnostics and individual audit decisions at the CLI boundary.

#![allow(
    clippy::indexing_slicing,
    reason = "The test uses fixed JSON fixtures and asserts their expected fields"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

/// Isolates process-level fixtures inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".workflow-diagnostics-{}-{name}",
            std::process::id()
        ));
        #[expect(clippy::create_dir, reason = "Fixture collisions must fail")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Calls the public workflow command without shell interpolation.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .arg("workflow")
        .args(args)
        .output()
        .unwrap()
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Errors name the source, prose field, and the precise repairable condition.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn invalid_prose_reports_source_field_control_and_title_bound() {
    let workspace = Workspace::new("prose");
    let artifact = workspace.0.join("reviewer.json");
    let output = run(&[
        "initialize-finding",
        "--output-directory",
        text(&workspace.0),
        "--reviewer-id",
        "reviewer",
        "--focus-area",
        "correctness",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before = fs::read(&artifact).unwrap();
    let mut finding = json!({
        "title":"Problem","description":"before\u{000D}after","reasoning":"Why",
        "consequence":"Impact","recommendedAction":"Fix",
        "anchor":{"kind":"file","path":"src/lib.rs"}
    });
    for (field, expected) in [("description", "'description'"), ("code", "U+000D")] {
        let source = workspace.write("input.json", &finding);
        let result = run(&[
            "add-finding",
            "--artifact-path",
            text(&artifact),
            "--finding-path",
            text(&source),
            "--origin",
            "reviewer",
        ]);
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{field}: {error}");
        assert!(result.stdout.is_empty());
        assert!(error.contains(text(&source)), "{error}");
        assert!(error.contains(expected), "{error}");
        assert_eq!(fs::read(&artifact).unwrap(), before);
    }
    finding["description"] = json!("Details");
    finding["title"] = json!("🤖".repeat(128));
    let source = workspace.write("input.json", &finding);
    let result = run(&[
        "add-finding",
        "--artifact-path",
        text(&artifact),
        "--finding-path",
        text(&source),
        "--origin",
        "reviewer",
    ]);
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success(), "{error}");
    assert!(error.contains(text(&source)), "{error}");
    assert!(error.contains("256 UTF-16 code units"), "{error}");
    assert!(error.contains("at most 255 UTF-16 code units"), "{error}");
    assert_eq!(fs::read(&artifact).unwrap(), before);

    finding["schemaVersion"] = json!(1);
    finding["id"] = json!("reviewer-1");
    finding["origin"] = json!("reviewer");
    finding["sourceFindingIds"] = json!([]);
    workspace.write(
        "reviewer.json",
        &json!({
            "schemaVersion":1,"kind":"reviewer","reviewerId":"reviewer",
            "focusArea":"correctness","status":"open","findings":[finding]
        }),
    );
    let result = run(&[
        "validate-findings",
        "--path",
        text(&artifact),
        "--fail-on-invalid",
    ]);
    assert!(!result.status.success());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["isValid"], false);
    let errors = report["errors"].to_string();
    assert!(errors.contains("reviewer.json"), "{errors}");
    assert!(errors.contains("256 UTF-16 code units"), "{errors}");
    assert!(errors.contains("at most 255 UTF-16 code units"), "{errors}");
    let before = fs::read(&artifact).unwrap();
    let source = workspace.write(
        "valid-input.json",
        &json!({
            "title":"Problem","description":"Details","reasoning":"Why",
            "consequence":"Impact","recommendedAction":"Fix",
            "anchor":{"kind":"file","path":"src/lib.rs"}
        }),
    );
    let result = run(&[
        "add-finding",
        "--artifact-path",
        text(&artifact),
        "--finding-path",
        text(&source),
        "--origin",
        "reviewer",
    ]);
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success());
    assert!(error.contains(text(&artifact)), "{error}");
    assert!(error.contains("256 UTF-16 code units"), "{error}");
    assert_eq!(fs::read(&artifact).unwrap(), before);
}

/// Canonicalization belongs to individual requests, not persisted plan or journal schemas.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn individual_decision_cli_normalizes_without_granting_policy_authority() {
    let workspace = Workspace::new("decisions");
    let journal = workspace.0.join("decisions.json");
    let item = workspace.write(
        "item.json",
        &json!({"findingId":"F1","decision":"APPROVED"}),
    );
    let timestamp = "2024-05-06T09:10:11Z";
    let invoke = |item: &Path, policy: bool| {
        let mut args = vec![
            "audit-add-decisions",
            "--path",
            text(&journal),
            "--item-path",
            text(item),
            "--recorded-utc",
            timestamp,
        ];
        if policy {
            args.push("--policy-classification");
        }
        run(&args)
    };
    let rejected = invoke(&item, true);
    assert!(!rejected.status.success());
    assert!(!journal.exists());
    let recorded = invoke(&item, false);
    assert!(
        recorded.status.success(),
        "{}",
        String::from_utf8_lossy(&recorded.stderr)
    );
    let result: Value = serde_json::from_slice(&recorded.stdout).unwrap();
    assert_eq!(result["recorded"], json!(["F1"]));
    let item = workspace.write(
        "item.json",
        &json!({"findingId":"F2","decision":" Auto-Approved "}),
    );
    assert!(invoke(&item, true).status.success());
    let document: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    assert_eq!(document["decisions"][0]["decision"], "approved");
    assert_eq!(document["decisions"][1]["decision"], "auto-approved");
    let before = fs::read(&journal).unwrap();
    let plan = workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"decisions":[
            {"findingId":"F3","decision":"approved"},
            {"findingId":"F4","decision":"APPROVED"}
        ]}),
    );
    let rejected = run(&[
        "audit-add-decisions",
        "--path",
        text(&journal),
        "--plan-path",
        text(&plan),
        "--recorded-utc",
        timestamp,
    ]);
    assert!(!rejected.status.success());
    assert_eq!(fs::read(&journal).unwrap(), before);

    let both = run(&[
        "audit-add-decisions",
        "--path",
        text(&journal),
        "--plan-path",
        text(&plan),
        "--item-path",
        text(&item),
        "--recorded-utc",
        timestamp,
    ]);
    assert!(!both.status.success());
    assert!(both.stdout.is_empty());
    assert_eq!(fs::read(&journal).unwrap(), before);
    let neither = run(&[
        "audit-add-decisions",
        "--path",
        text(&journal),
        "--recorded-utc",
        timestamp,
    ]);
    assert!(!neither.status.success());
    assert!(neither.stdout.is_empty());
    assert_eq!(fs::read(&journal).unwrap(), before);

    let invalid_item =
        workspace.write("item.json", &json!({"findingId":"F3","decision":"UNKNOWN"}));
    let rejected = invoke(&invalid_item, false);
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert_eq!(fs::read(&journal).unwrap(), before);

    let plan = workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"decisions":[{"findingId":"F3","decision":"rejected"}]}),
    );
    let recorded = run(&[
        "audit-add-decisions",
        "--path",
        text(&journal),
        "--plan-path",
        text(&plan),
        "--recorded-utc",
        timestamp,
    ]);
    assert!(
        recorded.status.success(),
        "{}",
        String::from_utf8_lossy(&recorded.stderr)
    );
    let document: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    assert_eq!(document["decisions"][2]["findingId"], "F3");
    assert_eq!(document["decisions"][2]["decision"], "rejected");
}
