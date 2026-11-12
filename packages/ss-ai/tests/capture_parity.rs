//! Compares complete native refresh transactions with retained provider captures.
#![allow(
    clippy::indexing_slicing,
    reason = "The fixture shapes and command arguments are fixed by these tests"
)]

use std::env::{join_paths, split_paths, var_os};
use std::ffi::OsString;
use std::fs::{copy, create_dir, create_dir_all, read, remove_dir_all, write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_slice, json, to_vec};
use sha2::{Digest, Sha256};
use ss_ai::Clock;
use ss_ai_review_context::{document_hash, read_capture, snapshot_id};
use uuid::Uuid;

/// Keeps capture timestamps deterministic and treats retries as test failures.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2026-08-04T12:00:00Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected retry")
    }
}

/// Owns the capture outputs and their locks inside the active checkout.
struct Output(PathBuf);

impl Output {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".capture-parity-{}", Uuid::new_v4()));
        #[expect(
            clippy::create_dir,
            reason = "An existing directory must fail instead of being removed by test cleanup"
        )]
        create_dir(&path).unwrap();
        Self(path)
    }

    fn capture(&self, prefix: &str) -> PathBuf {
        self.0.join(format!("{prefix}pr-context.json"))
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        remove_dir_all(&self.0).unwrap();
    }
}

/// Loads the immutable provider responses and PowerShell-generated expectations.
fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ss-review-native-capture")
        .join(format!("{name}.json"));
    from_slice(&read(path).unwrap()).unwrap()
}

/// Reads the exact semantic sidecar without applying it to the capture.
fn sidecar(path: &Path) -> Value {
    let mut name = path.as_os_str().to_os_string();
    name.push(".authorship.json");
    from_slice(&read(name).unwrap()).unwrap()
}

/// Locates the prepared launcher and rejects stale or modified test prerequisites.
fn prepared_provider() -> PathBuf {
    let path = var_os("SS_AI_RUST_TEST_PREREQUISITES")
        .expect("Prepare the selected Rust tests with scripts/Invoke-Tests.ps1 first");
    let record: Value = from_slice(&read(path).unwrap()).unwrap();
    assert_eq!(record["schemaVersion"], 3);
    assert_eq!(record["status"], "complete");
    let source = PathBuf::from(record["provider"]["source"]["path"].as_str().unwrap());
    let executable = PathBuf::from(record["provider"]["executable"]["path"].as_str().unwrap());
    assert!(source.is_absolute() && executable.is_absolute());
    let source_bytes = read(&source).unwrap();
    assert_eq!(source_bytes, include_bytes!("support/provider.rs"));
    assert_eq!(
        format!("{:X}", Sha256::digest(&source_bytes)),
        record["provider"]["source"]["sha256"]
    );
    assert_eq!(
        format!("{:X}", Sha256::digest(read(&executable).unwrap())),
        record["provider"]["executable"]["sha256"]
    );
    executable
}

/// Installs the prepared provider only inside the test's private launcher directory.
fn install_github_provider(output: &Output) -> OsString {
    let launcher = output
        .0
        .join(if cfg!(windows) { "provider.exe" } else { "gh" });
    copy(prepared_provider(), launcher).unwrap();
    #[cfg(windows)]
    write(output.0.join("gh.cmd"), "@\"%~dp0provider.exe\" %*\r\n").unwrap();
    let mut paths = vec![output.0.clone()];
    paths.extend(split_paths(&var_os("PATH").unwrap()));
    join_paths(paths).unwrap()
}

/// Gives the standalone launcher the same response bytes as the injected provider.
fn write_github_responses(output: &Output, edited: bool) {
    let mut data = fixture("github");
    let responses = &mut data["responses"];
    if edited {
        responses["reviewThreads"]["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"]
            [0]["comments"]["nodes"][0]["body"] = json!("Edited 🤖");
    }
    let directory = output.0.join("responses");
    create_dir_all(&directory).unwrap();
    for key in [
        "pullRequest",
        "reviews",
        "issueComments",
        "requestedReviewers",
        "reviewThreads",
        "issueEdits",
        "reviewEdits",
        "timeline",
        "user",
        "compare",
    ] {
        // The real gh --paginate --slurp boundary wraps each page.
        let value = if ["reviews", "issueComments", "requestedReviewers"].contains(&key) {
            json!([responses[key]])
        } else {
            responses[key].clone()
        };
        write(
            directory.join(format!("{key}.json")),
            to_vec(&value).unwrap(),
        )
        .unwrap();
    }
}

/// Keeps capture command parsing within the stack budget used by Windows.
fn capture_command() -> Command {
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "ulimit -c 0 && ulimit -s 768 && exec \"$@\"",
            "sh",
            env!("CARGO_BIN_EXE_ss-ai"),
        ]);
        command
    }

    #[cfg(not(target_os = "linux"))]
    {
        Command::new(env!("CARGO_BIN_EXE_ss-ai"))
    }
}

/// Invokes the real executable with a private PATH and no usable network provider.
fn cli(output: &Output, path: &OsString, arguments: &[OsString]) -> Value {
    let result = capture_command()
        .args(arguments)
        .env("PATH", path)
        .env("FIXTURE_ROOT", &output.0)
        .env("FIXTURE_CAPTURE", "1")
        .current_dir(&output.0)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "native command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    from_slice(&result.stdout).unwrap()
}

/// Exercises the actual binary, process launcher, and immutable refresh transaction.
#[test]
#[cfg_attr(miri, ignore = "Miri does not support subprocess execution")]
fn github_capture_works_with_real_cli_processes() {
    let output = Output::new();
    let path = install_github_provider(&output);
    write_github_responses(&output, false);
    let golden = fixture("github-refresh-parity");
    let baseline = output.capture("process-baseline-");
    let args = [
        "context".into(),
        "create".into(),
        "--url".into(),
        "https://github.com/octo/widget/pull/7".into(),
        "--output-directory".into(),
        output.0.as_os_str().to_owned(),
        "--output-prefix".into(),
        "process-baseline-".into(),
        "--now-utc".into(),
        "2026-08-04T12:00:00Z".into(),
    ];
    cli(&output, &path, &args);
    let raw: Value = from_slice(&read(&baseline).unwrap()).unwrap();
    assert_eq!(raw, fixture("github-expected"));
    assert_eq!(document_hash(&raw).unwrap(), golden["baselineRawHash"]);
    let classification = json!([{"threadId":"PRRT_1","commentId":"PRRC_1","kind":"human"}]);
    cli(
        &output,
        &path,
        &[
            "context".into(),
            "set-authorship".into(),
            "--path".into(),
            baseline.as_os_str().to_owned(),
            "--snapshot-id".into(),
            snapshot_id(&raw).unwrap().into(),
            "--classification".into(),
            classification.to_string().into(),
        ],
    );
    assert_eq!(sidecar(&baseline), golden["baselineDecisions"]);
    write_github_responses(&output, true);
    let result = cli(
        &output,
        &path,
        &[
            "context".into(),
            "create".into(),
            "--url".into(),
            "https://github.com/octo/widget/pull/7".into(),
            "--output-directory".into(),
            output.0.as_os_str().to_owned(),
            "--output-prefix".into(),
            "process-refresh-".into(),
            "--compare-path".into(),
            baseline.as_os_str().to_owned(),
            "--now-utc".into(),
            "2026-08-04T12:00:00Z".into(),
        ],
    );
    let refreshed = output.capture("process-refresh-");
    let raw: Value = from_slice(&read(&refreshed).unwrap()).unwrap();
    assert_eq!(raw, fixture("github-refresh-expected"));
    assert_eq!(sidecar(&refreshed), golden["refreshDecisions"]);
    assert_eq!(result["delta"], golden["delta"]);
    assert_eq!(
        read_capture(&refreshed, &FixedClock).unwrap(),
        golden["refreshEffective"]
    );
}
