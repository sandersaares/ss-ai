//! Checks publication anchors using recorded, in-memory diff evidence.
#![allow(
    clippy::indexing_slicing,
    reason = "The expected result and fixture shapes are fixed by these tests"
)]

use serde_json::{Value, json};
use ss_ai_review_workflow::anchors::resolve_request;

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
