//! Verifies case-sensitive provider identifiers in selective change evidence.
#![allow(
    clippy::indexing_slicing,
    reason = "The fixture and assertions use fixed JSON shapes"
)]

use serde_json::{Value, json};
use ss_ai_review_context::query;

/// Returns only the bounded changes page so assertions inspect the public projection.
fn changes(current: &Value, previous: &Value) -> Vec<Value> {
    let response = query(current, &json!({"query":"changes"}), Some(previous), None).unwrap();
    serde_json::from_str::<Value>(&response).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn comment_identifiers_that_differ_only_in_case_remain_distinct() {
    let mut previous: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-expected.json"
    ))
    .unwrap();
    let mut second = previous["threads"][0]["comments"][0].clone();
    previous["threads"][0]["comments"][0]["id"] = json!("PRRC_kwDOAbc");
    previous["threads"][0]["openingCommentId"] = json!("PRRC_kwDOAbc");
    second["id"] = json!("prrc_kwdoabc");
    previous["threads"][0]["comments"] = json!([previous["threads"][0]["comments"][0], second]);
    assert!(changes(&previous, &previous).is_empty());

    let mut current = previous.clone();
    current["threads"][0]["comments"][0]["lastEditedUtc"] = json!("2026-08-05T10:00:00Z");
    current["threads"][0]["comments"][1]["lastEditedUtc"] = json!("2026-08-05T11:00:00Z");
    let edited = changes(&current, &previous);
    for id in ["PRRC_kwDOAbc", "prrc_kwdoabc"] {
        assert!(
            edited
                .iter()
                .any(|entry| entry["change"] == "commentsEdited" && entry["commentId"] == id)
        );
    }

    let mut without_second = previous.clone();
    without_second["threads"][0]["comments"] = json!([previous["threads"][0]["comments"][0]]);
    let added = changes(&previous, &without_second);
    assert!(added.iter().any(|entry| entry["change"] == "commentsAdded" && entry["commentId"] == "prrc_kwdoabc"));
    let removed = changes(&without_second, &previous);
    assert!(
        removed
            .iter()
            .any(|entry| entry["change"] == "commentsRemoved"
                && entry["commentId"] == "prrc_kwdoabc")
    );
}

#[test]
fn thread_identifiers_that_differ_only_in_case_remain_distinct() {
    let mut previous: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-expected.json"
    ))
    .unwrap();
    previous["threads"][0]["id"] = json!("PRT_kwDOAbc");
    let mut thread = previous["threads"][0].clone();
    thread["id"] = json!("prt_kwdoabc");
    thread["comments"][0]["id"] = json!("c2");
    thread["openingCommentId"] = json!("c2");
    let mut current = previous.clone();
    current["threads"].as_array_mut().unwrap().push(thread);
    let added = changes(&current, &previous);
    assert!(
        added
            .iter()
            .any(|entry| entry["change"] == "threadsAdded" && entry["threadId"] == "prt_kwdoabc")
    );
    assert!(
        added
            .iter()
            .any(|entry| entry["change"] == "commentsAdded" && entry["commentId"] == "c2")
    );
    assert!(
        !added
            .iter()
            .any(|entry| entry["change"] == "threadsRemoved")
    );
}
