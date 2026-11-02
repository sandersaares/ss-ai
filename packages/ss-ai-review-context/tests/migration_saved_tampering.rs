//! Exercises saved-query evidence validation at the file transaction boundary.
#![allow(
    clippy::indexing_slicing,
    reason = "The tests construct fixed JSON fixtures and inspect their known fields"
)]

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{
    document_hash, prepare, query, read_diagnostics, read_saved, snapshot_id,
    write_comment_authorship,
};
use ss_ai_runtime::Clock;

/// Makes unexpected transaction contention fail without waiting on real time.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("saved-query fixture must not wait")
    }
}

/// Keeps each real file transaction independent without removing existing files.
struct Files {
    capture: PathBuf,
    request: PathBuf,
    diagnostics: PathBuf,
    document: Value,
}

impl Files {
    /// Allocates unique retained build artifacts for one saved-query scenario.
    fn new(long_body: bool) -> Self {
        static WORKSPACE: Once = Once::new();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        WORKSPACE.call_once(|| {
            let package = Path::new(env!("CARGO_MANIFEST_DIR"));
            std::env::set_current_dir(package.parent().unwrap().parent().unwrap()).unwrap();
        });
        // Cargo's persistent output directory keeps the retained test files out
        // of source even when the build uses a different output directory.
        fs::create_dir_all("target").unwrap();
        let output = Path::new("target");
        let mut document: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/ss-review-query-capture.json"
        ))
        .unwrap();
        if long_body {
            document["threads"][0]["comments"][0]["content"] = json!("evidence ".repeat(3000));
        }
        let mut reply = document["threads"][0]["comments"][0].clone();
        reply["id"] = json!("Reply");
        document["threads"][0]["comments"]
            .as_array_mut()
            .unwrap()
            .push(reply);

        // Exclusive creation handles process ID reuse; the bound avoids a stuck test
        // if the namespace cannot provide a fresh name.
        for _ in 0..1024 {
            // The counter only supplies candidate names, not fixture state.
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let stem = format!("migration-saved-{}-{id}", std::process::id());
            let capture = output.join(format!("{stem}-capture.json"));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&capture)
            {
                Ok(mut file) => {
                    file.write_all(&serde_json::to_vec(&document).unwrap())
                        .unwrap();
                    return Self {
                        capture,
                        request: output.join(format!("{stem}-request.json")),
                        diagnostics: output.join(format!("{stem}-diagnostics.json")),
                        document,
                    };
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot create test capture: {error}"),
            }
        }
        panic!("cannot allocate an unused saved-query test name");
    }

    /// Creates a request and records its first page using the production writer.
    fn prepare_first(&self, options: &Value) -> Value {
        let clock = NoWait;
        prepare(
            &self.capture,
            None,
            &self.request,
            &self.diagnostics,
            options,
            &clock,
        )
        .unwrap();
        read_saved(&self.request, false, false, &clock).unwrap();
        self.record()
    }

    /// Reads the record that a caller may inspect or edit between invocations.
    fn record(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.request).unwrap()).unwrap()
    }

    /// Simulates an external edit to a saved request.
    fn write_record(&self, record: &Value) {
        fs::write(&self.request, serde_json::to_vec(record).unwrap()).unwrap();
    }

    /// Both replay and continuation must refuse edited evidence without rewriting it.
    fn rejects_both(&self, diagnostic: &str) {
        let before = fs::read(&self.request).unwrap();
        for next in [false, true] {
            let error = read_saved(&self.request, next, false, &NoWait).unwrap_err();
            assert!(error.to_string().contains(diagnostic), "{error}");
            assert_eq!(fs::read(&self.request).unwrap(), before);
        }
    }
}

#[test]
fn edited_content_attribution_extra_field_and_continuation_are_rejected() {
    let files = Files::new(false);
    let original = files.prepare_first(&json!({"query":"comments","threadId":"T","pageSize":1}));
    let mut variants = Vec::new();
    let mut content = original.clone();
    content["lastPage"]["items"][0]["content"] = json!("substituted evidence");
    variants.push(content);
    let mut attribution = original.clone();
    attribution["lastPage"]["items"][0]
        .as_object_mut()
        .unwrap()
        .remove("author");
    variants.push(attribution);
    let mut extra = original.clone();
    extra["lastPage"]["items"][0]["inventedEvidence"] = json!("not in the capture");
    variants.push(extra);
    let mut continuation = original;
    let cursor = continuation["lastPage"]["nextCursor"].as_str().unwrap();
    let fingerprint = cursor.split(':').next().unwrap();
    continuation["lastPage"]["nextCursor"] = json!(format!("{fingerprint}:0:0"));
    variants.push(continuation);
    for record in variants {
        files.write_record(&record);
        files.rejects_both("does not match");
    }
}

#[test]
fn valid_page_from_another_selection_is_not_replayed() {
    let files = Files::new(false);
    let mut request = files.prepare_first(&json!({"query":"metadata","field":"title"}));
    request["lastPage"] = serde_json::from_str(
        &query(
            &files.document,
            &json!({"query":"metadata","field":"description"}),
            None,
            None,
        )
        .unwrap(),
    )
    .unwrap();
    files.write_record(&request);
    files.rejects_both("does not match");
}

#[test]
fn substituted_continuation_and_starting_cursor_are_rejected() {
    let files = Files::new(true);
    let mut request = files.prepare_first(&json!({"query":"comments","threadId":"T","pageSize":1}));
    let other = json!({"query":"comments","threadId":"T","pageSize":2});
    let first: Value =
        serde_json::from_str(&query(&files.document, &other, None, None).unwrap()).unwrap();
    let cursor = first["nextCursor"].as_str().unwrap();
    request["lastPageCursor"] = json!(cursor);
    request["lastPage"] =
        serde_json::from_str(&query(&files.document, &other, None, Some(cursor)).unwrap()).unwrap();
    files.write_record(&request);
    files.rejects_both("mismatched query cursor");
}

#[test]
fn saved_page_and_starting_cursor_must_be_present_together() {
    let files = Files::new(false);
    let clock = NoWait;
    prepare(
        &files.capture,
        None,
        &files.request,
        &files.diagnostics,
        &json!({"query":"metadata","field":"title"}),
        &clock,
    )
    .unwrap();
    let initial = files.record();
    assert!(initial["lastPage"].is_null());
    assert!(initial["lastPageCursor"].is_null());
    let mut without_page = initial.clone();
    without_page["lastPageCursor"] = json!("");
    files.write_record(&without_page);
    files.rejects_both("queryRequest");

    files.write_record(&initial);
    read_saved(&files.request, false, false, &clock).unwrap();
    let original = files.record();
    assert_eq!(original["lastPageCursor"], "");
    let mut null_cursor = original.clone();
    null_cursor["lastPageCursor"] = Value::Null;
    files.write_record(&null_cursor);
    files.rejects_both("queryRequest");
    let mut missing_cursor = original;
    missing_cursor
        .as_object_mut()
        .unwrap()
        .remove("lastPageCursor");
    files.write_record(&missing_cursor);
    files.rejects_both("queryRequest");
}

#[test]
fn replay_and_next_preserve_the_saved_comment_selection() {
    let files = Files::new(false);
    let clock = NoWait;
    prepare(
        &files.capture,
        None,
        &files.request,
        &files.diagnostics,
        &json!({"query":"comments","threadId":"T","pageSize":1}),
        &clock,
    )
    .unwrap();
    assert!(
        read_saved(&files.request, true, false, &clock)
            .unwrap_err()
            .to_string()
            .contains("first page")
    );
    let first = read_saved(&files.request, false, false, &clock).unwrap();
    assert_eq!(
        read_saved(&files.request, false, false, &clock).unwrap(),
        first
    );
    let second = read_saved(&files.request, true, false, &clock).unwrap();
    assert_eq!(
        read_saved(&files.request, false, false, &clock).unwrap(),
        second
    );
    let second: Value = serde_json::from_str(&second).unwrap();
    assert_eq!(second["items"][0]["commentId"], "Reply");
    assert_eq!(second["complete"], true);
    assert!(
        read_saved(&files.request, true, false, &clock)
            .unwrap_err()
            .to_string()
            .contains("complete")
    );
    assert_eq!(files.record()["options"]["pageSize"], 1);
}

#[test]
fn edited_selectors_reject_both_continuation_and_restart() {
    let files = Files::new(false);
    let mut request = files.prepare_first(&json!({"query":"comments","threadId":"T","pageSize":1}));
    request["options"]["threadId"] = json!("t");
    files.write_record(&request);
    let before = fs::read(&files.request).unwrap();
    for (next, restart) in [(true, false), (false, true)] {
        let error = read_saved(&files.request, next, restart, &NoWait).unwrap_err();
        assert!(error.to_string().contains("modified"));
        assert_eq!(fs::read(&files.request).unwrap(), before);
    }
}

#[test]
fn changed_capture_rejects_replay_and_next_until_explicit_restart() {
    let mut files = Files::new(false);
    files.prepare_first(&json!({"query":"comments","threadId":"T","pageSize":1}));
    files.document["threads"][0]["comments"][0]["content"] = json!("changed root");
    fs::write(&files.capture, serde_json::to_vec(&files.document).unwrap()).unwrap();
    files.rejects_both("capture changed");
    let clock = NoWait;
    let restarted: Value =
        serde_json::from_str(&read_saved(&files.request, false, true, &clock).unwrap()).unwrap();
    assert_eq!(restarted["items"][0]["content"], "changed root");
    assert_eq!(
        read_diagnostics(&files.diagnostics, &clock).unwrap()["counts"]["cursorRestarts"],
        1
    );
}

#[test]
fn changed_baseline_rejects_replay_and_restart_rebinds_changes() {
    let files = Files::new(false);
    let baseline = files.capture.with_extension("baseline.json");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&baseline)
        .unwrap()
        .write_all(&fs::read(&files.capture).unwrap())
        .unwrap();
    let clock = NoWait;
    prepare(
        &files.capture,
        Some(&baseline),
        &files.request,
        &files.diagnostics,
        &json!({"query":"changes"}),
        &clock,
    )
    .unwrap();
    read_saved(&files.request, false, false, &clock).unwrap();
    let mut changed = files.document.clone();
    changed["threads"][0]["comments"][0]["content"] = json!("older root");
    changed["threads"][0]["comments"][0]["lastEditedUtc"] = json!("2026-09-01T12:00:00Z");
    fs::write(&baseline, serde_json::to_vec(&changed).unwrap()).unwrap();
    let before = fs::read(&files.request).unwrap();
    let error = read_saved(&files.request, false, false, &clock).unwrap_err();
    assert!(error.to_string().contains("capture changed"));
    assert_eq!(fs::read(&files.request).unwrap(), before);
    let restarted: Value =
        serde_json::from_str(&read_saved(&files.request, false, true, &clock).unwrap()).unwrap();
    assert_eq!(restarted["items"][0]["change"], "commentsEdited");
}

#[test]
fn changed_semantic_authorship_requires_restart_without_changing_capture() {
    let files = Files::new(false);
    let options = json!({"query":"comments","authorship":"indeterminate","pageSize":1});
    let request = files.prepare_first(&options);
    let snapshot = request["snapshotId"].as_str().unwrap();
    let before = fs::read(&files.capture).unwrap();
    let clock = NoWait;
    write_comment_authorship(
        &files.capture,
        snapshot,
        &json!([{"threadId":"T","commentId":"C","kind":"human"}]),
        &clock,
    )
    .unwrap();
    assert_eq!(fs::read(&files.capture).unwrap(), before);
    files.rejects_both("capture changed");
    let restarted: Value =
        serde_json::from_str(&read_saved(&files.request, false, true, &clock).unwrap()).unwrap();
    assert_ne!(restarted["snapshotId"], snapshot);
    assert_eq!(restarted["items"][0]["commentId"], "Reply");
}

#[test]
fn setup_failures_and_repeated_setup_preserve_the_existing_request() {
    let files = Files::new(false);
    let clock = NoWait;
    prepare(
        &files.capture,
        None,
        &files.request,
        &files.diagnostics,
        &json!({"query":"metadata"}),
        &clock,
    )
    .unwrap_err();
    assert!(!files.request.exists());
    prepare(
        &files.capture,
        None,
        &files.request,
        &files.diagnostics,
        &json!({"query":"metadata","field":"title"}),
        &clock,
    )
    .unwrap();
    let before = fs::read(&files.request).unwrap();
    prepare(
        &files.capture,
        None,
        &files.request,
        &files.diagnostics,
        &json!({"query":"metadata","field":"description"}),
        &clock,
    )
    .unwrap_err();
    assert_eq!(fs::read(&files.request).unwrap(), before);
    let counts = &read_diagnostics(&files.diagnostics, &clock).unwrap()["counts"];
    assert_eq!(counts["setupCalls"], 3);
    assert_eq!(counts["repeatedSetupCalls"], 1);
    assert_eq!(counts["malformedQueryFailures"], 1);
}

#[test]
fn oversized_replay_refuses_the_page_and_preserves_the_request() {
    let files = Files::new(false);
    let mut request = files.prepare_first(&json!({"query":"metadata","field":"title"}));
    request["lastPage"]["items"][0]["content"] = json!("x".repeat(20000));
    files.write_record(&request);
    files.rejects_both("bounded output limit");
}

#[test]
fn missing_and_unknown_top_level_fields_reject_the_saved_record() {
    let files = Files::new(false);
    let request = files.prepare_first(&json!({"query":"metadata","field":"title"}));
    let mut extra = request.clone();
    extra["extra"] = json!("not a selector");
    files.write_record(&extra);
    files.rejects_both("queryRequest");

    let mut missing = request;
    missing.as_object_mut().unwrap().remove("lastPage");
    files.write_record(&missing);
    files.rejects_both("queryRequest");
}

#[test]
fn request_from_another_authorship_policy_requires_restart() {
    let files = Files::new(false);
    let mut request = files.prepare_first(&json!({"query":"metadata","field":"title"}));
    let capture_before = fs::read(&files.capture).unwrap();
    // Model a request from a different policy while preserving a correct request
    // identity, so rejection tests policy binding rather than selector tampering.
    let earlier_snapshot = document_hash(&json!({
        "policyVersion": "different-policy-version",
        "document": files.document
    }))
    .unwrap();
    assert_ne!(earlier_snapshot, snapshot_id(&files.document).unwrap());
    request["snapshotId"] = json!(earlier_snapshot);
    request["lastPage"]["snapshotId"] = json!(earlier_snapshot);
    request["requestId"] = json!(
        document_hash(&json!({
            "path": request["path"],
            "comparePath": request["comparePath"],
            "snapshotId": request["snapshotId"],
            "baselineId": request["baselineId"],
            "options": request["options"]
        }))
        .unwrap()
    );
    files.write_record(&request);
    files.rejects_both("capture changed");
    assert_eq!(fs::read(&files.capture).unwrap(), capture_before);
}
