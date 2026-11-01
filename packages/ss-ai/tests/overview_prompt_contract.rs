//! Checks the authored overview template against the executable validator.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};

/// Keeps the CLI inputs inside the checkout and removes them after the test.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".overview-prompt-contract-{}", std::process::id()));
        #[expect(clippy::create_dir, reason = "Fixture collisions must fail")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Prompt authors and the validator must agree on section order and content.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn prompt_template_passes_native_overview_validation() {
    let workspace = Workspace::new();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let prompt = fs::read_to_string(root.join("skills/ss-review/prepare-content-overview.md"))
        .unwrap()
        .replace("\r\n", "\n");
    let template = prompt
        .split_once("```text\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    let overview = workspace.0.join("overview.md");
    let capture = workspace.0.join("capture.json");
    let url = "https://example.test/pr/é";
    fs::write(
        &overview,
        template.replacen(
            "# Content overview\n",
            &format!("# Content overview\n\n**Pull request:** {url}\n"),
            1,
        ),
    )
    .unwrap();
    fs::write(
        &capture,
        serde_json::to_vec(&json!({
            "schemaVersion": 3,
            "platform": "gitHub",
            "repositoryUrl": "https://example.test/repo",
            "pullRequestUrl": url,
            "pullRequest": {
                "title": "Title",
                "description": "Body",
                "reviewedSourceCommit": "b".repeat(40),
                "reviewedMergeBaseCommit": "a".repeat(40),
                "isAuthoredBySelf": false
            },
            "reviewers": [],
            "revisions": [],
            "threads": []
        }))
        .unwrap(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["workflow", "validate-overview", "--path"])
        .arg(&overview)
        .arg("--context-path")
        .arg(&capture)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("isValid"), Some(&json!(true)));
}
