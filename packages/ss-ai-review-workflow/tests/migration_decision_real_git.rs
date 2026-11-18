//! Exercises approved finding anchors against an isolated real Git repository.
#![allow(
    clippy::indexing_slicing,
    reason = "The assertions use fixed JSON fixture shapes"
)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::evidence_hash;
use ss_ai_review_workflow::approved;
use ss_ai_runtime::{Clock, NativeProcess, Process, ProcessOutput, fail};

/// Refuses contention rather than consulting the wall clock or sleeping.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected lock contention"))
    }
}

/// Runs Git with a child-only environment isolated from the user's checkout.
struct IsolatedGit(PathBuf);

impl Process for IsolatedGit {
    fn execute(&self, program: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "git");
        NativeProcess::new(&self.0).isolated_git(&self.0, arguments)
    }
}

/// Owns the fixture repository and approved artifacts inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".decision-real-git-{}", std::process::id()));
        #[expect(clippy::create_dir, reason = "A collision must not reuse test data")]
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

/// Runs fixture Git commands with deterministic metadata and no inherited Git state.
fn git(root: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(name);
        }
    }
    let output = command
        .args([
            "-C",
            root.to_str().unwrap(),
            "-c",
            "user.name=Review Runner",
            "-c",
            "user.email=runner@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("missing-git-config"))
        .env("GIT_AUTHOR_DATE", "2024-05-06T09:10:11+00:00")
        .env("GIT_COMMITTER_DATE", "2024-05-06T09:10:11+00:00")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
#[cfg_attr(miri, ignore = "requires Git subprocesses")]
#[cfg_attr(mutants, ignore = "A mutated Git operation may block a subprocess")]
fn real_git_approval_rebinds_capture_and_filters_only_missing_files() {
    let workspace = Workspace::new();
    let repo = workspace.0.join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "--quiet"]);
    // The file extends beyond the finding's anchor, which lies outside the
    // changed hunk: approval eligibility is independent of inline placement.
    let content = "unchanged line\n".repeat(60);
    fs::write(repo.join("src/retry.rs"), &content).unwrap();
    fs::write(repo.join("src/parse.rs"), "unchanged\n").unwrap();
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "--quiet", "-m", "baseline"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(
        repo.join("src/retry.rs"),
        content.replacen("unchanged line\n", "changed line\n", 1),
    )
    .unwrap();
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "--quiet", "-m", "change"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let finding = |id, anchor| {
        json!({"schemaVersion":1,"id":id,"origin":"reviewer","sourceFindingIds":[],
        "title":"Problem","description":"Details","reasoning":"Evidence",
        "consequence":"Impact","recommendedAction":"Fix","anchor":anchor})
    };
    let artifact = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated",
        "findings":[finding("F1",json!({"kind":"line","path":"src/retry.rs","startLine":42})),
                    finding("F2",json!({"kind":"file","path":"src/parse.rs"}))],"removed":[]}),
    );
    let journal = workspace.put(
        "decisions.json",
        &json!({"schemaVersion":1,"decisions":[
        {"findingId":"F1","decision":"auto-approved","recordedUtc":"2024-05-06T09:10:11Z"},
        {"findingId":"F2","decision":"approved","recordedUtc":"2024-05-06T09:10:11Z"}]}),
    );
    let context = json!({"schemaVersion":3,"platform":"gitHub","repositoryUrl":"https://example.test/repo",
        "pullRequestUrl":"https://example.test/pr/1","target":{"platform":"gitHub","pullRequestId":1},
        "pullRequest":{"title":"Title","description":"Body","reviewedMergeBaseCommit":base,
            "reviewedSourceCommit":head},"reviewers":[],"revisions":[],"threads":[]});
    let context_path = workspace.put("capture.json", &context);
    let output = workspace.0.join("approved.json");
    let process = IsolatedGit(repo.clone());
    let result = approved::build_approved(
        &artifact,
        &journal,
        &output,
        Some(&context_path),
        Some(&repo),
        &FixedClock,
        &process,
    )
    .unwrap();
    assert_eq!(result["findingId"], json!(["F1"]));
    assert_eq!(result["autoApproved"], 1);
    assert_eq!(result["manuallyApproved"], 0);
    assert_eq!(result["dropped"], 0);
    assert_eq!(result["unanchoredFinding"][0]["findingId"], "F2");
    assert_eq!(result["unanchoredFinding"][0]["path"], "src/parse.rs");
    let document: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(document["target"], context["target"]);
    assert_eq!(document["reviewedSourceCommit"], head);
    assert_eq!(document["reviewedMergeBaseCommit"], base);
    assert_eq!(
        document["reviewedContextHash"],
        evidence_hash(&context).unwrap()
    );
    assert_eq!(document["findings"][0]["anchor"]["startLine"], 42);
    let before = fs::read(&output).unwrap();
    let missing = workspace.0.join("empty-repo");
    fs::create_dir_all(&missing).unwrap();
    git(&missing, &["init", "--quiet"]);
    let missing_process = IsolatedGit(missing.clone());
    approved::build_approved(
        &artifact,
        &journal,
        &output,
        Some(&context_path),
        Some(&missing),
        &FixedClock,
        &missing_process,
    )
    .unwrap_err();
    assert_eq!(fs::read(&output).unwrap(), before);
}
