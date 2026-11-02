//! Exercises decision recovery and capture-bound approval without retired helpers.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixtures use fixed record shapes and directly assert failures"
)]

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::evidence_hash;
use ss_ai_review_workflow::{approved, audit, findings};
use ss_ai_runtime::{Clock, Process, ProcessOutput, fail};

/// Keeps fixture timestamps deterministic and refuses unexpected contention.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected lock contention"))
    }
}

/// Isolates journals, captures, and Git repositories inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".migration-decisions-{}-{name}",
            std::process::id()
        ));
        #[expect(clippy::create_dir, reason = "A collision must not reuse test data")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn put(&self, name: &str, value: &Value) -> PathBuf {
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

/// Supplies complete stored findings rather than worker drafts.
fn finding(id: &str, path: &str, kind: &str) -> Value {
    let anchor = if kind == "line" {
        json!({"kind":"line","path":path,"startLine":42})
    } else {
        json!({"kind":"file","path":path})
    };
    json!({"schemaVersion":1,"id":id,"origin":"reviewer","sourceFindingIds":[],
        "title":"Problem","description":"Details","reasoning":"Evidence",
        "consequence":"Impact","recommendedAction":"Fix","anchor":anchor})
}

/// Supplies the evidence fields used to bind an approved set.
fn capture(base: &str, head: &str) -> Value {
    json!({"schemaVersion":3,"platform":"gitHub","repositoryUrl":"https://example.test/repo",
        "pullRequestUrl":"https://example.test/pr/1",
        "target":{"platform":"gitHub","pullRequestId":1},
        "pullRequest":{"title":"Title","description":"Body",
            "reviewedMergeBaseCommit":base,"reviewedSourceCommit":head},
        "reviewers":[],"revisions":[],"threads":[]})
}

/// Supplies an exact diff without external Git for decision-only cases.
struct Git;

impl Process for Git {
    fn execute(&self, _: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        let text = if arguments.iter().any(|arg| arg == "cat-file") {
            "commit\n"
        } else if arguments.iter().any(|arg| arg == "diff") {
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
        } else {
            ""
        };
        Ok(ProcessOutput::new(
            true,
            text.as_bytes().to_vec(),
            Vec::new(),
        ))
    }
}

/// Records one policy or human decision with a caller-provided authority flag.
fn record(workspace: &Workspace, id: &str, decision: &str, policy: bool, feedback: Option<&str>) {
    let mut item = json!({"findingId":id,"decision":decision});
    if let Some(feedback) = feedback {
        item["feedback"] = json!(feedback);
    }
    let item = workspace.put("item.json", &item);
    audit::add_decision(
        &workspace.path("decisions.json"),
        &item,
        None,
        policy,
        "2024-05-06T09:10:11Z",
        &FixedClock,
    )
    .unwrap();
}

#[test]
fn human_audit_resume_does_not_consume_or_approve_remaining_findings() {
    let workspace = Workspace::new("human-resume");
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,
        "kind":"consolidated","findings":[finding("F1","src/lib.rs","file"),
        finding("F2","src/lib.rs","file")],"removed":[]}),
    );
    let decisions = workspace.path("decisions.json");
    let output = workspace.path("approved.json");
    record(&workspace, "F1", "needs-audit", true, None);
    record(&workspace, "F2", "needs-audit", true, None);
    assert_eq!(
        audit::poll_classification(&artifact, &decisions, &FixedClock).unwrap()["auditFindingIds"],
        json!(["F1", "F2"])
    );
    record(&workspace, "F1", "approved", false, None);
    let before = fs::read(&decisions).unwrap();
    for _ in 0..2 {
        let result = audit::poll_classification(&artifact, &decisions, &FixedClock).unwrap();
        assert_eq!(result["isComplete"], true);
        assert_eq!(result["auditFindingIds"], json!(["F2"]));
        assert!(
            approved::build_approved(
                &artifact,
                &decisions,
                &output,
                None,
                None,
                &FixedClock,
                &Git
            )
            .is_err()
        );
        assert!(!output.exists());
        assert_eq!(fs::read(&decisions).unwrap(), before);
    }
    record(
        &workspace,
        "F2",
        "rejected",
        false,
        Some("Not applicable to this change."),
    );
    let result = approved::build_approved(
        &artifact,
        &decisions,
        &output,
        None,
        None,
        &FixedClock,
        &Git,
    )
    .unwrap();
    assert_eq!(result["findingId"], json!(["F1"]));
    assert_eq!(result["rejected"], 1);
    assert_eq!(
        audit::read_decisions(&decisions, &FixedClock).unwrap()["decisions"][3]["feedback"],
        "Not applicable to this change."
    );
    assert!(!workspace.path("merge-approval.json").exists());
}

#[test]
fn malformed_artifacts_and_journals_never_report_poll_completion() {
    let workspace = Workspace::new("poll-invalid");
    let artifact = workspace.path("findings.json");
    let journal = workspace.path("decisions.json");
    for value in [
        json!({"schemaVersion":1,"kind":"consolidated","removed":[]}),
        json!({"schemaVersion":1,"kind":"reviewer","reviewerId":"r","focusArea":"rules","status":"open","findings":[],"removed":[]}),
        json!({"schemaVersion":1,"kind":"consolidated","findings":[finding("F1","a.rs","file"),finding("F1","b.rs","file")],"removed":[]}),
    ] {
        fs::write(&artifact, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(audit::poll_classification(&artifact, &journal, &FixedClock).is_err());
    }
    workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated",
        "findings":[finding("F1","a.rs","file")],"removed":[]}),
    );
    workspace.put("decisions.json", &json!({"schemaVersion":1}));
    assert!(audit::poll_classification(&artifact, &journal, &FixedClock).is_err());
}

#[test]
fn empty_and_malformed_approved_sets_are_checked_before_publication() {
    let workspace = Workspace::new("approved-boundaries");
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":[],"removed":[]}),
    );
    let journal = workspace.path("decisions.json");
    let output = workspace.path("approved.json");
    let result =
        approved::build_approved(&artifact, &journal, &output, None, None, &FixedClock, &Git)
            .unwrap();
    assert_eq!(result["approved"], 0);
    assert_eq!(result["findingId"], json!([]));
    assert_eq!(result["unanchoredFinding"], json!([]));
    let document: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(document["findings"], json!([]));
    let before = fs::read(&output).unwrap();
    let mut invalid = finding("F1", "src/lib.rs", "file");
    invalid["description"] = json!("before\u{1b}after");
    workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":[invalid],"removed":[]}),
    );
    record(&workspace, "F1", "approved", false, None);
    assert!(
        approved::build_approved(&artifact, &journal, &output, None, None, &FixedClock, &Git)
            .is_err()
    );
    assert_eq!(fs::read(&output).unwrap(), before);
}

#[test]
fn follow_up_binding_requires_a_reply_capable_current_thread() {
    let workspace = Workspace::new("followups");
    let mut item = finding("F1", "src/lib.rs", "file");
    item["followUp"] = json!({"threadId":"T","commentId":"C"});
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":[item],"removed":[]}),
    );
    let journal = workspace.path("decisions.json");
    let output = workspace.path("approved.json");
    record(&workspace, "F1", "approved", false, None);
    assert_eq!(
        approved::build_approved(&artifact, &journal, &output, None, None, &FixedClock, &Git)
            .unwrap()["approved"],
        1
    );
    let mut context = capture(&"a".repeat(40), &"b".repeat(40));
    context["threads"] = json!([{"id":"T","openingCommentId":"C","kind":"text","resolution":"active",
        "isOutdated":false,"isAuthoredBySelf":false,"context":{"filePath":"src/lib.rs"},"decision":null,
        "url":null,"eventUtc":null,"lastUpdatedUtc":null,"comments":[]}]);
    let path = workspace.put("capture.json", &context);
    let build = || {
        approved::build_approved(
            &artifact,
            &journal,
            &output,
            Some(&path),
            Some(&workspace.0),
            &FixedClock,
            &Git,
        )
    };
    assert_eq!(build().unwrap()["approved"], 1);
    let before = fs::read(&output).unwrap();
    for change in ["missing", "system"] {
        let mut changed = context.clone();
        if change == "missing" {
            changed["threads"][0]["id"] = json!("other");
        } else {
            changed["threads"][0]["kind"] = json!("system");
        }
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(build().is_err(), "{change}");
        assert_eq!(fs::read(&output).unwrap(), before);
    }
}

#[test]
fn withdrawn_findings_are_excluded_from_poll_and_fresh_capture_binding() {
    let workspace = Workspace::new("source-movement");
    let artifact = workspace.put("findings.json", &json!({"schemaVersion":1,"kind":"consolidated",
        "findings":[finding("F1","src/lib.rs","file"),finding("F2","src/lib.rs","file")],"removed":[]}));
    let journal = workspace.path("decisions.json");
    findings::remove(
        &artifact,
        &[("F1".into(), "Fixed by the checked source".into())],
        &FixedClock,
    )
    .unwrap();
    record(&workspace, "F2", "needs-audit", true, None);
    let poll = audit::poll_classification(&artifact, &journal, &FixedClock).unwrap();
    assert_eq!(poll["auditFindingIds"], json!(["F2"]));
    assert_eq!(poll["isComplete"], true);
    let document = findings::read_artifact(&artifact, &FixedClock).unwrap();
    assert_eq!(document["removed"][0]["finding"]["id"], "F1");
    record(&workspace, "F2", "approved", false, None);
    let initial = capture(&"a".repeat(40), &"b".repeat(40));
    let changed = capture(&"a".repeat(40), &"c".repeat(40));
    let path = workspace.put("capture.json", &changed);
    let output = workspace.path("approved.json");
    approved::build_approved(
        &artifact,
        &journal,
        &output,
        Some(&path),
        Some(&workspace.0),
        &FixedClock,
        &Git,
    )
    .unwrap();
    let result: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(result["findings"][0]["id"], "F2");
    assert_eq!(
        result["reviewedSourceCommit"],
        changed["pullRequest"]["reviewedSourceCommit"]
    );
    assert_eq!(
        result["reviewedContextHash"],
        evidence_hash(&changed).unwrap()
    );
    assert_ne!(
        result["reviewedContextHash"],
        evidence_hash(&initial).unwrap()
    );
}
