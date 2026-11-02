//! Exercises selective projections and freshness pages without persistent files.
#![allow(
    clippy::indexing_slicing,
    reason = "The tests construct fixed JSON fixtures and inspect their known fields"
)]

use serde_json::{Value, json};
use ss_ai_review_context::{normalize_options, query};

/// Provides independent input for each projection.
fn capture() -> Value {
    serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-query-capture.json"
    ))
    .unwrap()
}

/// Decodes the bounded query result for assertions.
fn page(
    document: &Value,
    options: &Value,
    previous: Option<&Value>,
    cursor: Option<&str>,
) -> Value {
    let output = query(document, options, previous, cursor).unwrap();
    assert!(
        output
            .len()
            .checked_add(if cfg!(windows) { 2 } else { 1 })
            .unwrap()
            <= 16 * 1024
    );
    serde_json::from_str(&output).unwrap()
}

#[test]
fn repository_path_selection_preserves_identifier_case_and_foreign_filename_syntax() {
    let mut document = capture();
    document["threads"][0]["context"]["filePath"] = json!("/src/time:stamp.rs");
    let mut second = document["threads"][0].clone();
    second["id"] = json!("t");
    second["context"]["filePath"] = json!("src/other.rs");
    document["threads"].as_array_mut().unwrap().push(second);

    let selected = page(
        &document,
        &json!({"query":"threads","files":["src\\time:stamp.rs","src\\other.rs"]}),
        None,
        None,
    );
    assert_eq!(selected["items"][0]["threadId"], "T");
    assert_eq!(selected["items"][0]["resolution"], "resolved");
    assert_eq!(selected["items"][0]["isOutdated"], true);
    assert_eq!(selected["items"][1]["threadId"], "t");
    let different_case = page(
        &document,
        &json!({"query":"threads","files":["src/Time:stamp.rs"]}),
        None,
        None,
    );
    assert_eq!(different_case["totalCount"], 0);
    assert_eq!(different_case["complete"], true);
    let other = page(
        &document,
        &json!({"query":"threads","files":["src/other.rs"]}),
        None,
        None,
    );
    assert_eq!(other["items"].as_array().unwrap().len(), 1);
    assert_eq!(other["items"][0]["threadId"], "t");
}

#[test]
fn metadata_and_thread_discovery_expose_only_selected_evidence() {
    let mut document = capture();
    let selected = page(
        &document,
        &json!({"query":"metadata","field":"description"}),
        None,
        None,
    );
    assert_eq!(selected["items"].as_array().unwrap().len(), 1);
    let item = &selected["items"][0];
    assert_eq!(item["field"], "description");
    assert_eq!(item["content"], document["pullRequest"]["description"]);
    assert_eq!(item["contentComplete"], true);
    assert!(item.get("threads").is_none());
    assert!(item.get("reviewers").is_none());

    let mut reply = document["threads"][0]["comments"][0].clone();
    reply["id"] = json!("Reply");
    reply["content"] = json!("Structural opening comment");
    document["threads"][0]["openingCommentId"] = json!("Reply");
    document["threads"][0]
        .get_mut("comments")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(reply);
    let selected = page(
        &document,
        &json!({"query":"threads","files":["src/widget.rs"]}),
        None,
        None,
    );
    let item = &selected["items"][0];
    assert_eq!(item["openingCommentId"], "Reply");
    assert_eq!(item["excerpt"], "Structural opening comment");
    assert_eq!(item["commentCount"], 2);
    assert!(item.get("properties").is_none());
}

#[test]
fn metadata_description_retains_empty_null_and_timestamp_values() {
    let mut document = capture();
    for text in ["", "2026-10-01T12:00:00Z"] {
        document["pullRequest"]["description"] = json!(text);
        let result = page(
            &document,
            &json!({"query":"metadata","field":"description"}),
            None,
            None,
        );
        assert_eq!(result["items"][0]["content"].as_str(), Some(text));
    }
    document["pullRequest"]["description"] = Value::Null;
    let result = page(
        &document,
        &json!({"query":"metadata","field":"description"}),
        None,
        None,
    );
    assert!(result["items"][0].get("value").unwrap().is_null());
}

#[test]
fn direct_cursor_rejects_changed_thread_authorship_and_invalid_positions() {
    let mut document = capture();
    let mut reply = document["threads"][0]["comments"][0].clone();
    reply["id"] = json!("Reply");
    document["threads"][0]["comments"]
        .as_array_mut()
        .unwrap()
        .push(reply);
    let mut second = document["threads"][0].clone();
    second["id"] = json!("t");
    document["threads"].as_array_mut().unwrap().push(second);

    let options = json!({"query":"comments","threadId":"T","pageSize":1});
    let result = page(&document, &options, None, None);
    let cursor = result["nextCursor"].as_str().unwrap();
    let error = query(
        &document,
        &json!({"query":"comments","threadId":"t","pageSize":1}),
        None,
        Some(cursor),
    )
    .unwrap_err();
    assert!(error.to_string().contains("mismatched"));
    let fingerprint = cursor.split(':').next().unwrap();
    for invalid in [
        "broken".to_owned(),
        format!("{fingerprint}:999999999999999:0"),
        format!("{fingerprint}:99:0"),
    ] {
        assert!(
            query(&document, &options, None, Some(&invalid))
                .unwrap_err()
                .to_string()
                .to_lowercase()
                .contains("cursor")
        );
    }
    document["threads"][0]["comments"][0]["authorship"] = json!("human");
    let error = query(&document, &options, None, Some(cursor)).unwrap_err();
    assert!(error.to_string().contains("Stale"));
}

#[test]
fn selectors_reject_ambiguous_and_unintended_bulk_queries() {
    for options in [
        json!({"query":"all"}),
        json!({"query":"metadata"}),
        json!({"query":"metadata","field":"threads"}),
        json!({"query":"comments"}),
        json!({"query":"comments","commentId":"C"}),
        json!({"query":"search","text":" "}),
        json!({"query":"threads","text":"ignored"}),
        json!({"query":"threads","scope":"files"}),
        json!({"query":"threads","files":["a"],"scope":"global"}),
        json!({"query":"threads","files":["../a"]}),
        json!({"query":"threads","files":[42]}),
        json!({"query":"metadata","field":"title","kind":"text"}),
        json!({"query":"threads","indexOnly":true}),
        json!({"query":"threads","pageSize":51}),
    ] {
        normalize_options(&options).unwrap_err();
    }
}

#[test]
fn comments_preserve_order_attribution_and_case_sensitive_identifiers() {
    let mut document = capture();
    let mut reply = document["threads"][0]["comments"][0].clone();
    reply["id"] = json!("Reply");
    reply["authorship"] = json!("human");
    reply["isAuthoredBySelf"] = json!(false);
    document["threads"][0]["isAuthoredBySelf"] = json!(true);
    document["threads"][0]["comments"]
        .as_array_mut()
        .unwrap()
        .push(reply);
    let mut other = document["threads"][0].clone();
    other["id"] = json!("t");
    other["comments"][0]["id"] = json!("c");
    document["threads"].as_array_mut().unwrap().push(other);

    let result = page(
        &document,
        &json!({"query":"comments","threadId":"T"}),
        None,
        None,
    );
    assert_eq!(result["items"][0]["commentId"], "C");
    let reply = &result["items"][1];
    assert_eq!(reply["commentId"], "Reply");
    assert_eq!(reply["authorship"], "human");
    assert_eq!(reply["openingCommentId"], "C");
    assert_eq!(reply["context"]["filePath"], "src/widget.rs");
    assert_eq!(reply["isAuthoredBySelf"], false);
    assert_eq!(reply["threadIsAuthoredBySelf"], true);
    let exact = page(
        &document,
        &json!({"query":"comments","threadId":"t","commentId":"c"}),
        None,
        None,
    );
    assert_eq!(exact["items"].as_array().unwrap().len(), 1);
    assert_eq!(exact["items"][0]["commentId"], "c");
    for (options, diagnostic) in [
        (
            json!({"query":"comments","threadId":"t","commentId":"C"}),
            "no comment",
        ),
        (
            json!({"query":"comments","threadId":"missing"}),
            "no thread",
        ),
    ] {
        assert!(
            query(&document, &options, None, None)
                .unwrap_err()
                .to_string()
                .contains(diagnostic)
        );
    }
}

#[test]
fn comparison_failures_explain_missing_unused_and_unrelated_baselines() {
    let document = capture();
    let changes = json!({"query":"changes"});
    let missing = query(&document, &changes, None, None).unwrap_err();
    assert!(missing.to_string().contains("ComparePath"));
    let unused = query(
        &document,
        &json!({"query":"threads"}),
        Some(&document),
        None,
    )
    .unwrap_err();
    assert!(unused.to_string().contains("only valid"));
    let mut unrelated = document.clone();
    unrelated["pullRequestUrl"] = json!("https://example.invalid/pr/2");
    let mismatch = query(&document, &changes, Some(&unrelated), None).unwrap_err();
    assert!(mismatch.to_string().contains("different pull requests"));
}

#[test]
fn indeterminate_comment_index_pages_are_body_free_and_bound_to_selection() {
    let mut document = capture();
    let body = "private body 🤖".repeat(3000);
    document["threads"][0]["comments"][0]["content"] = json!(body);
    for (thread_id, comment_id, kind) in [
        ("summary", "summary-body", "reviewSummary"),
        ("global", "global-body", "text"),
    ] {
        let mut thread = document["threads"][0].clone();
        thread["id"] = json!(thread_id);
        thread["kind"] = json!(kind);
        thread["context"] = Value::Null;
        thread["openingCommentId"] = json!(comment_id);
        thread["comments"][0]["id"] = json!(comment_id);
        document["threads"].as_array_mut().unwrap().push(thread);
    }
    let original = document.clone();
    let options =
        json!({"query":"comments","authorship":"indeterminate","indexOnly":true,"pageSize":1});
    let mut cursor = None;
    for expected in ["C", "summary-body", "global-body"] {
        let result = page(&document, &options, None, cursor.as_deref());
        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        let item = &result["items"][0];
        assert_eq!(item["commentId"], expected);
        assert!(item.get("content").is_none());
        assert!(item.get("excerpt").is_none());
        assert_eq!(item["contentLength"], body.encode_utf16().count());
        if expected == "C" {
            let full = json!({"query":"comments","authorship":"indeterminate","pageSize":1});
            query(&document, &full, None, result["nextCursor"].as_str()).unwrap_err();
        }
        cursor = result["nextCursor"].as_str().map(str::to_owned);
    }
    assert!(cursor.is_none());
    assert_eq!(document, original);
}

#[test]
fn indeterminate_comment_pages_expose_all_matching_bodies() {
    let mut document = capture();
    document["threads"][0]["comments"][0]["content"] = json!("Widget concern");
    for (thread_id, comment_id, content) in [
        ("summary", "summary-body", "Global design decision"),
        ("global", "global-body", "PR-wide author clarification"),
    ] {
        let mut thread = document["threads"][0].clone();
        thread["id"] = json!(thread_id);
        thread["context"] = Value::Null;
        thread["comments"][0]["id"] = json!(comment_id);
        thread["comments"][0]["content"] = json!(content);
        document["threads"].as_array_mut().unwrap().push(thread);
    }
    let before = document.clone();
    let options = json!({"query":"comments","authorship":"indeterminate","pageSize":1});
    let mut cursor = None;
    for (id, body) in [
        ("C", "Widget concern"),
        ("summary-body", "Global design decision"),
        ("global-body", "PR-wide author clarification"),
    ] {
        let result = page(&document, &options, None, cursor.as_deref());
        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(result["items"][0]["commentId"], id);
        assert_eq!(result["items"][0]["content"], body);
        cursor = result["nextCursor"].as_str().map(str::to_owned);
    }
    assert!(cursor.is_none());
    assert_eq!(document, before);
}

#[test]
fn descriptor_pages_respect_byte_budget_before_the_item_limit() {
    let mut document = capture();
    let mut threads = Vec::new();
    for id in 0..50 {
        let mut thread = document["threads"][0].clone();
        thread["id"] = json!(id.to_string());
        thread["comments"][0]["content"] = json!("x".repeat(1000));
        threads.push(thread);
    }
    document["threads"] = json!(threads);
    let options = json!({"query":"threads","pageSize":50});
    let mut cursor = None;
    let mut ids = Vec::new();
    // At most one page per descriptor is necessary; the bound detects stalled cursors.
    for _ in 0..50 {
        let result = page(&document, &options, None, cursor.as_deref());
        ids.extend(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["threadId"].as_str().unwrap().to_owned()),
        );
        if result["complete"] == true {
            assert_ne!(cursor, None);
            assert_eq!(ids, (0..50).map(|id| id.to_string()).collect::<Vec<_>>());
            return;
        }
        let next = result["nextCursor"].as_str().unwrap().to_owned();
        assert_ne!(cursor.as_deref(), Some(next.as_str()));
        cursor = Some(next);
    }
    panic!("descriptor query did not finish within the fixture bound");
}

#[test]
fn freshness_pages_cover_each_change_category_and_capture_source() {
    let mut previous = capture();
    let mut removed = previous["threads"][0].clone();
    removed["id"] = json!("removed");
    removed["comments"][0]["id"] = json!("removed-comment");
    previous["threads"].as_array_mut().unwrap().push(removed);
    let mut current = previous.clone();
    current["threads"].as_array_mut().unwrap().pop();
    current["threads"][0]["comments"][0]["content"] = json!("edited");
    current["threads"][0]["resolution"] = json!("active");
    current["pullRequest"]["reviewedSourceCommit"] = json!("new-source");
    let mut added = current["threads"][0].clone();
    added["id"] = json!("added");
    added["comments"][0]["id"] = json!("added-comment");
    current["threads"].as_array_mut().unwrap().push(added);

    let options = json!({"query":"changes","pageSize":1});
    let mut cursor = None;
    let mut items = Vec::new();
    // The fixture has fewer categories than this bound, so a stalled cursor fails promptly.
    for _ in 0..16 {
        let result = page(&current, &options, Some(&previous), cursor.as_deref());
        items.extend(result["items"].as_array().unwrap().iter().cloned());
        if result["complete"] == true {
            break;
        }
        let next = result["nextCursor"].as_str().unwrap().to_owned();
        assert_ne!(cursor.as_deref(), Some(next.as_str()));
        cursor = Some(next);
    }
    let categories: Vec<_> = items
        .iter()
        .map(|item| item["change"].as_str().unwrap())
        .collect();
    assert_eq!(
        categories,
        [
            "threadsAdded",
            "threadsRemoved",
            "commentsAdded",
            "commentsRemoved",
            "commentsEdited",
            "resolutionChanged",
            "reviewedSourceCommit",
            "reviewEvidence"
        ]
    );
    assert_eq!(items[1]["capture"], "baseline");
    assert_eq!(items[3]["commentId"], "removed-comment");
    assert_eq!(items[4]["capture"], "both");
    assert_eq!(items[6]["from"], "source");
    assert_eq!(items[6]["to"], "new-source");
}
