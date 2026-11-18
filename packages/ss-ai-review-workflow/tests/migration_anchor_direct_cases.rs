//! Covers direct publication-anchor resolution against recorded diff evidence.
#![allow(
    clippy::indexing_slicing,
    reason = "The tests inspect known JSON fixtures and result fields"
)]

use serde_json::{Value, json};
use ss_ai_review_workflow::anchors::resolve_request;

/// Provides modified, renamed and deleted files for publication anchors.
fn diff() -> Value {
    json!({"files":[
        {"path":"src/lib.rs","previousPath":null,"status":"modified",
            "hunks":[{"oldStart":10,"oldCount":8,"newStart":10,"newCount":8}]},
        {"path":"src/new-name.rs","previousPath":"src/old-name.rs","status":"renamed",
            "hunks":[{"oldStart":1,"oldCount":3,"newStart":1,"newCount":3}]},
        {"path":"old/gone.txt","previousPath":null,"status":"deleted",
            "hunks":[{"oldStart":1,"oldCount":2,"newStart":0,"newCount":0}]}
    ]})
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "The fixtures are constructed inline for concise contract examples"
)]
fn resolve(anchor: Value) -> Value {
    resolve_request(&json!({"anchor":anchor,"diff":diff()})).unwrap()
}

#[test]
fn line_and_file_anchors_preserve_or_downgrade_positions() {
    let result = resolve(
        json!({"kind":"line","path":"src/lib.rs","startLine":14,"endLine":15,"side":"right"}),
    );
    assert_eq!(
        result,
        json!({"kind":"line","path":"src/lib.rs","startLine":14,"endLine":15,"side":"right","wasDowngraded":false})
    );
    let context = resolve(json!({"kind":"line","path":"src/lib.rs","startLine":10}));
    assert_eq!(
        context,
        json!({"kind":"line","path":"src/lib.rs","startLine":10,"endLine":10,"side":"right","wasDowngraded":false})
    );
    let downgraded = resolve(
        json!({"kind":"line","path":"src/lib.rs","startLine":300,"endLine":305,"side":"right"}),
    );
    assert_eq!(downgraded["kind"], "file");
    assert_eq!(downgraded["path"], "src/lib.rs");
    assert_eq!(downgraded["wasDowngraded"], true);
    let reason = downgraded["downgradeReason"].as_str().unwrap();
    for text in [
        "No hunk",
        "lines 300-305",
        "right",
        "src/lib.rs",
        "reported against the file",
    ] {
        assert!(reason.contains(text));
    }
    assert!(downgraded.get("startLine").is_none());
    assert!(downgraded.get("side").is_none());
    let file = resolve(json!({"kind":"file","path":"src/lib.rs"}));
    assert_eq!(
        file,
        json!({"kind":"file","path":"src/lib.rs","wasDowngraded":false})
    );
}

#[test]
fn deleted_files_resolve_old_lines_but_downgrade_new_lines() {
    assert_eq!(
        resolve(
            json!({"kind":"line","path":"old/gone.txt","startLine":1,"endLine":2,"side":"left"})
        ),
        json!({"kind":"line","path":"old/gone.txt","startLine":1,"endLine":2,"side":"left","wasDowngraded":false})
    );
    let result = resolve(json!({"kind":"line","path":"old/gone.txt","startLine":1,"side":"right"}));
    assert_eq!(result["kind"], "file");
    assert_eq!(result["path"], "old/gone.txt");
    assert_eq!(result["wasDowngraded"], true);
    let reason = result["downgradeReason"].as_str().unwrap();
    for text in [
        "No hunk",
        "line 1",
        "right",
        "old/gone.txt",
        "reported against the file",
    ] {
        assert!(reason.contains(text));
    }
    assert_eq!(
        resolve(json!({"kind":"file","path":"old/gone.txt"})),
        json!({"kind":"file","path":"old/gone.txt","wasDowngraded":false})
    );
}

#[test]
fn anchor_paths_match_post_change_files_and_reject_wrong_file() {
    let renamed = resolve(json!({"kind":"line","path":"src/new-name.rs","startLine":2}));
    assert_eq!(renamed["kind"], "line");
    assert_eq!(renamed["path"], "src/new-name.rs");
    assert_eq!(
        resolve(json!({"kind":"file","path":"./src/lib.rs"}))["path"],
        "src/lib.rs"
    );
    for (path, message) in [
        ("src/old-name.rs", "does not change 'src/old-name.rs'"),
        ("src/untouched.rs", "does not change 'src/untouched.rs'"),
        (".\\src\\lib.rs", "backslash"),
    ] {
        let error = resolve_request(&json!({"anchor":{"kind":"file","path":path},"diff":diff()}))
            .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    let error = resolve_request(&json!({
        "anchor":{"kind":"line","path":"src/lib.rs","startLine":12},
        "diffFile":diff()["files"][1]
    }))
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot be resolved against the diff entry for 'src/new-name.rs'")
    );
}

#[test]
fn malformed_anchor_fields_report_the_specific_problem() {
    for (anchor, message) in [
        (json!({"kind":"range","path":"src/lib.rs"}), "kind 'range'"),
        (json!({"kind":"line","path":"src/lib.rs"}), "names no line"),
        (
            json!({"kind":"line","path":"src/lib.rs","startLine":12,"side":"middle"}),
            "side 'middle'",
        ),
        (json!({"kind":"file","path":""}), "names no file"),
    ] {
        let error = resolve_request(&json!({"anchor":anchor,"diff":diff()})).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
}
