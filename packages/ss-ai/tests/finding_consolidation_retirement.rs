//! Exercises index and consolidation commands without retired review helpers.

#![allow(
    clippy::indexing_slicing,
    reason = "The test uses fixed JSON fixtures and checks their documented fields"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

/// Keeps the independent CLI fixture inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".consolidation-retirement-{}-{name}",
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

/// Invokes a native workflow command without a shell or PowerShell helpers.
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

/// Builds a stored record with content and provenance for an index consumer.
fn finding(id: &str, path: &str) -> Value {
    json!({
        "schemaVersion":1,"id":id,"origin":"reviewer","sourceFindingIds":[],
        "title":"Problem","description":"Details","reasoning":"Why",
        "consequence":"Impact","recommendedAction":"Fix",
        "anchor":{"kind":"file","path":path},"references":[],"impactedLocations":[]
    })
}

/// The public commands publish compact index entries and accurate consolidation summaries.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
#[expect(
    clippy::cognitive_complexity,
    reason = "The test follows the same reviewer artifacts through both commands"
)]
fn index_and_consolidation_report_complete_results() {
    let workspace = Workspace::new("results");
    let reviewers = workspace.0.join("reviewers");
    #[expect(clippy::create_dir, reason = "The fixture must start empty")]
    fs::create_dir(&reviewers).unwrap();
    let index = workspace.0.join("index.json");
    let empty = run(&[
        "finding-index",
        "--reviewer-directory",
        text(&reviewers),
        "--output-path",
        text(&index),
    ]);
    assert!(!empty.status.success());
    assert!(empty.stdout.is_empty());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("No reviewer artifacts"));
    assert!(!index.exists());
    let mut first = finding("g1-1", "src/a.rs");
    first["anchor"] = json!({"kind":"line","path":"src/a.rs","startLine":42});
    first["impactedLocations"] = json!([
        {"path":"src/a.rs","startLine":42},{"path":"src/extra.rs"},
        {"path":"src/a.rs","startLine":42}]);
    let mut second = finding("g1-2", "src/other.rs");
    second["followUp"] = json!({"threadId":"RT1","commentId":"C1"});
    let mut third = finding("g2-1", "src/b.rs");
    third["anchor"] = json!({"kind":"line","path":"src/b.rs","startLine":8});
    workspace.write(
        "reviewers/g1.json",
        &json!({"schemaVersion":1,"kind":"reviewer","reviewerId":"g1",
            "focusArea":"correctness","status":"complete","findings":[first,second]}),
    );
    workspace.write(
        "reviewers/g2.json",
        &json!({"schemaVersion":1,"kind":"reviewer","reviewerId":"g2",
            "focusArea":"security","status":"complete","findings":[third]}),
    );
    let result = run(&[
        "finding-index",
        "--reviewer-directory",
        text(&reviewers),
        "--output-path",
        text(&index),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stderr.is_empty());
    let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(summary["artifactCount"], 2);
    assert_eq!(summary["findingCount"], 3);
    let document: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    assert_eq!(
        document,
        json!({"schemaVersion":1,"findings":[
            {"id":"g1-1","reviewerId":"g1","focusArea":"correctness",
             "title":"Problem","description":"Details","recommendedAction":"Fix",
             "anchorPath":"src/a.rs","impactedPaths":["src/a.rs","src/extra.rs"]},
            {"id":"g1-2","reviewerId":"g1","focusArea":"correctness",
             "title":"Problem","description":"Details","recommendedAction":"Fix",
             "anchorPath":"src/other.rs","impactedPaths":[],"followUpThreadId":"RT1"},
            {"id":"g2-1","reviewerId":"g2","focusArea":"security",
             "title":"Problem","description":"Details","recommendedAction":"Fix",
             "anchorPath":"src/b.rs","impactedPaths":[]}
        ]})
    );

    let consolidated = workspace.0.join("consolidated.json");
    let command = |plan: Option<&Path>| {
        let mut args = vec![
            "consolidate-findings",
            "--reviewer-directory",
            text(&reviewers),
            "--output-path",
            text(&consolidated),
        ];
        if let Some(plan) = plan {
            args.extend(["--plan-path", text(plan)]);
        }

        run(&args)
    };
    let result = command(None);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(summary["findingCount"], 3);
    assert_eq!(summary["mergedCount"], 0);
    assert_eq!(summary["carriedOverCount"], 3);
    let before = fs::read(&consolidated).unwrap();
    let output: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(
        output["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|finding| (&finding["id"], &finding["sourceFindingIds"]))
            .collect::<Vec<_>>(),
        vec![
            (&json!("F1"), &json!(["g1-1"])),
            (&json!("F2"), &json!(["g1-2"])),
            (&json!("F3"), &json!(["g2-1"]))
        ]
    );
    let missing = command(Some(&workspace.0.join("missing.json")));
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert_eq!(fs::read(&consolidated).unwrap(), before);
    let empty_plan = workspace.write("plan.json", &json!({"schemaVersion":1,"groups":[]}));
    let empty = command(Some(&empty_plan));
    assert!(
        empty.status.success(),
        "{}",
        String::from_utf8_lossy(&empty.stderr)
    );
    let empty_summary: Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(empty_summary["findingCount"], 3);
    assert_eq!(empty_summary["mergedCount"], 0);
    assert_eq!(empty_summary["carriedOverCount"], 3);

    let singleton = workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"groups":[
            {"sourceFindingIds":["g1-1"],"findingPath":"merged.json"}]}),
    );
    let before = fs::read(&consolidated).unwrap();
    assert!(!command(Some(&singleton)).status.success());
    assert_eq!(fs::read(&consolidated).unwrap(), before);

    workspace.write(
        "merged.json",
        &json!({"title":"Merged","description":"Details","reasoning":"Why",
            "consequence":"Impact","recommendedAction":"Fix",
            "anchor":{"kind":"line","path":"src/a.rs","startLine":40,"endLine":50},
            "impactedLocations":[{"path":"src/extra.rs"},{"path":"src/b.rs"}]}),
    );
    let merge_plan = workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"groups":[
            {"sourceFindingIds":["g1-1","g2-1"],"findingPath":"merged.json"}]}),
    );
    let result = command(Some(&merge_plan));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stderr.is_empty());
    let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(summary["findingCount"], 2);
    assert_eq!(summary["mergedCount"], 1);
    assert_eq!(summary["carriedOverCount"], 1);
    let output: Value = serde_json::from_slice(&fs::read(&consolidated).unwrap()).unwrap();
    assert_eq!(output["kind"], "consolidated");
    assert_eq!(output["findings"][0]["title"], "Merged");
    assert_eq!(
        output["findings"][0]["sourceFindingIds"],
        json!(["g1-1", "g2-1"])
    );
    assert_eq!(output["findings"][1]["sourceFindingIds"], json!(["g1-2"]));

    fs::remove_file(&consolidated).unwrap();
    let duplicate = workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"groups":[
            {"sourceFindingIds":["g1-1","g2-1"],"findingPath":"merged.json"},
            {"sourceFindingIds":["g1-1","g1-2"],"findingPath":"merged.json"}]}),
    );
    let rejected = command(Some(&duplicate));
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("'g1-1'"));
    assert!(!consolidated.exists());
}

/// Empty output remains a complete index and incompatible reviewer inputs are rejected.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn empty_index_and_non_reviewer_artifacts_are_distinct() {
    let workspace = Workspace::new("empty");
    let reviewers = workspace.0.join("reviewers");
    #[expect(clippy::create_dir, reason = "The fixture must start empty")]
    fs::create_dir(&reviewers).unwrap();
    let index = workspace.0.join("index.json");
    workspace.write(
        "reviewers/g1.json",
        &json!({"schemaVersion":1,"kind":"reviewer","reviewerId":"g1",
            "focusArea":"correctness","status":"complete","findings":[]}),
    );
    let command = || {
        run(&[
            "finding-index",
            "--reviewer-directory",
            text(&reviewers),
            "--output-path",
            text(&index),
        ])
    };
    let result = command();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(summary["artifactCount"], 1);
    assert_eq!(summary["findingCount"], 0);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&index).unwrap()).unwrap(),
        json!({"schemaVersion":1,"findings":[]})
    );

    workspace.write(
        "reviewers/consolidated.json",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":[],"removed":[]}),
    );
    let before = fs::read(&index).unwrap();
    let rejected = command();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("reviewer artifact"));
    assert_eq!(fs::read(index).unwrap(), before);
}

/// CLI failures retain diagnostic context and do not replace previous results.
#[test]
#[cfg_attr(miri, ignore = "requires native subprocesses")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn consolidation_errors_identify_repairable_evidence() {
    let workspace = Workspace::new("diagnostics");
    let reviewers = workspace.0.join("reviewers");
    #[expect(clippy::create_dir, reason = "The fixture must start empty")]
    fs::create_dir(&reviewers).unwrap();
    let mut first = finding("g1-1", "src/retry.rs");
    first["anchor"] = json!({"kind":"line","path":"src/retry.rs","startLine":42});
    let mut second = finding("g2-1", "src/backoff.rs");
    second["anchor"] = json!({"kind":"line","path":"src/retry.rs","startLine":42});
    second["impactedLocations"] = json!([{"path":"src/backoff.rs","startLine":8}]);
    let artifact = |reviewer_id, record: &Value| {
        json!({"schemaVersion":1,"kind":"reviewer","reviewerId":reviewer_id,
            "focusArea":"correctness","status":"complete","findings":[record]})
    };
    workspace.write("reviewers/g1.json", &artifact("g1", &first));
    workspace.write("reviewers/g2.json", &artifact("g2", &first));
    let index = workspace.0.join("index.json");
    let consolidated = workspace.0.join("consolidated.json");
    let original = b"previous complete output";
    fs::write(&index, original).unwrap();
    fs::write(&consolidated, original).unwrap();
    let index_command = || {
        run(&[
            "finding-index",
            "--reviewer-directory",
            text(&reviewers),
            "--output-path",
            text(&index),
        ])
    };
    let consolidate_command = |plan: Option<&Path>| {
        let mut args = vec![
            "consolidate-findings",
            "--reviewer-directory",
            text(&reviewers),
            "--output-path",
            text(&consolidated),
        ];
        if let Some(plan) = plan {
            args.extend(["--plan-path", text(plan)]);
        }
        run(&args)
    };
    let rejected = index_command();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("'g1-1'"));
    assert_eq!(fs::read(&index).unwrap(), original);

    workspace.write("reviewers/g2.json", &artifact("g2", &second));
    first["description"] = json!("before\u{001b}after");
    workspace.write("reviewers/g1.json", &artifact("g1", &first));
    let rejected = consolidate_command(None);
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    let error = String::from_utf8_lossy(&rejected.stderr);
    for detail in ["g1.json", "'g1-1'", "'description'", "U+001B"] {
        assert!(error.contains(detail), "{error}");
    }
    assert_eq!(fs::read(&consolidated).unwrap(), original);

    first["description"] = json!("Details");
    workspace.write("reviewers/g1.json", &artifact("g1", &first));
    workspace.write(
        "merged.json",
        &json!({"title":"Merged","description":"Details","reasoning":"Why",
            "consequence":"Impact","recommendedAction":"Fix",
            "anchor":{"kind":"line","path":"src/retry.rs","startLine":42}}),
    );
    let plan = workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"groups":[
            {"sourceFindingIds":["g1-1","g9-4"],"findingPath":"merged.json"}]}),
    );
    let rejected = consolidate_command(Some(&plan));
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("'g9-4'"));
    assert_eq!(fs::read(&consolidated).unwrap(), original);

    workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"groups":[
            {"sourceFindingIds":["g1-1","g2-1"],"findingPath":"merged.json"}]}),
    );
    let rejected = consolidate_command(Some(&plan));
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    let error = String::from_utf8_lossy(&rejected.stderr);
    assert!(error.contains("src/backoff.rs:8"), "{error}");
    assert!(error.contains("'g2-1'"), "{error}");
    assert_eq!(fs::read(consolidated).unwrap(), original);
}
