//! Exercises inline overview comments through the CLI and injected providers.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Bounded provider fixtures favor concise assertions"
)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_slice, from_str, json, to_vec};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput, SystemClock, run};
use ss_ai_runtime::FileTransaction;
use tempfile::{Builder, TempDir};

/// Models GitHub issue-comment writes and independent readback.
#[derive(Default)]
struct GitHub {
    requests: RefCell<Vec<String>>,
    comments: RefCell<Vec<String>>,
    refuse_once: Cell<bool>,
    lose_once: Cell<bool>,
    reject_readback: bool,
}

impl Process for GitHub {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        let route = arguments.get(1).expect("gh route");
        let method = arguments
            .windows(2)
            .find(|pair| pair.first().is_some_and(|value| value == "--method"))
            .expect("gh method")
            .get(1)
            .expect("method value");
        self.requests.borrow_mut().push(format!("{method} {route}"));
        if method == "POST"
            && route.as_str() == "repos/owner/repo/issues/42/comments"
            && self.refuse_once.replace(false)
        {
            return Ok(ProcessOutput::new(
                false,
                b"HTTP/2 429\r\nRetry-After: 2\r\n\r\n{}".to_vec(),
                Vec::new(),
            ));
        }
        if method == "POST"
            && route.as_str() == "repos/owner/repo/issues/42/comments"
            && self.lose_once.replace(false)
        {
            return Ok(ProcessOutput::new(
                false,
                Vec::new(),
                b"lost response".to_vec(),
            ));
        }
        let response = match (method.as_str(), route.as_str()) {
            ("GET", "repos/owner/repo/pulls/42") => json!({ "state": "open" }),
            ("POST", "repos/owner/repo/issues/42/comments") => {
                let body: Value = from_slice(input).unwrap();
                self.comments
                    .borrow_mut()
                    .push(body["body"].as_str().unwrap().to_owned());
                let id = self.comments.borrow().len() + 98;
                json!({
                    "id": id,
                    "html_url": format!("https://github.com/owner/repo/issues/42#issuecomment-{id}")
                })
            }
            ("GET", route) if route.starts_with("repos/owner/repo/issues/comments/") => {
                let id: usize = route.rsplit('/').next().unwrap().parse().unwrap();
                let body = if self.reject_readback {
                    "different".to_owned()
                } else {
                    self.comments.borrow()[id - 99].clone()
                };
                json!({ "id": id, "body": body })
            }
            _ => panic!("unexpected GitHub request: {method} {route}"),
        };
        Ok(ProcessOutput::new(
            true,
            to_vec(&response).unwrap(),
            Vec::new(),
        ))
    }
}

impl Http for GitHub {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub overview publication must not use Azure HTTP")
    }
}

/// Models ADO thread creation as one resolved root comment per overview part.
#[derive(Default)]
struct Azure {
    requests: RefCell<Vec<String>>,
    comments: RefCell<Vec<String>>,
    throttle_once: Cell<bool>,
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
        if method == "POST" && self.throttle_once.replace(false) {
            let mut headers = BTreeMap::new();
            headers.insert("retry-after".to_owned(), "2".to_owned());
            return Ok(HttpResponse::new(429, headers, Vec::new()));
        }
        let response = if method == "POST" && url.contains("/threads?") {
            let body: Value = from_slice(body.unwrap()).unwrap();
            self.comments
                .borrow_mut()
                .push(body["comments"][0]["content"].as_str().unwrap().to_owned());
            json!({ "id": self.comments.borrow().len() })
        } else if url.contains("/threads/") {
            let id: usize = url
                .split("/threads/")
                .nth(1)
                .unwrap()
                .split('?')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            json!({
                "status": "fixed",
                "comments": [{ "content": self.comments.borrow()[id - 1] }]
            })
        } else {
            json!({ "status": "active" })
        };
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            to_vec(&response).unwrap(),
        ))
    }

    fn send_binary(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        panic!("inline overview publication must not upload binary artifacts")
    }
}

/// Produces deterministic retry waits without reading wall-clock time.
struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Ok(())
    }
}

/// Identifies controlled overlap points without relying on wall-clock timing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlapEvent {
    CommentWrite,
    PublisherWait,
    SnapshotWait,
}

/// Pauses the first provider comment while the transaction retains both locks.
struct PausedGitHub {
    provider: GitHub,
    events: Sender<OverlapEvent>,
    release: Receiver<()>,
    pause_on_write: usize,
    fail_after_pause: bool,
    writes: Cell<usize>,
}

impl Process for PausedGitHub {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        let is_comment = arguments
            .get(1)
            .is_some_and(|route| route == "repos/owner/repo/issues/42/comments")
            && arguments
                .windows(2)
                .any(|pair| pair == ["--method", "POST"]);
        if is_comment {
            let write = self
                .writes
                .get()
                .checked_add(1)
                .ok_or_else(|| io::Error::other("overview comment write count overflowed"))?;
            self.writes.set(write);
            if write == self.pause_on_write {
                self.events
                    .send(OverlapEvent::CommentWrite)
                    .map_err(io::Error::other)?;
                self.release.recv().map_err(io::Error::other)?;
                let response = self.provider.execute(program, arguments, input)?;
                if self.fail_after_pause {
                    return Ok(ProcessOutput::new(
                        false,
                        Vec::new(),
                        b"lost response".to_vec(),
                    ));
                }
                return Ok(response);
            }
        }
        self.provider.execute(program, arguments, input)
    }
}

impl Http for PausedGitHub {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub overview publication must not use Azure HTTP")
    }
}

/// Reports real file-lock contention and waits for caller-controlled release.
struct OverlapClock {
    event: OverlapEvent,
    events: Sender<OverlapEvent>,
    release: RefCell<Option<Receiver<()>>>,
}

impl Clock for OverlapClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        if let Some(release) = self.release.borrow_mut().take() {
            self.events
                .send(self.event)
                .map_err(|error| AppError::from(io::Error::other(error)))?;
            release
                .recv()
                .map_err(|error| AppError::from(io::Error::other(error)))?;
        }
        Ok(())
    }
}

/// Owns a Unicode fixture beneath the checkout target directory.
fn fixture() -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    fs::create_dir_all(&root).unwrap();
    Builder::new()
        .prefix("overview-inline-雪-")
        .tempdir_in(root)
        .unwrap()
}

/// Supplies the complete capture contract required by overview publication.
fn capture(azure: bool) -> Value {
    if azure {
        json!({
            "schemaVersion": 3, "platform": "azureDevOps",
            "repositoryUrl": "https://dev.azure.com/org/proj/_git/repo",
            "pullRequestUrl": "https://dev.azure.com/org/proj/_git/repo/pullrequest/42",
            "target": {
                "platform": "azureDevOps", "collectionUrl": "https://dev.azure.com/org",
                "organization": "org", "project": "proj", "repository": "repo",
                "pullRequestId": 42,
                "webUrl": "https://dev.azure.com/org/proj/_git/repo/pullrequest/42"
            },
            "pullRequest": {
                "id": 42, "title": "Title", "description": null,
                "reviewedSourceCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "reviewedMergeBaseCommit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            },
            "reviewers": [], "revisions": [], "threads": []
        })
    } else {
        json!({
            "schemaVersion": 3, "platform": "gitHub",
            "repositoryUrl": "https://github.com/owner/repo",
            "pullRequestUrl": "https://github.com/owner/repo/pull/42",
            "target": {
                "platform": "gitHub", "collectionUrl": null,
                "organization": "owner",
                "project": null, "repository": "repo", "pullRequestId": 42,
                "webUrl": "https://github.com/owner/repo/pull/42"
            },
            "pullRequest": {
                "id": 42, "title": "Title", "description": null,
                "reviewedSourceCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "reviewedMergeBaseCommit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            },
            "reviewers": [], "revisions": [], "threads": []
        })
    }
}

/// Supplies a complete ordered overview that exceeds one provider comment.
fn overview() -> String {
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
    headings
        .iter()
        .enumerate()
        .fold(String::new(), |mut overview, (index, name)| {
            let body = if index == 0 {
                "Résumé 雪 🦀\n\n".repeat(8000)
            } else {
                "Content\n".to_owned()
            };
            write!(overview, "## {name}\n{body}").unwrap();
            overview
        })
}

/// Builds the exact named CLI arguments for one provider fixture.
fn arguments(root: &Path, azure: bool) -> Vec<OsString> {
    let skill = root.join("skill 雪");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        root.join("capture 雪.json"),
        to_vec(&capture(azure)).unwrap(),
    )
    .unwrap();
    fs::write(root.join("overview 雪.md"), overview()).unwrap();
    fs::write(root.join("prefix 雪.txt"), "Attribution 雪\n").unwrap();
    let mut arguments: Vec<OsString> = ["ss-ai", "review", "publish-overview"]
        .into_iter()
        .map(Into::into)
        .collect();
    for (name, value) in [
        ("context-path", root.join("capture 雪.json")),
        ("overview-path", root.join("overview 雪.md")),
        ("output-directory", root.join("output 雪")),
        ("skill-root", skill),
        ("comment-prefix-path", root.join("prefix 雪.txt")),
    ] {
        let mut argument = OsString::from(format!("--{name}="));
        argument.push(value);
        arguments.push(argument);
    }
    arguments
}

/// Executes publication and decodes its interoperable result.
fn publish(
    arguments: &[OsString],
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
) -> Value {
    from_str(&run(arguments, process, http, clock).unwrap()).unwrap()
}

#[test]
fn github_posts_bounded_inline_parts_and_reuses_receipts() {
    let fixture = fixture();
    let arguments = arguments(fixture.path(), false);
    let provider = GitHub::default();
    let result = publish(&arguments, &provider, &provider, &SystemClock::default());
    assert_eq!(result["status"], "succeeded");
    assert!(result.get("artifact").is_none());
    let comments = provider.comments.borrow();
    assert!(comments.len() > 1);
    assert!(comments.iter().all(|comment| comment.len() <= 64 * 1024));
    assert!(
        comments
            .iter()
            .all(|comment| { comment.starts_with("[Copilot speaking]\n\nAttribution 雪\n\n") })
    );
    assert_eq!(result["comments"].as_array().unwrap().len(), comments.len());
    assert_eq!(
        result["operations"].as_array().unwrap().len(),
        comments.len()
    );
    assert!(
        provider
            .requests
            .borrow()
            .iter()
            .all(|request| !request.contains("gists"))
    );

    let request_count = provider.requests.borrow().len();
    assert_eq!(
        publish(&arguments, &provider, &provider, &SystemClock::default()),
        result
    );
    assert_eq!(provider.requests.borrow().len(), request_count);
}

#[test]
fn github_retries_only_proven_comment_refusals() {
    let fixture = fixture();
    let arguments = arguments(fixture.path(), false);
    let provider = GitHub {
        refuse_once: Cell::new(true),
        ..GitHub::default()
    };
    let result = publish(&arguments, &provider, &provider, &TestClock);
    assert_eq!(result["status"], "succeeded");
    assert_eq!(
        provider
            .requests
            .borrow()
            .iter()
            .filter(|request| { request.as_str() == "POST repos/owner/repo/issues/42/comments" })
            .count(),
        provider.comments.borrow().len() + 1
    );
}

#[test]
fn github_ambiguous_comment_write_prevents_replay() {
    let fixture = fixture();
    let arguments = arguments(fixture.path(), false);
    let provider = GitHub {
        lose_once: Cell::new(true),
        ..GitHub::default()
    };
    let result = publish(&arguments, &provider, &provider, &TestClock);
    assert_eq!(result["status"], "incomplete");
    assert_eq!(result["operations"][0]["status"], "ambiguous");
    let request_count = provider.requests.borrow().len();
    let resumed = publish(&arguments, &provider, &provider, &TestClock);
    assert_eq!(resumed["status"], "needsReconciliation");
    assert_eq!(provider.requests.borrow().len(), request_count);
}

#[test]
fn failed_readback_retains_every_completed_receipt() {
    let fixture = fixture();
    let arguments = arguments(fixture.path(), false);
    let provider = GitHub {
        reject_readback: true,
        ..GitHub::default()
    };
    let result = publish(&arguments, &provider, &provider, &TestClock);
    assert_eq!(result["status"], "incomplete");
    assert_eq!(result["comments"].as_array().unwrap().len(), 1);
    assert_eq!(result["operations"][0]["status"], "completed");
}

#[test]
fn azure_posts_inline_resolved_threads_without_artifacts() {
    let fixture = fixture();
    let arguments = arguments(fixture.path(), true);
    let provider = Azure::default();
    let result = publish(&arguments, &provider, &provider, &TestClock);
    assert_eq!(result["status"], "succeeded");
    assert!(provider.comments.borrow().len() > 1);
    assert!(
        provider
            .comments
            .borrow()
            .iter()
            .all(|comment| comment.len() <= 64 * 1024)
    );
    assert!(
        provider
            .requests
            .borrow()
            .iter()
            .all(|request| !request.contains("/attachments/"))
    );
}

#[test]
fn azure_retries_an_explicit_comment_throttle() {
    let fixture = fixture();
    let arguments = arguments(fixture.path(), true);
    let provider = Azure {
        throttle_once: Cell::new(true),
        ..Azure::default()
    };
    let result = publish(&arguments, &provider, &provider, &TestClock);
    assert_eq!(result["status"], "succeeded");
    assert_eq!(
        provider
            .requests
            .borrow()
            .iter()
            .filter(|request| request.starts_with("POST ") && request.contains("/threads?"))
            .count(),
        provider.comments.borrow().len() + 1
    );
}

#[test]
fn overlapping_publishers_and_snapshot_writer_contend_without_reposting() {
    for ambiguous in [false, true] {
        let fixture = fixture();
        let arguments = arguments(fixture.path(), false);
        let output = fixture.path().join("output 雪");
        let (events, observed) = mpsc::channel();
        let (comment_release, comment_wait) = mpsc::channel();
        let first = thread::spawn({
            let arguments = arguments.clone();
            let events = events.clone();
            move || {
                let provider = PausedGitHub {
                    provider: GitHub::default(),
                    events,
                    release: comment_wait,
                    pause_on_write: 2,
                    fail_after_pause: ambiguous,
                    writes: Cell::new(0),
                };
                publish(&arguments, &provider, &provider, &TestClock)
            }
        });
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(60)).unwrap(),
            OverlapEvent::CommentWrite
        );

        let (publisher_release, publisher_wait) = mpsc::channel();
        let publisher = thread::spawn({
            let arguments = arguments.clone();
            let events = events.clone();
            move || {
                let provider = GitHub::default();
                let clock = OverlapClock {
                    event: OverlapEvent::PublisherWait,
                    events,
                    release: RefCell::new(Some(publisher_wait)),
                };
                let result = publish(&arguments, &provider, &provider, &clock);
                (result, provider.requests.into_inner())
            }
        });

        let (snapshot_release, snapshot_wait) = mpsc::channel();
        let snapshot = thread::spawn({
            let events = events.clone();
            let snapshot_path = output.join("overview-snapshot.json");
            move || {
                let clock = OverlapClock {
                    event: OverlapEvent::SnapshotWait,
                    events,
                    release: RefCell::new(Some(snapshot_wait)),
                };
                FileTransaction::open(&snapshot_path, &clock).is_ok()
            }
        });

        let mut waits = vec![
            observed.recv_timeout(Duration::from_secs(60)).unwrap(),
            observed.recv_timeout(Duration::from_secs(60)).unwrap(),
        ];
        waits.sort_by_key(|event| match event {
            OverlapEvent::PublisherWait => 0,
            OverlapEvent::SnapshotWait => 1,
            OverlapEvent::CommentWrite => 2,
        });
        assert_eq!(
            waits,
            [OverlapEvent::PublisherWait, OverlapEvent::SnapshotWait]
        );

        comment_release.send(()).unwrap();
        let first = first.join().unwrap();
        publisher_release.send(()).unwrap();
        snapshot_release.send(()).unwrap();
        let (second, requests) = publisher.join().unwrap();
        assert!(snapshot.join().unwrap());

        if ambiguous {
            assert_eq!(first["status"], "incomplete");
            assert_eq!(first["comments"].as_array().unwrap().len(), 1);
            assert_eq!(first["operations"][0]["status"], "completed");
            assert_eq!(first["operations"][1]["status"], "ambiguous");
            assert_eq!(second["status"], "needsReconciliation");
            assert_eq!(second["operations"], first["operations"]);
            assert_eq!(second["comments"], first["comments"]);
        } else {
            assert_eq!(first["status"], "succeeded");
            assert_eq!(second, first);
        }
        assert!(requests.is_empty());
    }
}

#[test]
fn invalid_capture_and_inaccessible_output_do_not_write_comments() {
    let fixture = fixture();
    let arguments = arguments(fixture.path(), false);
    fs::write(fixture.path().join("capture 雪.json"), b"{}").unwrap();
    let provider = GitHub::default();
    run(&arguments, &provider, &provider, &SystemClock::default()).unwrap_err();
    assert!(provider.requests.borrow().is_empty());

    fs::write(
        fixture.path().join("capture 雪.json"),
        to_vec(&capture(false)).unwrap(),
    )
    .unwrap();
    fs::write(fixture.path().join("output 雪"), b"not a directory").unwrap();
    let result = publish(&arguments, &provider, &provider, &SystemClock::default());
    assert_eq!(result["status"], "journalFailure");
    assert!(provider.requests.borrow().is_empty());
}

#[test]
fn unpublished_private_paths_never_post() {
    let fixture = fixture();
    let arguments = arguments(fixture.path(), false);
    // `OverviewCli` converts its inputs to absolute paths before publication.
    // Match that spelling so this fixture does not depend on platform path cleanup.
    let private = std::path::absolute(fixture.path().join("output 雪")).unwrap();
    let source = format!("{}\n{}", overview(), private.display());
    fs::write(fixture.path().join("overview 雪.md"), source).unwrap();
    let provider = GitHub::default();
    let result = publish(&arguments, &provider, &provider, &SystemClock::default());
    assert_eq!(result["status"], "failed");
    assert_eq!(result["stage"], "freeze");
    assert!(result["error"].as_str().unwrap().contains("private path"));
    assert_eq!(provider.comments.borrow().len(), 0);
    assert_eq!(
        provider.requests.borrow().as_slice(),
        ["GET repos/owner/repo/pulls/42"]
    );
}

#[test]
fn malformed_sections_never_post() {
    let fixture = fixture();
    let arguments = arguments(fixture.path(), false);
    fs::write(
        fixture.path().join("overview 雪.md"),
        "# Content overview\n\n## Mission\nContent\n",
    )
    .unwrap();
    let provider = GitHub::default();
    let result = publish(&arguments, &provider, &provider, &SystemClock::default());
    assert_eq!(result["status"], "failed");
    assert_eq!(result["stage"], "freeze");
    assert_eq!(provider.comments.borrow().len(), 0);
    assert_eq!(
        provider.requests.borrow().as_slice(),
        ["GET repos/owner/repo/pulls/42"]
    );
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn real_binary_rejects_bad_capture_before_provider_access() {
    let fixture = fixture();
    let arguments = arguments(fixture.path(), false);
    fs::write(fixture.path().join("capture 雪.json"), b"{}").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(arguments.iter().skip(1))
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
