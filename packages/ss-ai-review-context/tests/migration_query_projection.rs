//! Exercises bounded query projections and cursor identity without file I/O.
#![allow(
    clippy::indexing_slicing,
    reason = "The tests construct fixed JSON fixtures and inspect their known fields"
)]

use serde_json::{Value, json};
use ss_ai_review_context::query;

/// Reuses a canonical capture while leaving every test's data independent.
fn capture() -> Value {
    serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-query-capture.json"
    ))
    .unwrap()
}

fn page(
    document: &Value,
    options: &Value,
    previous: Option<&Value>,
    cursor: Option<&str>,
) -> Value {
    serde_json::from_str(&query(document, options, previous, cursor).unwrap()).unwrap()
}

#[test]
fn metadata_collections_are_paged_and_scalar_types_are_preserved() {
    let mut document = capture();
    document["reviewers"] = json!([{"id":"one"}, {"id":"two"}]);
    document["revisions"] = json!([{"id":"one"}, {"id":"two"}]);
    document["pullRequest"]["title"] = json!("");
    document["pullRequest"]["description"] = Value::Null;
    document["pullRequest"]["createdUtc"] = json!("2026-01-01T00:00:00Z");
    for field in ["reviewers", "revisions"] {
        let options = json!({"query":"metadata","field":field,"pageSize":1});
        let first = page(&document, &options, None, None);
        assert_eq!(
            first["items"],
            json!([{"field":field,"value":{"id":"one"}}])
        );
        let next = first["nextCursor"].as_str().unwrap();
        let second = page(&document, &options, None, Some(next));
        assert_eq!(
            second["items"],
            json!([{"field":field,"value":{"id":"two"}}])
        );
        assert_eq!(second["complete"], true);
    }
    assert_eq!(
        page(
            &document,
            &json!({"query":"metadata","field":"title"}),
            None,
            None
        )["items"][0]["content"],
        ""
    );
    assert!(
        page(
            &document,
            &json!({"query":"metadata","field":"description"}),
            None,
            None
        )["items"][0]["value"]
            .is_null()
    );
    assert_eq!(
        page(
            &document,
            &json!({"query":"metadata","field":"createdUtc"}),
            None,
            None
        )["items"][0]["content"],
        "2026-01-01T00:00:00Z"
    );
}

#[test]
fn discovery_uses_structural_opener_and_index_omits_body() {
    let mut document = capture();
    let mut reply = document["threads"][0]["comments"][0].clone();
    reply["id"] = json!("reply");
    reply["content"] = json!("a reply preceding the opener");
    document["threads"][0]["comments"]
        .as_array_mut()
        .unwrap()
        .insert(0, reply);
    let original = document.clone();
    let result = page(
        &document,
        &json!({"query":"threads","files":["/src\\widget.rs"]}),
        None,
        None,
    );
    assert_eq!(result["items"][0]["openingCommentId"], "C");
    assert_eq!(
        result["items"][0]["excerpt"],
        "Quoted \"widget\"\n🤖 evidence"
    );
    let index = page(
        &document,
        &json!({"query":"comments","threadId":"T","indexOnly":true}),
        None,
        None,
    );
    let items = index["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["commentId"], "reply");
    assert_eq!(items[1]["commentId"], "C");
    assert!(items.iter().all(|item| item.get("content").is_none()));
    assert_eq!(document, original);
}

#[test]
fn cursors_reject_changed_capture_selectors_and_invalid_positions() {
    let mut document = capture();
    document["pullRequest"]["description"] = json!("🤖".repeat(9000));
    let options = json!({"query":"metadata","field":"description"});
    let first = page(&document, &options, None, None);
    let cursor = first["nextCursor"].as_str().unwrap();
    let fingerprint = cursor.split(':').next().unwrap();
    for bad in [
        "bad".to_owned(),
        format!("{fingerprint}:0:1"),
        format!("{fingerprint}:99:0"),
        format!("{fingerprint}:0:999999999999999999999"),
        format!("{fingerprint}:0:0:extra"),
    ] {
        query(&document, &options, None, Some(&bad)).unwrap_err();
    }
    query(
        &document,
        &json!({"query":"metadata","field":"title"}),
        None,
        Some(cursor),
    )
    .unwrap_err();
    document["pullRequest"]["title"] = json!("changed");
    query(&document, &options, None, Some(cursor)).unwrap_err();
}

#[test]
fn changes_cursor_binds_both_captures() {
    let baseline = capture();
    let mut current = baseline.clone();
    current["pullRequest"]["reviewedSourceCommit"] = json!("new source");
    current["pullRequest"]["reviewedMergeBaseCommit"] = json!("new base");
    let options = json!({"query":"changes","pageSize":1});
    let first = page(&current, &options, Some(&baseline), None);
    let cursor = first["nextCursor"].as_str().unwrap();
    let second = page(&current, &options, Some(&baseline), Some(cursor));
    assert_ne!(first["items"], second["items"]);
    let mut changed_baseline = baseline.clone();
    changed_baseline["pullRequest"]["title"] = json!("different baseline");
    assert!(
        query(&current, &options, Some(&changed_baseline), Some(cursor))
            .unwrap_err()
            .to_string()
            .contains("Stale")
    );
    let mut unrelated = baseline;
    unrelated["pullRequestUrl"] = json!("https://example.invalid/another");
    query(&current, &options, Some(&unrelated), None).unwrap_err();
    query(&current, &options, None, None).unwrap_err();
}

#[test]
fn oversized_attribution_is_rejected_without_dropping_evidence() {
    let mut document = capture();
    document["threads"][0]["comments"][0]["author"]["displayName"] = json!("x".repeat(20000));
    let options = json!({"query":"comments","threadId":"T","commentId":"C"});
    let error = query(&document, &options, None, None).unwrap_err();
    assert!(error.to_string().contains("cannot fit"));
}

#[test]
fn global_discovery_and_search_apply_file_and_resolution_filters() {
    let mut document = capture();
    document["pullRequest"]["description"] = json!("Marker in description");
    document["threads"][0]["comments"][0]["content"] =
        json!(format!("{} Marker in full comment", "prefix ".repeat(100)));
    let mut global = document["threads"][0].clone();
    global["id"] = json!("summary");
    global["kind"] = json!("reviewSummary");
    global["context"] = Value::Null;
    global["resolution"] = json!("indeterminate");
    global["comments"][0]["id"] = json!("summary-comment");
    global["openingCommentId"] = json!("summary-comment");
    let mut plain_global = global.clone();
    plain_global["id"] = json!("global");
    plain_global["kind"] = json!("text");
    document["threads"].as_array_mut().unwrap().push(global);
    document["threads"]
        .as_array_mut()
        .unwrap()
        .push(plain_global);

    let global = page(
        &document,
        &json!({"query":"threads","scope":"global"}),
        None,
        None,
    );
    assert_eq!(global["totalCount"], 2);
    assert_eq!(global["items"][0]["threadId"], "summary");
    assert_eq!(global["items"][1]["threadId"], "global");
    let summary = page(
        &document,
        &json!({"query":"threads","scope":"global","kind":"reviewSummary","resolution":"indeterminate"}),
        None,
        None,
    );
    assert_eq!(summary["totalCount"], 1);
    assert_eq!(summary["items"][0]["threadId"], "summary");
    let all = page(
        &document,
        &json!({"query":"search","text":"marker"}),
        None,
        None,
    );
    assert_eq!(all["totalCount"], 4);
    assert_eq!(all["items"][0]["field"], "description");
    assert!(all["items"][1]["matchOffset"].as_u64().unwrap() > 240);
    assert!(
        all["items"][1]["excerpt"]
            .as_str()
            .unwrap()
            .contains("Marker")
    );
    assert!(
        all["items"][1]["excerpt"]
            .as_str()
            .unwrap()
            .encode_utf16()
            .count()
            <= 240
    );
    assert!(all["items"][1].get("content").is_none());
    assert_eq!(
        page(
            &document,
            &json!({"query":"search","text":".*"}),
            None,
            None
        )["totalCount"],
        0
    );
    let filtered = page(
        &document,
        &json!({"query":"search","text":"Marker","files":["src/widget.rs"],"resolution":"resolved"}),
        None,
        None,
    );
    assert_eq!(filtered["totalCount"], 1);
    assert_eq!(filtered["items"][0]["threadId"], "T");
    assert!(filtered["items"][0]["matchOffset"].as_u64().unwrap() > 240);
    assert!(
        filtered["items"][0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("Marker")
    );
}

#[test]
fn fragmented_comment_reconstructs_unicode_without_exceeding_output_budget() {
    let mut document = capture();
    let body = concat!("🤖 Ω \"quoted\" '", "\\", "\n").repeat(3000);
    document["threads"][0]["comments"][0]["content"] = json!(body);
    let options = json!({"query":"comments","threadId":"T","commentId":"C"});
    let mut cursor = None;
    let mut reconstructed = String::new();
    let mut pages = 0_usize;
    // A fixed page bound catches a lost-progress mutation without waiting.
    for _ in 0..100 {
        let output = query(&document, &options, None, cursor.as_deref()).unwrap();
        assert!(output.len() + if cfg!(windows) { 2 } else { 1 } <= 16 * 1024);
        let result: Value = serde_json::from_str(&output).unwrap();
        let fragment = &result["items"][0];
        pages = pages.checked_add(1).unwrap();
        assert_eq!(
            fragment["contentOffset"].as_u64().unwrap(),
            u64::try_from(reconstructed.encode_utf16().count()).unwrap()
        );
        assert_eq!(
            fragment["contentLength"].as_u64().unwrap(),
            u64::try_from(body.encode_utf16().count()).unwrap()
        );
        let content = fragment["content"].as_str().unwrap();
        let units: Vec<_> = content.encode_utf16().collect();
        assert!(
            !units
                .first()
                .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
        );
        assert!(
            !units
                .last()
                .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
        );
        assert!(!content.contains('\u{fffd}'));
        reconstructed.push_str(content);
        if result["complete"] == true {
            assert!(pages > 1);
            assert_eq!(fragment["contentComplete"], true);
            assert_eq!(reconstructed, body);
            return;
        }
        let next = result["nextCursor"].as_str().unwrap().to_owned();
        assert_ne!(cursor.as_deref(), Some(next.as_str()));
        cursor = Some(next);
    }
    panic!("query did not finish within the bounded fixture");
}

#[test]
fn rejects_unscoped_comments_and_incompatible_comparison_arguments() {
    let document = capture();
    for options in [
        json!({"query":"comments"}),
        json!({"query":"comments","commentId":"C"}),
        json!({"query":"threads","scope":"files"}),
        json!({"query":"threads","files":["src/../file"]}),
        json!({"query":"search","text":"  "}),
    ] {
        query(&document, &options, None, None).unwrap_err();
    }
    query(
        &document,
        &json!({"query":"threads"}),
        Some(&document),
        None,
    )
    .unwrap_err();
}
