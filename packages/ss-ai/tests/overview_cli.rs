//! Exercises overview publication through the CLI and injected provider boundary.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::assigning_clones,
    clippy::format_collect,
    clippy::assertions_on_result_states,
    reason = "The bounded test fixtures favor concise assertions and setup"
)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf, absolute};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_slice, from_str, json, to_vec};
use sha2::{Digest, Sha256};
use ss_ai::{Http, HttpResponse, Process, ProcessOutput, SystemClock, run};
use ss_ai_review::{Clock, OverviewInput, OverviewRender, publish_overview};
use ss_ai_runtime::FileTransaction;
use tempfile::{Builder, TempDir};

/// Keeps fake provider state local to an individual publication.
#[derive(Default)]
struct Provider {
    requests: RefCell<Vec<String>>,
    markdown: RefCell<String>,
    comment: RefCell<String>,
    reject_comment_readback: bool,
    fail_upload: bool,
    refuse_upload: bool,
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        let route = &arguments[1];
        let method = &arguments[arguments.iter().position(|v| v == "--method").unwrap() + 1];
        self.requests.borrow_mut().push(format!("{method} {route}"));
        if self.refuse_upload && method == "POST" && route == "gists" {
            return Ok(ProcessOutput::new(
                false,
                b"HTTP/1.1 429 Too Many Requests\r\n\r\n".to_vec(),
                Vec::new(),
            ));
        }
        if self.fail_upload && method == "POST" && route == "gists" {
            return Ok(ProcessOutput::new(
                false,
                Vec::new(),
                b"rate limit exceeded".to_vec(),
            ));
        }
        let response = match (method.as_str(), route.as_str()) {
            ("GET", "repos/owner/repo/pulls/42") => json!({"state":"open"}),
            ("POST", "gists") => {
                let body: Value = from_slice(input).unwrap();
                *self.markdown.borrow_mut() = body["files"]["overview.md"]["content"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                json!({"id":"abcdef123456", "html_url":"https://gist.github.com/owner/abcdef123456"})
            }
            ("GET", "gists/abcdef123456") => json!({
                "id":"abcdef123456", "public":false,
                "html_url":"https://gist.github.com/owner/abcdef123456",
                "history":[{"version":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}],
                "files":{"overview.md":{"truncated":false,"content":*self.markdown.borrow()}}
            }),
            ("POST", "repos/owner/repo/issues/42/comments") => {
                let body: Value = from_slice(input).unwrap();
                *self.comment.borrow_mut() = body["body"].as_str().unwrap().to_owned();
                json!({"id":99,"html_url":"https://github.com/owner/repo/issues/42#issuecomment-99"})
            }
            ("GET", "repos/owner/repo/issues/comments/99") => json!({
                "id":99,
                "body":if self.reject_comment_readback { "different".to_owned() } else { self.comment.borrow().clone() }
            }),
            _ => panic!("unexpected provider request: {method} {route}"),
        };
        Ok(ProcessOutput::new(
            true,
            to_vec(&response).unwrap(),
            Vec::new(),
        ))
    }
}

impl Http for Provider {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub publication must not use Azure HTTP");
    }
}

/// Creates isolated Unicode paths in the checkout, never in the system temp directory.
fn fixture() -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    fs::create_dir_all(&root).unwrap();
    Builder::new()
        .prefix("overview-雪-")
        .tempdir_in(root)
        .unwrap()
}

/// Constructs a strictly readable captured PR with inspected revisions.
fn capture() -> Value {
    json!({
        "schemaVersion":3, "platform":"gitHub",
        "repositoryUrl":"https://github.com/owner/repo",
        "pullRequestUrl":"https://github.com/owner/repo/pull/42",
        "target":{"platform":"gitHub","collectionUrl":null,"organization":"owner",
            "project":null,"repository":"repo","pullRequestId":42,
            "webUrl":"https://github.com/owner/repo/pull/42"},
        "pullRequest":{"id":42,"title":"Title","description":null,
            "reviewedSourceCommit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "reviewedMergeBaseCommit":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
        "reviewers":[],"revisions":[],"threads":[]
    })
}

/// Supplies every required overview section in the prescribed order.
fn overview() -> String {
    [
        "Mission",
        "Substance",
        "Design changes",
        "Significant changes",
        "Structure",
        "Mechanism",
        "Completeness assessment",
        "Related history",
        "Related work",
    ]
    .iter()
    .map(|name| format!("## {name}\nContent\n"))
    .collect()
}

/// Builds the same named arguments accepted from the real executable.
fn arguments(root: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["ss-ai", "review", "publish-overview"]
        .into_iter()
        .map(Into::into)
        .collect();
    for (name, value) in [
        ("context-path", root.join("capture 雪.json")),
        ("overview-path", root.join("overview 雪.md")),
        ("output-directory", root.join("output 雪")),
        ("skill-root", root.join("skill 雪")),
        ("comment-prefix-path", root.join("prefix 雪.txt")),
    ] {
        let mut arg = OsString::from(format!("--{name}="));
        arg.push(value);
        args.push(arg);
    }
    args
}

/// Writes complete input files without depending on installed tools.
fn prepare(root: &Path) {
    fs::create_dir_all(root.join("skill 雪")).unwrap();
    fs::write(root.join("capture 雪.json"), to_vec(&capture()).unwrap()).unwrap();
    fs::write(root.join("overview 雪.md"), overview()).unwrap();
    fs::write(root.join("prefix 雪.txt"), "Attribution 雪\n").unwrap();
}

#[test]
fn github_publication_verifies_and_reuses_receipts_without_reposting() {
    let fixture = fixture();
    prepare(fixture.path());
    let provider = Provider::default();
    let args = arguments(fixture.path());
    let result: Value =
        from_str(&run(&args, &provider, &provider, &SystemClock::default()).unwrap()).unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["artifact"]["id"], "abcdef123456");
    assert_eq!(result["comment"]["id"], 99);
    assert!(
        provider
            .comment
            .borrow()
            .starts_with("Attribution 雪\n\n\n[Pull request overview")
    );
    let requests = provider.requests.borrow().len();
    let repeated: Value =
        from_str(&run(&args, &provider, &provider, &SystemClock::default()).unwrap()).unwrap();
    assert_eq!(result, repeated);
    assert_eq!(provider.requests.borrow().len(), requests);
}

#[test]
fn failed_readback_retains_comment_id_and_prevents_replay() {
    let fixture = fixture();
    prepare(fixture.path());
    let provider = Provider {
        reject_comment_readback: true,
        ..Provider::default()
    };
    let args = arguments(fixture.path());
    let result: Value =
        from_str(&run(&args, &provider, &provider, &SystemClock::default()).unwrap()).unwrap();
    assert_eq!(result["status"], "incomplete");
    assert_eq!(result["comment"]["id"], 99);
    let requests = provider.requests.borrow().len();
    let repeated: Value =
        from_str(&run(&args, &provider, &provider, &SystemClock::default()).unwrap()).unwrap();
    assert_eq!(repeated["status"], "needsReconciliation");
    assert_eq!(repeated["comment"]["id"], 99);
    assert_eq!(provider.requests.borrow().len(), requests);
}

#[test]
fn github_unproven_rate_limit_does_not_authorize_replay() {
    let fixture = fixture();
    prepare(fixture.path());
    let provider = Provider {
        fail_upload: true,
        ..Provider::default()
    };
    let args = arguments(fixture.path());
    let result: Value =
        from_str(&run(&args, &provider, &provider, &SystemClock::default()).unwrap()).unwrap();
    assert_eq!(result["operations"][0]["status"], "ambiguous");
    let requests = provider.requests.borrow().len();
    let retry: Value =
        from_str(&run(&args, &provider, &provider, &SystemClock::default()).unwrap()).unwrap();
    assert_eq!(retry["status"], "needsReconciliation");
    assert_eq!(provider.requests.borrow().len(), requests);
}

#[test]
fn github_interrupted_rate_refusal_allows_retry() {
    struct Interrupted;
    impl Clock for Interrupted {
        fn now(&self) -> Timestamp {
            Timestamp::UNIX_EPOCH
        }
        fn wait(&self, _: Duration) -> Result<(), AppError> {
            Err(ss_ai_runtime::fail("interrupted", "wait interrupted"))
        }
    }

    let fixture = fixture();
    prepare(fixture.path());
    let refused = Provider {
        refuse_upload: true,
        ..Provider::default()
    };
    let args = arguments(fixture.path());
    let result: Value = from_str(&run(&args, &refused, &refused, &Interrupted).unwrap()).unwrap();
    assert_eq!(result["operations"][0]["status"], "failed", "{result}");
    let accepted = Provider::default();
    let result: Value = from_str(&run(&args, &accepted, &accepted, &Interrupted).unwrap()).unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn real_binary_rejects_bad_capture_before_provider_access() {
    let fixture = fixture();
    prepare(fixture.path());
    fs::write(fixture.path().join("capture 雪.json"), "{}").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(arguments(fixture.path()).iter().skip(1))
        .current_dir(fixture.path())
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalidInput"));
    assert!(
        !fixture
            .path()
            .join("output 雪/overview-publication-result.json")
            .exists()
    );
}

#[test]
fn invalid_capture_is_rejected_without_provider_access() {
    let fixture = fixture();
    prepare(fixture.path());
    fs::write(fixture.path().join("capture 雪.json"), "{}").unwrap();
    let provider = Provider::default();
    assert!(
        run(
            &arguments(fixture.path()),
            &provider,
            &provider,
            &SystemClock::default()
        )
        .is_err()
    );
    assert!(provider.requests.borrow().is_empty());
}

#[test]
fn inaccessible_output_reports_a_best_effort_result() {
    let fixture = fixture();
    prepare(fixture.path());
    fs::write(fixture.path().join("output 雪"), b"not a directory").unwrap();
    let provider = Provider::default();
    let result: Value = from_str(
        &run(
            &arguments(fixture.path()),
            &provider,
            &provider,
            &SystemClock::default(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["status"], "journalFailure");
    assert!(provider.requests.borrow().is_empty());
}

/// Simulates OAuth and Azure responses while retaining exact attachment bytes.
#[derive(Default)]
struct Azure {
    requests: RefCell<Vec<String>>,
    archive: RefCell<Vec<u8>>,
    comment: RefCell<String>,
    reject_upload: bool,
}

impl Process for Azure {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "az");
        Ok(ProcessOutput::new(
            true,
            b"fixture-token".to_vec(),
            Vec::new(),
        ))
    }
}

impl Http for Azure {
    fn send(
        &self,
        method: &str,
        url: &str,
        _: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        self.requests.borrow_mut().push(format!("{method} {url}"));
        let response = if method == "POST" && url.contains("/threads?") {
            let body: Value = from_slice(body.unwrap()).unwrap();
            *self.comment.borrow_mut() =
                body["comments"][0]["content"].as_str().unwrap().to_owned();
            json!({"id":5})
        } else if url.contains("/threads/5?") {
            json!({"status":"fixed","comments":[{"content":*self.comment.borrow()}]})
        } else {
            json!({"status":"active"})
        };
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            to_vec(&response).unwrap(),
        ))
    }

    fn send_binary(
        &self,
        method: &str,
        url: &str,
        _: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        self.requests.borrow_mut().push(format!("{method} {url}"));
        if method == "POST" && self.reject_upload {
            return Ok(HttpResponse::new(429, BTreeMap::new(), Vec::new()));
        }
        let bytes = if method == "POST" {
            *self.archive.borrow_mut() = body.unwrap().to_vec();
            br#"{"id":3}"#.to_vec()
        } else {
            self.archive.borrow().clone()
        };
        Ok(HttpResponse::new(200, BTreeMap::new(), bytes))
    }
}

/// Supplies Azure routing in the same capture schema as the GitHub fixture.
fn azure_capture() -> Value {
    let mut capture = capture();
    let url = "https://dev.azure.com/org/proj/_git/repo/pullrequest/42";
    capture["platform"] = json!("azureDevOps");
    capture["repositoryUrl"] = json!("https://dev.azure.com/org/proj/_git/repo");
    capture["pullRequestUrl"] = json!(url);
    capture["target"] = json!({"platform":"azureDevOps","collectionUrl":"https://dev.azure.com/org",
        "organization":"org","project":"proj","repository":"repo","pullRequestId":42,"webUrl":url});
    capture
}

/// Runs the provider transaction with a narrow local artifact renderer.
fn azure_publish(root: &Path, provider: &Azure, missing_tools: bool) -> Value {
    let capture = azure_capture();
    let overview_path = root.join("overview 雪.md");
    let output = root.join("output 雪");
    let skill = root.join("skill 雪");
    let clock = SystemClock::default();
    let input = OverviewInput {
        capture: &capture,
        overview_path: &overview_path,
        output_directory: &output,
        skill_root: &skill,
        comment_prefix: "Prefix 雪",
        tenant: Some("example"),
    };
    publish_overview(&input, provider, provider, &clock, |_| {
        if missing_tools {
            return Ok(OverviewRender::MissingTools {
                missing: vec!["pandoc".to_owned()],
                diagnostics: vec!["not installed".to_owned()],
            });
        }
        let html = root.join("report.html");
        let archive = root.join("report.zip");
        fs::write(&html, b"html").unwrap();
        fs::write(&archive, b"archive").unwrap();
        Ok(OverviewRender::Ready {
            html_path: html,
            html_hash: format!("{:x}", Sha256::digest(b"html")),
            archive_path: archive,
            archive_hash: format!("{:x}", Sha256::digest(b"archive")),
        })
    })
    .unwrap()
}

#[test]
fn azure_publication_preserves_exact_archive_and_comment() {
    let fixture = fixture();
    prepare(fixture.path());
    let provider = Azure::default();
    let result = azure_publish(fixture.path(), &provider, false);
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["artifact"]["id"], 3);
    assert_eq!(result["comment"]["id"], 5);
    assert_eq!(&*provider.archive.borrow(), b"archive");
    assert!(
        provider
            .comment
            .borrow()
            .starts_with("Prefix 雪\n\n[Pull request overview")
    );
}

#[test]
fn missing_tools_report_snapshot_without_remote_writes() {
    let fixture = fixture();
    prepare(fixture.path());
    let provider = Azure::default();
    let result = azure_publish(fixture.path(), &provider, true);
    assert_eq!(result["stage"], "renderHtml");
    assert_eq!(result["missingTools"], json!(["pandoc"]));
    assert!(result["snapshot"].is_object());
    assert!(result["operations"].as_array().unwrap().is_empty());
    assert!(
        provider
            .requests
            .borrow()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}

#[test]
fn azure_proven_rate_refusal_allows_retry() {
    let fixture = fixture();
    prepare(fixture.path());
    let refused = Azure {
        reject_upload: true,
        ..Azure::default()
    };
    let result = azure_publish(fixture.path(), &refused, false);
    assert_eq!(result["operations"][0]["status"], "failed", "{result}");
    let accepted = Azure::default();
    let result = azure_publish(fixture.path(), &accepted, false);
    assert_eq!(result["status"], "succeeded", "{result}");
}

#[test]
fn changed_unpublished_overview_preserves_previous_generation() {
    let fixture = fixture();
    prepare(fixture.path());
    let provider = Azure::default();
    let first = azure_publish(fixture.path(), &provider, true);
    let first_id = first["snapshot"]["runId"].as_str().unwrap();
    let first_report = PathBuf::from(first["snapshot"]["markdownPath"].as_str().unwrap());
    let report_bytes = fs::read(&first_report).unwrap();
    let generation = fixture
        .path()
        .join("output 雪/overview-publication")
        .join(first_id);
    fs::create_dir_all(&generation).unwrap();
    fs::write(generation.join("report.html"), b"orphaned report").unwrap();

    fs::write(
        fixture.path().join("overview 雪.md"),
        format!("{}\nUpdated content\n", overview()),
    )
    .unwrap();
    let next = azure_publish(fixture.path(), &provider, true);
    assert_ne!(next["snapshot"]["runId"], first_id);
    assert_eq!(fs::read(first_report).unwrap(), report_bytes);
    assert_eq!(
        fs::read(generation.join("report.html")).unwrap(),
        b"orphaned report"
    );
}

#[test]
fn cli_reuses_completed_retry_artifact_and_preserves_orphan() {
    let fixture = fixture();
    prepare(fixture.path());
    fs::write(
        fixture.path().join("capture 雪.json"),
        to_vec(&azure_capture()).unwrap(),
    )
    .unwrap();
    let provider = Azure::default();
    // The unrecorded root report represents an interrupted attempt; the manifest
    // identifies a completed retry for the same snapshot that must be reused
    // without modifying the orphan.
    let pending = azure_publish(fixture.path(), &provider, true);
    let snapshot = &pending["snapshot"];
    // The CLI resolves the output path before recording artifact paths.
    let root = absolute(fixture.path().join("output 雪"))
        .unwrap()
        .join("overview-publication")
        .join(snapshot["runId"].as_str().unwrap());
    let retry = root.join(".overview-attempt-fixture");
    fs::create_dir_all(&retry).unwrap();
    fs::write(root.join("report.html"), b"orphaned output").unwrap();
    let html = retry.join("report.html");
    let archive = retry.join("report.zip");
    fs::write(&html, b"html").unwrap();
    fs::write(&archive, b"archive").unwrap();
    let manifest = json!({
        "sourcePath":snapshot["markdownPath"], "sourceHash":snapshot["markdownHash"],
        "htmlPath":html, "htmlUri":"file:///report.html",
        "htmlHash":format!("{:x}", Sha256::digest(b"html")), "diagramCount":0,
        "archivePath":archive, "archiveHash":format!("{:x}", Sha256::digest(b"archive"))
    });
    fs::write(root.join("html.json"), to_vec(&manifest).unwrap()).unwrap();

    let result: Value = from_str(
        &run(
            &arguments(fixture.path()),
            &provider,
            &provider,
            &SystemClock::default(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(
        fs::read(root.join("report.html")).unwrap(),
        b"orphaned output"
    );
    assert_eq!(&*provider.archive.borrow(), b"archive");
}

/// Coordinates a paused provider write and observed real lock contention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlapEvent {
    Upload,
    PublisherWait,
    SnapshotWait,
    FirstDone,
    PublisherDone,
    SnapshotDone,
}

/// Waits for explicit release after the first real contention observation.
struct OverlapClock {
    event: OverlapEvent,
    events: Sender<OverlapEvent>,
    release: RefCell<Option<Receiver<()>>>,
}

impl Clock for OverlapClock {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        if let Some(release) = self.release.borrow_mut().take() {
            self.events.send(self.event).map_err(io::Error::other)?;
            release.recv().map_err(io::Error::other)?;
        }
        Ok(())
    }
}

/// Blocks the first upload while the competing acquisitions are observed.
struct PausedProvider {
    provider: Provider,
    events: Sender<OverlapEvent>,
    release: Receiver<()>,
}

impl Process for PausedProvider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if arguments.get(1).is_some_and(|route| route == "gists")
            && arguments
                .windows(2)
                .any(|pair| pair == ["--method", "POST"])
        {
            self.events
                .send(OverlapEvent::Upload)
                .map_err(io::Error::other)?;
            self.release.recv().map_err(io::Error::other)?;
        }
        self.provider.execute(program, arguments, input)
    }
}

/// Runs the public publication boundary with independent per-worker providers.
fn overlap_publish(
    root: &Path,
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
) -> Value {
    let capture = capture();
    let overview = root.join("overview 雪.md");
    let output = root.join("output 雪");
    let skill = root.join("skill 雪");
    let input = OverviewInput {
        capture: &capture,
        overview_path: &overview,
        output_directory: &output,
        skill_root: &skill,
        comment_prefix: "first prefix",
        tenant: None,
    };
    publish_overview(&input, process, http, clock, |_| {
        panic!("GitHub publication must not render HTML")
    })
    .unwrap()
}

#[test]
fn overlapping_publishers_reuse_success_or_preserve_ambiguous_write() {
    for ambiguous in [false, true] {
        let fixture = fixture();
        prepare(fixture.path());
        let root = fixture.path().to_owned();
        let (events, received) = mpsc::channel();
        let (upload_release, upload_wait) = mpsc::channel();
        let first = thread::spawn({
            let root = root.clone();
            let events = events.clone();
            move || {
                let outcome = std::panic::catch_unwind(|| {
                    let provider = PausedProvider {
                        provider: Provider {
                            fail_upload: ambiguous,
                            ..Provider::default()
                        },
                        events: events.clone(),
                        release: upload_wait,
                    };
                    let clock = OverlapClock {
                        event: OverlapEvent::FirstDone,
                        events: events.clone(),
                        release: RefCell::new(None),
                    };
                    overlap_publish(&root, &provider, &provider.provider, &clock)
                });
                if events.send(OverlapEvent::FirstDone).is_err() {
                    eprintln!("overlap observer stopped before the first publisher finished");
                }
                outcome
            }
        });

        let initial = received.recv();
        if initial != Ok(OverlapEvent::Upload) {
            drop(upload_release);
            drop(first.join());
            panic!("first publisher did not reach upload: {initial:?}");
        }

        let (publisher_release, publisher_wait) = mpsc::channel();
        let second = thread::spawn({
            let root = root.clone();
            let events = events.clone();
            move || {
                let outcome = std::panic::catch_unwind(|| {
                    let provider = Provider::default();
                    let clock = OverlapClock {
                        event: OverlapEvent::PublisherWait,
                        events: events.clone(),
                        release: RefCell::new(Some(publisher_wait)),
                    };
                    let result = overlap_publish(&root, &provider, &provider, &clock);
                    (result, provider.requests.into_inner())
                });
                if events.send(OverlapEvent::PublisherDone).is_err() {
                    eprintln!("overlap observer stopped before the waiting publisher finished");
                }
                outcome
            }
        });
        let (snapshot_release, snapshot_wait) = mpsc::channel();
        let snapshot = thread::spawn({
            let root = root.clone();
            let events = events.clone();
            move || {
                let outcome = std::panic::catch_unwind(|| {
                    let clock = OverlapClock {
                        event: OverlapEvent::SnapshotWait,
                        events: events.clone(),
                        release: RefCell::new(Some(snapshot_wait)),
                    };
                    FileTransaction::open(&root.join("output 雪/overview-snapshot.json"), &clock)
                        .is_ok()
                });
                if events.send(OverlapEvent::SnapshotDone).is_err() {
                    eprintln!("overlap observer stopped before the snapshot acquisition finished");
                }
                outcome
            }
        });
        drop(events);

        // A completion signal also counts as progress, so a failed worker
        // cannot leave the other workers blocked while assertions run.
        let mut publisher_observation = None;
        let mut snapshot_observation = None;
        while publisher_observation.is_none() || snapshot_observation.is_none() {
            match received.recv() {
                Ok(event @ (OverlapEvent::PublisherWait | OverlapEvent::PublisherDone)) => {
                    publisher_observation = Some(event);
                }
                Ok(event @ (OverlapEvent::SnapshotWait | OverlapEvent::SnapshotDone)) => {
                    snapshot_observation = Some(event);
                }
                Ok(OverlapEvent::FirstDone) | Err(_) => break,
                Ok(OverlapEvent::Upload) => {}
            }
        }

        let upload_released = upload_release.send(()).is_ok();
        let first = first.join();
        let publisher_released = publisher_release.send(()).is_ok();
        let snapshot_released = snapshot_release.send(()).is_ok();
        let second = second.join();
        let snapshot = snapshot.join();

        assert_eq!(publisher_observation, Some(OverlapEvent::PublisherWait));
        assert_eq!(snapshot_observation, Some(OverlapEvent::SnapshotWait));
        assert!(upload_released && publisher_released && snapshot_released);
        let first = first.unwrap().unwrap();
        let (second, requests) = second.unwrap().unwrap();
        assert!(snapshot.unwrap().unwrap());
        assert!(requests.is_empty(), "{requests:?}");
        if ambiguous {
            assert_eq!(first["operations"][0]["status"], "ambiguous");
            assert_eq!(second["status"], "needsReconciliation");
            assert_eq!(second["operations"], first["operations"]);
        } else {
            assert_eq!(first["status"], "succeeded", "{first}");
            assert_eq!(second, first);
        }
    }
}
