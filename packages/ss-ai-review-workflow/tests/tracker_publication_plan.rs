//! Exercises local tracker publication preparation through its file boundary.

#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixtures use fixed record shapes and assert failures"
)]

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use ss_ai_review_context::evidence_hash;
use ss_ai_review_workflow::{markdown::render, tracker_publication_plan::prepare};
use ss_ai_runtime::SystemClock;

/// Isolates file inputs in the checkout for each integration test.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".tracker-plan-{}-{name}", std::process::id()));
        #[expect(clippy::create_dir, reason = "A collision must fail")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn put(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Matches the finding shape used by approved artifact tests.
fn finding(id: &str, origin: &str) -> Value {
    json!({"schemaVersion": 1, "id": id, "sourceFindingIds": [id], "origin": origin,
        "title": format!("Finding {id}"), "description": "Problem", "reasoning": "Reason",
        "consequence": "Impact", "recommendedAction": "Fix",
        "anchor": {"kind":"file","path":"src/widget.rs"}})
}

/// Creates the same binding as approved-set projection.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Fixtures construct and pass owned JSON values"
)]
fn approved(context: Option<&Value>, findings: Value) -> Value {
    json!({"schemaVersion": 2, "target": context.map(|c| &c["target"]),
        "reviewedSourceCommit": context.map(|c| &c["pullRequest"]["reviewedSourceCommit"]),
        "reviewedMergeBaseCommit": context.map(|c| &c["pullRequest"]["reviewedMergeBaseCommit"]),
        "reviewedContextHash": context.map(|c| evidence_hash(c).unwrap()), "findings": findings})
}

/// Uses the shared checked capture fixture, including its discussion.
fn capture() -> Value {
    serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-query-capture.json"
    ))
    .unwrap()
}

#[test]
fn local_tracker_prepares_ordered_items_and_destination_summary() {
    let workspace = Workspace::new("local");
    let clock = SystemClock::default();
    let findings = json!([
        finding("F1", "reviewer"),
        finding("F2", "synthesis"),
        finding("F3", "reviewer"),
        finding("F4", "synthesis")
    ]);
    let approved = workspace.put("approved.json", &approved(None, findings.clone()));
    let diagnostics = json!({"cache":"hit","inputState":"degraded","userInstructionFileCount":2});
    let diagnostics_path = workspace.put("diagnostics.json", &diagnostics);
    for platform in ["gitHub", "azureDevOps"] {
        let plan = prepare(&approved, &diagnostics_path, None, platform, &clock).unwrap();
        let items = plan["items"].as_array().unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item["findingId"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["F2", "F4", "F1", "F3"]
        );
        for (item, index) in items.iter().zip([1, 3, 0, 2]) {
            assert_eq!(
                *item,
                render(&json!({"operation":"trackerItem",
                "finding": findings[index]}))
                .unwrap()
            );
        }
        assert_eq!(
            plan["summaryBody"],
            render(&json!({"operation":"summary",
            "findings":findings,"platform":platform,"followUpCount":0,
            "diagnostics":diagnostics}))
            .unwrap()
        );
    }
}

#[test]
fn captured_follow_up_links_any_comment_and_uses_capture_not_tracker() {
    let workspace = Workspace::new("captured");
    let clock = SystemClock::default();
    let mut context = capture();
    // A summary thread has no reply-capable code context but is linkable.
    context["threads"][0]["kind"] = json!("reviewSummary");
    context["threads"][0]["context"] = Value::Null;
    context["threads"][0]["openingCommentId"] = json!("another-comment");
    let mut follow = finding("F1", "reviewer");
    follow["followUp"] = json!({"threadId":"T","commentId":"C"});
    let document = approved(Some(&context), json!([follow]));
    let approved_path = workspace.put("approved.json", &document);
    let context_path = workspace.put("capture.json", &context);
    let diagnostics_path = workspace.put("diagnostics.json", &json!({}));
    let plan = prepare(
        &approved_path,
        &diagnostics_path,
        Some(&context_path),
        "azureDevOps",
        &clock,
    )
    .unwrap();
    let body = plan["items"][0]["body"].as_str().unwrap();
    assert!(body.contains("**Follows up on**: https://example.invalid/comment/C"));
    assert!(body.contains("**Reviewed in**: https://example.invalid/pr/1"));
    assert!(
        plan["summaryBody"]
            .as_str()
            .unwrap()
            .contains("One finding follows up")
    );
    assert!(!plan["summaryBody"].as_str().unwrap().contains("<details>"));

    context["threads"][0]["comments"][0]["url"] = Value::Null;
    workspace.put("capture.json", &context);
    workspace.put("approved.json", &approved(Some(&context), json!([follow])));
    let plan = prepare(
        &approved_path,
        &diagnostics_path,
        Some(&context_path),
        "gitHub",
        &clock,
    )
    .unwrap();
    assert!(
        plan["items"][0]["body"]
            .as_str()
            .unwrap()
            .contains("**Follows up on**: https://example.invalid/pr/1")
    );
}

#[test]
fn refuses_stale_missing_malformed_and_unlinkable_inputs() {
    let workspace = Workspace::new("invalid");
    let clock = SystemClock::default();
    let context = capture();
    let mut follow = finding("F1", "reviewer");
    follow["followUp"] = json!({"threadId":"T","commentId":"C"});
    let document = approved(Some(&context), json!([follow.clone()]));
    let approved_path = workspace.put("approved.json", &document);
    let context_path = workspace.put("capture.json", &context);
    let diagnostics_path = workspace.put("diagnostics.json", &json!({}));
    let run = |context: Option<&PathBuf>, platform: &str| {
        prepare(
            &approved_path,
            &diagnostics_path,
            context.map(PathBuf::as_path),
            platform,
            &clock,
        )
    };
    assert!(run(None, "gitHub").is_err());
    assert!(run(Some(&context_path), "other").is_err());
    let mut stale = context.clone();
    stale["pullRequest"]["title"] = json!("Changed");
    workspace.put("capture.json", &stale);
    assert!(run(Some(&context_path), "gitHub").is_err());
    workspace.put("capture.json", &context);
    for (field, value) in [("threadId", "t"), ("commentId", "missing")] {
        let mut bad = follow.clone();
        bad["followUp"][field] = json!(value);
        workspace.put("approved.json", &approved(Some(&context), json!([bad])));
        assert!(run(Some(&context_path), "gitHub").is_err());
    }
    workspace.put("approved.json", &approved(None, json!([follow])));
    assert!(run(None, "gitHub").is_err());
    workspace.put("approved.json", &approved(None, json!([])));
    assert!(run(None, "gitHub").is_err());
    workspace.put(
        "approved.json",
        &approved(None, json!([finding("F", "reviewer")])),
    );
    workspace.put("diagnostics.json", &json!({"userInstructionFileCount":-1}));
    assert!(run(None, "gitHub").is_err());
    fs::write(&diagnostics_path, br#"{"cache":"hit","cache":"miss"}"#).unwrap();
    assert!(run(None, "gitHub").is_err());
    fs::remove_file(&diagnostics_path).unwrap();
    assert!(run(None, "gitHub").is_err());
}

#[test]
fn accepts_numeric_singleton_follow_up_and_rejects_bad_binding_fields() {
    let workspace = Workspace::new("binding");
    let clock = SystemClock::default();
    let mut context = capture();
    context["threads"][0]["id"] = json!(17);
    context["threads"][0]["comments"][0]["id"] = json!(18);
    context["threads"][0]["openingCommentId"] = json!(18);
    context["threads"][0]["comments"] = context["threads"][0]["comments"][0].clone();
    context["threads"] = context["threads"][0].clone();
    let mut follow = finding("F", "reviewer");
    follow["followUp"] = json!({"threadId":"17","commentId":"18"});
    let document = approved(Some(&context), json!([follow]));
    let approved_path = workspace.put("approved.json", &document);
    let context_path = workspace.put("capture.json", &context);
    let diagnostics_path = workspace.put("diagnostics.json", &json!({}));
    let run = || {
        prepare(
            &approved_path,
            &diagnostics_path,
            Some(&context_path),
            "gitHub",
            &clock,
        )
    };
    assert!(
        run().unwrap()["items"][0]["body"]
            .as_str()
            .unwrap()
            .contains("https://example.invalid/comment/C")
    );
    for (field, value) in [
        ("target", json!({"platform":"gitHub","pullRequestId":2})),
        ("reviewedSourceCommit", json!("other")),
        ("reviewedMergeBaseCommit", json!("other")),
        ("reviewedContextHash", json!("A".repeat(64))),
    ] {
        let mut bad = document.clone();
        bad[field] = value;
        workspace.put("approved.json", &bad);
        assert!(run().is_err(), "{field}");
    }
    workspace.put("approved.json", &document);
    workspace.put(
        "capture.json.authorship.json",
        &json!({"schemaVersion": 999}),
    );
    assert!(run().is_err());
}

#[test]
fn rejects_duplicate_keys_and_invalid_later_finding() {
    let workspace = Workspace::new("rendering");
    let clock = SystemClock::default();
    let mut second = finding("F2", "reviewer");
    // The renderer rejects controls in prose even when artifact parsing succeeds.
    second["description"] = json!("bad\rtext");
    let document = approved(None, json!([finding("F1", "reviewer"), second]));
    let approved_path = workspace.put("approved.json", &document);
    let diagnostics_path = workspace.put("diagnostics.json", &json!({}));
    assert!(prepare(&approved_path, &diagnostics_path, None, "gitHub", &clock).is_err());
    fs::write(&approved_path, br#"{"schemaVersion":2,"schemaVersion":2}"#).unwrap();
    assert!(prepare(&approved_path, &diagnostics_path, None, "gitHub", &clock).is_err());
}

#[test]
fn rejects_stale_and_unlinkable_evidence_before_missing_diagnostics() {
    let workspace = Workspace::new("evidence-first");
    let clock = SystemClock::default();
    let context = capture();
    let mut follow = finding("F", "reviewer");
    follow["followUp"] = json!({"threadId":"T","commentId":"C"});
    let approved_path = workspace.put(
        "approved.json",
        &approved(Some(&context), json!([follow.clone()])),
    );
    let context_path = workspace.put("capture.json", &context);
    let diagnostics_path = workspace.0.join("missing.json");
    let run = |context: Option<&PathBuf>| {
        prepare(
            &approved_path,
            &diagnostics_path,
            context.map(PathBuf::as_path),
            "gitHub",
            &clock,
        )
        .unwrap_err()
        .to_string()
    };
    assert!(run(None).contains("requires its capture"));
    let mut stale = context.clone();
    stale["pullRequest"]["title"] = json!("Changed");
    workspace.put("capture.json", &stale);
    assert!(run(Some(&context_path)).contains("not bound"));
    workspace.put("capture.json", &context);
    follow["followUp"]["commentId"] = json!("missing");
    workspace.put("approved.json", &approved(Some(&context), json!([follow])));
    assert!(run(Some(&context_path)).contains("Follow-up comment"));
    workspace.put(
        "approved.json",
        &approved(Some(&context), json!([finding("F", "reviewer")])),
    );
    assert!(run(Some(&context_path)).contains("diagnostics are missing"));
}
