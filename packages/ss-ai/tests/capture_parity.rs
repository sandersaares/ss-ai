//! Compares complete native refresh transactions with retained provider captures.
#![allow(
    clippy::indexing_slicing,
    reason = "The fixture shapes and command arguments are fixed by these tests"
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput, run};
use ss_ai_review_context::{document_hash, evidence_hash, read_capture, snapshot_id};
use uuid::Uuid;

/// Replays only the read requests made by the two fixed provider fixtures.
struct Provider {
    github: RefCell<VecDeque<(&'static str, Value)>>,
    azure: RefCell<VecDeque<(&'static str, Value)>>,
}

impl Provider {
    /// Builds a fresh request sequence so each capture has independent inputs.
    fn new(name: &str, edited: bool) -> Self {
        let mut fixture = fixture(name);
        let responses = &mut fixture["responses"];
        if edited {
            if name == "github" {
                responses["reviewThreads"]["data"]["repository"]["pullRequest"]["reviewThreads"]
                    ["nodes"][0]["comments"]["nodes"][0]["body"] = json!("Edited 🤖");
            } else {
                responses["threads"]["value"][0]["comments"][0]["content"] = json!("Edited 🤖");
            }
        }
        let mut github = VecDeque::new();
        let mut azure = VecDeque::new();
        if name == "github" {
            github.extend([
                ("repos/octo/widget/pulls/7", responses["pullRequest"].clone()),
                (
                    "repos/octo/widget/pulls/7/reviews?per_page=100",
                    json!([responses["reviews"]]),
                ),
                (
                    "repos/octo/widget/issues/7/comments?per_page=100",
                    json!([responses["issueComments"]]),
                ),
                (
                    "repos/octo/widget/pulls/7/requested_reviewers?per_page=100",
                    json!([responses["requestedReviewers"]]),
                ),
                ("graphql", responses["reviewThreads"].clone()),
                ("graphql", responses["issueEdits"].clone()),
                ("graphql", responses["reviewEdits"].clone()),
                ("graphql", responses["timeline"].clone()),
                ("user", responses["user"].clone()),
                (
                    "repos/octo/widget/compare/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb...aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    responses["compare"].clone(),
                ),
            ]);
        } else {
            azure.extend([
                ("_apis/connectionData", responses["connectionData"].clone()),
                ("pullRequests/4242?", responses["pullRequest"].clone()),
                ("/iterations?", responses["iterations"].clone()),
                ("/reviewers?", responses["reviewers"].clone()),
                ("/threads?", responses["threads"].clone()),
            ]);
        }
        Self {
            github: RefCell::new(github),
            azure: RefCell::new(azure),
        }
    }

    /// Missing and unexpected reads both indicate a truncated provider capture.
    fn assert_complete(&self) {
        assert!(self.github.borrow().is_empty());
        assert!(self.azure.borrow().is_empty());
    }
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if program == "az" {
            assert_eq!(&arguments[..2], ["account", "get-access-token"]);
            return Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]));
        }
        assert_eq!(program, "gh");
        let (route, response) = self
            .github
            .borrow_mut()
            .pop_front()
            .expect("unexpected request");
        assert_eq!(arguments[1], route);
        let method = &arguments
            .windows(2)
            .find(|pair| pair[0] == "--method")
            .unwrap()[1];
        if route == "graphql" {
            assert_eq!(method, "POST");
            let query: Value = serde_json::from_slice(input).unwrap();
            assert!(query["query"].as_str().unwrap().starts_with("query"));
        } else {
            assert_eq!(method, "GET");
            assert!(input.is_empty());
        }
        Ok(ProcessOutput::new(
            true,
            serde_json::to_vec(&response).unwrap(),
            vec![],
        ))
    }
}

impl Http for Provider {
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(method, "GET");
        assert_eq!(token, "fixture-token");
        assert!(body.is_none());
        let (route, response) = self
            .azure
            .borrow_mut()
            .pop_front()
            .expect("unexpected request");
        assert!(url.contains(route), "unexpected URL: {url}");
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            serde_json::to_vec(&response).unwrap(),
        ))
    }
}

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
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn capture(&self, prefix: &str) -> PathBuf {
        self.0.join(format!("{prefix}pr-context.json"))
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Loads the immutable provider responses and PowerShell-generated expectations.
fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ss-review-native-capture")
        .join(format!("{name}.json"));
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Runs the production command router with injected provider effects.
fn command(arguments: &[OsString], provider: &Provider) -> Value {
    serde_json::from_str(&run(arguments, provider, provider, &FixedClock).unwrap()).unwrap()
}

/// Captures one provider revision to an immutable destination.
fn create(
    name: &str,
    output: &Output,
    prefix: &str,
    compare: Option<&Path>,
    edited: bool,
) -> Value {
    let provider = Provider::new(name, edited);
    let expected = fixture(&format!("{name}-expected"));
    let mut args: Vec<OsString> = [
        "ss-ai",
        "context",
        "create",
        "--url",
        expected["pullRequestUrl"].as_str().unwrap(),
        "--output-directory",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(output.0.as_os_str().to_owned());
    args.extend(["--output-prefix", prefix].map(OsString::from));
    if let Some(compare) = compare {
        args.push("--compare-path".into());
        args.push(compare.as_os_str().to_owned());
    }
    let result = command(&args, &provider);
    provider.assert_complete();
    result
}

/// Reads the exact semantic sidecar without applying it to the capture.
fn sidecar(path: &Path) -> Value {
    let mut name = path.as_os_str().to_os_string();
    name.push(".authorship.json");
    serde_json::from_slice(&fs::read(name).unwrap()).unwrap()
}

/// Locates the prepared launcher and rejects stale or modified test prerequisites.
fn prepared_provider() -> PathBuf {
    let path = std::env::var_os("SS_AI_RUST_TEST_PREREQUISITES")
        .expect("Prepare the selected Rust tests with scripts/Invoke-Tests.ps1 first");
    let record: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(record["schemaVersion"], 3);
    assert_eq!(record["status"], "complete");
    let source = PathBuf::from(record["provider"]["source"]["path"].as_str().unwrap());
    let executable = PathBuf::from(record["provider"]["executable"]["path"].as_str().unwrap());
    assert!(source.is_absolute() && executable.is_absolute());
    let source_bytes = fs::read(&source).unwrap();
    assert_eq!(source_bytes, include_bytes!("support/provider.rs"));
    assert_eq!(
        format!("{:X}", Sha256::digest(&source_bytes)),
        record["provider"]["source"]["sha256"]
    );
    assert_eq!(
        format!("{:X}", Sha256::digest(fs::read(&executable).unwrap())),
        record["provider"]["executable"]["sha256"]
    );
    executable
}

/// Installs the prepared provider only inside the test's private launcher directory.
fn install_github_provider(output: &Output) -> OsString {
    let launcher = output
        .0
        .join(if cfg!(windows) { "provider.exe" } else { "gh" });
    fs::copy(prepared_provider(), launcher).unwrap();
    #[cfg(windows)]
    fs::write(output.0.join("gh.cmd"), "@\"%~dp0provider.exe\" %*\r\n").unwrap();
    let mut paths = vec![output.0.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(paths).unwrap()
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
    fs::create_dir_all(&directory).unwrap();
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
        fs::write(
            directory.join(format!("{key}.json")),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
}

/// Invokes the real executable with a private PATH and no usable network provider.
fn cli(output: &Output, path: &OsString, arguments: &[OsString]) -> Value {
    let result = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
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
    serde_json::from_slice(&result.stdout).unwrap()
}

/// Exercises the actual binary, process launcher, and immutable refresh transaction.
#[test]
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
    let raw: Value = serde_json::from_slice(&fs::read(&baseline).unwrap()).unwrap();
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
    let raw: Value = serde_json::from_slice(&fs::read(&refreshed).unwrap()).unwrap();
    assert_eq!(raw, fixture("github-refresh-expected"));
    assert_eq!(sidecar(&refreshed), golden["refreshDecisions"]);
    assert_eq!(result["delta"], golden["delta"]);
    assert_eq!(
        read_capture(&refreshed, &FixedClock).unwrap(),
        golden["refreshEffective"]
    );
}

/// Exercises classification, unchanged reuse, and edited-input rejection for both services.
#[test]
fn provider_capture_refresh_matches_powershell_expectations() {
    for name in ["github", "azure-devops"] {
        let output = Output::new();
        let golden = fixture(&format!("{name}-refresh-parity"));
        let baseline = output.capture("baseline-");
        create(name, &output, "baseline-", None, false);
        let raw_bytes = fs::read(&baseline).unwrap();
        let raw: Value = serde_json::from_slice(&raw_bytes).unwrap();
        assert_eq!(raw, fixture(&format!("{name}-expected")), "{name}");
        assert_eq!(document_hash(&raw).unwrap(), golden["baselineRawHash"]);
        assert_eq!(evidence_hash(&raw).unwrap(), golden["baselineEvidenceHash"]);
        assert_eq!(snapshot_id(&raw).unwrap(), golden["baselineSnapshot"]);

        let classifications = if name == "github" {
            json!([{"threadId":"PRRT_1","commentId":"PRRC_1","kind":"human"}])
        } else {
            json!([
                {"threadId":"101","commentId":"1","kind":"human"},
                {"threadId":"101","commentId":"2","kind":"agent"}
            ])
        };
        let provider = Provider::new(name, false);
        let args = [
            "ss-ai".into(),
            "context".into(),
            "set-authorship".into(),
            "--path".into(),
            baseline.as_os_str().to_owned(),
            "--snapshot-id".into(),
            snapshot_id(&raw).unwrap().into(),
            "--classification".into(),
            classifications.to_string().into(),
        ];
        command(&args, &provider);
        assert_eq!(sidecar(&baseline), golden["baselineDecisions"]);
        let classified = read_capture(&baseline, &FixedClock).unwrap();
        assert_eq!(
            snapshot_id(&classified).unwrap(),
            golden["classifiedSnapshot"]
        );
        assert_eq!(fs::read(&baseline).unwrap(), raw_bytes);

        let unchanged = output.capture("unchanged-");
        let unchanged_result = create(name, &output, "unchanged-", Some(&baseline), false);
        assert_eq!(
            unchanged_result["reusedClassificationCount"],
            golden["baselineDecisions"]["decisions"]
                .as_array()
                .unwrap()
                .len()
        );
        assert_eq!(read_capture(&unchanged, &FixedClock).unwrap(), classified);
        assert_eq!(unchanged_result["delta"]["hasChanges"], false);

        let refreshed = output.capture("edited-");
        let result = create(name, &output, "edited-", Some(&baseline), true);
        let refresh_raw: Value = serde_json::from_slice(&fs::read(&refreshed).unwrap()).unwrap();
        let effective = read_capture(&refreshed, &FixedClock).unwrap();
        assert_eq!(refresh_raw, fixture(&format!("{name}-refresh-expected")));
        assert_eq!(
            document_hash(&refresh_raw).unwrap(),
            golden["refreshRawHash"]
        );
        assert_eq!(
            evidence_hash(&effective).unwrap(),
            golden["refreshEvidenceHash"]
        );
        assert_eq!(snapshot_id(&effective).unwrap(), golden["refreshSnapshot"]);
        assert_eq!(effective, golden["refreshEffective"]);
        assert_eq!(sidecar(&refreshed), golden["refreshDecisions"]);
        assert_eq!(
            result["reusedClassificationCount"],
            golden["reusedClassificationCount"]
        );
        assert_eq!(result["delta"], golden["delta"]);
        assert_eq!(fs::read(&baseline).unwrap(), raw_bytes);
        assert_eq!(sidecar(&baseline), golden["baselineDecisions"]);
    }
}
