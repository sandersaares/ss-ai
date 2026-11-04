//! Native context queries, persisted requests, and diagnostics.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "The fixtures and their page counts are bounded"
)]

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai_review_context::{increment_diagnostic, normalize_options, query};
use ss_ai_runtime::{Clock, FileTransaction};
use uuid::Uuid;

/// Isolates every process transaction while keeping files in the checkout.
struct Fixture {
    root: PathBuf,
    capture: PathBuf,
    request: PathBuf,
    diagnostics: PathBuf,
}

impl Fixture {
    /// Copies the immutable shared contract into an independent working directory.
    fn new() -> Self {
        let root = repository()
            .join("target")
            .join(format!("context-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let capture = root.join("capture space é.json");
        fs::copy(
            repository().join("tests/fixtures/ss-review-query-capture.json"),
            &capture,
        )
        .unwrap();
        Self {
            request: root.join("request.json"),
            diagnostics: root.join("input-diagnostics.json"),
            root,
            capture,
        }
    }

    /// Returns a path suitable for separate process arguments.
    fn path(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    /// Invokes the compiled native executable without a provider.
    fn native(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ss-ai"))
            .args(["context"])
            .args(arguments)
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    /// Writes a capture whose escaped content requires continuation.
    fn long_capture(&self) -> String {
        let mut capture: Value = serde_json::from_slice(&fs::read(&self.capture).unwrap()).unwrap();
        // Mixed-width characters exercise UTF-16 offsets and the encoded byte budget.
        let text = "🤖\"é\n".repeat(6000);
        capture["pullRequest"]["description"] = json!(text);
        fs::write(&self.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
        text
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Locates the active checkout without relying on the caller's working directory.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}

/// Checks the process boundary, including the newline in the output budget.
fn page(output: Output) -> Value {
    let Output {
        status,
        stdout,
        stderr,
    } = output;
    assert!(
        status.success(),
        "process failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(stderr.is_empty());
    assert!(stdout.len() <= 16_384);
    assert!(stdout.ends_with(b"\n"));
    serde_json::from_slice(&stdout).unwrap()
}

/// Removes only the process line ending so JSON bytes can be compared directly.
fn line_payload(output: &[u8]) -> &[u8] {
    let output = output.strip_suffix(b"\n").expect("output must end in LF");
    output.strip_suffix(b"\r").unwrap_or(output)
}

/// Ensures a rejected request neither leaks stdout nor mutates persisted data.
fn rejected(output: Output) {
    let Output {
        status,
        stdout,
        stderr,
    } = output;
    assert!(!status.success());
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty());
}

/// Fails immediately if the initial diagnostic lock unexpectedly contends.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _duration: Duration) -> Result<(), AppError> {
        panic!("initial diagnostic lock must not wait")
    }
}

/// Publishes a concurrent update only after the operation reports contention.
struct DiagnosticClock<'a> {
    lock: RefCell<Option<FileTransaction<'a>>>,
    staged: Vec<u8>,
    waits: Cell<u32>,
}

impl Clock for DiagnosticClock<'_> {
    fn now(&self) -> Timestamp {
        panic!("diagnostic transactions do not read wall-clock time")
    }

    fn wait(&self, _duration: Duration) -> Result<(), AppError> {
        assert_eq!(self.waits.get(), 0, "unexpected repeated contention");
        self.waits.set(1);
        let transaction = self.lock.borrow_mut().take().expect("lock is held");
        transaction.write_bytes(&self.staged)?;
        drop(transaction);
        Ok(())
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires a native process and real file locks")]
#[cfg_attr(
    mutants,
    ignore = "the handshake requires both participants to progress"
)]
fn concurrent_native_diagnostic_updates_are_not_lost() {
    // Other tests launch processes; isolate the lock holder from their fork/exec windows.
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "diagnostic_update_child",
            "--ignored",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[test]
#[ignore = "launched by the isolated diagnostic overlap test"]
#[cfg_attr(miri, ignore = "requires a native process and real file locks")]
#[cfg_attr(
    mutants,
    ignore = "the handshake requires both participants to progress"
)]
fn diagnostic_update_child() {
    let fixture = Fixture::new();
    page(fixture.native(&[
        "diagnostics",
        "--path",
        Fixture::path(&fixture.diagnostics),
        "--record",
        "setupCalls",
    ]));
    let no_wait = NoWait;
    let transaction = FileTransaction::open(&fixture.diagnostics, &no_wait).unwrap();
    let mut staged: Value =
        serde_json::from_slice(&transaction.read_bytes().unwrap().unwrap()).unwrap();
    staged["counts"]["setupCalls"] = json!(2);
    let clock = DiagnosticClock {
        lock: RefCell::new(Some(transaction)),
        staged: serde_json::to_vec(&staged).unwrap(),
        waits: Cell::new(0),
    };
    increment_diagnostic(&fixture.diagnostics, "setupCalls", NonZeroU32::MIN, &clock).unwrap();
    assert_eq!(clock.waits.get(), 1);
    let result =
        page(fixture.native(&["diagnostics", "--path", Fixture::path(&fixture.diagnostics)]));
    assert_eq!(result["counts"]["setupCalls"], 3);
}

#[test]
#[cfg_attr(miri, ignore = "requires native subprocesses")]
fn direct_queries_match_canonical_fixture() {
    let fixture = Fixture::new();
    let expected: Value = serde_json::from_slice(
        &fs::read(repository().join("tests/fixtures/ss-review-query-expected.json")).unwrap(),
    )
    .unwrap();
    let before = fs::read(&fixture.capture).unwrap();
    for (query, native) in [
        ("metadata", vec!["--field", "title"]),
        ("comments", vec!["--thread-id", "T", "--comment-id", "C"]),
    ] {
        let mut args = vec![
            "read",
            "--path",
            Fixture::path(&fixture.capture),
            "--query",
            query,
        ];
        args.extend(native);
        let native_output = fixture.native(&args);
        if query == "metadata" {
            assert_eq!(
                native_output.stdout,
                concat!(
                    "{\"schemaVersion\":1,\"query\":\"metadata\",",
                    "\"snapshotId\":\"FEF75D9836BAC16552C0569D1172931FFAC8B38B8B61E2F540DE07DDD4A10AF5\",",
                    "\"totalCount\":1,\"items\":[{\"field\":\"title\",\"content\":\"Widget\",",
                    "\"contentOffset\":0,\"contentLength\":6,\"contentComplete\":true}],",
                    "\"nextCursor\":null,\"complete\":true}\n"
                ).as_bytes()
            );
        } else {
            assert_eq!(
                native_output.stdout,
                concat!(
                    "{\"schemaVersion\":1,\"query\":\"comments\",",
                    "\"snapshotId\":\"FEF75D9836BAC16552C0569D1172931FFAC8B38B8B61E2F540DE07DDD4A10AF5\",",
                    "\"totalCount\":1,\"items\":[{\"threadId\":\"T\",",
                    "\"url\":\"https://example.invalid/thread/T\",\"openingCommentId\":\"C\",",
                    "\"kind\":\"text\",\"resolution\":\"resolved\",\"isOutdated\":true,",
                    "\"context\":{\"filePath\":\"src/widget.rs\",\"side\":\"right\",\"startLine\":4,\"endLine\":4},",
                    "\"decision\":null,\"isAuthoredBySelf\":false,\"eventUtc\":\"2026-10-01T12:00:00Z\",",
                    "\"lastUpdatedUtc\":null,\"commentId\":\"C\",\"commentUrl\":\"https://example.invalid/comment/C\",",
                    "\"parentId\":null,\"author\":{\"id\":\"person\",\"displayName\":\"Reviewer\"},",
                    "\"authorship\":\"indeterminate\",\"authorshipEvidence\":[],\"threadIsAuthoredBySelf\":false,",
                    "\"publishedUtc\":\"2026-10-01T12:00:00Z\",\"lastEditedUtc\":null,",
                    "\"content\":\"Quoted \\\"widget\\\"\\n🤖 evidence\",\"contentOffset\":0,",
                    "\"contentLength\":27,\"contentComplete\":true}],\"nextCursor\":null,\"complete\":true}\n"
                ).as_bytes()
            );
        }
        let native = page(native_output);
        assert_eq!(native, expected[query]);
    }
    assert_eq!(fs::read(&fixture.capture).unwrap(), before);
}

#[test]
fn line_payload_preserves_json_bytes_with_either_line_ending() {
    let json = b"{\"text\":\"a\\nb\"}";
    assert_eq!(line_payload(b"{\"text\":\"a\\nb\"}\n"), json);
    assert_eq!(line_payload(b"{\"text\":\"a\\nb\"}\r\n"), json);
}

#[test]
#[cfg_attr(miri, ignore = "requires native subprocesses")]
fn direct_pagination_preserves_utf16_offsets() {
    let fixture = Fixture::new();
    let text = fixture.long_capture();
    let before = fs::read(&fixture.capture).unwrap();
    let mut cursor = String::new();
    let mut seen = HashSet::new();
    let mut content = String::new();
    // The output budget reserves the platform line ending, changing later fragments.
    let expected_hashes = if cfg!(windows) {
        [
            "972AB8854BB3A478203B12F148115F8C15B31F317A1D442E4E3A147AC4500F7B",
            "D022D6151737D940AB1D7B7A0A99BA7D82987053EA5737038ECA4AC0EAAFFA22",
            "9140140D3025BE278CB91C6BB6FB28953D50269D6120249C2C4B644FC413FB14",
            "731C573F4EA88F201E55E28A128AC2AF7A0E9D634D90105007745315178E10D6",
        ]
    } else {
        [
            "972AB8854BB3A478203B12F148115F8C15B31F317A1D442E4E3A147AC4500F7B",
            "D022D6151737D940AB1D7B7A0A99BA7D82987053EA5737038ECA4AC0EAAFFA22",
            "D9BC113931EBEFFEF0E4907EB4C9F5368F48F9157F2CB72A9CEA913C88BA8AF9",
            "74B9B1FEE173352C0C924FEF7AE392EAEC175ECA32C55D4CA17C6FC6C3094237",
        ]
    };
    // The chosen fixture needs only a few pages; the bound catches stalled pagination.
    for page_index in 0..64 {
        let mut native = vec![
            "read",
            "--path",
            Fixture::path(&fixture.capture),
            "--query",
            "metadata",
            "--field",
            "description",
            "--page-size",
            "1",
        ];
        if !cursor.is_empty() {
            native.extend(["--cursor", &cursor]);
        }
        let output = fixture.native(&native);
        assert_eq!(
            format!("{:X}", Sha256::digest(line_payload(&output.stdout))),
            expected_hashes[page_index],
            "page {page_index}"
        );
        let native = page(output);
        let items = native["items"].as_array().unwrap();
        assert!(!items.is_empty());
        for item in items {
            assert_eq!(
                usize::try_from(item["contentOffset"].as_u64().unwrap()).unwrap(),
                content.encode_utf16().count()
            );
            assert_eq!(
                usize::try_from(item["contentLength"].as_u64().unwrap()).unwrap(),
                text.encode_utf16().count()
            );
            content.push_str(item["content"].as_str().unwrap());
        }
        if native["complete"].as_bool().unwrap() {
            assert_eq!(page_index + 1, expected_hashes.len());
            assert!(native["nextCursor"].is_null());
            assert_eq!(content, text);
            assert_eq!(fs::read(&fixture.capture).unwrap(), before);
            return;
        }
        cursor = native["nextCursor"].as_str().unwrap().to_owned();
        assert!(seen.insert(cursor.clone()), "cursor must advance");
    }
    panic!("query did not complete within the fixture page bound");
}

#[test]
#[cfg_attr(miri, ignore = "requires native subprocesses")]
fn saved_requests_resume_and_restart_explicitly() {
    let fixture = Fixture::new();
    fixture.long_capture();
    let capture = Fixture::path(&fixture.capture);
    let request = Fixture::path(&fixture.request);
    let diagnostics = Fixture::path(&fixture.diagnostics);
    let prepared = page(fixture.native(&[
        "prepare",
        "--path",
        capture,
        "--request-path",
        request,
        "--diagnostics-path",
        diagnostics,
        "--query",
        "metadata",
        "--field",
        "description",
        "--page-size",
        "1",
    ]));
    let record: Value = serde_json::from_slice(&fs::read(&fixture.request).unwrap()).unwrap();
    assert_eq!(
        record,
        json!({
            "schemaVersion":1,"path":capture,"comparePath":null,
            "snapshotId":prepared["snapshotId"],"baselineId":null,
            "options":{"query":"metadata","field":"description","files":[],"scope":"all",
                "kind":"","resolution":"","threadId":"","commentId":"","authorship":"",
                "indexOnly":false,"text":"","pageSize":1},
            "requestId":prepared["requestId"],"diagnosticsPath":diagnostics,
            "lastPage":null,"lastPageCursor":null
        })
    );
    let native_read = || fixture.native(&["read-saved", "--request-path", request]);
    let first = page(native_read());
    assert_eq!(page(native_read()), first);
    assert!(!first["complete"].as_bool().unwrap());
    let second = page(fixture.native(&["read-saved", "--request-path", request, "--next"]));
    assert_ne!(first, second);
    assert_eq!(page(native_read()), second);

    let mut capture_data: Value =
        serde_json::from_slice(&fs::read(&fixture.capture).unwrap()).unwrap();
    capture_data["pullRequest"]["title"] = json!("changed");
    fs::write(&fixture.capture, serde_json::to_vec(&capture_data).unwrap()).unwrap();
    let saved = fs::read(&fixture.request).unwrap();
    rejected(native_read());
    assert_eq!(fs::read(&fixture.request).unwrap(), saved);
    let restarted = page(fixture.native(&["read-saved", "--request-path", request, "--restart"]));
    assert_ne!(restarted["snapshotId"], first["snapshotId"]);
    assert_eq!(page(native_read()), restarted);
    let sidecar = page(fixture.native(&["diagnostics", "--path", diagnostics]));
    assert_eq!(sidecar["counts"]["setupCalls"], 1);
    assert_eq!(sidecar["counts"]["cursorRestarts"], 1);
}

#[test]
#[cfg_attr(miri, ignore = "requires native subprocesses")]
fn conflicting_saved_actions_do_not_start_a_transaction() {
    let fixture = Fixture::new();
    let request = Fixture::path(&fixture.request);
    let output = fixture.native(&[
        "read-saved",
        "--request-path",
        request,
        "--next",
        "--restart",
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalidInput"), "{stderr}");
    assert!(
        stderr.contains("Next and Restart are mutually exclusive"),
        "{stderr}"
    );
    assert!(!fixture.request.exists());
    assert!(!fixture.root.join("request.json.lock").exists());
    assert!(!fixture.diagnostics.exists());
}

#[test]
#[cfg_attr(miri, ignore = "requires native subprocesses")]
fn malformed_saved_requests_are_rejected_without_mutation() {
    for missing in [true, false] {
        let fixture = Fixture::new();
        let capture = Fixture::path(&fixture.capture);
        let request = Fixture::path(&fixture.request);
        let diagnostics = Fixture::path(&fixture.diagnostics);
        page(fixture.native(&[
            "prepare",
            "--path",
            capture,
            "--request-path",
            request,
            "--diagnostics-path",
            diagnostics,
            "--query",
            "metadata",
            "--field",
            "title",
        ]));
        let mut record: Value =
            serde_json::from_slice(&fs::read(&fixture.request).unwrap()).unwrap();
        if missing {
            record["options"].as_object_mut().unwrap().remove("files");
        } else {
            record["options"]["unknown"] = json!("unexpected");
        }

        fs::write(&fixture.request, serde_json::to_vec(&record).unwrap()).unwrap();
        let saved = fs::read(&fixture.request).unwrap();
        let captured = fs::read(&fixture.capture).unwrap();
        let sidecar = fs::read(&fixture.diagnostics).unwrap();
        rejected(fixture.native(&["read-saved", "--request-path", request]));
        assert_eq!(fs::read(&fixture.request).unwrap(), saved);
        assert_eq!(fs::read(&fixture.capture).unwrap(), captured);
        assert_eq!(fs::read(&fixture.diagnostics).unwrap(), sidecar);
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn unicode_metadata_is_preserved_in_current_and_baseline() {
    let fixture = Fixture::new();
    let baseline = fixture.root.join("baseline.json");
    let mut capture: Value = serde_json::from_slice(&fs::read(&fixture.capture).unwrap()).unwrap();
    let author = json!({"displayName": "Élodie 東京", "地域": "München 🤖"});
    capture["pullRequest"]["author"] = author.clone();
    let original = serde_json::to_vec(&capture).unwrap();
    fs::write(&fixture.capture, &original).unwrap();
    fs::write(&baseline, &original).unwrap();
    let result = page(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "metadata",
        "--field",
        "author",
    ]));
    assert_eq!(result["items"][0]["value"], author);
    let changes = page(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "changes",
        "--compare-path",
        Fixture::path(&baseline),
    ]));
    assert_eq!(changes["totalCount"], 0);
    assert_eq!(fs::read(&fixture.capture).unwrap(), original);
    assert_eq!(fs::read(&baseline).unwrap(), original);
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn duplicate_capture_keys_are_rejected_without_mutation() {
    let fixture = Fixture::new();
    let baseline = fixture.root.join("baseline.json");
    let original = fs::read_to_string(&fixture.capture).unwrap();
    let invalid = original.replacen('{', "{\"schemaVersion\":3,", 1);
    fs::write(&fixture.capture, &invalid).unwrap();
    rejected(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "metadata",
        "--field",
        "title",
    ]));
    assert_eq!(fs::read_to_string(&fixture.capture).unwrap(), invalid);

    fs::write(&fixture.capture, &original).unwrap();
    fs::write(&baseline, &invalid).unwrap();
    rejected(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "changes",
        "--compare-path",
        Fixture::path(&baseline),
    ]));
    assert_eq!(fs::read_to_string(&fixture.capture).unwrap(), original);
    assert_eq!(fs::read_to_string(&baseline).unwrap(), invalid);
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn malformed_captures_fail_and_empty_discussions_remain_readable() {
    let fixture = Fixture::new();
    let mut capture: Value = serde_json::from_slice(&fs::read(&fixture.capture).unwrap()).unwrap();
    capture["threads"][0]["comments"][0]
        .as_object_mut()
        .unwrap()
        .remove("content");
    let malformed = serde_json::to_vec(&capture).unwrap();
    fs::write(&fixture.capture, &malformed).unwrap();
    rejected(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "threads",
    ]));
    assert_eq!(fs::read(&fixture.capture).unwrap(), malformed);

    capture["threads"] = json!([]);
    fs::write(&fixture.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
    let result = page(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "threads",
    ]));
    assert_eq!(result["totalCount"], 0);
    assert_eq!(result["items"], json!([]));
    assert_eq!(result["complete"], true);
}

#[test]
fn unicode_simple_case_search_uses_literal_text_and_utf16_offsets() {
    let mut capture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/ss-review-query-capture.json"
    ))
    .unwrap();
    // Pairs include non-ASCII, supplementary-plane and nontrivial simple folds.
    for (body, needle) in [
        ("É", "é"),
        ("Σ", "ς"),
        ("K", "k"),
        ("ẞ", "ß"),
        ("ſ", "S"),
        ("ᾀ", "ᾈ"),
        ("𐐀", "𐐨"),
    ] {
        let text = format!("a🤖{body} suffix");
        capture["pullRequest"]["description"] = json!(text);
        let options = normalize_options(&json!({"query":"search", "text":needle})).unwrap();
        let result: Value =
            serde_json::from_str(&query(&capture, &options, None, None).unwrap()).unwrap();
        assert_eq!(result["totalCount"], 1, "search for {needle:?} in {body:?}");
        let item = &result["items"][0];
        assert_eq!(item["field"], "description");
        assert_eq!(item["matchOffset"], 3);
        assert_eq!(item["excerptOffset"], 0);
        assert_eq!(item["excerpt"], text);
        assert_eq!(
            usize::try_from(item["contentLength"].as_u64().unwrap()).unwrap(),
            text.encode_utf16().count()
        );
    }

    capture["pullRequest"]["description"] = json!("a🤖Straße ﬃ é");
    for needle in ["STRASSE", "ffi", "e\u{301}", ".*"] {
        let options = normalize_options(&json!({"query":"search", "text":needle})).unwrap();
        let result: Value =
            serde_json::from_str(&query(&capture, &options, None, None).unwrap()).unwrap();
        assert_eq!(result["totalCount"], 0, "search for {needle:?}");
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn unicode_search_crosses_the_process_boundary() {
    let fixture = Fixture::new();
    let mut capture: Value = serde_json::from_slice(&fs::read(&fixture.capture).unwrap()).unwrap();
    capture["pullRequest"]["description"] = json!("a🤖É suffix");
    fs::write(&fixture.capture, serde_json::to_vec(&capture).unwrap()).unwrap();
    let result = page(fixture.native(&[
        "read",
        "--path",
        Fixture::path(&fixture.capture),
        "--query",
        "search",
        "--text",
        "é",
    ]));
    assert_eq!(result["totalCount"], 1);
    assert_eq!(result["items"][0]["matchOffset"], 3);
    assert_eq!(result["items"][0]["excerpt"], "a🤖É suffix");
}

#[test]
#[cfg_attr(miri, ignore = "requires native subprocesses")]
fn authorship_sidecar_invalidates_native_saved_snapshot() {
    let fixture = Fixture::new();
    let capture = Fixture::path(&fixture.capture);
    let request = Fixture::path(&fixture.request);
    let diagnostics = Fixture::path(&fixture.diagnostics);
    let prepared = page(fixture.native(&[
        "prepare",
        "--path",
        capture,
        "--request-path",
        request,
        "--diagnostics-path",
        diagnostics,
        "--query",
        "comments",
        "--thread-id",
        "T",
        "--comment-id",
        "C",
    ]));
    let snapshot = prepared["snapshotId"].as_str().unwrap();
    let raw = fs::read(&fixture.capture).unwrap();
    page(fixture.native(&[
        "set-authorship",
        "--path",
        capture,
        "--snapshot-id",
        snapshot,
        "--classification",
        r#"[{"threadId":"T","commentId":"C","kind":"human"}]"#,
    ]));
    let sidecar = PathBuf::from(format!("{capture}.authorship.json"));
    let sidecar_before = fs::read(&sidecar).unwrap();
    let saved = fs::read(&fixture.request).unwrap();
    rejected(fixture.native(&["read-saved", "--request-path", request]));
    assert_eq!(fs::read(&fixture.request).unwrap(), saved);
    let native = page(fixture.native(&["read-saved", "--request-path", request, "--restart"]));
    assert_eq!(
        native,
        page(fixture.native(&["read-saved", "--request-path", request]))
    );
    assert_eq!(native["items"][0]["authorship"], "human");
    assert_ne!(native["snapshotId"], snapshot);
    assert_eq!(fs::read(&fixture.capture).unwrap(), raw);
    assert_eq!(fs::read(&sidecar).unwrap(), sidecar_before);

    let mut invalid: Value = serde_json::from_slice(&sidecar_before).unwrap();
    invalid["schemaVersion"] = json!(99);
    fs::write(&sidecar, serde_json::to_vec(&invalid).unwrap()).unwrap();
    let malformed = fs::read(&sidecar).unwrap();
    rejected(fixture.native(&[
        "read",
        "--path",
        capture,
        "--query",
        "comments",
        "--thread-id",
        "T",
    ]));
    assert_eq!(fs::read(&sidecar).unwrap(), malformed);
    assert_eq!(fs::read(&fixture.capture).unwrap(), raw);
}
