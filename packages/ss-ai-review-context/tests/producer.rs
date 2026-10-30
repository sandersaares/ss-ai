//! Compares native capture publication with retained PowerShell model fixtures.
#![allow(
    clippy::indexing_slicing,
    reason = "The fixtures have fixed, known shapes"
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
use ss_ai_review_context::{
    document_hash, prepare_capture, read_capture, snapshot_id, write_comment_authorship,
};
use ss_ai_runtime::{Clock, SystemClock};

/// Keeps each test's capture and retained locks in the active checkout.
struct Output(PathBuf);

impl Output {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The counter only separates names; it does not synchronize test data.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(
            std::env::current_dir()
                .unwrap()
                .join(format!(".capture-test-{}-{id}.json", std::process::id())),
        )
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        for suffix in ["", ".lock", ".authorship.json", ".authorship.json.lock"] {
            let mut path = self.0.as_os_str().to_os_string();
            path.push(suffix);
            drop(fs::remove_file(path));
        }
    }
}

/// Loads a provider or canonical document supplied by the retained model.
fn fixture(name: &str) -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ss-review-native-capture");
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}

/// Exposes contention without allowing tests to depend on elapsed time.
struct ContendedClock {
    reached: mpsc::Sender<()>,
    release: RefCell<mpsc::Receiver<()>>,
}

impl Clock for ContendedClock {
    fn now(&self) -> Timestamp {
        panic!("capture preparation does not read the clock")
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        self.reached.send(()).unwrap();
        self.release.borrow_mut().recv().unwrap();
        Ok(())
    }
}

#[test]
fn publishes_identical_captures_for_both_providers() {
    let clock = SystemClock::default();
    for (source, expected) in [
        ("github-context.json", "github-expected.json"),
        ("azure-devops-context.json", "azure-devops-expected.json"),
        (
            "azure-devops-refresh-context.json",
            "azure-devops-refresh-expected.json",
        ),
    ] {
        let context = fixture(source);
        let expected = fixture(expected);
        let output = Output::new();
        let target = context["target"]["webUrl"].as_str().unwrap();
        let prepared = prepare_capture(&output.0, None, target, &clock).unwrap();
        let summary = prepared
            .publish(&context, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        let actual: Value = serde_json::from_slice(&fs::read(&output.0).unwrap()).unwrap();
        assert_eq!(actual, expected, "{source}");
        assert_eq!(
            document_hash(&actual).unwrap(),
            document_hash(&expected).unwrap(),
            "{source}"
        );
        assert_eq!(read_capture(&output.0, &clock).unwrap(), expected);
        assert_eq!(
            usize::try_from(summary["threadCount"].as_u64().unwrap()).unwrap(),
            expected["threads"].as_array().unwrap().len()
        );
        assert!(summary["delta"].is_null());
    }
}

#[test]
fn refresh_reuses_only_unchanged_decisions_and_preserves_baseline() {
    let clock = SystemClock::default();
    let context = fixture("azure-devops-context.json");
    let changed = fixture("azure-devops-refresh-context.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    let refresh = Output::new();
    prepare_capture(&baseline.0, None, target, &clock)
        .unwrap()
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let raw = fs::read(&baseline.0).unwrap();
    let document = read_capture(&baseline.0, &clock).unwrap();
    let snapshot = snapshot_id(&document).unwrap();
    write_comment_authorship(
        &baseline.0,
        &snapshot,
        &json!([
            {"threadId":"101", "commentId":"1", "kind":"human"},
            {"threadId":"101", "commentId":"2", "kind":"agent"}
        ]),
        &clock,
    )
    .unwrap();
    let result = prepare_capture(&refresh.0, Some(&baseline.0), target, &clock)
        .unwrap()
        .publish(&changed, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    assert_eq!(fs::read(&baseline.0).unwrap(), raw);
    assert_eq!(result["reusedClassificationCount"], 1);
    assert_eq!(result["indeterminateCommentCount"], 1);
    assert_eq!(result["delta"]["commentsEditedCount"], 1);
    assert_eq!(result["delta"]["hasChanges"], true);
    prepare_capture(&refresh.0, Some(&baseline.0), target, &clock).unwrap_err();
    prepare_capture(&baseline.0, Some(&baseline.0), target, &clock).unwrap_err();
    prepare_capture(
        &Output::new().0,
        Some(&baseline.0),
        "https://example.invalid/another",
        &clock,
    )
    .unwrap_err();
}

#[test]
fn concurrent_creators_do_not_replace_the_published_capture() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let output = Output::new();
    let url = context["target"]["webUrl"].as_str().unwrap();
    let first = prepare_capture(&output.0, None, url, &clock).unwrap();
    let path = output.0.clone();
    let url = url.to_owned();
    let (reached_send, reached_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let worker = thread::spawn(move || {
        let clock = ContendedClock {
            reached: reached_send,
            release: RefCell::new(release_receive),
        };
        prepare_capture(&path, None, &url, &clock).is_err()
    });
    reached_receive.recv().unwrap();
    first
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let original = fs::read(&output.0).unwrap();
    release_send.send(()).unwrap();
    assert!(worker.join().unwrap());
    assert_eq!(fs::read(&output.0).unwrap(), original);
}
