//! Exercises semantic authorship persistence through the local file boundary.
#![allow(
    clippy::assertions_on_result_states,
    clippy::indexing_slicing,
    reason = "The tests use fixed fixture shapes and assert failures directly"
)]

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{read_capture, snapshot_id, write_comment_authorship};
use ss_ai_runtime::{Clock, FileTransaction, SystemClock};

/// Isolates capture and sidecar files in the checkout for each test.
struct Files {
    capture: PathBuf,
    sidecar: PathBuf,
}

impl Files {
    /// Copies the shared PowerShell parity fixture.
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The atomic supplies unique names and does not synchronize other data.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let capture = std::env::current_dir()
            .unwrap()
            .join(format!(".authorship-test-{}-{id}.json", std::process::id()));
        fs::write(
            &capture,
            include_bytes!("../../../tests/fixtures/ss-review-query-capture.json"),
        )
        .unwrap();
        let mut sidecar = capture.as_os_str().to_os_string();
        sidecar.push(".authorship.json");
        Self {
            capture,
            sidecar: sidecar.into(),
        }
    }

    /// Obtains the snapshot a classifier would have read.
    fn snapshot(&self) -> String {
        snapshot_id(&read_capture(&self.capture, &SystemClock::default()).unwrap()).unwrap()
    }

    /// Reads the persisted semantic record.
    fn record(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.sidecar).unwrap()).unwrap()
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        drop(fs::remove_file(&self.capture));
        drop(fs::remove_file(&self.sidecar));
        let mut lock = self.sidecar.as_os_str().to_os_string();
        lock.push(".lock");
        drop(fs::remove_file(lock));
    }
}

/// Signals a lock conflict and resumes only when the test releases the holder.
struct ContendedClock {
    reached: mpsc::Sender<()>,
    release: RefCell<mpsc::Receiver<()>>,
}

impl Clock for ContendedClock {
    fn now(&self) -> Timestamp {
        panic!("Authorship writes do not need wall-clock time")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        self.reached.send(()).unwrap();
        self.release.borrow_mut().recv().unwrap();
        Ok(())
    }
}

#[test]
fn persists_powershell_hashes_and_reports_overlay_progress() {
    let files = Files::new();
    let original = fs::read(&files.capture).unwrap();
    let snapshot = files.snapshot();
    assert_eq!(
        snapshot,
        "FEF75D9836BAC16552C0569D1172931FFAC8B38B8B61E2F540DE07DDD4A10AF5"
    );
    let classification = json!([{"threadId":"T","commentId":"C","kind":"Agent"}]);
    let clock = SystemClock::default();
    let summary =
        write_comment_authorship(&files.capture, &snapshot, &classification, &clock).unwrap();
    assert_eq!(
        summary,
        json!({
            "contextPath": files.capture.to_str().unwrap(),
            "applied": 1, "unchanged": 0, "indeterminate": 0, "isComplete": true
        })
    );
    assert_eq!(
        files.record(),
        json!({
            "schemaVersion": 1,
            "policyVersion": "2",
            "captureHash": "9EBFC89794C925995FA3FC0BB44EFF7CF94001EA71C3F27ECA4F9A4E23053289",
            "decisions": [{
                "threadId": "T", "commentId": "C", "kind": "agent",
                "inputHash": "E164CBD830B371E31C58EB2D2819AF0D3EE07AEED04D4F491E4264825A24537C",
                "origin": "classified"
            }]
        })
    );
    assert_eq!(fs::read(&files.capture).unwrap(), original);
    let effective = read_capture(&files.capture, &clock).unwrap();
    assert_eq!(
        effective["threads"][0]["comments"][0]["authorship"],
        "agent"
    );
    assert_eq!(
        effective["threads"][0]["comments"][0]["authorshipEvidence"],
        json!(["classification:reviewed"])
    );
    let next = snapshot_id(&effective).unwrap();
    assert_ne!(snapshot, next);
    let summary = write_comment_authorship(&files.capture, &next, &classification, &clock).unwrap();
    assert_eq!(summary["unchanged"], 1);
    assert_eq!(summary["applied"], 0);
    assert_eq!(files.record()["decisions"].as_array().unwrap().len(), 1);
}

#[test]
fn rejects_entire_batch_without_changing_existing_sidecar() {
    let files = Files::new();
    let clock = SystemClock::default();
    let snapshot = files.snapshot();
    write_comment_authorship(&files.capture, &snapshot, &json!([]), &clock).unwrap();
    let before = fs::read(&files.sidecar).unwrap();
    let invalid = [
        json!({}),
        json!([{"threadId":"T","commentId":"C","kind":"agent","extra":1}]),
        json!([{"threadId":"T","commentId":"C","kind":"unknown"}]),
        json!([{"threadId":"missing","commentId":"C","kind":"agent"}]),
        json!([{"threadId":"T","commentId":"missing","kind":"agent"}]),
        json!([{"threadId":"T","commentId":"C","kind":"agent"},{"threadId":"T","commentId":"C","kind":"agent"}]),
        json!([{"threadId":"T","commentId":"C","kind":"agent"},{"threadId":"T","commentId":"missing","kind":"human"}]),
    ];
    for batch in invalid {
        assert!(write_comment_authorship(&files.capture, &snapshot, &batch, &clock).is_err());
        assert_eq!(fs::read(&files.sidecar).unwrap(), before);
    }
    assert!(write_comment_authorship(&files.capture, "stale", &json!([]), &clock).is_err());
    assert_eq!(fs::read(&files.sidecar).unwrap(), before);
}

#[test]
fn deterministic_authorship_cannot_be_overridden_and_missing_evidence_fails() {
    let files = Files::new();
    let clock = SystemClock::default();
    let mut capture: Value = serde_json::from_slice(&fs::read(&files.capture).unwrap()).unwrap();
    capture["threads"][0]["comments"][0]["authorship"] = json!("human");
    fs::write(&files.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
    let snapshot = files.snapshot();
    assert!(
        write_comment_authorship(
            &files.capture,
            &snapshot,
            &json!([{"threadId":"T","commentId":"C","kind":"agent"}]),
            &clock
        )
        .is_err()
    );
    let summary = write_comment_authorship(
        &files.capture,
        &snapshot,
        &json!([{"threadId":"T","commentId":"C","kind":"human"}]),
        &clock,
    )
    .unwrap();
    assert_eq!(summary["unchanged"], 1);
    assert_eq!(files.record()["decisions"], json!([]));
    capture["threads"][0]["comments"][0]
        .as_object_mut()
        .unwrap()
        .remove("authorshipEvidence");
    fs::write(&files.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
    assert!(write_comment_authorship(&files.capture, &snapshot, &json!([]), &clock).is_err());
}

#[test]
fn sequential_batches_preserve_matching_decisions_and_reject_stale_snapshots() {
    let files = Files::new();
    let clock = SystemClock::default();
    let mut capture: Value = serde_json::from_slice(&fs::read(&files.capture).unwrap()).unwrap();
    let mut second = capture["threads"][0]["comments"][0].clone();
    second["id"] = json!("C2");
    capture["threads"][0]["comments"]
        .as_array_mut()
        .unwrap()
        .push(second);
    fs::write(&files.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
    let first_snapshot = files.snapshot();
    let first = json!([{"threadId":"T","commentId":"C","kind":"agent"}]);
    let result = write_comment_authorship(&files.capture, &first_snapshot, &first, &clock).unwrap();
    assert_eq!(result["indeterminate"], 1);
    assert_eq!(result["isComplete"], false);
    let before = fs::read(&files.sidecar).unwrap();
    let second = json!([{"threadId":"T","commentId":"C2","kind":"human"}]);
    assert!(write_comment_authorship(&files.capture, &first_snapshot, &second, &clock).is_err());
    assert_eq!(fs::read(&files.sidecar).unwrap(), before);
    let next_snapshot = files.snapshot();
    let result = write_comment_authorship(&files.capture, &next_snapshot, &second, &clock).unwrap();
    assert_eq!(result["isComplete"], true);
    assert_eq!(files.record()["decisions"].as_array().unwrap().len(), 2);
}

#[test]
fn old_policy_decisions_do_not_override_new_classification() {
    let files = Files::new();
    let clock = SystemClock::default();
    let original = fs::read(&files.capture).unwrap();
    let first = json!([{"threadId":"T","commentId":"C","kind":"agent"}]);
    write_comment_authorship(&files.capture, &files.snapshot(), &first, &clock).unwrap();
    let mut record = files.record();
    record["policyVersion"] = json!("old");
    fs::write(&files.sidecar, serde_json::to_vec(&record).unwrap()).unwrap();

    let document = read_capture(&files.capture, &clock).unwrap();
    assert_eq!(
        document["threads"][0]["comments"][0]["authorship"],
        "indeterminate"
    );
    let next = json!([{"threadId":"T","commentId":"C","kind":"human"}]);
    let result = write_comment_authorship(
        &files.capture,
        &snapshot_id(&document).unwrap(),
        &next,
        &clock,
    )
    .unwrap();
    assert_eq!(result["applied"], 1);
    assert_eq!(files.record()["policyVersion"], "2");
    assert_eq!(files.record()["decisions"][0]["kind"], "human");
    assert_eq!(fs::read(&files.capture).unwrap(), original);
}

#[test]
fn rejects_malformed_existing_sidecars_without_replacement() {
    let files = Files::new();
    let clock = SystemClock::default();
    let snapshot = files.snapshot();
    let malformed =
        br#"{"schemaVersion":1,"policyVersion":"2","captureHash":"wrong","decisions":[]}"#;
    fs::write(&files.sidecar, malformed).unwrap();
    assert!(write_comment_authorship(&files.capture, &snapshot, &json!([]), &clock).is_err());
    assert_eq!(fs::read(&files.sidecar).unwrap(), malformed);
}

#[test]
fn rejected_classifications_explain_the_specific_problem() {
    let files = Files::new();
    let clock = SystemClock::default();
    let snapshot = files.snapshot();
    let bad_kind = json!([{"threadId":"T","commentId":"C","kind":"Robot"}]);
    let error = write_comment_authorship(&files.capture, &snapshot, &bad_kind, &clock)
        .unwrap_err()
        .to_string();
    assert!(error.contains("authorship 'Robot'"));
    assert!(error.contains("Use 'agent' or 'human'"));
    let missing = json!([{"threadId":"T","commentId":"98","kind":"human"}]);
    let error = write_comment_authorship(&files.capture, &snapshot, &missing, &clock)
        .unwrap_err()
        .to_string();
    assert!(error.contains("no comment '98'"));
    assert!(error.contains("thread 'T'"));

    let valid = json!([{"threadId":"T","commentId":"C","kind":"agent"}]);
    write_comment_authorship(&files.capture, &snapshot, &valid, &clock).unwrap();
    let conflict = json!([{"threadId":"T","commentId":"C","kind":"human"}]);
    let error = write_comment_authorship(&files.capture, &files.snapshot(), &conflict, &clock)
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot be reclassified"));

    let mut capture: Value = serde_json::from_slice(&fs::read(&files.capture).unwrap()).unwrap();
    capture["schemaVersion"] = json!(99);
    fs::write(&files.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
    let error = write_comment_authorship(&files.capture, &snapshot, &json!([]), &clock)
        .unwrap_err()
        .to_string();
    assert!(error.contains("version 99"));
    assert!(error.contains("Capture the pull request again"));
}

#[test]
fn writer_checks_snapshot_after_acquiring_the_sibling_lock() {
    let files = Files::new();
    let snapshot = files.snapshot();
    let clock = SystemClock::default();
    let holder = FileTransaction::open(&files.sidecar, &clock).unwrap();
    let (reached_send, reached_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let capture = files.capture.clone();
    let worker = thread::spawn(move || {
        let clock = ContendedClock {
            reached: reached_send,
            release: RefCell::new(release_receive),
        };
        write_comment_authorship(&capture, &snapshot, &json!([]), &clock)
    });
    reached_receive.recv().unwrap();
    let original = files.snapshot();
    let document = read_capture(&files.capture, &SystemClock::default()).unwrap();
    let mut changed = document;
    changed["threads"][0]["comments"][0]["content"] = json!("changed");
    fs::write(&files.capture, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert_ne!(original, files.snapshot());
    drop(holder);
    release_send.send(()).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert!(!files.sidecar.exists());
}
