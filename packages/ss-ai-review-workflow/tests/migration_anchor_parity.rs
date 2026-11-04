//! Checks publication anchors using recorded, in-memory diff evidence.
#![allow(
    clippy::indexing_slicing,
    reason = "The expected result and fixture shapes are fixed by these tests"
)]

use serde_json::{Value, json};
use ss_ai_git::{FileDiff, parse};
use ss_ai_review_workflow::anchors::resolve_request;
use ss_ai_runtime::classification;

/// Represents separated hunks on a renamed file with different side positions.
fn diff() -> Value {
    json!({"files":[{
        "path":"2026-01-01T00:00:00Z",
        "previousPath":"old-name",
        "status":"renamed",
        "hunks":[
            {"oldStart":3,"oldCount":3,"newStart":10,"newCount":3},
            {"oldStart":20,"oldCount":2,"newStart":30,"newCount":2}
        ]
    }]})
}

fn resolve(anchor: &Value) -> Value {
    resolve_request(&json!({"anchor":anchor,"diff":diff()})).unwrap()
}

#[test]
fn line_positions_use_the_selected_side_and_preserve_timestamp_paths() {
    let path = "2026-01-01T00:00:00Z";
    assert_eq!(
        resolve(&json!({"kind":"line","path":format!("./{path}"),"startLine":11})),
        json!({"kind":"line","path":path,"startLine":11,"endLine":11,
            "side":"right","wasDowngraded":false})
    );
    assert_eq!(
        resolve(&json!({"kind":"line","path":path,"startLine":3,"endLine":5,"side":"left"})),
        json!({"kind":"line","path":path,"startLine":3,"endLine":5,
            "side":"left","wasDowngraded":false})
    );
    assert!(
        resolve(&json!({"kind":"line","path":path,"startLine":3}))["wasDowngraded"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn ranges_crossing_hunks_downgrade_and_changed_file_lookup_is_exact() {
    let path = "2026-01-01T00:00:00Z";
    let result = resolve(&json!({"kind":"line","path":path,"startLine":12,"endLine":30}));
    assert_eq!(result["kind"], "file");
    assert_eq!(result["path"], path);
    assert_eq!(result["wasDowngraded"], true);
    assert!(
        result["downgradeReason"]
            .as_str()
            .unwrap()
            .contains("right")
    );
    for path in ["old-name", "unmodified", "folder\\file"] {
        resolve_request(&json!({
            "anchor":{"kind":"file","path":path},
            "diff":diff()
        }))
        .unwrap_err();
    }
}

#[test]
fn batch_keeps_each_result_and_rejects_malformed_unreferenced_diff() {
    let request = json!({
        "anchors":[
            {"kind":"file","path":"2026-01-01T00:00:00Z"},
            {"kind":"line","path":"2026-01-01T00:00:00Z","startLine":100},
            {"kind":"file","path":"unmodified"},
            null
        ],
        "diff":diff()
    });
    let result = resolve_request(&request).unwrap();
    assert_eq!(result["results"].as_array().unwrap().len(), 4);
    assert_eq!(result["results"][0]["anchor"]["wasDowngraded"], false);
    assert_eq!(result["results"][1]["anchor"]["wasDowngraded"], true);
    assert!(
        result["results"][2]["error"]
            .as_str()
            .unwrap()
            .contains("unmodified")
    );
    assert!(result["results"][3]["argumentError"].is_string());

    let mut malformed = request;
    malformed["diff"]["files"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "path":"unreferenced",
            "previousPath":null,
            "status":"modified",
            "hunks":[{"oldStart":0,"oldCount":1,"newStart":1,"newCount":1}]
        }));
    resolve_request(&malformed).unwrap_err();
}

#[test]
fn deserialization_enforces_file_and_hunk_invariants() {
    let file = json!({
        "path": "file",
        "previousPath": null,
        "status": "modified",
        "hunks": [{"oldStart": 0, "oldCount": 0, "newStart": u32::MAX, "newCount": 1}]
    });
    let parsed: FileDiff = serde_json::from_value(file.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), file);
    assert!(parsed.contains_range(u32::MAX, u32::MAX, false));
    assert!(!parsed.contains_range(1, 1, true));

    for (field, value, message) in [
        ("path", json!(""), "Diff file has no path"),
        ("oldStart", json!(0), "Invalid Git diff line range"),
        ("newStart", json!(0), "Invalid Git diff line range"),
        ("oldStart", json!(u32::MAX), "Invalid Git diff line range"),
        ("newStart", json!(u32::MAX), "Invalid Git diff line range"),
    ] {
        let mut malformed = file.clone();
        if field == "path" {
            malformed[field] = value;
        } else {
            malformed["hunks"][0][field] = value;
            malformed["hunks"][0][if field == "oldStart" {
                "oldCount"
            } else {
                "newCount"
            }] = if malformed["hunks"][0][field] == json!(0) {
                json!(1)
            } else {
                json!(2)
            };
        }
        let error = serde_json::from_value::<FileDiff>(malformed.clone()).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
        let error = resolve_request(&json!({
            "anchor": {"kind": "file", "path": "file"},
            "diffFile": malformed
        }))
        .unwrap_err();
        assert_eq!(classification(&error), "invalidInput");
        assert!(error.to_string().contains(message), "{error}");
    }

    let mut unknown = file;
    unknown["hunks"][0]["unexpected"] = json!(true);
    assert!(
        serde_json::from_value::<FileDiff>(unknown)
            .unwrap_err()
            .to_string()
            .contains("unknown field")
    );
}

#[test]
fn full_diff_validation_and_path_matching_remain_unchanged() {
    let file = json!({"path":"Case/File","previousPath":null,"status":"modified","hunks":[]});
    let anchor = json!({"kind":"file","path":"case/file"});
    let result = resolve_request(&json!({"anchor":anchor,"diffFile":file})).unwrap();
    assert_eq!(result["path"], "Case/File");
    let error = resolve_request(&json!({"anchor":anchor,"diff":{"files":[file]}})).unwrap_err();
    assert_eq!(classification(&error), "invalidInput");
    assert!(error.to_string().contains("does not change"));

    let duplicate = json!({"files":[file, file]});
    for request in [
        json!({"anchor":{"path":"Case/File"},"diff":duplicate}),
        json!({"anchors":[],"diff":duplicate}),
    ] {
        let error = resolve_request(&request).unwrap_err();
        assert_eq!(classification(&error), "invalidInput");
        assert!(error.to_string().contains("duplicate file paths"));
    }

    let bad = json!({"path":"","previousPath":null,"status":"modified","hunks":[]});
    for request in [
        json!({"anchor":{"path":"Case/File"},"diff":{"files":[file,bad]}}),
        json!({"anchors":[],"diff":{"files":[file,bad]}}),
    ] {
        let error = resolve_request(&request).unwrap_err();
        assert_eq!(classification(&error), "invalidInput");
        assert!(error.to_string().contains("Diff file has no path"));
    }

    let parsed = parse("diff --git a/Case/File b/Case/File\n").unwrap();
    let result = ss_ai_review_workflow::anchors::resolve_anchor(&anchor, &parsed[0]).unwrap();
    assert_eq!(result["path"], "Case/File");
}
