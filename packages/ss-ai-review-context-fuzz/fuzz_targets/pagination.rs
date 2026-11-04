//! Exercises pagination and cursor validation on bounded generated evidence.
#![no_main]
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "The generated evidence and fixture shapes are bounded"
)]

use std::collections::HashSet;

use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};
use ss_ai_review_context::{normalize_options, query};

// The input cap and repetition exercise multiple pages without making each
// fuzz iteration too expensive. The page allowance is deliberately much larger
// than needed even if each generated character uses six JSON-escaped bytes.
const MAX_INPUT_BYTES: usize = 4096;
const MAX_PAGES: usize = 64;

fuzz_target!(|input: &[u8]| {
    if input.is_empty() || input.len() > MAX_INPUT_BYTES {
        return;
    }

    let fixture = include_str!("../../../tests/fixtures/ss-review-query-capture.json");
    let mut document: Value = serde_json::from_str(fixture).unwrap();
    // Escaping and non-BMP characters make the byte and UTF-16 limits differ.
    let text: String = input
        .iter()
        .map(|byte| match byte % 5 {
            0 => '🤖',
            1 => '"',
            2 => '\n',
            3 => 'é',
            _ => 'a',
        })
        .collect::<String>()
        .repeat(8);
    document["pullRequest"]["description"] = json!(text);
    let options = normalize_options(&json!({
        "query": "metadata", "field": "description", "files": [], "scope": "all",
        "kind": "", "resolution": "", "threadId": "", "commentId": "",
        "authorship": "", "indexOnly": false, "text": "", "pageSize": 1
    }))
    .unwrap();

    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
    let mut reconstructed = String::new();
    for _ in 0..MAX_PAGES {
        let output = query(&document, &options, None, cursor.as_deref()).unwrap();
        assert_eq!(
            output,
            query(&document, &options, None, cursor.as_deref()).unwrap()
        );
        assert!(output.len() + if cfg!(windows) { 2 } else { 1 } <= 16_384);
        let page: Value = serde_json::from_str(&output).unwrap();
        let items = page["items"].as_array().unwrap();
        assert!(!items.is_empty(), "a page must advance");
        for item in items {
            assert_eq!(
                usize::try_from(item["contentOffset"].as_u64().unwrap()).unwrap(),
                reconstructed.encode_utf16().count()
            );
            assert_eq!(
                usize::try_from(item["contentLength"].as_u64().unwrap()).unwrap(),
                text.encode_utf16().count()
            );
            reconstructed.push_str(item["content"].as_str().unwrap());
        }
        if page["complete"].as_bool().unwrap() {
            assert!(page["nextCursor"].is_null());
            assert_eq!(reconstructed, text);
            return;
        }
        let next = page["nextCursor"].as_str().unwrap().to_owned();
        assert!(seen.insert(next.clone()), "cursor must advance");
        let mut stale = document.clone();
        stale["pullRequest"]["title"] = json!("changed");
        query(&stale, &options, None, Some(&next)).unwrap_err();
        let mut different_options = options.clone();
        different_options["field"] = json!("title");
        query(&document, &different_options, None, Some(&next)).unwrap_err();
        query(&document, &options, None, Some("invalid")).unwrap_err();
        cursor = Some(next);
    }
    panic!("bounded evidence did not complete within the page limit");
});
