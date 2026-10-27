//! Real executable, provider launcher, HTTP, and retained file-transaction coverage.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Integration tests use bounded, explicit fixture shapes"
)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::panic::catch_unwind;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_skills_util::{
    Clock, Http, HttpResponse, NativeHttp, NativeProcess, Process, ProcessOutput, SystemClock, run,
};
use tempfile::TempDir;

#[test]
fn system_clock_drives_tick_without_requiring_an_ambient_runtime() {
    // A zero delay exercises runtime ownership without measuring or waiting on real time.
    SystemClock::default().wait(Duration::ZERO).unwrap();
}

#[test]
fn isolated_azure_git_scopes_oauth_to_the_repository_without_writing_config() {
    let root = TempDir::new().unwrap();
    let process = NativeProcess::new(root.path());
    // Synthetic credentials prove URL scoping without contacting any provider.
    let url = "https://dev.azure.com/org/project/_git/widgets";
    let token = "synthetic-oauth";
    for (requested, expected) in [(url, true), ("https://different.example/repository", false)] {
        let output = process
            .isolated_git_oauth(
                root.path(),
                &["config", "--get-urlmatch", "http.extraheader", requested].map(str::to_owned),
                url,
                token,
            )
            .unwrap();
        assert_eq!(output.success, expected);
        if expected {
            assert_eq!(
                String::from_utf8(output.stdout).unwrap().trim(),
                format!("Authorization: Bearer {token}")
            );
        } else {
            assert!(output.stdout.is_empty());
        }
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

/// Supplies test-owned provider launchers without changing this process's PATH.
struct Fixture {
    root: TempDir,
    program: PathBuf,
    path: OsString,
}

impl Fixture {
    /// Copies immutable prerequisites into a private launcher and response directory.
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("skills fixture with spaces ")
            .tempdir()
            .unwrap();
        let program = root.path().join(if cfg!(windows) {
            "provider.exe"
        } else {
            "provider"
        });
        fs::copy(Self::prepared_provider(), &program).unwrap();

        for name in ["gh", "az"] {
            #[cfg(windows)]
            fs::write(
                root.path().join(format!("{name}.cmd")),
                "@\"%~dp0provider.exe\" %*\r\n",
            )
            .unwrap();
            #[cfg(not(windows))]
            fs::copy(&program, root.path().join(name)).unwrap();
        }

        let mut paths = vec![root.path().to_owned()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let path = std::env::join_paths(paths).unwrap();
        Self {
            root,
            program,
            path,
        }
    }

    /// Enforces the standalone helper prerequisite even when Cargo bypasses the repository runner.
    fn prepared_provider() -> PathBuf {
        let record = std::env::var_os("SS_AI_RUST_TEST_PREREQUISITES")
            .expect("Run scripts/Invoke-Tests.ps1 -Suite Rust -Phase Prepare and set SS_AI_RUST_TEST_PREREQUISITES to its absolute record path");
        Self::prepared_provider_from(Path::new(&record))
    }

    /// Validates completion, exact paths, compiled source identity, and immutable artifact bytes.
    fn prepared_provider_from(record: &Path) -> PathBuf {
        assert!(record.is_absolute(), "Prerequisite record must be absolute");
        let record: Value = serde_json::from_slice(
            &fs::read(record).expect("Cannot read prepared Rust prerequisites"),
        )
        .expect("Malformed Rust prerequisites");
        assert_eq!(record["schemaVersion"], 1, "Unsupported prerequisites");
        assert_eq!(record["status"], "complete", "Incomplete prerequisites");

        let provider = &record["provider"];
        let source = Path::new(provider["source"]["path"].as_str().unwrap());
        assert!(source.is_absolute(), "Provider source must be absolute");
        let source_bytes = fs::read(source).expect("Missing provider source");
        assert_eq!(
            source_bytes,
            include_bytes!("support/provider.rs"),
            "Provider source changed; prepare and compile tests again"
        );

        let executable = Path::new(provider["executable"]["path"].as_str().unwrap());
        assert!(
            executable.is_absolute(),
            "Provider executable must be absolute"
        );

        for (artifact, digest) in [
            (
                &provider["source"],
                format!("{:X}", Sha256::digest(source_bytes)),
            ),
            (&provider["executable"], Self::executable_digest(executable)),
        ] {
            assert!(
                Path::new(artifact["path"].as_str().unwrap()).is_absolute(),
                "Prepared provider paths must be absolute"
            );
            assert_eq!(
                digest,
                artifact["sha256"].as_str().unwrap(),
                "Modified provider prerequisite; prepare again"
            );
        }

        PathBuf::from(provider["executable"]["path"].as_str().unwrap())
    }

    /// Validates executable integrity without retaining an executable-sized allocation.
    fn executable_digest(path: &Path) -> String {
        let mut file = File::open(path).expect("Missing prepared provider executable");
        let mut digest = Sha256::new();
        // An arbitrary small chunk amortizes file reads while keeping stack use bounded.
        let mut buffer = [0; 8192];
        loop {
            let length = file
                .read(&mut buffer)
                .expect("Cannot read prepared provider executable");
            if length == 0 {
                break;
            }
            digest.update(&buffer[..length]);
        }
        format!("{:X}", digest.finalize())
    }

    /// Keeps launcher state and working-directory effects private to this fixture.
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ss-skills-util"));
        command
            .env("PATH", &self.path)
            .env("FIXTURE_ROOT", self.root.path())
            // Live executable tests supply fresh service metadata, not timing assertions.
            .env("FIXTURE_COMMIT_DATE", Timestamp::now().to_string())
            .current_dir(self.root.path());
        command
    }

    /// Supplies one stable review identity for executable transaction scenarios.
    fn review(&self, operation: &str) -> Command {
        let mut command = self.command();
        command
            .args([
                "review",
                operation,
                "--repository-url",
                "https://github.com/example/widgets",
                "--session-id",
                "native-session",
                "--state-path",
            ])
            .arg(self.root.path().join("state.json"))
            .args([
                "--claim-message",
                "Unicode \u{4e16}\u{754c}; quotes \" and & | % stay in JSON.",
            ]);
        command
    }
}

#[test]
fn provider_prerequisites_reject_missing_incomplete_stale_and_modified_artifacts() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("provider.rs");
    let executable = root.path().join("provider");
    let record = root.path().join("prerequisites.json");
    let source_bytes = include_bytes!("support/provider.rs");
    // Validation needs artifact bytes, not another compiler invocation or a runnable substitute.
    let executable_bytes = b"immutable artifact";
    fs::write(&source, source_bytes).unwrap();
    fs::write(&executable, executable_bytes).unwrap();
    let valid = json!({
        "schemaVersion": 1,
        "status": "complete",
        "provider": {
            "source": {"path": source, "sha256": format!("{:X}", Sha256::digest(source_bytes))},
            "executable": {"path": executable, "sha256": format!("{:X}", Sha256::digest(executable_bytes))}
        }
    });
    fs::write(&record, serde_json::to_vec(&valid).unwrap()).unwrap();
    assert_eq!(Fixture::prepared_provider_from(&record), executable);

    for (pointer, replacement) in [
        ("/schemaVersion", json!(2)),
        ("/status", json!("preparing")),
        ("/status", json!("providerFailed")),
        ("/provider/source/path", json!("relative.rs")),
        ("/provider/executable/path", json!("relative")),
        ("/provider/executable/sha256", json!("modified")),
        (
            "/provider/executable/path",
            json!(root.path().join("missing")),
        ),
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        fs::write(&record, serde_json::to_vec(&invalid).unwrap()).unwrap();
        catch_unwind(|| Fixture::prepared_provider_from(&record)).unwrap_err();
    }

    fs::write(&record, serde_json::to_vec(&valid).unwrap()).unwrap();
    fs::write(&source, b"changed source at the same package version").unwrap();
    catch_unwind(|| Fixture::prepared_provider_from(&record)).unwrap_err();
    catch_unwind(|| Fixture::prepared_provider_from(&root.path().join("absent"))).unwrap_err();
}

#[test]
fn concurrent_provider_fixtures_own_independent_mutable_state() {
    // Construction overlaps without spawning children while another test may hold retained locks.
    thread::scope(|scope| {
        let first = scope.spawn(Fixture::new);
        let second = scope.spawn(Fixture::new);
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert_ne!(first.root.path(), second.root.path());

        let response = "remote-claim.json";
        fs::write(first.root.path().join(response), b"first").unwrap();
        fs::write(second.root.path().join(response), b"second").unwrap();
        assert_eq!(
            fs::read(first.root.path().join(response)).unwrap(),
            b"first"
        );
        assert_eq!(
            fs::read(second.root.path().join(response)).unwrap(),
            b"second"
        );
        assert_eq!(
            fs::read(&first.program).unwrap(),
            fs::read(&second.program).unwrap()
        );
    });
}

/// Preserves both output streams when a native fixture command unexpectedly fails.
fn successful(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Requires successful execution before decoding a command's JSON contract.
fn value(output: &Output) -> Value {
    successful(output);
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[cfg_attr(
    mutants,
    ignore = "Real subprocess retries use SystemClock; deterministic retries have unit coverage"
)]
fn executable_github_selection_confirmation_and_registration() {
    let fixture = Fixture::new();
    let claimed = value(&fixture.review("select").output().unwrap());
    assert_eq!(claimed["status"], "claimed");
    let path = fixture.root.path().join("state.json");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
        claimed
    );
    assert_eq!(claimed["target"]["pullRequestId"], 10);
    assert!(
        claimed["claim"]["body"]
            .as_str()
            .unwrap()
            .contains("\u{4e16}\u{754c}")
    );
    // Preparation has finished; supplying an elapsed deadline avoids a real-time wait.
    let mut prepared = claimed;
    prepared["claim"]["checkAfter"] = json!("2000-01-01T00:00:00Z");
    fs::write(&path, serde_json::to_vec(&prepared).unwrap()).unwrap();
    let confirmed = value(&fixture.review("confirm").output().unwrap());
    assert_eq!(confirmed["status"], "confirmed");
    assert_eq!(
        confirmed["diagnostics"][0]["reason"],
        "soleLiveClaimAfterPreparation"
    );
    let replay = fixture.review("confirm").output().unwrap();
    assert!(!replay.status.success());
    assert!(replay.stdout.is_empty());
    assert!(String::from_utf8_lossy(&replay.stderr).contains("invalidState"));
    let registered = value(
        &fixture
            .command()
            .args([
                "reviewer",
                "register",
                "--url",
                "https://github.com/example/widgets/pull/10?tab=files",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(registered["status"], "added");
    assert_eq!(registered["identity"]["id"], "42");
    assert!(fixture.root.path().join("registration.json").exists());
}

#[test]
#[cfg_attr(
    mutants,
    ignore = "Real subprocess retries use SystemClock; deterministic retries have unit coverage"
)]
fn executable_failed_writes_leave_nonreplayable_complete_journal() {
    let fixture = Fixture::new();
    let failed = fixture
        .review("select")
        .env("FIXTURE_LOST_RESPONSE", "1")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("ambiguousWrite"));
    let before = fs::read(fixture.root.path().join("state.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&before).unwrap()["status"],
        "claiming"
    );
    for operation in ["select", "confirm"] {
        let failed = fixture.review(operation).output().unwrap();
        assert!(!failed.status.success());
        assert!(failed.stdout.is_empty());
    }
    assert_eq!(
        fs::read(fixture.root.path().join("state.json")).unwrap(),
        before
    );
}

#[test]
#[cfg_attr(
    mutants,
    ignore = "Real process pipes can block under mutations; no timeout substitutes for progress"
)]
fn executable_discovery_reads_metadata_without_remote_effects_in_either_mode() {
    let fixture = Fixture::new();
    let policy = fixture.root.path().join("policy.md");
    fs::write(&policy, "Trusted fixture policy").unwrap();
    for dry_run in [false, true] {
        let path = fixture
            .root
            .path()
            .join(format!("discovery-{dry_run}.json"));
        let mut command = fixture.command();
        command
            .args([
                "autoreapprove",
                "discover",
                "--repository-url",
                "https://github.com/example/widgets",
                "--state-path",
            ])
            .arg(&path)
            .arg("--policy-path")
            .arg(&policy);
        if dry_run {
            command.arg("--dry-run");
        }
        let summary = value(&command.output().unwrap());
        assert_eq!(summary["dryRun"], dry_run);
        assert_eq!(summary["state"], "processing");
        assert_eq!(summary["counts"]["assessmentRequired"], 1);
        assert_eq!(summary["items"][0]["pullRequestId"], 10);
        assert_eq!(summary["items"][0]["status"], "assessmentRequired");
        let output = fixture
            .command()
            .args(["autoreapprove", "results", "--state-path"])
            .arg(&path)
            .args(["--limit", "1"])
            .output()
            .unwrap();
        assert_eq!(value(&output), summary);
        assert!(path.is_file());
        assert!(!fixture.root.path().join("registration.json").exists());
        assert!(!fixture.root.path().join("remote-claim.json").exists());
    }
}

#[test]
#[cfg_attr(
    mutants,
    ignore = "Real process pipes can block under mutations; no timeout substitutes for progress"
)]
fn native_pipe_and_launcher_arguments_are_lossless() {
    let fixture = Fixture::new();
    let process = NativeProcess::new(fixture.root.path());
    let input = "\u{4e16}\u{754c}\"\\&".repeat(32 * 1024).into_bytes();
    let output = process
        .execute(
            fixture.program.to_str().unwrap(),
            &["pipe".to_owned()],
            &input,
        )
        .unwrap();
    assert!(output.success);
    let mut expected = vec![b'o'; 256 * 1024];
    expected.extend(input);
    assert_eq!(output.stdout, expected);
    assert_eq!(output.stderr, vec![b'e'; 256 * 1024]);
    let mut arguments = vec!["arguments".to_owned()];
    arguments.extend(
        [
            "with spaces",
            "tenant & punctuation",
            "Unicode \u{4e16}\u{754c}",
            "",
            "ending\\",
        ]
        .map(str::to_owned),
    );
    #[cfg(windows)]
    let launcher = fixture.root.path().join("az.cmd");
    #[cfg(not(windows))]
    let launcher = fixture.root.path().join("az");
    let output = process
        .execute(launcher.to_str().unwrap(), &arguments, &[])
        .unwrap();
    assert!(output.success);
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n"),
        arguments[1..].join("\n") + "\n"
    );
}

#[test]
#[cfg(unix)]
#[cfg_attr(
    mutants,
    ignore = "Real subprocess retries use SystemClock; deterministic retries have unit coverage"
)]
fn unix_provider_lookup_skips_nonexecutable_path_entries() {
    let fixture = Fixture::new();
    let shadow = fixture.root.path().join("nonexecutable");
    fs::create_dir_all(&shadow).unwrap();
    let blocked = shadow.join("gh");
    fs::write(&blocked, "not an executable").unwrap();
    // A readable file without execute permission must not shadow a later executable.
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o644)).unwrap();
    let path =
        std::env::join_paths(std::iter::once(shadow).chain(std::env::split_paths(&fixture.path)))
            .unwrap();
    let output = fixture
        .command()
        .env("PATH", path)
        .args([
            "reviewer",
            "register",
            "--url",
            "https://github.com/example/widgets/pull/10",
        ])
        .output()
        .unwrap();
    assert_eq!(value(&output)["status"], "added");
}

#[test]
fn executable_requires_named_complete_inputs_before_provider_access() {
    for arguments in [
        vec![],
        vec!["review", "select"],
        vec!["reviewer", "register", "--platform", "unsupported"],
        vec![
            "reviewer",
            "register",
            "--platform",
            "gitHub",
            "--organization",
            "org",
            "--pull-request-id",
            "10",
        ],
        vec![
            "reviewer",
            "register",
            "--url",
            "https://github.com/org/repo/pull/10",
            "--pull-request-id",
            "0",
        ],
        vec![
            "review",
            "select",
            "--repository-url",
            "https://github.com/org/repo",
            "--session-id",
            "session",
            "--state-path",
            "relative.json",
            "--claim-message",
            "hello",
        ],
        vec![
            "reviewer",
            "register",
            "https://github.com/org/repo/pull/10",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ss-skills-util"))
            .args(arguments)
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalidInput"));
    }
}

/// Reads a bounded HTTP request after its announced body arrives, without sleeps or deadlines.
fn request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.extend(byte);
        assert!(bytes.len() < 32 * 1024, "HTTP header did not terminate");
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(length < 1024 * 1024);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    (headers, body)
}

fn respond(stream: &mut TcpStream, body: &Value) {
    let body = serde_json::to_vec(body).unwrap();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(&body).unwrap();
}

#[test]
#[cfg_attr(
    mutants,
    ignore = "Live HTTP and subprocess waits cannot enforce deterministic mutation progress"
)]
fn executable_ado_registration_uses_native_oauth_shim_and_http_without_erasing_votes() {
    for existing in [false, true] {
        let fixture = Fixture::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let collection = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let replies = [
                json!({"authenticatedUser":{"id":"reviewer-id","providerDisplayName":"Unicode \u{4e16}\u{754c}","subjectDescriptor":"aad.reviewer"},"authorizedUser":{"id":"other"}}),
                json!({"repository":{"name":"A Repo","project":{"name":"A Project"}}}),
                json!({"count":1,"value":[{"id":if existing {"reviewer-id"} else {"other"},"vote":-10,"hasDeclined":true,"isRequired":true,"isFlagged":true}]}),
                json!({"id":"reviewer-id"}),
            ];
            for (index, response) in replies
                .iter()
                .take(if existing { 3 } else { 4 })
                .enumerate()
            {
                let (mut stream, _) = listener.accept().unwrap();
                let (header, body) = request(&mut stream);
                assert!(
                    header
                        .to_ascii_lowercase()
                        .contains("authorization: bearer fixture-oauth")
                );
                let path = match index {
                    0 => "/_apis/connectionData?api-version=7.1-preview",
                    1 => "/_apis/git/pullrequests/10?api-version=7.1",
                    2 => {
                        "/A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10/reviewers?api-version=7.1"
                    }
                    _ => {
                        "/A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10/reviewers/reviewer-id?api-version=7.1"
                    }
                };
                assert!(header.starts_with(&format!(
                    "{} {path} HTTP/1.1",
                    if index == 3 { "PUT" } else { "GET" }
                )));
                if index == 3 {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&body).unwrap(),
                        json!({"id":"reviewer-id","vote":0})
                    );
                } else {
                    assert!(body.is_empty());
                }
                respond(&mut stream, response);
            }
        });
        let output = fixture
            .command()
            .args([
                "reviewer",
                "register",
                "--platform",
                "azureDevOps",
                "--organization",
                "org",
                "--pull-request-id",
                "10",
                "--collection-url",
                &collection,
                "--tenant",
                "tenant with spaces & punctuation",
            ])
            .output()
            .unwrap();
        // Close the listener through its worker on success; no timing-based synchronization.
        let registered = value(&output);
        server.join().unwrap();
        assert_eq!(
            registered["status"],
            if existing {
                "alreadyRegistered"
            } else {
                "added"
            }
        );
        assert!(
            registered["pullRequestUrl"]
                .as_str()
                .unwrap()
                .contains("/A%20Project/_git/A%20Repo/pullrequest/10")
        );
    }
}

/// Tests file transactions with channels controlling contention and service completion.
struct ControlledClock {
    contended: mpsc::Sender<()>,
    retry: mpsc::Receiver<()>,
}

impl Clock for ControlledClock {
    fn now(&self) -> Timestamp {
        "2026-09-28T00:00:00Z".parse().unwrap()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        self.contended.send(()).unwrap();
        self.retry.recv().unwrap();
        Ok(())
    }
}

struct NoHttp;
impl Http for NoHttp {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("Unexpected HTTP call")
    }
}

struct EmptyProvider {
    initialized: Option<mpsc::Sender<()>>,
    finish: Option<mpsc::Receiver<()>>,
}

impl Process for EmptyProvider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> std::io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        assert!(input.is_empty());
        let reply = match arguments[1].as_str() {
            "user" => r#"{"id":42,"login":"reviewer"}"#,
            "repos/example/widgets/pulls?state=open&per_page=100" => {
                if let Some(initialized) = &self.initialized {
                    initialized.send(()).unwrap();
                    self.finish.as_ref().unwrap().recv().unwrap();
                }
                "[[]]"
            }
            unexpected => panic!("Unexpected request {unexpected}"),
        };
        Ok(ProcessOutput::new(
            true,
            reply.as_bytes().to_vec(),
            Vec::new(),
        ))
    }
}

fn selection(path: &Path) -> Vec<OsString> {
    let mut args = [
        "ss-skills-util",
        "review",
        "select",
        "--repository-url",
        "https://github.com/example/widgets",
        "--session-id",
        "transaction-test",
        "--claim-message",
        "Reserving",
        "--state-path",
    ]
    .map(OsString::from)
    .to_vec();
    args.push(path.as_os_str().to_owned());
    args
}

#[test]
#[cfg_attr(
    mutants,
    ignore = "A mutated missing synchronization signal blocks real threads"
)]
fn overlapping_native_transactions_and_open_readers_preserve_exact_records() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let (initialized_tx, initialized_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let (contended_tx, contended_rx) = mpsc::channel();
    let (retry_tx, retry_rx) = mpsc::channel();
    let owner = thread::spawn({
        let path = path.clone();
        move || {
            let (unexpected, _) = mpsc::channel();
            let (_, no_wait) = mpsc::channel();
            let clock = ControlledClock {
                contended: unexpected,
                retry: no_wait,
            };
            run(
                &selection(&path),
                &EmptyProvider {
                    initialized: Some(initialized_tx),
                    finish: Some(finish_rx),
                },
                &NoHttp,
                &clock,
            )
            .unwrap()
        }
    });
    initialized_rx.recv().unwrap();
    let initial = fs::read(&path).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&initial).unwrap()["status"],
        "selecting"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        options.share_mode(0x1 | 0x4); // FILE_SHARE_READ | FILE_SHARE_DELETE permits atomic publication.
    }
    let mut reader = options.open(&path).unwrap();
    let contender = thread::spawn({
        let path = path.clone();
        move || {
            run(
                &selection(&path),
                &EmptyProvider {
                    initialized: None,
                    finish: None,
                },
                &NoHttp,
                &ControlledClock {
                    contended: contended_tx,
                    retry: retry_rx,
                },
            )
            .err()
            .unwrap()
        }
    });
    contended_rx.recv().unwrap();
    assert_eq!(fs::read(&path).unwrap(), initial);
    finish_tx.send(()).unwrap();
    let completed: Value = serde_json::from_str(&owner.join().unwrap()).unwrap();
    retry_tx.send(()).unwrap();
    assert_eq!(
        ss_skills_util::classification(&contender.join().unwrap()),
        "invalidState"
    );
    let mut retained = Vec::new();
    reader.read_to_end(&mut retained).unwrap();
    assert_eq!(retained, initial);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
        completed
    );
    assert_eq!(completed["status"], "noCandidate");
    assert!(root.path().join("state.json.lock").is_file());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
#[cfg_attr(
    mutants,
    ignore = "A mutated missing HTTP exchange blocks the real listener"
)]
fn real_http_bytes_and_headers_preserve_unicode() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/resource", listener.local_addr().unwrap());
    let payload = json!({"body":"Unicode \u{4e16}\u{754c} and \"quotes\""});
    let server = thread::spawn({
        let payload = payload.clone();
        move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (header, body) = request(&mut stream);
            assert!(
                header
                    .to_ascii_lowercase()
                    .contains("content-type: application/json; charset=utf-8")
            );
            assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), payload);
            respond(&mut stream, &payload);
        }
    });
    let response = NativeHttp::new()
        .unwrap()
        .send(
            "POST",
            &url,
            "fixture-oauth",
            Some(&serde_json::to_vec(&payload).unwrap()),
        )
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&response.body).unwrap(),
        payload
    );
    server.join().unwrap();
}

/// Supplies deterministic ADO service evidence around the real command/file boundary.
#[derive(Default)]
struct AdoFlow {
    thread: RefCell<Option<Value>>,
    writes: RefCell<Vec<Value>>,
}

impl Process for AdoFlow {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> std::io::Result<ProcessOutput> {
        assert_eq!(program, "az");
        assert!(input.is_empty());
        assert_eq!(arguments.last().unwrap(), "fixture-tenant");
        Ok(ProcessOutput::new(
            true,
            b"fixture-oauth".to_vec(),
            Vec::new(),
        ))
    }
}

impl Http for AdoFlow {
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(token, "fixture-oauth");
        assert!(url.starts_with("https://dev.azure.com/org/"));
        let path = url.strip_prefix("https://dev.azure.com/org/").unwrap();
        let response = match (method, path) {
            ("GET", "_apis/connectionData?api-version=7.1-preview") => {
                json!({"authenticatedUser":{"id":"reviewer-id","providerDisplayName":"Reviewer","subjectDescriptor":"aad.reviewer"}})
            }
            (
                "GET",
                "Project/_apis/git/repositories/Repo/pullRequests?%24skip=0&%24top=100&searchCriteria.status=active&api-version=7.1",
            ) => json!({"count":1,"value":[{"pullRequestId":10}]}),
            (
                "GET",
                "Project/_apis/git/repositories/Repo/pullRequests?%24skip=1&%24top=100&searchCriteria.status=active&api-version=7.1"
                | "Project/_apis/git/repositories/Repo/pullRequests/10/reviewers?api-version=7.1",
            ) => json!({"count":0,"value":[]}),
            ("GET", "Project/_apis/git/repositories/Repo/pullRequests/10?api-version=7.1") => {
                json!({"status":"active","createdBy":{"id":"author"},"isDraft":false})
            }
            (
                "GET",
                "Project/_apis/git/repositories/Repo/pullRequests/10/commits?%24top=100&api-version=7.1",
            ) => json!({"count":1,"value":[{"committer":{"date":"2026-09-27T00:00:00Z"}}]}),
            (
                "GET",
                "Project/_apis/git/repositories/Repo/pullRequests/10/threads?api-version=7.1",
            ) => {
                // Deleted evidence cannot contend; resolved discussion must still be considered.
                let mut threads = vec![
                    json!({"isDeleted":true,"id":9,"comments":[]}),
                    json!({"id":8,"comments":[{"isDeleted":true}]}),
                ];
                threads.extend(self.thread.borrow().iter().cloned());
                json!({"count":threads.len(),"value":threads})
            }
            (
                "POST",
                "Project/_apis/git/repositories/Repo/pullRequests/10/threads?api-version=7.1",
            ) => {
                let value: Value = serde_json::from_slice(body.unwrap()).unwrap();
                assert_eq!(value["status"], 2);
                assert_eq!(value["comments"].as_array().unwrap().len(), 1);
                assert_eq!(value["comments"][0]["parentCommentId"], 0);
                assert_eq!(value["comments"][0]["commentType"], 1);
                let thread = json!({"id":101,"status":"fixed","comments":[{"id":55,"content":value["comments"][0]["content"],
                    "publishedDate":"2026-09-28T00:00:00Z","author":{"id":"reviewer-id","descriptor":"aad.reviewer"}}]});
                self.thread.replace(Some(thread.clone()));
                self.writes.borrow_mut().push(value);
                thread
            }
            _ => panic!("Unexpected ADO operation: {method} {path}"),
        };
        Ok(HttpResponse::new(
            200,
            std::collections::BTreeMap::new(),
            serde_json::to_vec(&response).unwrap(),
        ))
    }
}

/// Records the production wait request without delaying the integration test.
struct Timeline(RefCell<Timestamp>);
impl Clock for Timeline {
    fn now(&self) -> Timestamp {
        *self.0.borrow()
    }
    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        assert_eq!(duration, Duration::from_secs(60));
        *self.0.borrow_mut() += duration;
        Ok(())
    }
}

#[test]
fn ado_command_selection_and_confirmation_preserve_resolved_threads_and_schema_two() {
    for legacy in [false, true] {
        let root = tempfile::tempdir().unwrap();
        // A long path also exercises native Windows extended-path replacement.
        let path = root
            .path()
            .join("long-segment".repeat(8))
            .join("long-segment".repeat(8))
            .join("Unicode \u{4e16}\u{754c} state.json");
        let service = AdoFlow::default();
        let clock = Timeline(RefCell::new("2026-09-28T00:00:00Z".parse().unwrap()));
        let mut args = [
            "ss-skills-util",
            "review",
            "select",
            "--repository-url",
            "https://dev.azure.com/org/Project/_git/Repo",
            "--session-id",
            "legacy-session",
            "--claim-message",
            "Review \u{4e16}\u{754c}",
            "--tenant",
            "fixture-tenant",
            "--state-path",
        ]
        .map(OsString::from)
        .to_vec();
        args.push(path.as_os_str().to_owned());
        if legacy {
            let body = "Review\r\n\r\n<!-- ss-review-proactor:v1 session=legacy-session claim=aaaaaaaa-1234-1234-1234-123456789abc -->";
            // Schema-2 fields and PowerShell's timestamp precision, including explicit nulls.
            let state = json!({
                "schemaVersion":2,"sessionId":"legacy-session","repositoryUrl":"https://dev.azure.com/org/Project/_git/Repo",
                "identity":{"platform":"azureDevOps","id":"reviewer-id","displayName":"Reviewer","uniqueName":null,"descriptor":"aad.reviewer","accountType":null,"kind":"person","evidence":[]},
                "status":"claimed","target":{"platform":"azureDevOps","collectionUrl":"https://dev.azure.com/org","organization":"org","project":"Project","repository":"Repo","pullRequestId":10,"webUrl":"https://dev.azure.com/org/Project/_git/Repo/pullrequest/10"},
                "claim":{"token":"aaaaaaaa-1234-1234-1234-123456789abc","body":body,"id":"55","threadId":"101","checkAfter":"2026-09-28T00:01:00.0000000+00:00"},
                "attempted":[10],"diagnostics":[],"statePath":path
            });
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut bytes = vec![0xEF, 0xBB, 0xBF];
            bytes.extend(serde_json::to_vec(&state).unwrap());
            fs::write(&path, bytes).unwrap();
            service.thread.replace(Some(json!({"id":101,"status":"fixed","comments":[{"id":55,"content":body,"publishedDate":"2026-09-28T00:00:00.0000000Z","author":{"id":"reviewer-id","descriptor":"aad.reviewer"}}]})));
        } else {
            let selected: Value =
                serde_json::from_str(&run(&args, &service, &service, &clock).unwrap()).unwrap();
            assert_eq!(selected["status"], "claimed");
            assert_eq!(selected["claim"]["threadId"], "101");
        }
        args[2] = "confirm".into();
        let confirmed: Value =
            serde_json::from_str(&run(&args, &service, &service, &clock).unwrap()).unwrap();
        assert_eq!(confirmed["status"], "confirmed");
        assert_eq!(
            confirmed["diagnostics"][0]["reason"],
            "soleLiveClaimAfterPreparation"
        );
        assert_eq!(service.writes.borrow().len(), usize::from(!legacy));
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
            confirmed
        );
    }
}
