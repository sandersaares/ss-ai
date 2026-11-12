//! Integration coverage for durable local audit operations.
#![allow(
    clippy::assertions_on_result_states,
    clippy::indexing_slicing,
    reason = "Tests use fixed fixture shapes and assert operation failures directly"
)]

use std::cell::Cell;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json, to_vec};
use ss_ai_review_workflow::audit::{
    add_decision, add_decisions, add_feedback, get_feedback, last_decision, poll_classification,
    read_decisions, set_merge_approval, set_merge_approval_from_files, test_merge_approval,
};
use ss_ai_review_workflow::workflow_plan::DecisionValue;
use ss_ai_runtime::{Clock, FileTransaction, SystemClock, caused_by, classification, fail};

/// Prevents a concurrent subprocess launch from temporarily inheriting a test's held lock.
/// Poisoning does not compromise fixture data: the mutex protects only exclusion.
static PROCESS_LAUNCH: Mutex<()> = Mutex::new(());

/// Keeps non-concurrent tests independent of wall time and contention delays.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected lock contention"))
    }
}

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
        fs::write(&path, to_vec(value).unwrap()).unwrap();
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

/// Produces BOM-marked Unicode encodings accepted at the file boundary.
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

/// Waits for the parent to release a lock before retrying.
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

/// Runs a native transaction only when a parent process supplies its inputs.
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
                false,
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
#[cfg_attr(mutants, ignore = "a mutated subprocess may not signal contention")]
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
        let launch = PROCESS_LAUNCH
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
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
        drop(launch);
        let mut input = child.stdin.take().unwrap();
        writeln!(input, "retry").unwrap();
        drop(input);
        let output = child.wait_with_output().unwrap();
        let mut diagnostics = String::new();
        error.read_to_string(&mut diagnostics).unwrap();
        assert!(
            output.status.success(),
            "child failed: {}\n{diagnostics}",
            String::from_utf8_lossy(&output.stdout),
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
    assert_eq!(
        last_decision(&document, "F-1").unwrap(),
        Some(DecisionValue::Approved)
    );
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

/// Single authored decisions are canonicalized before policy checks and persistence.
#[test]
fn individual_decision_normalization_keeps_plans_and_journals_strict() {
    // A concurrent subprocess launch can briefly inherit a held file lock.
    let _launch = PROCESS_LAUNCH
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let workspace = Workspace::new("individual-decisions");
    let path = workspace.path("decisions.json");
    let clock = FixedClock;
    let timestamp = "2026-01-01T00:00:00Z";
    for (index, input, expected, verdict, policy) in [
        ("1", "APPROVED", "approved", DecisionValue::Approved, false),
        (
            "2",
            " Auto-Approved ",
            "auto-approved",
            DecisionValue::AutoApproved,
            true,
        ),
        (
            "3",
            "NEEDS-AUDIT",
            "needs-audit",
            DecisionValue::NeedsAudit,
            true,
        ),
        (
            "4",
            " ReJeCtEd ",
            "rejected",
            DecisionValue::Rejected,
            false,
        ),
        ("5", "DROPPED", "dropped", DecisionValue::Dropped, false),
    ] {
        let item = workspace.put(
            "item.json",
            &json!({"findingId":index,"decision":input,"reason":"Reason"}),
        );
        add_decision(&path, &item, None, policy, timestamp, &clock).unwrap();
        let journal = read_decisions(&path, &clock).unwrap();
        assert_eq!(last_decision(&journal, index).unwrap(), Some(verdict));
        assert_eq!(
            journal["decisions"].as_array().unwrap().last().unwrap()["decision"],
            expected
        );
    }

    let before = fs::read(&path).unwrap();
    for value in ["APPROVED", "rejected", "unknown"] {
        let item = workspace.put("item.json", &json!({"findingId":"6","decision":value}));
        let error = add_decision(&path, &item, None, true, timestamp, &clock).unwrap_err();
        if value == "unknown" {
            assert!(error.to_string().contains("'unknown'"));
            assert!(error.to_string().contains("'6'"));
        }
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let item = workspace.path("item.json");
    fs::write(
        &item,
        br#"{"findingId":"6","decision":"approved","decision":"rejected"}"#,
    )
    .unwrap();
    assert!(add_decision(&path, &item, None, false, timestamp, &clock).is_err());
    assert!(add_decision(&path, &path, None, false, timestamp, &clock).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    let plan = plan(&workspace, json!([{"findingId":"6","decision":"APPROVED"}]));
    assert!(add_decisions(&path, &plan, None, false, timestamp, &clock).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);

    let noncanonical = workspace.put(
        "noncanonical.json",
        &json!({"schemaVersion":1,"decisions":[
            {"findingId":"1","decision":"APPROVED","recordedUtc":timestamp}]}),
    );
    assert!(read_decisions(&noncanonical, &clock).is_err());
}

#[test]
fn blank_optional_text_is_omitted_only_after_feedback_authority_checks() {
    let workspace = Workspace::new("blank-decision-text");
    let journal = workspace.path("decisions.json");
    let feedback = workspace.put("feedback.json", &json!({"schemaVersion":1,"feedback":[]}));
    let timestamp = "2026-01-01T00:00:00Z";
    let clock = SystemClock::default();
    let item = workspace.put(
        "item.json",
        &json!({"findingId":"F-1","decision":"approved","reason":" \n","feedback":" "}),
    );

    let error = add_decision(&journal, &item, Some(&feedback), false, timestamp, &clock)
        .unwrap_err()
        .to_string();
    assert!(error.contains("without inline feedback"), "{error}");
    assert!(!journal.exists());
    add_decision(&journal, &item, None, false, timestamp, &clock).unwrap();
    assert_eq!(
        read_decisions(&journal, &clock).unwrap()["decisions"][0],
        json!({"findingId":"F-1","decision":"approved","recordedUtc":timestamp})
    );
    let input = plan(
        &workspace,
        json!([{"findingId":"F-2","decision":"needs-audit","reason":" ","feedback":" "}]),
    );
    add_decisions(&journal, &input, None, true, timestamp, &clock).unwrap();
    assert_eq!(
        read_decisions(&journal, &clock).unwrap()["decisions"][1],
        json!({"findingId":"F-2","decision":"needs-audit","recordedUtc":timestamp})
    );

    let before = fs::read(&journal).unwrap();
    let input = plan(
        &workspace,
        json!([{"findingId":"F-3","decision":"auto-approved","feedback":"user words"}]),
    );
    assert!(add_decisions(&journal, &input, None, true, timestamp, &clock).is_err());
    assert_eq!(fs::read(&journal).unwrap(), before);
}

#[test]
fn invalid_journal_entries_reject_appends_without_losing_history() {
    let workspace = Workspace::new("invalid-decision-journal");
    let journal = workspace.path("decisions.json");
    let input = plan(
        &workspace,
        json!([{"findingId":"F-2","decision":"approved"}]),
    );
    let clock = SystemClock::default();
    let timestamp = "2026-01-01T00:00:00Z";
    for record in [
        json!({"findingId":"F-1","decision":"approved","recordedUtc":"bad"}),
        json!({"findingId":"F-1","decision":"APPROVED","recordedUtc":timestamp}),
        json!({"findingId":"F-1","decision":"approved","recordedUtc":timestamp,"feedback":null}),
        json!({"findingId":"F-1","decision":"approved","recordedUtc":timestamp,"extra":true}),
    ] {
        let document = json!({"schemaVersion":1,"decisions":[record]});
        fs::write(&journal, to_vec(&document).unwrap()).unwrap();
        let before = fs::read(&journal).unwrap();
        assert!(read_decisions(&journal, &clock).is_err());
        assert!(add_decisions(&journal, &input, None, false, timestamp, &clock).is_err());
        assert_eq!(fs::read(&journal).unwrap(), before);
    }
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
fn feedback_text_preserves_bom_encoded_prose() {
    let workspace = Workspace::new("feedback-encodings");
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1, "kind":"consolidated",
            "findings":[finding("F-1","src/lib.rs")], "removed":[]}),
    );
    let text_path = workspace.path("text.txt");
    let clock = SystemClock::default();
    let text = "\u{feff}  first 😀\r\nsecond  ";
    for (index, bytes) in encoded(text).iter().enumerate() {
        fs::write(&text_path, bytes).unwrap();
        let feedback = workspace.path(&format!("feedback-{index}.json"));
        add_feedback(&feedback, &artifact, "response", "F-1", &text_path, &clock).unwrap();
        assert_eq!(get_feedback(&feedback, "F-1", &clock).unwrap(), text);
    }
    fs::write(&text_path, b"words \xff").unwrap();
    let feedback = workspace.path("feedback-lossy.json");
    add_feedback(&feedback, &artifact, "response", "F-1", &text_path, &clock).unwrap();
    assert_eq!(
        get_feedback(&feedback, "F-1", &clock).unwrap(),
        "words \u{fffd}"
    );
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
            Some(DecisionValue::Approved)
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

/// Stored audit feedback rejects malformed string bytes before an append can
/// rewrite the existing record.
#[test]
fn malformed_feedback_record_is_not_rewritten() {
    let workspace = Workspace::new("malformed-feedback-encoding");
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1, "kind":"consolidated",
            "findings":[finding("F-1","src/lib.rs")], "removed":[]}),
    );
    let feedback = workspace.path("feedback.json");
    let text_path = workspace.path("text.txt");
    fs::write(&text_path, "new feedback").unwrap();
    let record = r#"{"schemaVersion":1,"feedback":[{"responseId":"response","findingId":"F-1","text":"Z"}]}"#;
    let replacements: [(&[u8], &[u8]); 5] = [
        (b"Z", &[0xff]),
        (&[b'Z', 0], &[0x00, 0xd8]),
        (&[0, b'Z'], &[0xd8, 0x00]),
        (&[b'Z', 0, 0, 0], &[0x00, 0xd8, 0, 0]),
        (&[0, 0, 0, b'Z'], &[0, 0, 0xd8, 0x00]),
    ];
    for (mut bytes, (needle, replacement)) in encoded(record).into_iter().zip(replacements) {
        let offset = bytes
            .windows(needle.len())
            .position(|part| part == needle)
            .unwrap();
        bytes[offset..offset + needle.len()].copy_from_slice(replacement);
        fs::write(&feedback, &bytes).unwrap();
        let error = add_feedback(
            &feedback,
            &artifact,
            "another",
            "F-1",
            &text_path,
            &SystemClock::default(),
        )
        .unwrap_err();
        assert_eq!(classification(&error), "invalidInput");
        assert_eq!(fs::read(&feedback).unwrap(), bytes);
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
#[cfg_attr(miri, ignore = "requires subprocesses and file locks")]
#[cfg_attr(mutants, ignore = "a mutated subprocess may not signal contention")]
fn overlapping_native_updates_keep_both_transactions() {
    for mode in ["feedback", "decision"] {
        let workspace = Workspace::new(&format!("overlap-{mode}"));
        let artifact = workspace.put(
            "findings.json",
            &json!({"schemaVersion":1,
            "kind":"consolidated", "findings":[finding("F-1","src/lib.rs"),
                finding("F-2","src/lib.rs")], "removed":[]}),
        );
        let record = workspace.path("record.json");
        let clock = SystemClock::default();
        let child_input = if mode == "feedback" {
            let path = workspace.path("child-response.txt");
            fs::write(&path, "Child response").unwrap();
            path
        } else {
            workspace.put(
                "child-plan.json",
                &json!({"schemaVersion":1,"decisions":[
                {"findingId":"F-2","decision":"approved"}]}),
            )
        };
        let launch = PROCESS_LAUNCH
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let transaction = FileTransaction::open(&record, &clock).unwrap();
        let mut child = Command::new(env::current_exe().unwrap())
            .args(["native_audit_lock_interop_helper", "--exact", "--nocapture"])
            .env("SS_AI_AUDIT_INTEROP_OPERATION", mode)
            .env("SS_AI_AUDIT_INTEROP_RECORD", &record)
            .env("SS_AI_AUDIT_INTEROP_INPUT", &child_input)
            .env("SS_AI_AUDIT_INTEROP_FINDINGS", &artifact)
            .env("SS_AI_AUDIT_INTEROP_RESPONSE", "child")
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
        drop(transaction);
        drop(launch);
        if mode == "feedback" {
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
        writeln!(child.stdin.take().unwrap(), "retry").unwrap();
        let output = child.wait_with_output().unwrap();
        let mut diagnostics = String::new();
        error.read_to_string(&mut diagnostics).unwrap();
        assert!(
            output.status.success(),
            "{}\n{diagnostics}",
            String::from_utf8_lossy(&output.stdout)
        );
        let document: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        if mode == "feedback" {
            assert_eq!(document["feedback"].as_array().unwrap().len(), 2);
            assert_eq!(
                get_feedback(&record, "F-1", &clock).unwrap(),
                "Native response\n\nChild response"
            );
        } else {
            assert_eq!(document["decisions"].as_array().unwrap().len(), 2);
            assert_eq!(document["decisions"][0]["decision"], "needs-audit");
            assert_eq!(document["decisions"][1]["findingId"], "F-2");
            assert_eq!(document["decisions"][1]["decision"], "approved");
        }
    }
}
