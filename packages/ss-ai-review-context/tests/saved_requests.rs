//! Exercises saved request and diagnostics file transactions.
#![allow(
    clippy::assertions_on_result_states,
    clippy::indexing_slicing,
    reason = "Tests use fixed fixture shapes and assert operation failures directly"
)]

use std::cell::RefCell;
use std::fs;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{
    increment_diagnostic, prepare, prepare_failure, read_diagnostics, read_saved,
};
use ss_ai_runtime::{Clock, FileTransaction, SystemClock, fail};

/// Coordinates a lock retry without waiting for real time.
struct ContendedClock {
    reached: mpsc::Sender<()>,
    release: RefCell<mpsc::Receiver<()>>,
}

impl Clock for ContendedClock {
    fn now(&self) -> Timestamp {
        panic!("Diagnostic updates do not need wall-clock time")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        self.reached.send(()).unwrap();
        self.release.borrow_mut().recv().unwrap();
        Ok(())
    }
}

/// Isolates test files in the package checkout and removes them after use.
struct Files {
    capture: PathBuf,
    baseline: PathBuf,
    request: PathBuf,
    diagnostics: PathBuf,
}

impl Files {
    /// Creates a two-page synthetic capture without modifying the shared fixture.
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The atomic only allocates distinct names; no other data depends on its ordering.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let base = std::env::current_dir()
            .unwrap()
            .join(format!(".saved-request-test-{}-{id}", std::process::id()));
        let files = Self {
            capture: base.with_extension("capture.json"),
            baseline: base.with_extension("baseline.json"),
            request: base.with_extension("request.json"),
            diagnostics: base.with_extension("diagnostics.json"),
        };
        let mut capture: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/ss-review-query-capture.json"
        ))
        .unwrap();
        let mut second = capture["threads"][0].clone();
        second["id"] = json!("T2");
        second["comments"][0]["id"] = json!("C2");
        capture["threads"].as_array_mut().unwrap().push(second);
        fs::write(&files.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
        fs::copy(&files.capture, &files.baseline).unwrap();
        files
    }

    /// Prepares a thread query whose page size forces continuation.
    fn prepare(&self) {
        prepare(
            &self.capture,
            None,
            &self.request,
            &self.diagnostics,
            &json!({"query":"threads", "pageSize":1}),
            &SystemClock::default(),
        )
        .unwrap();
    }

    /// Reads the current request record for controlled corruption tests.
    fn request(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.request).unwrap()).unwrap()
    }

    /// Replaces the request record with deliberately edited test data.
    fn write_request(&self, value: &Value) {
        fs::write(&self.request, serde_json::to_vec(value).unwrap()).unwrap();
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        for path in [
            &self.capture,
            &self.baseline,
            &self.request,
            &self.diagnostics,
        ] {
            drop(fs::remove_file(path));
            let mut lock = path.as_os_str().to_os_string();
            lock.push(".lock");
            drop(fs::remove_file(lock));
        }
    }
}

#[test]
fn replay_advances_once_and_rejects_complete_or_unstarted_next() {
    let files = Files::new();
    files.prepare();
    let clock = SystemClock::default();
    assert!(read_saved(&files.request, true, false, &clock).is_err());
    let first = read_saved(&files.request, false, false, &clock).unwrap();
    assert_eq!(
        read_saved(&files.request, false, false, &clock).unwrap(),
        first
    );
    let second = read_saved(&files.request, true, false, &clock).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        read_saved(&files.request, false, false, &clock).unwrap(),
        second
    );
    assert!(read_saved(&files.request, true, false, &clock).is_err());
    assert!(read_saved(&files.request, true, true, &clock).is_err());
}

#[test]
fn edited_cached_page_and_selectors_are_not_replayed() {
    let files = Files::new();
    files.prepare();
    let clock = SystemClock::default();
    read_saved(&files.request, false, false, &clock).unwrap();
    let original = files.request();
    let mut altered = original.clone();
    altered["lastPage"]["items"][0]["id"] = json!("forged");
    files.write_request(&altered);
    assert!(read_saved(&files.request, false, false, &clock).is_err());
    altered = original;
    altered["options"]["pageSize"] = json!(2);
    files.write_request(&altered);
    assert!(read_saved(&files.request, false, false, &clock).is_err());
}

#[test]
fn malformed_saved_options_report_workflow_schema_and_preserve_the_record() {
    let files = Files::new();
    files.prepare();
    let original = files.request();
    let clock = SystemClock::default();
    for missing in [true, false] {
        let mut request = original.clone();
        let options = request["options"].as_object_mut().unwrap();
        if missing {
            options.shift_remove("files");
        } else {
            options.insert("unknown".to_owned(), json!("unexpected"));
        }
        files.write_request(&request);
        let before = fs::read(&files.request).unwrap();
        let error = read_saved(&files.request, false, false, &clock).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Invalid 'queryRequest' workflow data")
        );
        assert!(
            error
                .to_string()
                .contains("Unexpected or missing record fields")
        );
        assert_eq!(fs::read(&files.request).unwrap(), before);
    }
}

#[test]
fn oversized_saved_page_reports_the_bound_and_preserves_the_record() {
    let files = Files::new();
    files.prepare();
    let clock = SystemClock::default();
    read_saved(&files.request, false, false, &clock).unwrap();
    let mut request = files.request();
    request["lastPage"]["items"][0]["oversized"] = json!("x".repeat(20_000));
    files.write_request(&request);
    let before = fs::read(&files.request).unwrap();
    let error = read_saved(&files.request, false, false, &clock).unwrap_err();
    assert!(error.to_string().contains("bounded output limit"));
    assert_eq!(fs::read(&files.request).unwrap(), before);
}

#[test]
fn stale_capture_requires_explicit_restart_and_counts_attempts() {
    let files = Files::new();
    files.prepare();
    let clock = SystemClock::default();
    read_saved(&files.request, false, false, &clock).unwrap();
    let mut capture: Value = serde_json::from_slice(&fs::read(&files.capture).unwrap()).unwrap();
    capture["pullRequest"]["title"] = json!("Changed");
    fs::write(&files.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
    assert!(read_saved(&files.request, false, false, &clock).is_err());
    read_saved(&files.request, false, true, &clock).unwrap();
    let diagnostics = read_diagnostics(&files.diagnostics, &clock).unwrap();
    assert_eq!(diagnostics["counts"]["cursorRestarts"], 1);
}

#[test]
fn comparison_baseline_is_bound_and_restart_rebinds_it() {
    let files = Files::new();
    let clock = SystemClock::default();
    prepare(
        &files.capture,
        Some(&files.baseline),
        &files.request,
        &files.diagnostics,
        &json!({"query":"changes"}),
        &clock,
    )
    .unwrap();
    read_saved(&files.request, false, false, &clock).unwrap();
    let mut baseline: Value = serde_json::from_slice(&fs::read(&files.baseline).unwrap()).unwrap();
    baseline["pullRequest"]["title"] = json!("Another title");
    fs::write(&files.baseline, serde_json::to_vec(&baseline).unwrap()).unwrap();
    assert!(read_saved(&files.request, false, false, &clock).is_err());
    read_saved(&files.request, false, true, &clock).unwrap();
}

#[test]
fn workflow_records_accept_bom_marked_unicode_encodings() {
    let files = Files::new();
    files.prepare();
    let clock = SystemClock::default();
    let request = fs::read_to_string(&files.request).unwrap();
    let mut request_utf16 = vec![0xff, 0xfe];
    for word in request.encode_utf16() {
        request_utf16.extend_from_slice(&word.to_le_bytes());
    }
    fs::write(&files.request, request_utf16).unwrap();
    read_saved(&files.request, false, false, &clock).unwrap();

    let diagnostic = fs::read_to_string(&files.diagnostics).unwrap();
    let mut utf32 = vec![0, 0, 0xfe, 0xff];
    for character in diagnostic.chars() {
        utf32.extend_from_slice(&u32::from(character).to_be_bytes());
    }
    fs::write(&files.diagnostics, utf32).unwrap();
    increment_diagnostic(&files.diagnostics, "setupCalls", NonZeroU32::MIN, &clock).unwrap();
    assert_eq!(
        read_diagnostics(&files.diagnostics, &clock).unwrap()["counts"]["setupCalls"],
        2
    );
}

#[test]
fn duplicate_properties_are_rejected_without_changing_records() {
    let files = Files::new();
    files.prepare();
    let clock = SystemClock::default();

    let original_request = fs::read_to_string(&files.request).unwrap();
    for key in ["schemaVersion", "SchemaVersion"] {
        let request = original_request.replacen('{', &format!("{{\"{key}\":0,"), 1);
        fs::write(&files.request, &request).unwrap();
        let error = read_saved(&files.request, false, false, &clock).unwrap_err();
        assert!(error.to_string().to_lowercase().contains("duplicate"));
        assert_eq!(fs::read_to_string(&files.request).unwrap(), request);
    }

    let original_diagnostics = fs::read_to_string(&files.diagnostics).unwrap();
    for key in ["setupCalls", "SetupCalls"] {
        let diagnostics = original_diagnostics.replace(
            "\"setupCalls\":1",
            &format!("\"{key}\":99,\"setupCalls\":1"),
        );
        assert_ne!(diagnostics, original_diagnostics);
        fs::write(&files.diagnostics, &diagnostics).unwrap();
        let error = read_diagnostics(&files.diagnostics, &clock).unwrap_err();
        assert!(error.to_string().to_lowercase().contains("duplicate"));
        assert!(
            increment_diagnostic(&files.diagnostics, "setupCalls", NonZeroU32::MIN, &clock)
                .is_err()
        );
        assert_eq!(fs::read_to_string(&files.diagnostics).unwrap(), diagnostics);
    }
}

/// Distinguishes initial malformed setup from rejection of an existing request,
/// while requiring diagnostics records to remain complete.
#[test]
fn diagnostics_reject_malformed_records_and_account_for_failed_setup() {
    let files = Files::new();
    let clock = SystemClock::default();
    assert!(
        prepare(
            &files.capture,
            None,
            &files.request,
            &files.diagnostics,
            &json!({"query":"invalid"}),
            &clock
        )
        .is_err()
    );
    files.prepare();
    assert!(
        prepare(
            &files.capture,
            None,
            &files.request,
            &files.diagnostics,
            &json!({"query":"invalid"}),
            &clock
        )
        .is_err()
    );
    let diagnostic = read_diagnostics(&files.diagnostics, &clock).unwrap();
    assert_eq!(diagnostic["counts"]["setupCalls"], 3);
    assert_eq!(diagnostic["counts"]["repeatedSetupCalls"], 1);
    assert_eq!(diagnostic["counts"]["malformedQueryFailures"], 1);
    fs::write(&files.diagnostics, br#"{"schemaVersion":1,"counts":{}}"#).unwrap();
    assert!(
        increment_diagnostic(&files.diagnostics, "setupCalls", NonZeroU32::MIN, &clock).is_err()
    );
    assert!(read_diagnostics(&files.diagnostics, &clock).is_err());
}

#[test]
fn input_errors_preserve_detail_unless_the_request_already_exists() {
    let files = Files::new();
    let clock = SystemClock::default();
    let error = prepare_failure(
        &files.request,
        &files.diagnostics,
        fail("io", "Cannot read search input"),
        &clock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("Cannot read search input"));
    files.prepare();
    let error = prepare_failure(
        &files.request,
        &files.diagnostics,
        fail("io", "Cannot read search input"),
        &clock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("already exists"));
    let counts = read_diagnostics(&files.diagnostics, &clock).unwrap();
    assert_eq!(counts["counts"]["setupCalls"], 3);
    assert_eq!(counts["counts"]["malformedQueryFailures"], 1);
    assert_eq!(counts["counts"]["repeatedSetupCalls"], 1);
}

#[test]
fn diagnostics_cannot_replace_the_capture() {
    let files = Files::new();
    let original = fs::read(&files.capture).unwrap();
    assert!(
        prepare(
            &files.capture,
            None,
            &files.request,
            &files.capture,
            &json!({"query":"threads"}),
            &SystemClock::default(),
        )
        .is_err()
    );
    assert_eq!(fs::read(&files.capture).unwrap(), original);
}

#[test]
#[cfg_attr(
    mutants,
    ignore = "The controlled lock handshake requires both participants to progress"
)]
fn contended_diagnostic_increment_keeps_the_other_writers_update() {
    let files = Files::new();
    let clock = SystemClock::default();
    increment_diagnostic(&files.diagnostics, "setupCalls", NonZeroU32::MIN, &clock).unwrap();
    let transaction = FileTransaction::open(&files.diagnostics, &clock).unwrap();
    let (reached_send, reached_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let worker = thread::spawn({
        let diagnostics = files.diagnostics.clone();
        move || {
            let clock = ContendedClock {
                reached: reached_send,
                release: RefCell::new(release_receive),
            };
            increment_diagnostic(&diagnostics, "setupCalls", NonZeroU32::MIN, &clock).unwrap();
        }
    });
    reached_receive.recv().unwrap();
    let mut record: Value =
        serde_json::from_slice(&transaction.read_bytes().unwrap().unwrap()).unwrap();
    record["counts"]["setupCalls"] = json!(4);
    transaction
        .write_bytes(&serde_json::to_vec(&record).unwrap())
        .unwrap();
    drop(transaction);
    release_send.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(
        read_diagnostics(&files.diagnostics, &clock).unwrap()["counts"]["setupCalls"],
        5
    );
}
