//! Covers rendering contracts formerly exercised through the PowerShell helpers.

#![allow(
    clippy::indexing_slicing,
    reason = "The tests construct the JSON objects they inspect"
)]

use serde_json::{Value, json};
use ss_ai_review_workflow::markdown::render;

/// Supplies complete authored prose while allowing each test to vary one field.
fn finding() -> Value {
    json!({
        "id": "F1",
        "origin": "reviewer",
        "title": "Finding",
        "description": "Description",
        "reasoning": "Reason",
        "consequence": "Impact",
        "recommendedAction": "Remedy",
        "anchor": {"kind":"line","path":"src/file.rs","startLine":42}
    })
}

#[test]
fn retired_rendering_contract_single_line_reference_order_and_repeatability() {
    let mut record = finding();
    record["references"] = json!([
        {"text":"first"},
        {"text":"second","url":"https://example.invalid/reference"}
    ]);
    record["impactedLocations"] = json!([{"path":"src/file.rs","startLine":42,"endLine":42}]);
    for destination in ["pullRequest", "tracker"] {
        let request = json!({"operation":"finding","destination":destination,"finding":record});
        let first = render(&request).unwrap();
        assert_eq!(first, render(&request).unwrap());
        let text = first.as_str().unwrap();
        assert!(text.contains("- `src/file.rs:42`"));
        assert!(
            text.find("- first").unwrap()
                < text
                    .find("- [second](https://example.invalid/reference)")
                    .unwrap()
        );
    }
}

#[test]
fn retired_rendering_contract_documents_preserve_single_and_multiple_bodies() {
    let first = finding();
    let mut second = finding();
    second["id"] = json!("F2");
    second["title"] = json!("Second");
    for destination in ["pullRequest", "tracker"] {
        let document = |records: &[Value]| {
            render(&json!({"operation":"document","destination":destination,"findings":records}))
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(document(&[]), "");
        let one = document(std::slice::from_ref(&first));
        let first_body =
            render(&json!({"operation":"finding","destination":destination,"finding":first}))
                .unwrap();
        assert_eq!(one, first_body.as_str().unwrap());
        let two = document(&[first.clone(), second.clone()]);
        assert!(two.starts_with(&one));
        assert!(two.contains("\n\n---\n\n"));
        // Tracker documents contain standalone items, each with its own authorship marker.
        let expected_markers = if destination == "tracker" { 2 } else { 1 };
        assert_eq!(two.matches("[Copilot speaking]").count(), expected_markers);
        assert!(two.contains("Second"));
    }
}

#[test]
fn retired_rendering_contract_omits_each_empty_partial_prose_field() {
    for field in [
        "description",
        "reasoning",
        "consequence",
        "recommendedAction",
    ] {
        let mut absent = finding();
        absent.as_object_mut().unwrap().remove(field);
        let mut empty = absent.clone();
        empty[field] = json!("   ");
        for destination in ["pullRequest", "tracker"] {
            let request = |record: &Value| {
                render(&json!({"operation":"findingRender","destination":destination,"finding":record}))
                    .unwrap()
            };
            assert_eq!(request(&absent), request(&empty), "{destination}/{field}");
        }
    }
}

#[test]
fn retired_rendering_contract_single_item_list_is_a_separate_block() {
    for (field, label) in [
        ("description", None),
        ("reasoning", Some("Reproducible reasoning")),
        ("consequence", Some("Consequence")),
        ("recommendedAction", Some("Recommended action")),
    ] {
        let mut record = finding();
        record[field] = json!("- Only item");
        for destination in ["pullRequest", "tracker"] {
            let result = render(
                &json!({"operation":"findingRender","destination":destination,"finding":record}),
            )
            .unwrap();
            let block = label.map_or_else(
                || "- Only item".to_owned(),
                |name| format!("**{name}**:\n\n- Only item"),
            );
            assert!(
                result["blocks"].as_array().unwrap().contains(&json!(block)),
                "{destination}/{field}"
            );
        }
    }
}

#[test]
fn retired_rendering_contract_clean_summary_has_authorship_without_zero_publications() {
    for platform in ["gitHub", "azureDevOps"] {
        let output =
            render(&json!({"operation":"summary","platform":platform,"findings":[]})).unwrap();
        let summary = output.as_str().unwrap();
        assert!(summary.starts_with("[Copilot speaking]\n"));
        assert!(summary.contains("This review raises no findings."));
        assert!(!summary.contains("Published 0"));
    }
}
