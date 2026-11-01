//! Exercises the native audit and approval commands across process boundaries.

#![allow(
    clippy::indexing_slicing,
    reason = "The test uses fixed JSON fixtures and checks their documented fields"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

/// Isolates decision journals and artifacts inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".decision-retirement-{}-{name}",
            std::process::id()
        ));
        #[expect(clippy::create_dir, reason = "Fixture collisions must fail")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Runs a workflow command with each argument passed directly to the process.
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

/// Represents a complete stored finding suitable for audit.
fn finding(id: &str) -> Value {
    json!({"schemaVersion":1,"id":id,"origin":"reviewer","sourceFindingIds":[],
        "title":"Problem","description":"Details","reasoning":"Why",
        "consequence":"Impact","recommendedAction":"Fix",
        "anchor":{"kind":"file","path":"src/lib.rs"},"references":[],"impactedLocations":[]})
}

/// Reads the command result without treating an error stream as success.
fn success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A durable classification remains pending until a separate human decision.
#[test]
#[cfg_attr(miri, ignore = "requires native subprocesses")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
#[expect(
    clippy::cognitive_complexity,
    reason = "The test follows one journal across the audit and approval processes"
)]
fn classification_and_approval_resume_across_processes() {
    let workspace = Workspace::new("resume");
    let first = finding("F1");
    let second = finding("F2");
    let artifact = workspace.write(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated",
            "findings":[first,second],"removed":[]}),
    );
    let journal = workspace.path("decisions.json");
    let approved = workspace.path("approved.json");
    let poll = || {
        run(&[
            "audit-poll-classification",
            "--artifact-path",
            text(&artifact),
            "--decisions-path",
            text(&journal),
        ])
    };
    let build = || {
        run(&[
            "approved",
            "build",
            "--path",
            text(&artifact),
            "--decisions-path",
            text(&journal),
            "--output-path",
            text(&approved),
        ])
    };
    let record = |id: &str, decision: &str, policy: bool| {
        let item = workspace.write(
            "item.json",
            &json!({"findingId":id,"decision":decision,"reason":"Classification"}),
        );
        let mut args = vec![
            "audit-add-decisions",
            "--path",
            text(&journal),
            "--item-path",
            text(&item),
            "--recorded-utc",
            "2024-05-06T09:10:11.0000000Z",
        ];
        if policy {
            args.push("--policy-classification");
        }
        success(&run(&args))
    };

    let page = success(&poll());
    assert_eq!(
        page.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["isComplete", "remainingCount", "finding", "auditFindingIds"]
    );
    assert_eq!(
        page,
        json!({"isComplete":false,"remainingCount":2,"finding":first,
            "auditFindingIds":["F1","F2"]})
    );
    assert_eq!(success(&poll()), page);
    assert!(!journal.exists());
    assert_eq!(
        record("F1", "auto-approved", true)["recorded"],
        json!(["F1"])
    );
    let page = success(&poll());
    assert_eq!(page["finding"], second);
    assert_eq!(page["remainingCount"], 1);
    assert_eq!(page["auditFindingIds"], json!(["F2"]));
    assert_eq!(record("F2", "needs-audit", true)["recorded"], json!(["F2"]));
    assert_eq!(
        success(&poll()),
        json!({"isComplete":true,"remainingCount":0,"finding":null,"auditFindingIds":["F2"]})
    );
    let blocked = build();
    assert!(!blocked.status.success());
    assert!(blocked.stdout.is_empty());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("'F2'"));
    assert!(!approved.exists());

    assert_eq!(record("F2", "approved", false)["recorded"], json!(["F2"]));
    let summary = success(&build());
    assert_eq!(summary["approved"], 2);
    assert_eq!(summary["autoApproved"], 1);
    assert_eq!(summary["manuallyApproved"], 1);
    let result: Value = serde_json::from_slice(&fs::read(&approved).unwrap()).unwrap();
    assert_eq!(result["findings"], json!([first, second]));
    assert!(result["target"].is_null());
    assert!(result["reviewedSourceCommit"].is_null());
    assert!(result["reviewedMergeBaseCommit"].is_null());
    let history: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    assert_eq!(history["decisions"].as_array().unwrap().len(), 3);
    assert_eq!(history["decisions"][0]["reason"], "Classification");
    assert_eq!(
        history["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["decision"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["auto-approved", "needs-audit", "approved"]
    );
    assert!(
        history["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record.get("feedback").is_none())
    );
    assert!(!workspace.path("audit-feedback.json").exists());
    let response = workspace.write(
        "item.json",
        &json!({"findingId":"F2","decision":"rejected",
            "reason":"The user disagreed.",
            "feedback":"Generated code is exempt from this rule."}),
    );
    success(&run(&[
        "audit-add-decisions",
        "--path",
        text(&journal),
        "--item-path",
        text(&response),
        "--recorded-utc",
        "2024-05-06T09:10:11.0000000Z",
    ]));
    let history: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    assert_eq!(
        history["decisions"][3],
        json!({"findingId":"F2","decision":"rejected","reason":"The user disagreed.",
            "feedback":"Generated code is exempt from this rule.","recordedUtc":"2024-05-06T09:10:11.0000000Z"})
    );
    let response = workspace.write(
        "item.json",
        &json!({"findingId":"F1","decision":"approved"}),
    );
    success(&run(&[
        "audit-add-decisions",
        "--path",
        text(&journal),
        "--item-path",
        text(&response),
        "--recorded-utc",
        "2024-05-06T09:10:11.0000000Z",
    ]));
    let history: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    assert_eq!(
        history["decisions"][4],
        json!({"findingId":"F1","decision":"approved","recordedUtc":"2024-05-06T09:10:11.0000000Z"})
    );
    let summary = success(&build());
    assert_eq!(summary["approved"], 1);
    assert_eq!(summary["rejected"], 1);
    let result: Value = serde_json::from_slice(&fs::read(&approved).unwrap()).unwrap();
    assert_eq!(result["findings"], json!([first]));
    let response = workspace.write(
        "item.json",
        &json!({"findingId":"F1","decision":"Approved"}),
    );
    success(&run(&[
        "audit-add-decisions",
        "--path",
        text(&journal),
        "--item-path",
        text(&response),
        "--recorded-utc",
        "2024-05-06T09:10:11.0000000Z",
    ]));
    let history: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    assert_eq!(history["decisions"][5]["decision"], "approved");
    record("F1", "auto-approved", true);
    let summary = success(&build());
    assert_eq!(summary["autoApproved"], 1);
    assert_eq!(summary["rejected"], 1);
    let result: Value = serde_json::from_slice(&fs::read(&approved).unwrap()).unwrap();
    assert_eq!(result["findings"], json!([first]));
}

/// Empty work is complete, whereas missing and malformed inputs fail explicitly.
#[test]
#[cfg_attr(miri, ignore = "requires native subprocesses")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn empty_and_corrupt_audit_inputs_are_distinct() {
    let workspace = Workspace::new("boundaries");
    let artifact = workspace.write(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":[],"removed":[]}),
    );
    let journal = workspace.path("decisions.json");
    let poll = |path: &Path| {
        run(&[
            "audit-poll-classification",
            "--artifact-path",
            text(path),
            "--decisions-path",
            text(&journal),
        ])
    };
    assert_eq!(
        success(&poll(&artifact)),
        json!({"isComplete":true,"remainingCount":0,"finding":null,"auditFindingIds":[]})
    );
    assert!(!journal.exists());
    let missing = poll(&workspace.path("missing.json"));
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(!missing.stderr.is_empty());
    let empty_log = workspace.write("decisions.json", &json!({"schemaVersion":1,"decisions":[]}));
    let before = fs::read(&empty_log).unwrap();
    let item = workspace.write(
        "item.json",
        &json!({"findingId":"F1","decision":"approved"}),
    );
    let refused = run(&[
        "audit-add-decisions",
        "--path",
        text(&journal),
        "--item-path",
        text(&item),
        "--recorded-utc",
        "2024-05-06T09:10:11Z",
        "--policy-classification",
    ]);
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("Policy classification cannot record")
    );
    assert_eq!(fs::read(&empty_log).unwrap(), before);

    let plan = workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"decisions":[
            {"findingId":"F1","decision":"approved"},
            {"findingId":"F2","decision":"deferred"}]}),
    );
    let refused = run(&[
        "audit-add-decisions",
        "--path",
        text(&journal),
        "--plan-path",
        text(&plan),
        "--recorded-utc",
        "2024-05-06T09:10:11Z",
    ]);
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("decisions/1/decision"));
    assert_eq!(fs::read(&journal).unwrap(), before);
    workspace.write(
        "decisions.json",
        &json!({"schemaVersion":1,"decisions":[
            {"findingId":"F1","decision":"maybe","recordedUtc":"2024-05-06T09:10:11Z"}]}),
    );
    let corrupt = poll(&artifact);
    assert!(!corrupt.status.success());
    assert!(corrupt.stdout.is_empty());
    let error = String::from_utf8_lossy(&corrupt.stderr);
    assert!(
        error.contains("'maybe'") && error.contains("'F1'"),
        "{error}"
    );
}

/// A multi-record plan persists every decision in order with its authored details.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn decision_plan_records_complete_batch() {
    let workspace = Workspace::new("batch");
    let journal = workspace.path("decisions.json");
    let plan = workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"decisions":[
            {"findingId":"F1","decision":"approved"},
            {"findingId":"F2","decision":"dropped","reason":"The code moved on."}]}),
    );
    let result = success(&run(&[
        "audit-add-decisions",
        "--path",
        text(&journal),
        "--plan-path",
        text(&plan),
        "--recorded-utc",
        "2024-05-06T09:10:11.0000000Z",
    ]));
    assert_eq!(result["recorded"], json!(["F1", "F2"]));
    let document: Value = serde_json::from_slice(&fs::read(journal).unwrap()).unwrap();
    assert_eq!(
        document["decisions"],
        json!([
            {"findingId":"F1","decision":"approved","recordedUtc":"2024-05-06T09:10:11.0000000Z"},
            {"findingId":"F2","decision":"dropped","reason":"The code moved on.",
             "recordedUtc":"2024-05-06T09:10:11.0000000Z"}
        ])
    );
    let before = fs::read(workspace.path("decisions.json")).unwrap();
    for verdict in ["approved", "rejected", "dropped"] {
        workspace.write(
            "plan.json",
            &json!({"schemaVersion":1,"decisions":[
                {"findingId":"F3","decision":"auto-approved"},
                {"findingId":"F4","decision":verdict}]}),
        );
        let rejected = run(&[
            "audit-add-decisions",
            "--path",
            text(&workspace.path("decisions.json")),
            "--plan-path",
            text(&plan),
            "--recorded-utc",
            "2024-05-06T09:10:11.0000000Z",
            "--policy-classification",
        ]);
        assert!(!rejected.status.success(), "{verdict}");
        assert!(rejected.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&rejected.stderr)
                .contains("Policy classification cannot record")
        );
        assert_eq!(fs::read(workspace.path("decisions.json")).unwrap(), before);
    }
}

/// Polling uses the latest verdict and the current set of findings without consuming either.
#[test]
#[cfg_attr(miri, ignore = "requires native subprocesses")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn classification_resumes_from_current_findings_and_latest_verdicts() {
    let workspace = Workspace::new("resume-matrix");
    let artifact = workspace.write(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated",
                "findings":[finding("F1"),finding("F2")],"removed":[]}),
    );
    let journal = workspace.path("decisions.json");
    let poll = || {
        success(&run(&[
            "audit-poll-classification",
            "--artifact-path",
            text(&artifact),
            "--decisions-path",
            text(&journal),
        ]))
    };
    let entry = |id, decision| json!({"findingId":id,"decision":decision,"recordedUtc":"2024-05-06T09:10:11Z"});
    for verdict in ["approved", "auto-approved", "rejected", "dropped"] {
        workspace.write(
            "decisions.json",
            &json!({"schemaVersion":1,"decisions":[
                    entry("F1", "needs-audit"),entry("F1", verdict)]}),
        );
        let before = fs::read(&journal).unwrap();
        let result = poll();
        assert_eq!(result["finding"]["id"], "F2", "{verdict}");
        assert_eq!(result["auditFindingIds"], json!(["F2"]), "{verdict}");
        assert_eq!(fs::read(&journal).unwrap(), before);
    }

    for (verdict, expected) in [
        ("auto-approved", json!([])),
        ("needs-audit", json!(["F1", "F2"])),
    ] {
        workspace.write(
            "decisions.json",
            &json!({"schemaVersion":1,"decisions":[entry("F1", verdict),entry("F2", verdict)]}),
        );
        let result = poll();
        assert_eq!(result["isComplete"], true);
        assert_eq!(result["remainingCount"], 0);
        assert!(result["finding"].is_null());
        assert_eq!(result["auditFindingIds"], expected);
    }

    workspace.write(
        "decisions.json",
        &json!({"schemaVersion":1,"decisions":[entry("F1", "approved"),
                entry("F2", "auto-approved"),entry("F1", "needs-audit")]}),
    );
    workspace.write(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated",
                "findings":[finding("F1"),finding("F2"),finding("F3")],"removed":[]}),
    );
    let result = poll();
    assert_eq!(result["finding"]["id"], "F3");
    assert_eq!(result["auditFindingIds"], json!(["F1", "F3"]));
    let output = workspace.path("approved.json");
    let rejected = run(&[
        "approved",
        "build",
        "--path",
        text(&artifact),
        "--decisions-path",
        text(&journal),
        "--output-path",
        text(&output),
    ]);
    assert!(!rejected.status.success());
    let diagnostic = String::from_utf8_lossy(&rejected.stderr);
    assert!(diagnostic.contains("'F1'") && diagnostic.contains("'F3'"));
    assert!(!output.exists());

    workspace.write(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated",
                "findings":[finding("F2"),finding("F3")],
                "removed":[{"finding":finding("F1"),"reason":"Unsupported.",
                    "removedUtc":"2024-05-06T09:10:11Z"}]}),
    );
    let result = poll();
    assert_eq!(result["finding"]["id"], "F3");
    assert_eq!(result["remainingCount"], 1);
    assert_eq!(result["auditFindingIds"], json!(["F3"]));
}
