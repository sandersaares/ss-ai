//! Integration coverage for durable local audit operations.
#![allow(
    clippy::assertions_on_result_states,
    clippy::indexing_slicing,
    reason = "Tests use fixed fixture shapes and assert operation failures directly"
)]

use std::cell::Cell;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::audit::{
    add_decisions, add_feedback, get_feedback, last_decision, poll_classification, read_decisions,
    set_merge_approval, set_merge_approval_from_files, test_merge_approval,
};
use ss_ai_runtime::{Clock, FileTransaction, SystemClock, caused_by, fail};

/// Keeps each integration test's files in the checkout, away from other tests.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = env::current_dir()
            .unwrap()
            .join(format!(".audit-test-{}-{name}", std::process::id()));
        #[expect(
            clippy::create_dir,
            reason = "A collision must fail rather than reuse another test's files"
        )]
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

/// Builds the capture evidence needed for commit-bound decisions.
fn capture(commit: &str, self_authored: bool) -> Value {
    json!({
        "schemaVersion": 3, "platform": "gitHub",
        "repositoryUrl": "https://example.test/repo",
        "pullRequestUrl": "https://example.test/pr/1",
        "pullRequest": { "title": "Title", "description": "Body",
            "reviewedSourceCommit": commit, "reviewedMergeBaseCommit": "a".repeat(40),
            "isAuthoredBySelf": self_authored },
        "reviewers": [], "revisions": [], "threads": []
    })
}

/// Synchronizes a predecessor writer with a native transaction through pipes.
struct PredecessorWriter {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl PredecessorWriter {
    fn start(
        workspace: &Workspace,
        mode: &str,
        artifact: &Path,
        record: &Path,
        context: &Path,
        diff: &Path,
    ) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit_predecessor.ps1");
        let mut child = Command::new("pwsh")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(script)
            .arg("-Root")
            .arg(root)
            .arg("-Mode")
            .arg(mode)
            .arg("-Artifact")
            .arg(artifact)
            .arg("-Decisions")
            .arg(record)
            .arg("-Context")
            .arg(context)
            .arg("-Diff")
            .arg(diff)
            .current_dir(&workspace.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("PowerShell 7.6 is required for interoperation tests");
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
        }
    }

    fn expect(&mut self, expected: &str) {
        let mut line = String::new();
        assert_ne!(
            self.output.read_line(&mut line).unwrap(),
            0,
            "fixture exited"
        );
        assert_eq!(line.trim_end(), expected);
    }

    fn retry(&mut self) {
        writeln!(self.input, "retry").unwrap();
        self.input.flush().unwrap();
    }

    fn finish(&mut self) {
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for PredecessorWriter {
    fn drop(&mut self) {
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

/// Supplies complete stored findings rather than authored drafts.
fn finding(id: &str, path: &str) -> Value {
    json!({"schemaVersion": 1, "id": id, "sourceFindingIds": [], "origin": "reviewer",
        "title":"Title", "description":"Description", "reasoning":"Reasoning",
        "consequence":"Consequence", "recommendedAction":"Action",
        "references":[], "impactedLocations":[], "anchor":{"kind":"file","path":path}})
}

/// Creates the same versioned decision plan accepted by the plan authoring API.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Callers construct each one-use plan inline"
)]
fn plan(workspace: &Workspace, entries: Value) -> PathBuf {
    workspace.put(
        "plan.json",
        &json!({"schemaVersion": 1, "decisions": entries}),
    )
}

/// Produces the Unicode encodings recognized by the predecessor stream reader.
fn encoded(text: &str) -> Vec<Vec<u8>> {
    let mut utf8 = vec![0xef, 0xbb, 0xbf];
    utf8.extend_from_slice(text.as_bytes());
    let mut utf16_le = vec![0xff, 0xfe];
    let mut utf16_be = vec![0xfe, 0xff];
    for word in text.encode_utf16() {
        utf16_le.extend_from_slice(&word.to_le_bytes());
        utf16_be.extend_from_slice(&word.to_be_bytes());
    }
    let mut utf32_le = vec![0xff, 0xfe, 0x00, 0x00];
    let mut utf32_be = vec![0x00, 0x00, 0xfe, 0xff];
    for character in text.chars() {
        utf32_le.extend_from_slice(&u32::from(character).to_le_bytes());
        utf32_be.extend_from_slice(&u32::from(character).to_be_bytes());
    }
    vec![utf8, utf16_le, utf16_be, utf32_le, utf32_be]
}

/// Waits for the PowerShell parent to release a lock before retrying.
struct ProcessInteropClock {
    waited: Cell<bool>,
}

impl Clock for ProcessInteropClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        if self.waited.replace(true) {
            return Err(fail("contention", "Unexpected repeated lock contention"));
        }
        writeln!(io::stderr(), "lock-contended")
            .map_err(|error| caused_by("io", "Cannot signal contention", error))?;
        let mut reply = String::new();
        io::stdin()
            .read_line(&mut reply)
            .map_err(|error| caused_by("io", "Cannot receive retry", error))?;
        if reply.trim_end() != "retry" {
            return Err(fail("invalidInput", "Expected retry permission"));
        }
        Ok(())
    }
}

/// Runs a native transaction only when a PowerShell parent supplies its inputs.
#[test]
fn native_audit_lock_interop_helper() {
    let Some(operation) = env::var_os("SS_AI_AUDIT_INTEROP_OPERATION") else {
        return;
    };
    let record = env::var_os("SS_AI_AUDIT_INTEROP_RECORD").expect("Parent supplies the record");
    let input = env::var_os("SS_AI_AUDIT_INTEROP_INPUT").expect("Parent supplies the input");
    let clock = ProcessInteropClock {
        waited: Cell::new(false),
    };
    match operation.to_str() {
        Some("decision") => {
            add_decisions(
                Path::new(&record),
                Path::new(&input),
                None,
                true,
                "2026-10-08T00:00:00Z",
                &clock,
            )
            .unwrap();
        }
        Some("feedback") => {
            let findings =
                env::var_os("SS_AI_AUDIT_INTEROP_FINDINGS").expect("Parent supplies findings");
            let response = env::var("SS_AI_AUDIT_INTEROP_RESPONSE").expect("Parent supplies ID");
            let finding = env::var("SS_AI_AUDIT_INTEROP_FINDING").expect("Parent supplies finding");
            add_feedback(
                Path::new(&record),
                Path::new(&findings),
                &response,
                &finding,
                Path::new(&input),
                &clock,
            )
            .unwrap();
        }
        _ => panic!("Parent supplies a supported audit operation"),
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires subprocesses and file locks")]
fn audit_interop_helper_reports_real_contention_for_each_operation() {
    for operation in ["decision", "feedback"] {
        let workspace = Workspace::new(&format!("helper-{operation}"));
        let record = workspace.path("record.json");
        let clock = SystemClock::default();
        let input = if operation == "decision" {
            plan(
                &workspace,
                json!([{"findingId":"F-1","decision":"needs-audit"}]),
            )
        } else {
            let path = workspace.path("response.txt");
            fs::write(&path, "response").unwrap();
            path
        };
        let findings = workspace.put(
            "findings.json",
            &json!({"schemaVersion":1, "kind":"consolidated",
                "findings":[finding("F-1","a.rs")], "removed":[]}),
        );
        let lock = FileTransaction::open(&record, &clock).unwrap();
        let mut child = Command::new(env::current_exe().unwrap())
            .args(["native_audit_lock_interop_helper", "--exact", "--nocapture"])
            .env("SS_AI_AUDIT_INTEROP_OPERATION", operation)
            .env("SS_AI_AUDIT_INTEROP_RECORD", &record)
            .env("SS_AI_AUDIT_INTEROP_INPUT", &input)
            .env("SS_AI_AUDIT_INTEROP_FINDINGS", &findings)
            .env("SS_AI_AUDIT_INTEROP_RESPONSE", "response-1")
            .env("SS_AI_AUDIT_INTEROP_FINDING", "F-1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut error = BufReader::new(child.stderr.take().unwrap());
        let mut signal = String::new();
        error.read_line(&mut signal).unwrap();
        assert_eq!(signal.trim_end(), "lock-contended");
        drop(lock);
        let mut input = child.stdin.take().unwrap();
        writeln!(input, "retry").unwrap();
        drop(input);
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(record.exists());
    }
}

#[test]
fn decisions_validate_complete_batches_and_preserve_verbatim_feedback() {
    let workspace = Workspace::new("decisions");
    let path = workspace.path("decisions.json");
    let clock = SystemClock::default();
    let timestamp = "2026-01-01T00:00:00Z";
    let input = plan(
        &workspace,
        json!([{"findingId":"F-1","decision":"needs-audit"}]),
    );
    let result = add_decisions(&path, &input, None, true, timestamp, &clock).unwrap();
    assert_eq!(result["recorded"], json!(["F-1"]));
    let before = fs::read(&path).unwrap();
    for entries in [
        json!([{"findingId":"F-2","decision":"auto-approved"},{"findingId":"F-3","decision":"approved"}]),
        json!([{"findingId":"F-2","decision":"auto-approved"},{"findingId":"F-2","decision":"needs-audit"}]),
        json!([{"findingId":"F-2","decision":"auto-approved","feedback":"user words"}]),
        json!([{"findingId":"F-2","decision":"invalid"}]),
    ] {
        let input = plan(&workspace, entries);
        assert!(add_decisions(&path, &input, None, true, timestamp, &clock).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let input = plan(
        &workspace,
        json!([{"findingId":"F-1","decision":"approved",
        "feedback":"  Exact words\nnext line  "}]),
    );
    assert!(add_decisions(&path, &input, None, false, "bad timestamp", &clock).is_err());
    add_decisions(&path, &input, None, false, timestamp, &clock).unwrap();
    let document = read_decisions(&path, &clock).unwrap();
    assert_eq!(last_decision(&document, "F-1").unwrap(), Some("approved"));
    assert_eq!(
        document["decisions"][1]["feedback"],
        "  Exact words\nnext line  "
    );
    assert_eq!(last_decision(&document, "F-2").unwrap(), None);
    fs::write(
        &path,
        br#"{"schemaVersion":1,"decisions":[],"decisions":[]}"#,
    )
    .unwrap();
    assert!(add_decisions(&path, &input, None, false, timestamp, &clock).is_err());
    assert!(read_decisions(&path, &clock).is_err());
}

#[test]
fn empty_decision_batches_leave_existing_and_missing_journals_untouched() {
    let workspace = Workspace::new("empty-decisions");
    let journal = workspace.path("decisions.json");
    let lock = workspace.path("decisions.json.lock");
    let input = plan(&workspace, json!([]));
    let clock = SystemClock::default();
    let timestamp = "2026-01-01T00:00:00Z";

    for policy in [false, true] {
        assert!(add_decisions(&journal, &input, None, policy, timestamp, &clock).is_err());
        assert!(!journal.exists());
        assert!(!lock.exists());
    }

    let original = br#"{"schemaVersion":1,"decisions":[]}"#;
    fs::write(&journal, original).unwrap();
    for policy in [false, true] {
        assert!(add_decisions(&journal, &input, None, policy, timestamp, &clock).is_err());
        assert_eq!(fs::read(&journal).unwrap(), original);
        assert!(!lock.exists());
    }
}

#[test]
fn feedback_is_verbatim_idempotent_and_does_not_create_verdicts() {
    let workspace = Workspace::new("feedback");
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,
        "kind":"consolidated", "findings":[finding("F-1","src/lib.rs")], "removed":[]}),
    );
    let path = workspace.path("feedback.json");
    let text_path = workspace.path("text.txt");
    fs::write(&text_path, "  First\nline  ").unwrap();
    let clock = SystemClock::default();
    add_feedback(&path, &artifact, "response-1", "F-1", &text_path, &clock).unwrap();
    let before = fs::read(&path).unwrap();
    add_feedback(&path, &artifact, "response-1", "F-1", &text_path, &clock).unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::write(&text_path, "different").unwrap();
    assert!(add_feedback(&path, &artifact, "response-1", "F-1", &text_path, &clock).is_err());
    assert!(
        add_feedback(
            &path,
            &artifact,
            "response-2",
            "missing",
            &text_path,
            &clock
        )
        .is_err()
    );
    assert!(add_feedback(&path, &artifact, "bad id", "F-1", &text_path, &clock).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        get_feedback(&path, "F-1", &clock).unwrap(),
        "  First\nline  "
    );
    let journal = workspace.path("decisions.json");
    let input = plan(
        &workspace,
        json!([{"findingId":"F-1","decision":"approved"}]),
    );
    add_decisions(
        &journal,
        &input,
        Some(&path),
        false,
        "2026-01-01T00:00:00Z",
        &clock,
    )
    .unwrap();
    assert_eq!(
        read_decisions(&journal, &clock).unwrap()["decisions"][0]["feedback"],
        "  First\nline  "
    );
    assert!(
        add_decisions(
            &journal,
            &input,
            Some(&path),
            true,
            "2026-01-01T00:00:00Z",
            &clock
        )
        .is_err()
    );
    let before = fs::read(&journal).unwrap();
    assert!(
        add_decisions(
            &journal,
            &journal,
            None,
            false,
            "2026-01-01T00:00:00Z",
            &clock
        )
        .is_err()
    );
    assert_eq!(fs::read(&journal).unwrap(), before);
    fs::write(&path, br#"{"schemaVersion":1,"feedback":[{"responseId":"x","findingId":"F-1","text":"a"},{"responseId":"x","findingId":"F-1","text":"a"}]}"#).unwrap();
    assert!(get_feedback(&path, "F-1", &clock).is_err());
}

#[test]
#[cfg_attr(miri, ignore = "requires a PowerShell subprocess")]
fn feedback_text_matches_predecessor_for_bom_encodings() {
    let workspace = Workspace::new("feedback-encodings");
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1, "kind":"consolidated",
            "findings":[finding("F-1","src/lib.rs")], "removed":[]}),
    );
    let text_path = workspace.path("text.txt");
    let clock = SystemClock::default();
    let text = "\u{feff}  first 😀\r\nsecond  ";
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit_predecessor.ps1");
    for (index, bytes) in encoded(text).iter().enumerate() {
        fs::write(&text_path, bytes).unwrap();
        let output = Command::new("pwsh")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(&script)
            .arg("-Root")
            .arg(&root)
            .args(["-Mode", "read-text", "-Artifact"])
            .arg(&artifact)
            .arg("-Decisions")
            .arg(workspace.path("unused.json"))
            .arg("-Context")
            .arg(workspace.path("unused-context.json"))
            .arg("-Diff")
            .arg(&text_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let predecessor: String = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(predecessor, text);
        let feedback = workspace.path(&format!("feedback-{index}.json"));
        add_feedback(&feedback, &artifact, "response", "F-1", &text_path, &clock).unwrap();
        assert_eq!(get_feedback(&feedback, "F-1", &clock).unwrap(), predecessor);
    }
}

#[test]
fn bom_marked_audit_records_preserve_duplicate_key_validation() {
    let workspace = Workspace::new("audit-record-encodings");
    let journal = workspace.path("decisions.json");
    let feedback = workspace.path("feedback.json");
    let clock = SystemClock::default();
    let journal_text = r#"{"schemaVersion":1,"decisions":[{"findingId":"F-1","decision":"approved","recordedUtc":"2026-01-01T00:00:00Z"}]}"#;
    let feedback_text = r#"{"schemaVersion":1,"feedback":[{"responseId":"response","findingId":"F-1","text":"  words 😀  "}]}"#;
    for (journal_bytes, feedback_bytes) in encoded(journal_text).iter().zip(encoded(feedback_text))
    {
        fs::write(&journal, journal_bytes).unwrap();
        fs::write(&feedback, feedback_bytes).unwrap();
        assert_eq!(
            last_decision(&read_decisions(&journal, &clock).unwrap(), "F-1").unwrap(),
            Some("approved")
        );
        assert_eq!(
            get_feedback(&feedback, "F-1", &clock).unwrap(),
            "  words 😀  "
        );
    }
    let duplicate = r#"{"schemaVersion":1,"feedback":[],"Feedback":[]}"#;
    for bytes in encoded(duplicate) {
        fs::write(&feedback, bytes).unwrap();
        assert!(get_feedback(&feedback, "F-1", &clock).is_err());
    }
}

#[test]
fn bom_marked_audit_inputs_are_read_during_transactions() {
    let workspace = Workspace::new("audit-input-encodings");
    let clock = SystemClock::default();
    let artifact = workspace.path("findings.json");
    let input = workspace.path("plan.json");
    let journal = workspace.path("journal.json");
    let feedback = workspace.path("feedback.json");
    let merge = workspace.path("merge.json");
    let text_path = workspace.path("text.txt");
    fs::write(&text_path, "reply").unwrap();
    let findings = json!({"schemaVersion":1, "kind":"consolidated",
        "findings":[finding("F-1","src/lib.rs")], "removed":[]})
    .to_string();
    let plan = r#"{"schemaVersion":1,"decisions":[{"findingId":"F-1","decision":"approved"}]}"#;
    let decisions = r#"{"schemaVersion":1,"decisions":[]}"#;
    let responses = r#"{"schemaVersion":1,"feedback":[]}"#;
    let commit = "b".repeat(40);
    let merge_decision = json!({"schemaVersion":1,"pullRequestUrl":"https://example.test/pr/1",
        "approveMerge":true,"approvedSourceCommit":commit})
    .to_string();

    for ((((findings, plan), decisions), responses), merge_decision) in encoded(&findings)
        .iter()
        .zip(encoded(plan))
        .zip(encoded(decisions))
        .zip(encoded(responses))
        .zip(encoded(&merge_decision))
    {
        fs::write(&artifact, findings).unwrap();
        fs::write(&input, plan).unwrap();
        fs::write(&journal, decisions).unwrap();
        fs::write(&feedback, responses).unwrap();
        fs::write(&merge, merge_decision).unwrap();
        add_feedback(&feedback, &artifact, "response", "F-1", &text_path, &clock).unwrap();
        add_decisions(
            &journal,
            &input,
            Some(&feedback),
            false,
            "2026-01-01T00:00:00Z",
            &clock,
        )
        .unwrap();
        assert_eq!(
            read_decisions(&journal, &clock).unwrap()["decisions"][0]["feedback"],
            "reply"
        );
        assert!(test_merge_approval(&merge, "https://example.test/pr/1", &commit, &clock).unwrap());
    }
}

#[test]
fn polling_is_non_consuming_and_respects_the_last_decision() {
    let workspace = Workspace::new("poll");
    let artifact = workspace.put("findings.json", &json!({"schemaVersion":1,
        "kind":"consolidated", "findings":[finding("F-1","a.rs"),finding("F-2","b.rs")], "removed":[]}));
    let journal = workspace.path("decisions.json");
    let clock = SystemClock::default();
    let first = poll_classification(&artifact, &journal, &clock).unwrap();
    assert_eq!(first["remainingCount"], 2);
    assert_eq!(first["finding"]["id"], "F-1");
    assert_eq!(
        poll_classification(&artifact, &journal, &clock).unwrap(),
        first
    );
    let input = plan(
        &workspace,
        json!([{"findingId":"F-1","decision":"needs-audit"},
        {"findingId":"F-2","decision":"auto-approved"}]),
    );
    add_decisions(&journal, &input, None, true, "2026-01-01T00:00:00Z", &clock).unwrap();
    let done = poll_classification(&artifact, &journal, &clock).unwrap();
    assert_eq!(done["isComplete"], true);
    assert!(done["finding"].is_null());
    assert_eq!(done["auditFindingIds"], json!(["F-1"]));
    fs::write(&journal, b"{}").unwrap();
    assert!(poll_classification(&artifact, &journal, &clock).is_err());
}

#[test]
fn rejected_audit_inputs_identify_the_invalid_verdict() {
    let workspace = Workspace::new("diagnostics");
    let journal = workspace.path("decisions.json");
    let clock = SystemClock::default();
    let input = plan(
        &workspace,
        json!([{"findingId":"F1","decision":"approved"},
            {"findingId":"F2","decision":"deferred"}]),
    );
    let error = add_decisions(
        &journal,
        &input,
        None,
        false,
        "2026-01-01T00:00:00Z",
        &clock,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("decisions/1/decision"), "{error}");
    assert!(!journal.exists());
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1, "kind":"consolidated",
            "findings":[finding("F1","a.rs")], "removed":[]}),
    );
    fs::write(
        &journal,
        br#"{"schemaVersion":1,"decisions":[{"findingId":"F1","decision":"maybe"}]}"#,
    )
    .unwrap();
    let error = poll_classification(&artifact, &journal, &clock)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("'maybe'") && error.contains("'F1'"),
        "{error}"
    );
    let error = poll_classification(&workspace.path("missing.json"), &journal, &clock)
        .unwrap_err()
        .to_string();
    assert!(error.contains("missing"), "{error}");
}

#[test]
fn merge_decisions_bind_commits_and_only_carry_existing_approvals() {
    let workspace = Workspace::new("merge");
    let first = "b".repeat(40);
    let second = "c".repeat(40);
    let old = workspace.put("old.json", &capture(&first, false));
    let new = workspace.put("new.json", &capture(&second, false));
    let self_authored = workspace.put("self.json", &capture(&second, true));
    let mut other_capture = capture(&second, false);
    other_capture["pullRequestUrl"] = json!("https://example.test/pr/2");
    let other = workspace.put("other.json", &other_capture);
    let record = workspace.path("approval.json");
    let clock = SystemClock::default();
    assert_eq!(
        set_merge_approval(&record, &new, false, Some(&old), true, &clock).unwrap(),
        json!({"status":"NoApproval", "sourceCommit": second})
    );
    assert!(!record.exists());
    assert!(set_merge_approval(&record, &self_authored, true, None, false, &clock).is_err());
    set_merge_approval(&record, &old, true, None, false, &clock).unwrap();
    let before = fs::read(&record).unwrap();
    assert!(set_merge_approval(&record, &other, false, None, false, &clock).is_err());
    assert!(set_merge_approval(&record, &new, false, Some(&other), true, &clock).is_err());
    assert_eq!(fs::read(&record).unwrap(), before);
    assert!(test_merge_approval(&record, "https://example.test/pr/1", &second, &clock).is_err());
    assert_eq!(
        set_merge_approval(&record, &new, false, Some(&old), true, &clock).unwrap(),
        json!({"status":"Recorded", "sourceCommit": second})
    );
    assert_eq!(
        set_merge_approval(&record, &new, false, Some(&old), true, &clock).unwrap(),
        json!({"status":"Recorded", "sourceCommit": second})
    );
    assert!(set_merge_approval(&record, &self_authored, false, Some(&old), true, &clock).is_err());
    assert!(test_merge_approval(&record, "https://example.test/pr/1", &second, &clock).unwrap());
    set_merge_approval(&record, &new, false, None, false, &clock).unwrap();
    assert!(!test_merge_approval(&record, "https://example.test/pr/1", &first, &clock).unwrap());
    assert_eq!(
        set_merge_approval(&record, &new, false, Some(&old), true, &clock).unwrap(),
        json!({"status":"NoApproval", "sourceCommit": second})
    );
}

#[test]
fn in_memory_merge_contexts_use_the_same_binding_and_authority_checks() {
    let workspace = Workspace::new("merge-context-values");
    let first = "a".repeat(40);
    let second = "b".repeat(40);
    let context = |commit: &str, authored: Value| {
        json!({"pullRequestUrl":"https://example.test/pr/1",
            "pullRequest":{"reviewedSourceCommit":commit, "isAuthoredBySelf":authored}})
    };
    let old = workspace.put("old.json", &context(&first, json!(false)));
    let new = workspace.put("new.json", &context(&second, json!(false)));
    let self_authored = workspace.put("self.json", &context(&second, json!(true)));
    let record = workspace.path("approval.json");
    let clock = SystemClock::default();
    assert_eq!(
        set_merge_approval_from_files(&record, &new, false, Some(&old), true, &clock).unwrap(),
        json!({"status":"NoApproval", "sourceCommit": second})
    );
    set_merge_approval_from_files(&record, &old, true, None, false, &clock).unwrap();
    assert!(
        set_merge_approval_from_files(&record, &self_authored, false, Some(&old), true, &clock)
            .is_err()
    );
    assert_eq!(
        set_merge_approval_from_files(&record, &new, false, Some(&old), true, &clock).unwrap(),
        json!({"status":"Recorded", "sourceCommit": second})
    );
    fs::write(&new, br#"{"pullRequestUrl":"a","pullRequestUrl":"b"}"#).unwrap();
    assert!(set_merge_approval_from_files(&record, &new, false, None, false, &clock).is_err());
}

#[test]
fn malformed_merge_decision_identifies_schema_failure() {
    let workspace = Workspace::new("merge-schema");
    let path = workspace.put(
        "approval.json",
        &json!({"schemaVersion": 1, "pullRequestUrl": "https://example.test/pr/1",
            "approveMerge": "false", "approvedSourceCommit": "a".repeat(40)}),
    );
    let error = test_merge_approval(
        &path,
        "https://example.test/pr/1",
        &"a".repeat(40),
        &SystemClock::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("schema") && error.contains("boolean"),
        "{error}"
    );
}

#[test]
#[cfg_attr(miri, ignore = "requires PowerShell subprocesses and file locks")]
fn overlapping_predecessor_and_native_updates_keep_both_transactions() {
    for mode in ["feedback-contender", "decision-contender"] {
        let workspace = Workspace::new(mode);
        let artifact = workspace.put(
            "findings.json",
            &json!({"schemaVersion":1,
            "kind":"consolidated", "findings":[finding("F-1","src/lib.rs"),
                finding("F-2","src/lib.rs")], "removed":[]}),
        );
        let context = workspace.put("context.json", &capture(&"b".repeat(40), false));
        let diff = workspace.put("diff.json", &json!({"files":[]}));
        let record = workspace.path("record.json");
        let clock = SystemClock::default();
        let transaction = FileTransaction::open(&record, &clock).unwrap();
        let mut contender =
            PredecessorWriter::start(&workspace, mode, &artifact, &record, &context, &diff);
        contender.expect("contended");
        drop(transaction);
        if mode == "feedback-contender" {
            let text = workspace.path("text.txt");
            fs::write(&text, "Native response").unwrap();
            add_feedback(&record, &artifact, "native", "F-1", &text, &clock).unwrap();
        } else {
            let input = plan(
                &workspace,
                json!([{"findingId":"F-1","decision":"needs-audit"}]),
            );
            add_decisions(&record, &input, None, true, "2024-05-06T09:10:11Z", &clock).unwrap();
        }
        contender.retry();
        contender.expect("complete");
        contender.finish();
        let document: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        if mode == "feedback-contender" {
            assert_eq!(document["feedback"].as_array().unwrap().len(), 2);
            assert_eq!(
                get_feedback(&record, "F-1", &clock).unwrap(),
                "Native response\n\nPowerShell response"
            );
        } else {
            assert_eq!(document["decisions"].as_array().unwrap().len(), 2);
            assert_eq!(document["decisions"][0]["decision"], "needs-audit");
            assert_eq!(document["decisions"][1]["decision"], "approved");
        }
    }
}
