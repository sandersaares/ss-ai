//! Exercises approved-set projection and publication freshness boundaries.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixtures use fixed record shapes and assert failures"
)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use ss_ai_review_workflow::{anchors, approved};
use ss_ai_runtime::{Process, ProcessOutput, SystemClock};

/// Isolates persistent test inputs and staging files inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".approved-test-{}-{name}", std::process::id()));
        #[expect(
            clippy::create_dir,
            reason = "A collision must fail instead of reusing another test's files"
        )]
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

/// Supplies a deterministic exact diff to the same Git comparison boundary.
struct Git;

impl Process for Git {
    fn execute(&self, _: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        let text = if arguments.iter().any(|arg| arg == "cat-file") {
            "commit\n"
        } else if arguments.iter().any(|arg| arg == "diff") {
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n context\n\
             diff --git a/old b/new\nsimilarity index 100%\nrename from old\nrename to new\n\
             diff --git a/gone b/gone\ndeleted file mode 100644\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-deleted\n"
        } else {
            ""
        };
        Ok(ProcessOutput::new(
            true,
            text.as_bytes().to_vec(),
            Vec::new(),
        ))
    }
}

/// Provides valid authored records while allowing each test to vary the anchor.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Fixtures construct and pass owned JSON values"
)]
fn finding(id: &str, anchor: Value) -> Value {
    json!({"schemaVersion": 1, "id": id, "sourceFindingIds": [id], "origin": "reviewer",
        "title": "Finding", "description": "Problem", "reasoning": "Reason",
        "consequence": "Impact", "recommendedAction": "Fix", "anchor": anchor})
}

/// Captures all evidence required by the native capture reader.
fn capture() -> Value {
    json!({"schemaVersion": 3, "platform": "gitHub", "repositoryUrl": "https://example.test/repo",
        "pullRequestUrl": "https://example.test/pr/1", "target": {"platform": "gitHub", "pullRequestId": 1},
        "pullRequest": {"title": "Title", "description": "Body",
            "reviewedSourceCommit": "b".repeat(40), "reviewedMergeBaseCommit": "a".repeat(40)},
        "reviewers": [], "revisions": [], "threads": []})
}

/// Writes an append-only audit journal with reproducible timestamps.
fn decisions(entries: &[(&str, &str)]) -> Value {
    json!({"schemaVersion": 1, "decisions": entries.iter().map(|(id, decision)|
        json!({"findingId": id, "decision": decision, "recordedUtc": "2025-01-01T00:00:00Z"})).collect::<Vec<_>>()})
}

#[test]
fn projection_uses_last_decisions_and_refuses_incomplete_work() {
    let workspace = Workspace::new("decisions");
    let clock = SystemClock::default();
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated",
        "findings":[finding("F1", json!({"kind":"file","path":"src/lib.rs"})),
                    finding("F2", json!({"kind":"file","path":"src/lib.rs"}))]}),
    );
    let log = workspace.put("decisions.json", &decisions(&[("F1", "approved")]));
    let output = workspace.0.join("approved.json");
    let missing = workspace.0.join("missing.json");
    let error = approved::build_approved(&artifact, &missing, &output, None, None, &clock, &Git)
        .unwrap_err()
        .to_string();
    assert!(error.contains("'F1' or 'F2'"));
    let error =
        approved::build_approved(&artifact, &log, &output, Some(&missing), None, &clock, &Git)
            .unwrap_err()
            .to_string();
    assert!(error.contains("-RepositoryPath"));
    let error = approved::build_approved(
        &artifact,
        &log,
        &output,
        None,
        Some(&workspace.0),
        &clock,
        &Git,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("-ContextPath"));
    assert!(approved::build_approved(&artifact, &log, &output, None, None, &clock, &Git).is_err());
    assert!(!output.exists());
    workspace.put(
        "decisions.json",
        &decisions(&[
            ("F1", "approved"),
            ("F1", "dropped"),
            ("F2", "auto-approved"),
        ]),
    );
    let result =
        approved::build_approved(&artifact, &log, &output, None, None, &clock, &Git).unwrap();
    assert_eq!(result["findingId"], json!(["F2"]));
    assert_eq!(result["autoApproved"], 1);
    assert_eq!(result["dropped"], 1);
    approved::check_approved(&output, None, &clock).unwrap();
    workspace.put(
        "decisions.json",
        &decisions(&[("F1", "approved"), ("F2", "needs-audit")]),
    );
    let before = fs::read(&output).unwrap();
    assert!(approved::build_approved(&artifact, &log, &output, None, None, &clock, &Git).is_err());
    assert_eq!(before, fs::read(&output).unwrap());
    assert!(
        approved::build_approved(&artifact, &log, &artifact, None, None, &clock, &Git).is_err()
    );
}

#[test]
fn anchors_preserve_authored_positions_and_check_exact_diff() {
    let workspace = Workspace::new("anchors");
    let clock = SystemClock::default();
    let anchors = [
        json!({"kind":"line","path":"src/lib.rs","startLine":1,"side":"right"}),
        json!({"kind":"line","path":"src/lib.rs","startLine":300,"side":"right"}),
        json!({"kind":"file","path":"absent"}),
        json!({"kind":"line","path":"gone","startLine":1,"side":"left"}),
        json!({"kind":"file","path":"new"}),
    ];
    let records: Vec<_> = anchors
        .iter()
        .enumerate()
        .map(|(i, anchor)| finding(&format!("F{i}"), anchor.clone()))
        .collect();
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":records}),
    );
    let log = workspace.put(
        "decisions.json",
        &decisions(&[
            ("F0", "approved"),
            ("F1", "auto-approved"),
            ("F2", "approved"),
            ("F3", "approved"),
            ("F4", "approved"),
        ]),
    );
    let context = workspace.put("context.json", &capture());
    let output = workspace.0.join("approved.json");
    let result = approved::build_approved(
        &artifact,
        &log,
        &output,
        Some(&context),
        Some(&workspace.0),
        &clock,
        &Git,
    )
    .unwrap();
    assert_eq!(result["findingId"], json!(["F0", "F1", "F3", "F4"]));
    assert_eq!(result["unanchored"], 1);
    assert_eq!(result["unanchoredFinding"][0]["findingId"], "F2");
    let document: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(document["findings"][1]["anchor"], anchors[1]);
    approved::check_approved(&output, Some(&context), &clock).unwrap();
    let diff = ss_ai_git::compare(&Git, Path::new("."), &"a".repeat(40), &"b".repeat(40)).unwrap();
    assert_eq!(
        anchors::resolve_anchor_in_diff(&anchors[0], &diff).unwrap()["kind"],
        "line"
    );
    assert_eq!(
        anchors::resolve_anchor_in_diff(
            &json!({"kind":"line","path":"src/lib.rs","startLine":1,"endLine":0}),
            &diff
        )
        .unwrap()["endLine"],
        1
    );
    for start in [json!(0), json!(2147483648_u64), json!(-1), json!(1.5)] {
        assert!(
            anchors::resolve_anchor_in_diff(
                &json!({"kind":"line","path":"src/lib.rs","startLine":start}),
                &diff
            )
            .is_err()
        );
    }
    assert_eq!(
        anchors::resolve_anchor_in_diff(&anchors[1], &diff).unwrap()["wasDowngraded"],
        true
    );
    assert_eq!(
        anchors::resolve_anchor_in_diff(&anchors[3], &diff).unwrap()["kind"],
        "line"
    );
    assert!(anchors::resolve_anchor_in_diff(&json!({"kind":"file","path":"old"}), &diff).is_err());
    assert!(
        anchors::resolve_anchor_in_diff(&json!({"kind": null, "path":"src/lib.rs"}), &diff)
            .is_err()
    );
    assert!(
        anchors::resolve_anchor_in_diff(
            &json!({"kind":"file","path":"src/lib.rs","startLine":1}),
            &diff
        )
        .is_err()
    );
    assert_eq!(
        anchors::resolve_anchor_in_diff(&json!({"kind":"line","path":"gone","startLine":1}), &diff)
            .unwrap()["wasDowngraded"],
        true
    );
}

#[test]
fn freshness_rejects_changed_commits_and_discussion() {
    let workspace = Workspace::new("freshness");
    let clock = SystemClock::default();
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated",
        "findings":[finding("F", json!({"kind":"file","path":"src/lib.rs"}))]}),
    );
    let log = workspace.put("decisions.json", &decisions(&[("F", "approved")]));
    let context = workspace.put("context.json", &capture());
    let output = workspace.0.join("approved.json");
    approved::build_approved(
        &artifact,
        &log,
        &output,
        Some(&context),
        Some(&workspace.0),
        &clock,
        &Git,
    )
    .unwrap();
    let original = fs::read(&output).unwrap();
    let document: Value = serde_json::from_slice(&original).unwrap();
    let request = workspace.put(
        "freshness-request.json",
        &json!({"approved":document,"context":capture()}),
    );
    approved::check_approved_request_path(&request, &clock).unwrap();
    let sidecar = workspace.0.join("context.json.authorship.json");
    assert!(
        approved::build_approved(
            &artifact,
            &log,
            &sidecar,
            Some(&context),
            Some(&workspace.0),
            &clock,
            &Git
        )
        .is_err()
    );
    assert!(!sidecar.exists());
    for (field, value) in [
        ("title", json!("Changed")),
        ("reviewedSourceCommit", json!("c".repeat(40))),
        ("reviewedMergeBaseCommit", json!("d".repeat(40))),
    ] {
        let mut current = capture();
        current["pullRequest"][field] = value;
        workspace.put("context.json", &current);
        assert!(approved::check_approved(&output, Some(&context), &clock).is_err());
        // The request retains the checked snapshot even after the capture file changes.
        approved::check_approved_request_path(&request, &clock).unwrap();
        assert_eq!(original, fs::read(&output).unwrap());
    }
    let mut current = capture();
    current["threads"] = json!([{"id":"T","openingCommentId":"C","kind":"text","resolution":"active",
        "isOutdated":false,"isAuthoredBySelf":false,"context":{"filePath":"src/lib.rs"},"decision":null,
        "url":null,"eventUtc":null,"lastUpdatedUtc":null,"comments":[]}]);
    workspace.put("context.json", &current);
    assert!(approved::check_approved(&output, Some(&context), &clock).is_err());
    assert_eq!(original, fs::read(&output).unwrap());
    assert!(approved::check_approved(&output, None, &clock).is_err());
}

#[test]
fn follow_up_must_name_reply_capable_opening_comment() {
    let workspace = Workspace::new("followup");
    let clock = SystemClock::default();
    let mut record = finding("F", json!({"kind":"file","path":"not-in-diff"}));
    record["followUp"] = json!({"threadId":"T","commentId":"C"});
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":[record]}),
    );
    let log = workspace.put("decisions.json", &decisions(&[("F", "approved")]));
    let mut current = capture();
    current["threads"] = json!([{"id":"T","openingCommentId":"C","kind":"text","resolution":"active",
        "isOutdated":false,"isAuthoredBySelf":false,"context":{"filePath":"src/lib.rs"},"decision":null,
        "url":null,"eventUtc":null,"lastUpdatedUtc":null,"comments":[]}]);
    let context = workspace.put("context.json", &current);
    let output = workspace.0.join("approved.json");
    let result = approved::build_approved(
        &artifact,
        &log,
        &output,
        Some(&context),
        Some(&workspace.0),
        &clock,
        &Git,
    )
    .unwrap();
    assert_eq!(result["approved"], 1);
    current["threads"] = current["threads"][0].clone();
    workspace.put("context.json", &current);
    assert!(
        approved::build_approved(
            &artifact,
            &log,
            &output,
            Some(&context),
            Some(&workspace.0),
            &clock,
            &Git
        )
        .is_ok()
    );
    current["threads"] = json!([current["threads"]]);
    let before = fs::read(&output).unwrap();
    current["threads"][0]["context"] = Value::Null;
    workspace.put("context.json", &current);
    assert!(
        approved::build_approved(
            &artifact,
            &log,
            &output,
            Some(&context),
            Some(&workspace.0),
            &clock,
            &Git
        )
        .is_err()
    );
    assert_eq!(before, fs::read(&output).unwrap());
    current["platform"] = json!("azureDevOps");
    workspace.put("context.json", &current);
    assert!(
        approved::build_approved(
            &artifact,
            &log,
            &output,
            Some(&context),
            Some(&workspace.0),
            &clock,
            &Git
        )
        .is_ok()
    );
    current["threads"][0]["openingCommentId"] = json!("different");
    workspace.put("context.json", &current);
    assert!(
        approved::build_approved(
            &artifact,
            &log,
            &output,
            Some(&context),
            Some(&workspace.0),
            &clock,
            &Git
        )
        .is_err()
    );
}

#[test]
#[cfg_attr(miri, ignore = "requires PowerShell subprocesses")]
fn freshness_does_not_replace_destination_follow_up_rules() {
    let workspace = Workspace::new("destination-followup");
    let clock = SystemClock::default();
    let mut context = capture();
    let comment = |id: &str| {
        json!({"id":id,"parentId":null,"url":null,"publishedUtc":null,"lastEditedUtc":null,
            "author":null,"content":"Comment","authorship":"human","authorshipEvidence":[],
            "isAuthoredBySelf":false})
    };
    context["threads"] = json!([{"id":"T","openingCommentId":"C1","kind":"text","resolution":"active",
        "isOutdated":false,"isAuthoredBySelf":false,"context":{"filePath":"src/lib.rs"},"decision":null,
        "url":null,"eventUtc":null,"lastUpdatedUtc":null,"comments":[comment("C1"),comment("C2")]}]);
    let mut finding = finding("F", json!({"kind":"file","path":"src/lib.rs"}));
    finding["followUp"] = json!({"threadId":"T","commentId":"C2"});
    let approved = json!({"schemaVersion":2,"target":context["target"],
        "reviewedSourceCommit":context["pullRequest"]["reviewedSourceCommit"],
        "reviewedMergeBaseCommit":context["pullRequest"]["reviewedMergeBaseCommit"],
        "reviewedContextHash":ss_ai_review_context::evidence_hash(&context).unwrap(),"findings":[finding]});
    let context_path = workspace.put("context.json", &context);
    let finding_path = workspace.put("finding.json", &finding);
    let approved_path = workspace.put("approved.json", &approved);
    let request = workspace.put(
        "request.json",
        &json!({"approved":approved,"context":context}),
    );
    approved::check_approved(&approved_path, Some(&context_path), &clock).unwrap();
    approved::check_approved_request_path(&request, &clock).unwrap();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/approved_followup_predecessor.ps1");
    let output = Command::new("pwsh")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(script)
        .arg("-Root")
        .arg(root)
        .arg("-ContextPath")
        .arg(context_path)
        .arg("-FindingPath")
        .arg(finding_path)
        .output()
        .expect("PowerShell is required for destination parity coverage");
    assert!(
        output.status.success(),
        "Destination validation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn structured_anchor_requests_validate_diff_and_match_native_resolution() {
    let workspace = Workspace::new("anchor-request");
    let clock = SystemClock::default();
    let diff = ss_ai_git::compare(&Git, Path::new("."), &"a".repeat(40), &"b".repeat(40)).unwrap();
    let files = serde_json::to_value(diff.files()).unwrap();
    let anchor = json!({"kind":"line","path":"src/lib.rs","startLine":1});
    let expected = anchors::resolve_anchor_in_diff(&anchor, &diff).unwrap();
    let request = json!({"anchor":anchor, "diff":{"files":files}});
    let path = workspace.put("request.json", &request);
    assert_eq!(
        anchors::resolve_request_path(&path, &clock).unwrap(),
        expected
    );
    assert_eq!(
        anchors::resolve_request(&json!({"anchor": anchor, "diffFile": files[0]})).unwrap(),
        expected
    );
    let mut invalid = request;
    invalid["diff"]["files"][0]["hunks"][0]["newCount"] = json!(u32::MAX);
    invalid["diff"]["files"][0]["hunks"][0]["newStart"] = json!(2);
    assert!(anchors::resolve_request(&invalid).is_err());
    invalid["diff"]["files"] = json!([files[0], files[0]]);
    assert!(anchors::resolve_request(&invalid).is_err());
    assert!(
        anchors::resolve_request(&json!({"anchor":anchor,"diff":{"files":[]},"diffFile":files[0]}))
            .is_err()
    );
}

#[test]
fn batched_anchor_requests_preserve_results_and_validate_the_whole_diff() {
    let diff = ss_ai_git::compare(&Git, Path::new("."), &"a".repeat(40), &"b".repeat(40)).unwrap();
    let files = serde_json::to_value(diff.files()).unwrap();
    let anchors = json!([
        {"kind":"line","path":"./src/lib.rs","startLine":1},
        {"kind":"line","path":"src/lib.rs","startLine":100},
        {"kind":"file","path":"SRC/lib.rs"},
        {"kind":"file","path":"old"},
        null,
        "malformed"
    ]);
    let request = json!({"anchors":anchors,"diff":{"files":files}});
    let result = anchors::resolve_request(&request).unwrap();
    assert_eq!(
        result["results"][0]["anchor"],
        anchors::resolve_anchor_in_diff(&anchors[0], &diff).unwrap()
    );
    assert_eq!(result["results"][1]["anchor"]["wasDowngraded"], true);
    for index in [2, 3] {
        assert!(
            result["results"][index]["error"]
                .as_str()
                .unwrap()
                .contains("Unresolvable anchor:")
        );
    }
    assert!(result["results"][4]["argumentError"].is_string());
    assert!(result["results"][5]["argumentError"].is_string());

    let mut invalid = request;
    invalid["diff"]["files"][2]["hunks"][0]["oldCount"] = json!(u32::MAX);
    invalid["diff"]["files"][2]["hunks"][0]["oldStart"] = json!(2);
    assert!(anchors::resolve_request(&invalid).is_err());
    invalid["diff"]["files"] = json!([files[0], files[0]]);
    assert!(anchors::resolve_request(&invalid).is_err());
    assert!(anchors::resolve_request(&json!({"anchors": {}, "diff":{"files":[]}})).is_err());
    assert!(
        anchors::resolve_request(&json!({"anchors": [], "diff":{"files":[]}, "anchor":{}}))
            .is_err()
    );
}

#[test]
#[cfg_attr(miri, ignore = "requires PowerShell subprocesses")]
fn reviewed_context_hash_matches_powershell_for_discussion() {
    let workspace = Workspace::new("evidence-parity");
    let clock = SystemClock::default();
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":[]}),
    );
    let mut context = capture();
    context["pullRequest"]["description"] = json!("Details\nWith non-ASCII: ö");
    context["threads"] = json!([{"id":"T","openingCommentId":"C","kind":"text","resolution":"resolved",
        "isOutdated":false,"isAuthoredBySelf":false,"context":{"filePath":"src/lib.rs"},"decision":null,
        "url":null,"eventUtc":null,"lastUpdatedUtc":null,
        "comments":[{"id":"C","parentId":null,"url":null,"publishedUtc":null,"lastEditedUtc":null,
        "author":{"displayName":"Reviewer"},"content":"Discuss the fix\nand its effect.",
        "authorship":"human","authorshipEvidence":["classification:reviewed","provider:human"],
        "isAuthoredBySelf":false}]}]);
    let context_path = workspace.put("context.json", &context);
    let log = workspace.0.join("missing.json");
    let output = workspace.0.join("approved.json");
    approved::build_approved(
        &artifact,
        &log,
        &output,
        Some(&context_path),
        Some(&workspace.0),
        &clock,
        &Git,
    )
    .unwrap();
    let document: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/approved_hash_predecessor.ps1");
    let predecessor = Command::new("pwsh")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(script)
        .arg("-Root")
        .arg(root)
        .arg("-ContextPath")
        .arg(context_path)
        .output()
        .expect("PowerShell is required for evidence parity coverage");
    assert!(
        predecessor.status.success(),
        "PowerShell hashing failed: {}",
        String::from_utf8_lossy(&predecessor.stderr)
    );
    assert_eq!(
        document["reviewedContextHash"].as_str().unwrap(),
        String::from_utf8(predecessor.stdout).unwrap().trim()
    );
}
