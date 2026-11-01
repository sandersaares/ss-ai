//! Exercises destination rendering and self-review classification.

#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "Test fixtures and expected result checks favor direct JSON access and assertions"
)]

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::markdown::{render, render_files_json, render_json};
use ss_ai_runtime::{Clock, fail};

/// Avoids real waits while exercising atomic file publication.
struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        "2024-05-06T07:08:09Z".parse().unwrap()
    }

    fn wait(&self, _duration: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected test lock contention"))
    }
}

/// Isolates persistent test outputs inside the checkout.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".markdown-test-{}-{name}", std::process::id()));
        #[expect(clippy::create_dir, reason = "Test directory collisions must fail")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Produces a complete record so tests can vary only the relevant fields.
fn finding(id: &str, origin: &str) -> Value {
    json!({
        "id": id, "origin": origin, "title": "Terminate the retry loop",
        "description": "The loop condition is never updated.",
        "reasoning": "- First step\n- Second step",
        "consequence": "A transient failure hangs the caller indefinitely.",
        "recommendedAction": "```rust\ncount += 1;\n```",
        "references": [{"text":"docs/design.md"}, {"text":"RFC","url":"https://example.invalid/rfc"}],
        "impactedLocations": [{"path":"src/retry.rs","startLine":40,"endLine":44}, {"path":"src/legacy.rs"}],
        "anchor": {"kind":"line","path":"src/retry.rs","startLine":42,"endLine":42,"side":"right"}
    })
}

#[test]
fn file_output_uses_selected_audit_and_complete_combined_document() {
    let directory = TestDirectory::new("output");
    let output = directory.0.join("finding files");
    let audit = directory.0.join("audit files (local)/audit-findings.md");
    let combined = directory.0.join("self-review.md");
    let records = [finding("F1", "reviewer"), finding("F2", "synthesis")];
    let request = json!({
        "findings": records,
        "selectedFindingIds": ["F1"],
        "destination": "tracker",
        "outputDirectory": output,
        "combinedPath": combined,
        "auditDocumentPath": audit,
        "overview": "Résumé\r\n"
    });
    let result = render_files_json(&serde_json::to_vec(&request).unwrap(), &TestClock).unwrap();
    assert_eq!(result["findings"].as_array().unwrap().len(), 1);
    assert_eq!(result["findings"][0]["findingId"], "F1");
    assert!(output.join("F1.md").is_file());
    assert!(!output.join("F2.md").exists());
    let audit_text = fs::read_to_string(&audit).unwrap();
    assert!(audit_text.starts_with("Résumé\n\n---\n\n"));
    assert!(audit_text.contains("**Finding `F1`**"));
    assert!(!audit_text.contains("**Finding `F2`**"));
    let combined_text = fs::read_to_string(&combined).unwrap();
    assert_eq!(combined_text.matches("Title: Terminate").count(), 2);
    assert!(!combined_text.contains('\r'));
}

#[test]
fn file_output_rejects_bad_selection_before_writing() {
    let directory = TestDirectory::new("invalid");
    let output = directory.0.join("findings");
    let request = json!({
        "findings": [finding("F1", "reviewer")],
        "selectedFindingIds": ["missing"],
        "destination": "pullRequest",
        "outputDirectory": output
    });
    assert!(render_files_json(&serde_json::to_vec(&request).unwrap(), &TestClock).is_err());
    assert!(!output.exists());
    let mut invalid = request;
    invalid["selectedFindingIds"] = json!([]);
    invalid["findings"][0]["id"] = json!("../escape");
    assert!(render_files_json(&serde_json::to_vec(&invalid).unwrap(), &TestClock).is_err());
    assert!(!output.exists());
}

#[test]
fn complete_rendering_exposes_parts_without_reparsing_markdown() {
    let finding = json!({
        "title":"Heading", "description":"One paragraph\n\nAnother paragraph",
        "reasoning":"- First\n- Second",
        "anchor":{"kind":"file","path":"src/file.rs"}
    });
    for destination in ["pullRequest", "tracker"] {
        let output = render(&json!({
            "operation":"findingRender", "finding":finding, "destination":destination,
            "followUpUrl":"comment", "pullRequestUrl":"pr"
        }))
        .unwrap();
        let blocks = output["blocks"].as_array().unwrap();
        let joined = blocks
            .iter()
            .map(|block| block.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_eq!(output["body"], joined);
        assert_eq!(
            output["published"],
            format!("[Copilot speaking]\n\n{joined}")
        );
        assert!(joined.contains("One paragraph\n\nAnother paragraph"));
    }
}

#[test]
#[cfg_attr(miri, ignore = "The PowerShell adapter runs in a separate process")]
fn compatibility_adapters_use_whole_rendering_requests() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = package.parent().unwrap().parent().unwrap();
    let output = Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(package.join("tests/markdown_boundary.ps1"))
        .arg("-RepositoryRoot")
        .arg(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn comment_preserves_all_prose_and_block_boundaries() {
    let result = render(&json!({"operation":"finding", "destination":"pullRequest", "finding":finding("F1", "reviewer")})).unwrap();
    assert_eq!(
        result,
        json!(concat!(
            "[Copilot speaking]\n\n# Terminate the retry loop\n\n",
            "The loop condition is never updated.\n\n",
            "**Reproducible reasoning**:\n\n- First step\n- Second step\n\n",
            "**Consequence**:\n\nA transient failure hangs the caller indefinitely.\n\n",
            "**Recommended action**:\n\n```rust\ncount += 1;\n```\n\n",
            "**References**:\n\n- docs/design.md\n- [RFC](https://example.invalid/rfc)\n\n",
            "**Impacted locations**:\n\n- `src/retry.rs:40-44`\n- `src/legacy.rs`"
        ))
    );
}

#[test]
fn tracker_has_location_and_captured_follow_up_with_fallback() {
    let mut finding = finding("F1", "reviewer");
    finding["followUp"] = json!({"threadId":"t","commentId":"c"});
    let context = json!({"pullRequestUrl":"https://example.invalid/pr","threads":[
        {"id":"t","comments":[{"id":"c","url":"https://example.invalid/comment"}]}
    ]});
    let item =
        render(&json!({"operation":"trackerItem","finding":finding,"context":context})).unwrap();
    assert_eq!(item["title"], "Terminate the retry loop");
    let body = item["body"].as_str().unwrap();
    assert!(body.starts_with("[Copilot speaking]\n\nThe loop condition"));
    assert!(body.contains("**Location**: `src/retry.rs:42`"));
    assert!(body.contains("**Follows up on**: https://example.invalid/comment"));
    assert!(body.ends_with("**Reviewed in**: https://example.invalid/pr"));
    let item = render(&json!({"operation":"trackerItem","finding":finding,"context":{"pullRequestUrl":"https://example.invalid/pr"}})).unwrap();
    assert!(
        item["body"]
            .as_str()
            .unwrap()
            .contains("**Follows up on**: https://example.invalid/pr")
    );
}

#[test]
fn tracker_complete_body_matches_publication_contract() {
    let mut finding = finding("F1", "reviewer");
    finding["followUp"] = json!({"threadId":"t","commentId":"c"});
    let item = render(&json!({"operation":"trackerItem","finding":finding,
    "context":{"pullRequestUrl":"https://example.invalid/pr","threads":[
        {"id":"t","comments":[{"id":"c","url":"https://example.invalid/comment"}]}
    ]}}))
    .unwrap();
    assert_eq!(
        item["body"],
        concat!(
            "[Copilot speaking]\n\nThe loop condition is never updated.\n\n",
            "**Location**: `src/retry.rs:42`\n\n",
            "**Reproducible reasoning**:\n\n- First step\n- Second step\n\n",
            "**Consequence**:\n\nA transient failure hangs the caller indefinitely.\n\n",
            "**Recommended action**:\n\n```rust\ncount += 1;\n```\n\n",
            "**References**:\n\n- docs/design.md\n- [RFC](https://example.invalid/rfc)\n\n",
            "**Impacted locations**:\n\n- `src/retry.rs:40-44`\n- `src/legacy.rs`\n\n",
            "**Follows up on**: https://example.invalid/comment\n\n",
            "**Reviewed in**: https://example.invalid/pr"
        )
    );
}

#[test]
fn presentation_order_is_stable_within_each_origin_for_both_destinations() {
    let findings = [
        finding("F1", "reviewer"),
        finding("F2", "synthesis"),
        finding("F3", "reviewer"),
        finding("F4", "synthesis"),
    ];
    for destination in ["pullRequest", "tracker"] {
        let audit = render(
            &json!({"operation":"auditDocument","destination":destination,"findings":findings}),
        )
        .unwrap();
        let audit = audit.as_str().unwrap();
        let positions: Vec<_> = ["F2", "F4", "F1", "F3"]
            .iter()
            .map(|id| audit.find(&format!("**Finding `{id}`**")).unwrap())
            .collect();
        assert!(positions.windows(2).all(|window| window[0] < window[1]));
    }
}

#[test]
fn documents_order_findings_and_audit_shows_publication_text() {
    let reviewer = finding("F1", "reviewer");
    let mut synthesis = finding("F2", "synthesis");
    synthesis["title"] = json!("Synthesis");
    let input = json!([reviewer, synthesis]);
    let doc = render(&json!({"operation":"document","destination":"pullRequest","findings":input}))
        .unwrap();
    let doc = doc.as_str().unwrap();
    assert_eq!(doc.matches("[Copilot speaking]").count(), 1);
    assert!(doc.find("# Synthesis").unwrap() < doc.find("# Terminate").unwrap());
    assert!(doc.contains("\n\n---\n\n"));
    let audit = render(&json!({"operation":"auditDocument","destination":"tracker","findings":input,"overview":"Overview\r\n"})).unwrap();
    let audit = audit.as_str().unwrap();
    assert!(audit.starts_with(
        "Overview\n\n---\n\n**Finding `F2`**\n\nTitle: Synthesis\n\n[Copilot speaking]"
    ));
    assert!(audit.contains("**Finding `F1`**\n\nTitle: Terminate"));
    assert_eq!(
        render(&json!({"operation":"document","destination":"tracker","findings":[]})).unwrap(),
        ""
    );
}

#[test]
fn self_review_keeps_skipped_audit_distinct_and_in_document() {
    let findings = json!([finding("F1", "reviewer"), finding("F2", "synthesis")]);
    let result = render(&json!({"operation":"selfReview","findings":findings,"destination":"tracker","auditFindingIds":["F2"],"policyApprovedFindingIds":["F1"]})).unwrap();
    assert_eq!(result["policyApproved"][0]["findingId"], "F1");
    assert_eq!(result["auditSkipped"][0]["findingId"], "F2");
    assert_eq!(
        result["document"]
            .as_str()
            .unwrap()
            .matches("Title: ")
            .count(),
        2
    );
    assert!(render(&json!({"operation":"selfReview","findings":findings,"destination":"tracker","auditFindingIds":["F3"],"policyApprovedFindingIds":["F1"]})).is_err());
    assert!(render(&json!({"operation":"selfReview","findings":findings,"destination":"tracker","auditFindingIds":[],"policyApprovedFindingIds":["F1"]})).is_err());
    assert!(
        render(&json!({"operation":"selfReview","findings":findings,"destination":"tracker"}))
            .is_err()
    );
}

#[test]
fn summary_reports_diagnostics_without_inventing_a_verdict() {
    let input = json!({"operation":"summary","findings":[finding("F1","reviewer")],"followUpCount":1,
        "platform":"gitHub","diagnostics":{"cache":"hit","inputState":"Degraded","userInstructionFileCount":0}});
    let summary = render(&input).unwrap();
    let summary = summary.as_str().unwrap();
    assert!(
        summary.contains(
            "Published 1 finding. One finding follows up on an existing discussion thread."
        )
    );
    assert!(summary.contains("<details><summary>See diagnostics</summary>"));
    assert!(summary.contains(
        "| Cache | Hit |\n| User instruction files | 0 |\n| Instruction expansion | Degraded |"
    ));
    assert!(!summary.contains("approve"));
    let empty =
        render(&json!({"operation":"summary","findings":[],"platform":"azureDevOps"})).unwrap();
    assert!(
        empty
            .as_str()
            .unwrap()
            .contains("This review raises no findings.")
    );
    assert!(!empty.as_str().unwrap().contains("<details>"));
}

#[test]
fn summary_counts_duplicate_and_lightweight_findings() {
    let finding = finding("F1", "reviewer");
    let summary = render(&json!({
        "operation":"summary", "platform":"gitHub", "followUpCount":2,
        "findings":[finding, finding, finding]
    }))
    .unwrap();
    assert!(
        summary
            .as_str()
            .unwrap()
            .contains("Published 3 findings. 2 findings follow up on existing discussion threads.")
    );
    let summary = render(&json!({
        "operation":"summary", "platform":"azureDevOps", "findings":[{}, {"id":"F1"}]
    }))
    .unwrap();
    assert!(summary.as_str().unwrap().contains("Published 2 findings."));
    assert!(render(&json!({"operation":"summary","platform":"gitHub","findings":{}})).is_err());
}

#[test]
fn rejects_malformed_input_instead_of_coercing_or_dropping_it() {
    let mut invalid = finding("F1", "reviewer");
    for (field, value) in [
        ("description", json!(5)),
        ("reasoning", json!(" ")),
        ("references", json!("not a list")),
        (
            "anchor",
            json!({"kind":"line","path":"a","startLine":2,"endLine":1}),
        ),
        ("impactedLocations", json!([{"path":"x","endLine":2}])),
        ("followUp", json!({"threadId":"t"})),
    ] {
        let old = invalid[field].clone();
        invalid[field] = value;
        assert!(
            render(&json!({"operation":"finding","destination":"tracker","finding":invalid}))
                .is_err(),
            "{field}"
        );
        invalid[field] = old;
    }
    assert!(
        render(&json!({"operation":"document","destination":"invalid","findings":[]})).is_err()
    );
    assert!(render(&json!({"operation":"document","destination":"tracker"})).is_err());
    assert!(render(&json!({"operation":"summary","platform":"gitHub","findings":[],"diagnostics":{"userInstructionFileCount":-1}})).is_err());
}

#[test]
fn audit_body_matches_the_selected_destination_and_empty_overview_is_rejected() {
    let finding = finding("F1", "reviewer");
    for destination in ["pullRequest", "tracker"] {
        let body =
            render(&json!({"operation":"finding","destination":destination,"finding":finding}))
                .unwrap();
        let audit = render(
            &json!({"operation":"auditDocument","destination":destination,"findings":[finding]}),
        )
        .unwrap();
        assert_eq!(
            audit,
            format!("**Finding `F1`**\n\n{}", body.as_str().unwrap())
        );
    }
    assert_eq!(
        render(&json!({"operation":"auditDocument","destination":"tracker","findings":[],"overview":"Overview\r\n"})).unwrap(),
        "Overview"
    );
    assert!(render(&json!({"operation":"auditDocument","destination":"tracker","findings":[],"overview":" \n"})).is_err());
    assert!(render(&json!({"operation":"document","destination":"tracker","findings":[],"overview":"Overview"})).is_err());
    assert!(
        render(
            &json!({"operation":"document","destination":"pullRequest","findings":[],"context":{}})
        )
        .is_err()
    );
}

#[test]
fn classification_requires_an_exact_disjoint_partition() {
    let finding = finding("F1", "reviewer");
    for (approved, skipped) in [
        (json!([]), json!([])),
        (json!(["F1"]), json!(["F1"])),
        (json!(["F1", "F1"]), json!([])),
        (json!(["unknown"]), json!([])),
    ] {
        assert!(render(&json!({"operation":"selfReview","findings":[finding],"destination":"tracker","policyApprovedFindingIds":approved,"auditFindingIds":skipped})).is_err());
    }
}

#[test]
fn markdown_block_forms_and_unicode_survive_every_destination_and_prose_field() {
    let examples = [
        "First paragraph.\n\nSecond paragraph.",
        "- First item\n- Second item",
        "1. First step\n2. Second step",
        "```text\nFirst line\nSecond line\n```",
        "> First line\n>\n> Second line",
        "| Input | Result |\n| --- | --- |\n| Timeout | Retry |",
        "Em dash — “quoted” 🤖 and tab\tremain unchanged.",
    ];
    for field in [
        "description",
        "reasoning",
        "consequence",
        "recommendedAction",
    ] {
        for content in examples {
            let mut finding = finding("F1", "reviewer");
            finding[field] = json!(content);
            for destination in ["pullRequest", "tracker"] {
                let result = render(
                    &json!({"operation":"finding","destination":destination,"finding":finding}),
                )
                .unwrap();
                let result = result.as_str().unwrap();
                let expected = match field {
                    "description" => content.to_owned(),
                    "reasoning" => format!("**Reproducible reasoning**:\n\n{content}"),
                    "consequence" => format!("**Consequence**:\n\n{content}"),
                    _ => format!("**Recommended action**:\n\n{content}"),
                };
                assert!(
                    result.contains(&expected),
                    "{destination} {field}: {content}"
                );
            }
        }
    }
}

#[test]
fn optional_collections_and_provenance_are_omitted_when_absent() {
    let mut finding = finding("F1", "reviewer");
    finding.as_object_mut().unwrap().remove("references");
    finding.as_object_mut().unwrap().remove("impactedLocations");
    let comment =
        render(&json!({"operation":"finding","destination":"pullRequest","finding":finding}))
            .unwrap();
    let body = comment.as_str().unwrap();
    assert!(!body.contains("**References**"));
    assert!(!body.contains("**Impacted locations**"));
    let item = render(&json!({"operation":"trackerItem","finding":finding})).unwrap();
    let body = item["body"].as_str().unwrap();
    assert!(!body.contains("**Follows up on**"));
    assert!(!body.contains("**Reviewed in**"));
    let item = render(&json!({"operation":"trackerItem","finding":finding,"context":{"pullRequestUrl":"https://example.invalid/pr"}})).unwrap();
    let body = item["body"].as_str().unwrap();
    assert!(!body.contains("**Follows up on**"));
    assert!(body.ends_with("**Reviewed in**: https://example.invalid/pr"));
    finding["references"] = Value::Null;
    finding["impactedLocations"] = Value::Null;
    finding["followUp"] = Value::Null;
    assert_eq!(
        comment,
        render(&json!({"operation":"finding","destination":"pullRequest","finding":finding}))
            .unwrap()
    );
    assert_eq!(
        item,
        render(&json!({"operation":"trackerItem","finding":finding,"context":{"pullRequestUrl":"https://example.invalid/pr"}})).unwrap()
    );
    finding["followUp"] = json!({"threadId":"t","commentId":"c"});
    let comment_with_follow_up =
        render(&json!({"operation":"finding","destination":"pullRequest","finding":finding}))
            .unwrap();
    assert_eq!(comment, comment_with_follow_up);
}

#[test]
fn file_and_ranged_anchors_produce_standalone_locations() {
    let mut finding = finding("F1", "reviewer");
    for (anchor, expected) in [
        (
            json!({"kind":"file","path":"src/legacy.rs"}),
            "`src/legacy.rs`",
        ),
        (
            json!({"kind":"line","path":"src/retry.rs","startLine":40,"endLine":44,"side":"left"}),
            "`src/retry.rs:40-44`",
        ),
        (
            json!({"kind":"line","path":"src/retry.rs","startLine":42}),
            "`src/retry.rs:42`",
        ),
    ] {
        finding["anchor"] = anchor;
        let item = render(&json!({"operation":"trackerItem","finding":finding})).unwrap();
        let body = item["body"].as_str().unwrap();
        let location = format!("**Location**: {expected}");
        assert!(body.contains(&location));
        assert!(body.find("The loop condition").unwrap() < body.find(&location).unwrap());
        assert!(body.find(&location).unwrap() < body.find("**Reproducible reasoning**").unwrap());
    }
}

#[test]
fn tracker_provenance_uses_exact_ids_and_preserves_capture_fallback() {
    let mut finding = finding("F1", "reviewer");
    finding["followUp"] = json!({"threadId":"Thread","commentId":"Comment"});
    let contexts = [
        (
            json!({"pullRequestUrl":"pr","threads":[{"id":"thread","comments":[{"id":"Comment","url":"wrong"}]}]}),
            "pr",
        ),
        (
            json!({"pullRequestUrl":"pr","threads":[{"id":"Thread","comments":[{"id":"comment","url":"wrong"}]}]}),
            "pr",
        ),
        (
            json!({"pullRequestUrl":"pr","threads":[{"id":"Thread","comments":[{"id":"Comment","url":""}]}]}),
            "pr",
        ),
        (
            json!({"pullRequestUrl":"pr","threads":[{"id":"Thread","comments":[{"id":"Comment","url":"comment"}]}]}),
            "comment",
        ),
    ];
    for (context, expected) in contexts {
        let item = render(&json!({"operation":"trackerItem","finding":finding,"context":context}))
            .unwrap();
        let body = item["body"].as_str().unwrap();
        assert!(body.contains(&format!("**Follows up on**: {expected}")));
        assert!(body.ends_with("**Reviewed in**: pr"));
    }
    for context in [
        json!({"pullRequestUrl":3}),
        json!({"threads":"invalid"}),
        json!({"threads":[{"id":"t","comments":[{"id":"c","url":42}]}]}),
    ] {
        assert!(
            render(&json!({"operation":"trackerItem","finding":finding,"context":context}))
                .is_err()
        );
    }
}

#[test]
fn captured_singleton_collections_render_like_arrays() {
    let plain = finding("F1", "reviewer");
    let mut follow = finding("F2", "reviewer");
    follow["followUp"] = json!({"threadId":"42","commentId":"7"});
    let comment = json!({"id":7,"url":"comment"});
    let baseline_context = json!({"pullRequestUrl":"pr","threads":[
        {"id":42,"comments":[comment]}
    ]});
    for singleton_thread in [false, true] {
        for singleton_comment in [false, true] {
            let comments = if singleton_comment {
                comment.clone()
            } else {
                json!([comment])
            };
            let thread = json!({"id":42,"comments":comments});
            let threads = if singleton_thread {
                thread
            } else {
                json!([thread])
            };
            let context = json!({"pullRequestUrl":"pr","threads":threads});
            for record in [&plain, &follow] {
                let expected = render(&json!({"operation":"trackerItem","finding":record,
                    "context":baseline_context}))
                .unwrap();
                let actual = render(&json!({"operation":"trackerItem","finding":record,
                    "context":context}))
                .unwrap();
                assert_eq!(actual, expected);
            }
            for operation in ["document", "auditDocument"] {
                let expected = render(&json!({"operation":operation,"destination":"tracker",
                    "findings":[plain,follow],"context":baseline_context}))
                .unwrap();
                let actual = render(&json!({"operation":operation,"destination":"tracker",
                    "findings":[plain,follow],"context":context}))
                .unwrap();
                assert_eq!(actual, expected);
                assert!(
                    actual
                        .as_str()
                        .unwrap()
                        .contains("**Follows up on**: comment")
                );
            }
        }
    }

    let directory = TestDirectory::new("singleton-collections");
    let output = directory.0.join("findings");
    let combined = directory.0.join("combined.md");
    let audit = directory.0.join("audit.md");
    let request = json!({
        "findings":[plain,follow], "selectedFindingIds":[], "destination":"tracker",
        "context":{"pullRequestUrl":"pr","threads":{"id":42,"comments":comment}},
        "outputDirectory":output, "combinedPath":combined, "auditDocumentPath":audit
    });
    render_files_json(&serde_json::to_vec(&request).unwrap(), &TestClock).unwrap();
    assert!(
        !fs::read_to_string(output.join("F1.md"))
            .unwrap()
            .contains("**Follows up on**")
    );
    for path in [output.join("F2.md"), combined, audit] {
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("**Follows up on**: comment")
        );
    }
}

#[test]
fn captured_singleton_collections_preserve_fallback_and_validation() {
    let mut follow = finding("F1", "reviewer");
    follow["followUp"] = json!({"threadId":"t","commentId":"c"});
    for context in [
        json!({"pullRequestUrl":"pr"}),
        json!({"pullRequestUrl":"pr","threads":null}),
        json!({"pullRequestUrl":"pr","threads":[]}),
        json!({"pullRequestUrl":"pr","threads":{"id":"t"}}),
        json!({"pullRequestUrl":"pr","threads":{"id":"t","comments":null}}),
        json!({"pullRequestUrl":"pr","threads":{"id":"t","comments":[]}}),
        json!({"pullRequestUrl":"pr","threads":{"id":"other","comments":{"id":"c","url":"wrong"}}}),
        json!({"pullRequestUrl":"pr","threads":{"id":"t","comments":{"id":"other","url":"wrong"}}}),
        json!({"pullRequestUrl":"pr","threads":{"id":"t","comments":{"id":"c","url":""}}}),
    ] {
        let item =
            render(&json!({"operation":"trackerItem","finding":follow,"context":context})).unwrap();
        let body = item["body"].as_str().unwrap();
        assert!(body.contains("**Follows up on**: pr"), "{context}");
        assert!(body.ends_with("**Reviewed in**: pr"));
    }

    let plain = finding("F2", "reviewer");
    for context in [
        json!({"threads":true}),
        json!({"threads":"invalid"}),
        json!({"threads":12}),
        json!({"threads":[null]}),
        json!({"threads":{}}),
        json!({"threads":{"id":"t","comments":true}}),
        json!({"threads":{"id":"t","comments":"invalid"}}),
        json!({"threads":{"id":"t","comments":12}}),
        json!({"threads":{"id":"t","comments":[null]}}),
        json!({"threads":{"id":"t","comments":{}}}),
        json!({"threads":{"id":"t","comments":{"id":"c","url":42}}}),
    ] {
        for record in [&plain, &follow] {
            assert!(
                render(&json!({"operation":"document","destination":"tracker",
                "findings":[record],"context":context}))
                .is_err(),
                "{context}"
            );
        }
    }
    for field in ["references", "impactedLocations"] {
        let mut malformed = plain.clone();
        malformed[field] = json!({"text":"not an array","path":"file"});
        assert!(render(&json!({"operation":"trackerItem","finding":malformed})).is_err());
    }
    let directory = TestDirectory::new("invalid-singleton");
    let output = directory.0.join("findings");
    let request = json!({
        "findings":[plain], "selectedFindingIds":[], "destination":"tracker",
        "context":{"threads":{"id":"t","comments":{"id":"c","url":42}}},
        "outputDirectory":output
    });
    assert!(render_files_json(&serde_json::to_vec(&request).unwrap(), &TestClock).is_err());
    assert!(!output.exists());
}

#[test]
#[cfg_attr(miri, ignore = "The PowerShell adapter runs in a separate process")]
fn powershell_tracker_adapter_preserves_singleton_capture_for_native_documents() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = package.parent().unwrap().parent().unwrap();
    let script = "
#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. ./skills/ss-review/scripts/lib/JsonValue.ps1
. ./skills/ss-review/scripts/lib/TrackerItem.ps1
function Invoke-NativeReviewMarkdown {
    param([Parameter(Mandatory)][System.Collections.IDictionary] $Request)
    return $Request
}
$inputData = ConvertFrom-Json -InputObject ([Console]::In.ReadToEnd()) -AsHashtable
$provenance = Get-TrackerItemProvenance -Finding $inputData.finding -Context $inputData.context
if ($provenance.followUpUrl -cne 'comment') { throw 'PowerShell provenance did not find the comment.' }
$requests = @(
    ConvertTo-FindingDestinationDocument -Finding @($inputData.finding) -Destination tracker -Context $inputData.context
    ConvertTo-FindingAuditDocument -Finding @($inputData.finding) -Destination tracker -Context $inputData.context
)
ConvertTo-Json -InputObject $requests -Depth 100 -Compress
";
    let mut follow = finding("F1", "reviewer");
    follow["followUp"] = json!({"threadId":"42","commentId":"7"});
    let input = json!({"finding":follow,"context":{"pullRequestUrl":"pr",
        "threads":{"id":42,"comments":{"id":7,"url":"comment"}}}});
    let mut process = Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&input).unwrap())
        .unwrap();
    let output = process.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(request["context"]["threads"].is_object());
        assert!(request["context"]["threads"]["comments"].is_object());
        assert!(
            render(&request)
                .unwrap()
                .as_str()
                .unwrap()
                .contains("**Follows up on**: comment")
        );
    }
}

#[test]
fn captured_identifiers_support_integers_in_document_and_file_rendering() {
    let directory = TestDirectory::new("numeric-identifiers");
    let output = directory.0.join("findings");
    let combined = directory.0.join("combined.md");
    let audit = directory.0.join("audit.md");
    let plain = finding("F1", "reviewer");
    let mut follow = finding("F2", "reviewer");
    follow["followUp"] =
        json!({"threadId": i64::MIN.to_string(), "commentId": u64::MAX.to_string()});
    let context = json!({"pullRequestUrl":"pr","threads":[
        {"id": i64::MIN,"comments":[{"id":u64::MAX,"url":"comment"}]}
    ]});
    for operation in ["document", "auditDocument"] {
        let without_follow_up = render(&json!({
            "operation":operation,"destination":"tracker","findings":[plain],"context":context
        }))
        .unwrap();
        assert!(
            !without_follow_up
                .as_str()
                .unwrap()
                .contains("**Follows up on**")
        );
        let with_follow_up = render(&json!({
            "operation":operation,"destination":"tracker","findings":[follow],"context":context
        }))
        .unwrap();
        assert!(
            with_follow_up
                .as_str()
                .unwrap()
                .contains("**Follows up on**: comment")
        );
    }

    let request = json!({
        "findings":[plain, follow], "selectedFindingIds":[], "destination":"tracker",
        "context":context, "outputDirectory":output, "combinedPath":combined,
        "auditDocumentPath":audit
    });
    render_files_json(&serde_json::to_vec(&request).unwrap(), &TestClock).unwrap();
    assert!(
        !fs::read_to_string(output.join("F1.md"))
            .unwrap()
            .contains("**Follows up on**")
    );
    for path in [output.join("F2.md"), combined, audit] {
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("**Follows up on**: comment")
        );
    }
}

#[test]
fn captured_identifiers_match_exact_decimal_or_string_values() {
    for (thread, comment, thread_id, comment_id, expected) in [
        (json!(42), json!(7), "42", "7", "comment"),
        (json!(0), json!(-7), "0", "-7", "comment"),
        (json!("42"), json!("7"), "42", "7", "comment"),
        (json!(42), json!(7), "042", "7", "pr"),
        (json!(42), json!(7), "42", "07", "pr"),
        (json!("Thread"), json!("Comment"), "thread", "Comment", "pr"),
    ] {
        let mut finding = finding("F1", "reviewer");
        finding["followUp"] = json!({"threadId":thread_id,"commentId":comment_id});
        let item = render(&json!({"operation":"trackerItem","finding":finding,
        "context":{"pullRequestUrl":"pr","threads":[
            {"id":thread,"comments":[{"id":comment,"url":"comment"}]}
        ]}}))
        .unwrap();
        assert!(
            item["body"]
                .as_str()
                .unwrap()
                .contains(&format!("**Follows up on**: {expected}"))
        );
    }
}

#[test]
fn captured_identifiers_reject_malformed_values_and_follow_up_remains_string_only() {
    let finding = finding("F1", "reviewer");
    // The capture contract permits any nonempty identifier string.
    assert!(
        render(&json!({"operation":"document","destination":"tracker",
            "findings":[finding],"context":{"threads":[{"id":" ","comments":[{"id":" "}]}]}}))
        .is_ok()
    );
    for invalid in [
        json!(null),
        json!(""),
        json!(true),
        json!({}),
        json!([]),
        json!(1.0),
        json!(1.5),
    ] {
        for context in [
            json!({"threads":[{"id":invalid,"comments":[]}]}),
            json!({"threads":[{"id":"thread","comments":[{"id":invalid}]}]}),
        ] {
            assert!(
                render(&json!({"operation":"document","destination":"tracker",
                "findings":[finding],"context":context}))
                .is_err(),
                "{invalid}"
            );
        }
    }
    for context in [
        json!({"threads":[{"comments":[]}]}),
        json!({"threads":[{"id":"thread","comments":[{}]}]}),
    ] {
        assert!(
            render(&json!({"operation":"document","destination":"tracker",
            "findings":[finding],"context":context}))
            .is_err()
        );
    }
    let directory = TestDirectory::new("invalid-captured-identifiers");
    let output = directory.0.join("findings");
    let request = json!({
        "findings":[finding], "selectedFindingIds":[], "destination":"tracker",
        "context":{"threads":[{"id":42,"comments":[{"id":1.5}]}]},
        "outputDirectory":output
    });
    assert!(render_files_json(&serde_json::to_vec(&request).unwrap(), &TestClock).is_err());
    assert!(!output.exists());
    for follow_up in [
        json!({"threadId":42,"commentId":"7"}),
        json!({"threadId":"42","commentId":7}),
    ] {
        let mut finding = finding.clone();
        finding["followUp"] = follow_up;
        assert!(render(&json!({"operation":"trackerItem","finding":finding})).is_err());
    }
}

#[test]
fn summary_pluralization_and_diagnostics_match_both_platforms() {
    for platform in ["gitHub", "azureDevOps"] {
        for cache in ["Hit", "Miss"] {
            for count in [0, 7] {
                let summary = render(&json!({"operation":"summary","platform":platform,
                        "findings":[finding("F1","reviewer"),finding("F2","reviewer"),finding("F3","synthesis")],
                        "followUpCount":2,"diagnostics":{"cache":cache,"userInstructionFileCount":count,"inputState":"Complete"}})).unwrap();
                let summary = summary.as_str().unwrap();
                assert!(summary.contains(
                    "Published 3 findings. 2 findings follow up on existing discussion threads."
                ));
                assert!(summary.contains(&format!(
                    "| Cache | {cache} |\n| User instruction files | {count} |"
                )));
                assert!(!summary.contains("Instruction expansion"));
                assert_eq!(summary.contains("<details>"), platform == "gitHub");
                assert!(!summary.contains("Terminate the retry loop"));
            }
        }
        let summary = render(&json!({"operation":"summary","platform":platform,"findings":[finding("F1","reviewer")]})).unwrap();
        let summary = summary.as_str().unwrap();
        assert!(summary.contains("No finding follows up on an existing discussion thread."));
        assert!(summary.contains("| Cache | Miss |\n| User instruction files | Not recorded |"));
    }
    for count in [json!(-1), json!("seven"), json!(1.5), json!(u64::MAX)] {
        assert!(render(&json!({"operation":"summary","platform":"gitHub","findings":[],"diagnostics":{"userInstructionFileCount":count}})).is_err());
    }
    assert!(render(&json!({"operation":"summary","platform":"gitHub","findings":[],"followUpCount":2147483648_u64})).is_err());
}

#[test]
fn self_review_retains_all_or_no_findings_without_inventing_approval() {
    let all = [finding("F1", "reviewer"), finding("F2", "synthesis")];
    let result = render(&json!({"operation":"selfReview","findings":all,"destination":"tracker","policyApprovedFindingIds":[],"auditFindingIds":["F1","F2"]})).unwrap();
    assert_eq!(result["policyApproved"], json!([]));
    assert_eq!(result["auditSkipped"].as_array().unwrap().len(), 2);
    assert!(result["document"].as_str().unwrap().contains("Title: "));
    let empty = render(&json!({"operation":"selfReview","findings":[],"destination":"tracker","policyApprovedFindingIds":[],"auditFindingIds":[]})).unwrap();
    assert_eq!(
        empty,
        json!({"policyApproved":[],"auditSkipped":[],"document":""})
    );
}

#[test]
fn finding_model_rejects_bad_fields_and_retains_non_ascii_controls() {
    let mut finding = finding("F1", "reviewer");
    finding["description"] = json!("Non-ASCII character \u{85} remains intact");
    assert!(
        render(&json!({"operation":"finding","destination":"pullRequest","finding":finding}))
            .unwrap()
            .as_str()
            .unwrap()
            .contains('\u{85}')
    );
    finding["description"] = json!("illegal\rcontrol");
    assert!(
        render(&json!({"operation":"finding","destination":"pullRequest","finding":finding}))
            .is_err()
    );
    finding["description"] = json!("Valid description");
    finding["title"] = json!("🤖".repeat(128));
    assert!(
        render(&json!({"operation":"finding","destination":"tracker","finding":finding})).is_err()
    );
    finding["title"] = json!("Valid title");
    finding["anchor"]["startLine"] = json!(u64::MAX);
    assert!(
        render(&json!({"operation":"finding","destination":"tracker","finding":finding})).is_err()
    );
}

#[test]
fn complete_rendering_preserves_partial_findings() {
    let finding = json!({
        "title":"Heading", "description":"  ", "reasoning":"- First\n- Second",
        "consequence":null, "recommendedAction":"", "references":[{"text":"local"}],
        "anchor":{"kind":"file","path":"src/file.rs","side":"right"}
    });
    let blocks = render(
        &json!({"operation":"findingRender","finding":finding,"destination":"tracker",
        "followUpUrl":"comment","pullRequestUrl":"pr"}),
    )
    .unwrap();
    assert_eq!(
        blocks["blocks"],
        json!([
            "**Location**: `src/file.rs`",
            "**Reproducible reasoning**:\n\n- First\n- Second",
            "**References**:\n\n- local",
            "**Follows up on**: comment",
            "**Reviewed in**: pr"
        ])
    );
    let body =
        render(&json!({"operation":"findingRender","finding":finding,"destination":"pullRequest"}))
            .unwrap();
    assert_eq!(
        body["body"],
        "# Heading\n\n**Reproducible reasoning**:\n\n- First\n- Second\n\n**References**:\n\n- local"
    );
    let blocks = render(
        &json!({"operation":"findingRender","finding":finding,"destination":"pullRequest",
        "followUpUrl":"comment","pullRequestUrl":"pr"}),
    )
    .unwrap();
    assert_eq!(
        blocks["blocks"],
        json!([
            "# Heading",
            "**Reproducible reasoning**:\n\n- First\n- Second",
            "**References**:\n\n- local"
        ])
    );
    assert_eq!(
        render(&json!({"operation":"findingRender","finding":{},"destination":"pullRequest"}))
            .unwrap()["body"],
        ""
    );
    assert_eq!(
        render(&json!({"operation":"findingRender","finding":{},"destination":"tracker"})).unwrap()
            ["blocks"],
        json!([])
    );
    assert!(render(&json!({"operation":"findingRender","finding":{"description":42},"destination":"pullRequest"})).is_err());
}

#[test]
fn direct_tracker_urls_are_not_mixed_with_capture_context() {
    let finding = finding("F1", "reviewer");
    let item = render(&json!({"operation":"trackerItem","finding":finding,
        "followUpUrl":"https://example.invalid/comment","pullRequestUrl":"https://example.invalid/pr"})).unwrap();
    let body = item["body"].as_str().unwrap();
    assert!(body.contains("**Follows up on**: https://example.invalid/comment"));
    assert!(body.ends_with("**Reviewed in**: https://example.invalid/pr"));
    assert!(
        render(
            &json!({"operation":"trackerItem","finding":finding,"followUpUrl":"url","context":{}})
        )
        .is_err()
    );
    assert!(
        render(&json!({"operation":"trackerItem","finding":finding,"pullRequestUrl":9})).is_err()
    );
    let item = render(&json!({"operation":"trackerItem","finding":finding,"followUpUrl":null,"pullRequestUrl":""})).unwrap();
    assert!(!item["body"].as_str().unwrap().contains("**Follows up on**"));
}

#[test]
fn refuses_component_level_rendering_requests() {
    for operation in ["body", "blocks", "reference", "location", "order", "join"] {
        assert!(render(&json!({"operation":operation})).is_err());
    }
}

#[test]
fn strict_json_requests_reject_duplicates_unknown_fields_and_invalid_bytes() {
    assert_eq!(
        render_json(br#"{"operation":"findingRender","finding":{},"destination":"tracker"}"#)
            .unwrap()["published"],
        "[Copilot speaking]"
    );
    assert_eq!(
        render_json(b"\xef\xbb\xbf{\"operation\":\"findingRender\",\"finding\":{},\"destination\":\"tracker\"}").unwrap()["body"],
        ""
    );
    assert!(
        render_json(br#"{"operation":"findingRender","operation":"summary","finding":{}}"#)
            .is_err()
    );
    assert!(render_json(br#"{"operation":"findingRender","finding":{"id":"F1","id":"F2"},"destination":"tracker"}"#).is_err());
    assert!(
        render_json(
            br#"{"operation":"findingRender","finding":{},"destination":"tracker","typo":1}"#
        )
        .is_err()
    );
    assert!(render_json(b"\xef\xbb\xbf\xef\xbb\xbf{}").is_err());
    assert!(render_json(b"\xff").is_err());
    assert!(render_json(b"{").is_err());
}

#[test]
fn tracker_fields_renders_partial_and_legacy_anchor_shapes() {
    let finding = json!({"title":"Standalone", "description":"Details"});
    let fields = render(&json!({"operation":"trackerFields","finding":finding,
        "followUpUrl":"comment","pullRequestUrl":"pr"}))
    .unwrap();
    assert_eq!(
        fields,
        json!({
            "title":"Standalone",
            "body":"[Copilot speaking]\n\nDetails\n\n**Follows up on**: comment\n\n**Reviewed in**: pr"
        })
    );
    for (anchor, expected) in [
        (
            json!({"kind":"file","path":"src/file.rs","side":"right"}),
            "src/file.rs",
        ),
        (
            json!({"kind":"range","path":"src/file.rs","startLine":40,"endLine":44,"side":"right"}),
            "src/file.rs:40-44",
        ),
    ] {
        let finding = json!({"title":"Standalone","anchor":anchor});
        let fields = render(&json!({"operation":"trackerFields","finding":finding})).unwrap();
        assert_eq!(
            fields["body"],
            format!("[Copilot speaking]\n\n**Location**: `{expected}`")
        );
        let rendered =
            render(&json!({"operation":"findingRender","finding":finding,"destination":"tracker"}))
                .unwrap();
        assert_eq!(
            rendered["blocks"],
            json!([format!("**Location**: `{expected}`")])
        );
    }
    assert_eq!(
        render(&json!({"operation":"trackerFields","finding":{}})).unwrap(),
        json!({"title":"","body":"[Copilot speaking]"})
    );
    assert!(render(&json!({"operation":"trackerFields","finding":{"title":5}})).is_err());
}

#[test]
fn comment_includes_authorship_for_partial_finding() {
    let finding = json!({"title":"Heading", "description":"", "consequence":"- Detail"});
    assert_eq!(
        render(&json!({"operation":"comment","finding":finding})).unwrap(),
        "[Copilot speaking]\n\n# Heading\n\n**Consequence**:\n\n- Detail"
    );
    assert_eq!(
        render(&json!({"operation":"comment","finding":{}})).unwrap(),
        "[Copilot speaking]\n\n"
    );
    assert!(
        render(&json!({"operation":"finding","destination":"pullRequest","finding":finding}))
            .is_err()
    );
}
