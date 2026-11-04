//! Exercises saved-query transitions over bounded generated operation sequences.
#![no_main]
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "The generated evidence and operation sequences are bounded"
)]

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};
use ss_ai_review_context::{normalize_options, prepare, query, read_saved};
use ss_ai_runtime::SystemClock;

// Keep file operations per iteration bounded while allowing several changes,
// restarts and page transitions in a single sequence.
const MAX_OPERATIONS: usize = 24;
const MAX_EVIDENCE_BYTES: usize = 128;
// This allowance greatly exceeds the pages needed for the bounded escaped text.
const MAX_PAGES: usize = 64;

/// Isolates every fuzz input, including concurrent worker processes.
struct Files {
    root: PathBuf,
    capture: PathBuf,
    request: PathBuf,
    diagnostics: PathBuf,
}

impl Files {
    #[expect(
        clippy::create_dir,
        reason = "An existing directory must not be reused"
    )]
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The counter only generates distinct names within the process.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".saved-fuzz-{}-{id}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self {
            capture: root.join("capture.json"),
            request: root.join("request.json"),
            diagnostics: root.join("diagnostics.json"),
            root,
        }
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Requires failed operations to preserve the saved request exactly.
fn rejected(files: &Files, next: bool, restart: bool, clock: &SystemClock) {
    let before = fs::read(&files.request).unwrap();
    read_saved(&files.request, next, restart, clock).unwrap_err();
    assert_eq!(fs::read(&files.request).unwrap(), before);
}

fuzz_target!(|input: &[u8]| {
    let files = Files::new();
    let clock = SystemClock::default();
    let mut document: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/ss-review-query-capture.json"
    ))
    .unwrap();
    // Escaped text forces fragmentation even for an empty input; generated
    // characters add mixed byte and UTF-16 widths.
    let mut text = "\"🤖".repeat(4_000);
    for byte in input.iter().take(MAX_EVIDENCE_BYTES) {
        text.push(match byte % 5 {
            0 => '🤖',
            1 => '"',
            2 => '\n',
            3 => 'é',
            _ => 'a',
        });
    }
    document["pullRequest"]["description"] = json!(text);
    fs::write(&files.capture, serde_json::to_vec(&document).unwrap()).unwrap();
    let options = normalize_options(&json!({
        "query": "metadata", "field": "description", "pageSize": 1
    }))
    .unwrap();
    prepare(
        &files.capture,
        None,
        &files.request,
        &files.diagnostics,
        &options,
        &clock,
    )
    .unwrap();

    let mut last: Option<String> = None;
    let mut stale = false;
    let mut changes = 0;
    rejected(&files, true, false, &clock);
    for operation in input.iter().take(MAX_OPERATIONS) {
        match operation % 5 {
            0 if stale => rejected(&files, false, false, &clock),
            0 => {
                let expected = last
                    .clone()
                    .unwrap_or_else(|| query(&document, &options, None, None).unwrap());
                let actual = read_saved(&files.request, false, false, &clock).unwrap();
                assert_eq!(actual, expected);
                last = Some(actual);
            }
            1 if stale || last.is_none() => rejected(&files, true, false, &clock),
            1 => {
                let previous: Value = serde_json::from_str(last.as_ref().unwrap()).unwrap();
                if previous["complete"] == true {
                    rejected(&files, true, false, &clock);
                } else {
                    let cursor = previous["nextCursor"].as_str().unwrap();
                    let expected = query(&document, &options, None, Some(cursor)).unwrap();
                    let actual = read_saved(&files.request, true, false, &clock).unwrap();
                    assert_eq!(actual, expected);
                    assert_ne!(actual, *last.as_ref().unwrap());
                    last = Some(actual);
                }
            }
            2 => {
                let expected = query(&document, &options, None, None).unwrap();
                let actual = read_saved(&files.request, false, true, &clock).unwrap();
                assert_eq!(actual, expected);
                last = Some(actual);
                stale = false;
            }
            3 => {
                changes += 1;
                document["pullRequest"]["title"] = json!(format!("changed-{changes}"));
                fs::write(&files.capture, serde_json::to_vec(&document).unwrap()).unwrap();
                stale = true;
            }
            _ => rejected(&files, true, true, &clock),
        }
        if !stale && let Some(expected) = &last {
            assert_eq!(
                read_saved(&files.request, false, false, &clock).unwrap(),
                *expected
            );
        }
    }

    // Drain from the current capture independently of the generated sequence.
    let mut page = read_saved(&files.request, false, true, &clock).unwrap();
    let mut reconstructed = String::new();
    let mut cursors = HashSet::new();
    for _ in 0..MAX_PAGES {
        let value: Value = serde_json::from_str(&page).unwrap();
        let items = value["items"].as_array().unwrap();
        assert!(!items.is_empty(), "each page must contain a fragment");
        for item in items {
            assert_eq!(
                usize::try_from(item["contentOffset"].as_u64().unwrap()).unwrap(),
                reconstructed.encode_utf16().count()
            );
            assert_eq!(
                usize::try_from(item["contentLength"].as_u64().unwrap()).unwrap(),
                text.encode_utf16().count()
            );
            let fragment = item["content"].as_str().unwrap();
            assert!(!fragment.is_empty(), "each fragment must advance");
            reconstructed.push_str(fragment);
        }
        assert!(text.starts_with(&reconstructed));
        if value["complete"] == true {
            assert!(value["nextCursor"].is_null());
            assert_eq!(reconstructed, text);
            rejected(&files, true, false, &clock);
            assert_eq!(
                read_saved(&files.request, false, false, &clock).unwrap(),
                page
            );
            return;
        }
        let cursor = value["nextCursor"].as_str().unwrap();
        assert!(cursors.insert(cursor.to_owned()), "cursor must advance");
        let expected = query(&document, &options, None, Some(cursor)).unwrap();
        page = read_saved(&files.request, true, false, &clock).unwrap();
        assert_eq!(page, expected);
    }
    panic!("bounded evidence did not complete within the page limit");
});
