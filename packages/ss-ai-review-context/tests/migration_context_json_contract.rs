//! Exercises machine-readable capture values without PowerShell adapter objects.
#![allow(
    clippy::indexing_slicing,
    reason = "The test walks a fixed nested fixture"
)]

use serde_json::{Value, json};
use ss_ai_review_context::{canonical_capture_from_evidence, powershell_json};

#[test]
fn nested_content_and_timestamp_strings_survive_canonical_serialization() {
    let mut context: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-context.json"
    ))
    .unwrap();
    let body = "2026-08-04T10:33:54.677Z";
    context["threads"][0]["comments"][0]["content"] = json!(body);
    let mut nested = json!("café 🤖");
    for _ in 0..20 {
        nested = json!({"value": nested});
    }
    context["threads"][0]["properties"]["nested"] = nested;
    let document = canonical_capture_from_evidence(&context).unwrap();
    let text = powershell_json(&document).unwrap();
    let restored: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(restored["threads"][0]["comments"][0]["content"], body);
    let mut value = &restored["threads"][0]["properties"]["nested"];
    for _ in 0..20 {
        value = &value["value"];
    }
    assert_eq!(value, "café 🤖");
}
