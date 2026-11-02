//! Exercises the file-based overview and Markdown command contracts.

#![allow(
    clippy::indexing_slicing,
    clippy::format_collect,
    reason = "The test uses fixed JSON fixtures and small generated documents"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

/// Keeps each process-level fixture inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".workflow-markdown-cli-{}", std::process::id()));
        #[expect(clippy::create_dir, reason = "Fixture collisions must fail")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write_json(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Invokes the installed command boundary with file paths as separate arguments.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .arg("workflow")
        .args(args)
        .output()
        .unwrap()
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Produces the byte-order-marked encodings accepted by `StreamReader`.
fn marked_encodings(value: &str) -> Vec<Vec<u8>> {
    let mut utf8 = vec![0xef, 0xbb, 0xbf];
    utf8.extend_from_slice(value.as_bytes());
    let mut utf16le = vec![0xff, 0xfe];
    let mut utf16be = vec![0xfe, 0xff];
    for word in value.encode_utf16() {
        utf16le.extend_from_slice(&word.to_le_bytes());
        utf16be.extend_from_slice(&word.to_be_bytes());
    }
    let mut utf32le = vec![0xff, 0xfe, 0x00, 0x00];
    let mut utf32be = vec![0x00, 0x00, 0xfe, 0xff];
    for ch in value.chars() {
        utf32le.extend_from_slice(&(ch as u32).to_le_bytes());
        utf32be.extend_from_slice(&(ch as u32).to_be_bytes());
    }
    vec![utf8, utf16le, utf16be, utf32le, utf32be]
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[expect(
    clippy::cognitive_complexity,
    reason = "The test follows one artifact through validation and rendering"
)]
fn validates_overview_and_prepares_rendered_files() {
    let workspace = Workspace::new();
    let capture = workspace.write_json("capture.json", &json!({
        "schemaVersion": 3, "platform":"gitHub", "repositoryUrl":"https://example.test/repo",
        "pullRequestUrl":"https://example.test/pr/é",
        "pullRequest":{"title":"Title","description":"Body","reviewedSourceCommit":"b".repeat(40),
            "reviewedMergeBaseCommit":"a".repeat(40), "isAuthoredBySelf":false},
        "reviewers":[], "revisions":[], "threads":[]
    }));
    let overview = workspace.path("overview.md");
    let headings = [
        "Mission",
        "Substance",
        "Design changes",
        "Significant changes",
        "Structure",
        "Mechanism",
        "Completeness assessment",
        "Related history",
        "Related work",
    ];
    let content = format!(
        "# Content overview\r\n\r\n**Pull request:** https://example.test/pr/é\r\n{}",
        headings
            .iter()
            .map(|name| format!("## {name}\r\nRésumé\r\n"))
            .collect::<String>()
    );
    fs::write(&overview, &content).unwrap();
    let result = run(&[
        "validate-overview",
        "--path",
        text(&overview),
        "--context-path",
        text(&capture),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["isValid"],
        true
    );
    assert!(
        !run(&["validate-overview", "--path", text(&overview)])
            .status
            .success()
    );
    for invalid in [
        content.replace("## Mission", "## Other"),
        content.replace("## Mission\r\nRésumé", "## Mission\r\n"),
        content.replace("## Mission", "## Mission\r\nText\r\n## Mission"),
        content.replace("## Mission", "```\r\n## Mission\r\n```"),
    ] {
        fs::write(&overview, invalid).unwrap();
        assert!(
            !run(&[
                "validate-overview",
                "--path",
                text(&overview),
                "--context-path",
                text(&capture)
            ])
            .status
            .success()
        );
    }
    // Publication-only placeholder and private-path rules do not limit the
    // locally prepared overview.
    fs::write(
        &overview,
        format!("{content}\n{{{{TODO}}}}\n{}", workspace.0.display()),
    )
    .unwrap();
    assert!(
        run(&[
            "validate-overview",
            "--path",
            text(&overview),
            "--context-path",
            text(&capture)
        ])
        .status
        .success()
    );
    fs::write(&overview, &content).unwrap();

    let finding = |id: &str| {
        json!({"schemaVersion":1, "id":id, "sourceFindingIds":[], "origin":"reviewer",
        "title":"Problème", "description":"Résumé", "reasoning":"Pourquoi", "consequence":"Impact",
        "recommendedAction":"Corriger", "anchor":{"kind":"file","path":"src/lib.rs"}})
    };
    let approved = workspace.write_json(
        "approved.json",
        &json!({
            "schemaVersion":2, "target":{"platform":"gitHub"}, "reviewedSourceCommit":"bound",
            "reviewedMergeBaseCommit":"bound", "reviewedContextHash":"bound",
            "findings":[finding("F1"),finding("F2")]
        }),
    );
    let output = workspace.path("sortie é");
    let audit = workspace.path("audit é.md");
    let combined = workspace.path("combined.md");
    let args = [
        "prepare-markdown",
        "--path",
        text(&approved),
        "--output-directory",
        text(&output),
        "--audit-document-path",
        text(&audit),
        "--combined-path",
        text(&combined),
        "--overview-path",
        text(&overview),
        "--finding-id",
        " F2,F2 ",
    ];
    let result = run(&args);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["findings"].as_array().unwrap().len(), 1);
    assert_eq!(result["findings"][0]["findingId"], "F2");
    assert!(
        result["auditDocumentUri"]
            .as_str()
            .unwrap()
            .contains("audit%20%C3%A9.md")
    );
    let audit_text = fs::read_to_string(&audit).unwrap();
    assert!(audit_text.contains("Résumé\n"));
    assert!(audit_text.contains("**Finding `F2`**"));
    assert!(!audit_text.contains("**Finding `F1`**"));
    assert_eq!(
        fs::read_to_string(&combined)
            .unwrap()
            .matches("Problème")
            .count(),
        2
    );
    // Both operations must accept the same BOM-selected encodings and preserve
    // the overview's Unicode content, including non-BMP characters.
    let encoded_content = content.replace("Résumé", "Résumé 🦀");
    for bytes in marked_encodings(&encoded_content) {
        fs::write(&overview, bytes).unwrap();
        let validation = run(&[
            "validate-overview",
            "--path",
            text(&overview),
            "--context-path",
            text(&capture),
        ]);
        assert!(
            validation.status.success(),
            "{}",
            String::from_utf8_lossy(&validation.stderr)
        );
        let rendering = run(&args);
        assert!(
            rendering.status.success(),
            "{}",
            String::from_utf8_lossy(&rendering.stderr)
        );
        assert!(
            fs::read_to_string(&audit)
                .unwrap()
                .starts_with(&encoded_content.replace("\r\n", "\n").trim_end().to_owned())
        );
    }
    fs::write(&overview, &content).unwrap();

    let artifact = workspace.write_json(
        "artifact.json",
        &json!({
            "schemaVersion":1, "kind":"consolidated", "findings":[finding("F1")]
        }),
    );
    let tracker_output = workspace.path("tracker");
    let result = run(&[
        "prepare-markdown",
        "--path",
        text(&artifact),
        "--output-directory",
        text(&tracker_output),
        "--destination",
        "tracker",
        "--context-path",
        text(&capture),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        fs::read_to_string(tracker_output.join("F1.md"))
            .unwrap()
            .contains("https://example.test/pr/é")
    );

    let invalid_output = workspace.path("invalid-output");
    let result = run(&[
        "prepare-markdown",
        "--path",
        text(&approved),
        "--output-directory",
        text(&invalid_output),
        "--overview-path",
        text(&overview),
    ]);
    assert!(!result.status.success());
    assert!(!invalid_output.exists());
    let result = run(&[
        "prepare-markdown",
        "--path",
        text(&artifact),
        "--output-directory",
        text(&invalid_output),
        "--context-path",
        text(&capture),
    ]);
    assert!(!result.status.success());
    assert!(!invalid_output.exists());
    let bad_overview = workspace.path("invalid-utf8.md");
    let mut invalid_text = content.as_bytes().to_vec();
    invalid_text.push(0xff);
    fs::write(&bad_overview, &invalid_text).unwrap();
    assert!(
        !run(&[
            "validate-overview",
            "--path",
            text(&bad_overview),
            "--context-path",
            text(&capture)
        ])
        .status
        .success()
    );
    let replacement_output = workspace.path("replacement-output");
    let result = run(&[
        "prepare-markdown",
        "--path",
        text(&artifact),
        "--output-directory",
        text(&replacement_output),
        "--overview-path",
        text(&bad_overview),
        "--audit-document-path",
        text(&audit),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(fs::read_to_string(&audit).unwrap().contains('\u{fffd}'));
    let result = run(&[
        "prepare-markdown",
        "--path",
        text(&approved),
        "--output-directory",
        text(&invalid_output),
        "--finding-id",
        "missing",
    ]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("missing"));
    assert!(!invalid_output.exists());

    let invalid = workspace.write_json("invalid.json", &json!({"schemaVersion":2,"findings":[]}));
    assert!(
        !run(&[
            "prepare-markdown",
            "--path",
            text(&invalid),
            "--output-directory",
            text(&invalid_output)
        ])
        .status
        .success()
    );
    let result = run(&[
        "validate-findings",
        "--path",
        text(&invalid),
        "--fail-on-invalid",
    ]);
    assert!(!result.status.success());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["isValid"], false);
    assert!(!report["errors"].as_array().unwrap().is_empty());
    let reviewer = workspace.write_json(
        "reviewer.json",
        &json!({"schemaVersion":1,"kind":"reviewer","reviewerId":"alice",
            "focusArea":"correctness","status":"open","findings":[]}),
    );
    let result = run(&[
        "validate-findings",
        "--path",
        text(&reviewer),
        "--reviewer-id",
        "alice",
        "--reviewer-id",
        "bob",
        "--fail-on-invalid",
    ]);
    assert!(!result.status.success());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["isValid"], false);
    assert_eq!(report["missingReviewers"], json!(["bob"]));
    assert_eq!(report["incompleteReviewers"], json!(["alice"]));
    fs::write(
        &reviewer,
        serde_json::to_vec(
            &json!({"schemaVersion":1,"kind":"reviewer","reviewerId":"alice",
            "focusArea":"correctness","status":"complete","findings":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    let result = run(&[
        "validate-findings",
        "--path",
        text(&reviewer),
        "--reviewer-id",
        "alice",
        "--fail-on-invalid",
    ]);
    assert!(result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["isValid"],
        true
    );
    let result = run(&["validate-findings", "--path", text(&invalid)]);
    assert!(result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["isValid"],
        false
    );
}
