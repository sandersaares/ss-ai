//! Isolated Git objects and real CLI coverage for exact comparison consumers.
#![allow(
    clippy::indexing_slicing,
    reason = "Assertions access explicit JSON fixture fields"
)]

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use ss_ai::{NativeProcess, run_git};
use tempfile::TempDir;

/// A private repository with deterministic identity and no developer Git hooks.
struct Repository {
    root: TempDir,
}

impl Repository {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("git comparison with spaces ")
            .tempdir()
            .unwrap();
        let repository = Self { root };
        repository.git(&["init", "--quiet", "--initial-branch=main"]);
        repository
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn git(&self, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=",
                "-c",
                "commit.gpgSign=false",
            ])
            .args(arguments)
            .current_dir(self.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.path().join("unused-config"))
            .output()
            .unwrap();
        successful(&output);
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn commit(&self) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--message", "Fixture"]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn compare(&self, old: &str, new: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ss-ai"))
            .args(["git", "diff", "--repository-path"])
            .arg(self.path())
            .args(["--old-commit", old, "--new-commit", new])
            .output()
            .unwrap()
    }
}

fn successful(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn value(output: &Output) -> Value {
    successful(output);
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn borrowed_arguments_keep_the_original_executable_name_in_help() {
    let arguments = [
        OsString::from("custom-executable"),
        OsString::from("git"),
        OsString::from("diff"),
        OsString::from("--help"),
    ];
    let local = arguments
        .first()
        .into_iter()
        .chain(arguments.iter().skip(2));
    let output = run_git(local, &NativeProcess::new(Path::new("."))).unwrap();
    assert!(output.contains("custom-executable git diff"));
}

#[test]
fn exact_comparison_retains_force_push_removals_without_checkout_changes() {
    let repository = Repository::new();
    fs::write(repository.path().join("kept.txt"), "unchanged\n").unwrap();
    let base = repository.commit();
    fs::write(repository.path().join("removed.txt"), "source only\n").unwrap();
    let approved = repository.commit();
    repository.git(&["checkout", "--quiet", &base]);
    fs::write(repository.path().join("new.txt"), "rewritten history\n").unwrap();
    let current = repository.commit();
    fs::write(repository.path().join("kept.txt"), "uncommitted change\n").unwrap();
    let before = repository.git(&["status", "--porcelain=v1"]);
    let diff = value(&repository.compare(&approved, &current));
    assert_eq!(diff["oldCommit"], approved);
    assert_eq!(diff["newCommit"], current);
    let files = diff["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert!(
        files
            .iter()
            .any(|file| file["path"] == "removed.txt" && file["status"] == "deleted")
    );
    assert!(
        files
            .iter()
            .any(|file| file["path"] == "new.txt" && file["status"] == "added")
    );
    assert_eq!(repository.git(&["status", "--porcelain=v1"]), before);
    assert_eq!(repository.git(&["rev-parse", "HEAD"]), current);
    assert_eq!(
        fs::read_to_string(repository.path().join("kept.txt")).unwrap(),
        "uncommitted change\n"
    );
}

#[test]
fn comparison_preserves_unicode_modes_binary_and_renames() {
    let repository = Repository::new();
    let unicode = "caf\u{e9} \u{1f916}.txt";
    fs::write(repository.path().join(unicode), "before\n").unwrap();
    fs::write(repository.path().join("binary"), [0, 1, 2]).unwrap();
    fs::write(repository.path().join("mode"), "unchanged\n").unwrap();
    fs::write(repository.path().join("old name"), "rename unchanged\n").unwrap();
    let old = repository.commit();
    fs::write(repository.path().join(unicode), "after\n").unwrap();
    fs::write(repository.path().join("binary"), [0, 1, 3]).unwrap();
    fs::rename(
        repository.path().join("old name"),
        repository.path().join("new name"),
    )
    .unwrap();
    repository.git(&["add", "--all"]);
    repository.git(&["update-index", "--chmod=+x", "mode"]);
    repository.git(&["commit", "--quiet", "--message", "Updated fixture"]);
    let new = repository.git(&["rev-parse", "HEAD"]);
    // Hostile presentation settings must not replace or omit the comparison.
    repository.git(&["config", "diff.external", "nonexistent-external-diff"]);
    repository.git(&["config", "diff.relative", "true"]);
    let diff = value(&repository.compare(&old, &new));
    let files = diff["files"].as_array().unwrap();
    assert_eq!(files.len(), 4);
    assert!(files.iter().any(|file| file["path"] == unicode));
    for path in ["mode", "binary"] {
        let file = files.iter().find(|file| file["path"] == path).unwrap();
        assert_eq!(file["status"], "modified");
        assert!(file["hunks"].as_array().unwrap().is_empty());
    }
    let renamed = files
        .iter()
        .find(|file| file["path"] == "new name")
        .unwrap();
    assert_eq!(renamed["status"], "renamed");
    assert_eq!(renamed["previousPath"], "old name");
}

#[test]
fn missing_objects_and_revision_expressions_never_emit_success() {
    let repository = Repository::new();
    for old in ["HEAD", "--help", &"a".repeat(40)] {
        let output = repository.compare(old, &"b".repeat(40));
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    fs::write(repository.path().join("file"), "content\n").unwrap();
    let commit = repository.commit();
    repository.git(&[
        "-c",
        "tag.gpgSign=false",
        "tag",
        "-a",
        "tag",
        "-m",
        "Fixture",
    ]);
    let tag = repository.git(&["rev-parse", "tag"]);
    let tree = repository.git(&["show", "--no-patch", "--format=%T", "HEAD"]);
    for object in [tag, tree, "b".repeat(40)] {
        for output in [
            repository.compare(&object, &commit),
            repository.compare(&commit, &object),
        ] {
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
fn parser_stdin_preserves_header_shaped_content_and_rejects_truncation() {
    let prefix = "diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n";
    for (body, succeeds) in [
        ("--- a/not-a-file\n+++ b/not-a-file\n", true),
        ("-missing\n", false),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
            .args(["git", "parse"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{prefix}{body}").as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.success(), succeeds);
        if succeeds {
            assert_eq!(value(&output)["files"][0]["path"], "file");
        } else {
            assert!(output.stdout.is_empty());
        }
    }
}
