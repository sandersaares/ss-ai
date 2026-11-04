//! Exercises overview publication without invoking a remote provider.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai::{Http, HttpResponse, Process, ProcessOutput, run};
use ss_ai_review::{Clock, OverviewInput, OverviewRender, publish_overview};
use ss_ai_review_workflow::overview_artifact::{Preparation, freeze};
use ss_ai_runtime::fail;

/// Stops unexpected retries without reading or waiting on the real clock.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("this scenario must not wait")
    }
}

/// Owns the fixture paths needed by the file-based publication boundary.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        // The counter only assigns distinct names; no fixture state is shared,
        // so it does not need to order other memory accesses.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".overview-retirement-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        #[expect(clippy::create_dir, reason = "Fixture collisions must fail")]
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn scenario(&self, name: &str, source: &str, prefix: &str) -> Vec<OsString> {
        let root = self.0.join(name);
        fs::create_dir_all(root.join("skill")).unwrap();
        fs::write(root.join("overview.md"), source).unwrap();
        fs::write(root.join("prefix.txt"), prefix).unwrap();
        fs::write(
            root.join("capture.json"),
            serde_json::to_vec(&json!({
                "schemaVersion": 3,
                "platform": "gitHub",
                "repositoryUrl": "https://github.com/owner/repo",
                "pullRequestUrl": "https://github.com/owner/repo/pull/42",
                "target": {
                    "platform": "gitHub", "collectionUrl": null,
                    "organization": "owner", "project": null, "repository": "repo",
                    "pullRequestId": 42, "webUrl": "https://github.com/owner/repo/pull/42"
                },
                "pullRequest": {
                    "id": 42, "title": "Title", "description": null,
                    "reviewedSourceCommit": "a".repeat(40),
                    "reviewedMergeBaseCommit": "b".repeat(40)
                },
                "reviewers": [], "revisions": [], "threads": []
            }))
            .unwrap(),
        )
        .unwrap();
        let mut args: Vec<OsString> = ["ss-ai", "review", "publish-overview"]
            .into_iter()
            .map(Into::into)
            .collect();
        for (name, path) in [
            ("context-path", root.join("capture.json")),
            ("overview-path", root.join("overview.md")),
            ("output-directory", root.join("output")),
            ("skill-root", root.join("skill")),
            ("comment-prefix-path", root.join("prefix.txt")),
        ] {
            let mut argument = OsString::from(format!("--{name}="));
            argument.push(path);
            args.push(argument);
        }
        args
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Records each scripted request and returns content for readback verification.
#[derive(Default)]
struct Provider {
    requests: RefCell<Vec<String>>,
    markdown: RefCell<String>,
    comment: RefCell<String>,
    fail_comment: Cell<bool>,
    fail_request: Option<&'static str>,
    truncated_response: Option<&'static str>,
    refuse_upload_once: Cell<bool>,
    refuse_comment_once: Cell<bool>,
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        let route = arguments.get(1).unwrap();
        let method = arguments
            .windows(2)
            .find(|pair| pair.first().is_some_and(|value| value == "--method"))
            .unwrap()
            .get(1)
            .unwrap();
        self.requests.borrow_mut().push(format!("{method} {route}"));
        if method == "POST" && self.truncated_response == Some(route.as_str()) {
            let status = if route == "gists" { 429 } else { 403 };
            return Ok(ProcessOutput::new(
                false,
                format!("HTTP/2 {status}\r\n\r\n{{\"id\":\"accepted\"").into_bytes(),
                b"rate limit".to_vec(),
            ));
        }
        let refusal = match (method.as_str(), route.as_str()) {
            ("POST", "gists") if self.refuse_upload_once.replace(false) => Some(
                "HTTP/2 403\r\nRetry-After: 2\r\n\r\n{\"message\":\"API rate limit exceeded\"}",
            ),
            ("POST", "repos/owner/repo/issues/42/comments")
                if self.refuse_comment_once.replace(false) =>
            {
                Some("HTTP/2 429\r\nRetry-After: 2\r\n\r\n{}")
            }
            _ => None,
        };
        if let Some(refusal) = refusal {
            return Ok(ProcessOutput::new(
                false,
                refusal.as_bytes().to_vec(),
                vec![],
            ));
        }
        if self.fail_request == Some(route.as_str()) {
            return Ok(ProcessOutput::new(
                false,
                Vec::new(),
                b"scripted provider failure".to_vec(),
            ));
        }
        let response = match (method.as_str(), route.as_str()) {
            ("GET", "repos/owner/repo/pulls/42") => json!({"state": "open"}),
            ("POST", "gists") => {
                let body: Value = serde_json::from_slice(input).unwrap();
                body.pointer("/files/overview.md/content")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .clone_into(&mut self.markdown.borrow_mut());
                json!({"id":"abc123","html_url":"https://gist.github.com/owner/abc123"})
            }
            ("GET", "gists/abc123") => json!({
                "id":"abc123", "public":false,
                "html_url":"https://gist.github.com/owner/abc123",
                "history":[{"version":"a".repeat(40)}],
                "files":{"overview.md":{"truncated":false,"content":*self.markdown.borrow()}}
            }),
            ("POST", "repos/owner/repo/issues/42/comments") => {
                let body: Value = serde_json::from_slice(input).unwrap();
                body.get("body")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .clone_into(&mut self.comment.borrow_mut());
                if self.fail_comment.get() {
                    return Ok(ProcessOutput::new(
                        false,
                        Vec::new(),
                        b"lost response".to_vec(),
                    ));
                }
                json!({"id":99,"html_url":"https://github.com/owner/repo/issues/42#issuecomment-99"})
            }
            ("GET", "repos/owner/repo/issues/comments/99") => {
                json!({"id":99,"body":*self.comment.borrow()})
            }
            _ => panic!("unexpected request: {method} {route}"),
        };
        Ok(ProcessOutput::new(
            true,
            serde_json::to_vec(&response).unwrap(),
            Vec::new(),
        ))
    }
}

impl Http for Provider {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub publication must not use Azure HTTP")
    }
}

/// Scripts Azure operations and rejects writes unless a scenario permits them.
#[derive(Default)]
struct AzureProvider {
    requests: RefCell<Vec<String>>,
    allow_writes: bool,
    archive: RefCell<Vec<u8>>,
    comment: RefCell<String>,
}

impl Process for AzureProvider {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "az");
        Ok(ProcessOutput::new(
            true,
            b"fixture-token".to_vec(),
            Vec::new(),
        ))
    }
}

impl Http for AzureProvider {
    fn send(
        &self,
        method: &str,
        url: &str,
        _: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        self.requests.borrow_mut().push(format!("{method} {url}"));
        let response = if method == "POST" && url.contains("/threads?") {
            assert!(self.allow_writes);
            let body: Value = serde_json::from_slice(body.unwrap()).unwrap();
            body.pointer("/comments/0/content")
                .unwrap()
                .as_str()
                .unwrap()
                .clone_into(&mut self.comment.borrow_mut());
            json!({"id":5})
        } else if url.contains("/threads/5?") {
            json!({"status":"fixed","comments":[{"content":*self.comment.borrow()}]})
        } else {
            assert_eq!(method, "GET");
            json!({"status":"active"})
        };
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            serde_json::to_vec(&response).unwrap(),
        ))
    }

    fn send_binary(
        &self,
        method: &str,
        url: &str,
        _: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert!(self.allow_writes, "invalid artifacts must not be uploaded");
        self.requests.borrow_mut().push(format!("{method} {url}"));
        let response = if method == "POST" {
            body.unwrap().clone_into(&mut self.archive.borrow_mut());
            br#"{"id":3}"#.to_vec()
        } else {
            assert_eq!(method, "GET");
            self.archive.borrow().clone()
        };
        Ok(HttpResponse::new(200, BTreeMap::new(), response))
    }
}

/// Uses the publication command's named file arguments with offline effects.
fn publish(args: &[OsString], provider: &Provider) -> Value {
    serde_json::from_str(&run(args, provider, provider, &NoWait).unwrap()).unwrap()
}

/// Gives the shared capture fixture Azure routing for attachment publication.
fn azure_capture(root: &Path) -> Value {
    let mut capture: Value =
        serde_json::from_slice(&fs::read(root.join("capture.json")).unwrap()).unwrap();
    let url = "https://dev.azure.com/org/proj/_git/repo/pullrequest/42";
    *capture.get_mut("platform").unwrap() = json!("azureDevOps");
    *capture.get_mut("repositoryUrl").unwrap() = json!("https://dev.azure.com/org/proj/_git/repo");
    *capture.get_mut("pullRequestUrl").unwrap() = json!(url);
    *capture.get_mut("target").unwrap() = json!({
        "platform":"azureDevOps", "collectionUrl":"https://dev.azure.com/org",
        "organization":"org", "project":"proj", "repository":"repo",
        "pullRequestId":42, "webUrl":url
    });
    capture
}

/// Supplies a complete, ordered overview while keeping individual sections distinct.
#[expect(
    clippy::format_collect,
    reason = "The bounded fixture is clearer as a list of headings"
)]
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
    .map(|heading| format!("## {heading}\nEvidence\n"))
    .collect()
}

/// Prefix bytes, validated artifacts, and durable receipts define publication.
#[test]
fn publication_preserves_prefix_and_partial_effects() {
    let workspace = Workspace::new();
    let source = overview();
    let link = format!(
        "[Pull request overview (auto-generated).](https://gist.github.com/owner/abc123/{})",
        "a".repeat(40)
    );
    for (name, prefix) in [
        ("empty", ""),
        ("attribution", "[Copilot speaking]"),
        ("unicode", "署名\n\n"),
    ] {
        let args = workspace.scenario(name, &source, prefix);
        let provider = Provider::default();
        let result = publish(&args, &provider);
        assert_eq!(result.get("status"), Some(&json!("succeeded")));
        assert_eq!(
            result.pointer("/operations/0/status"),
            Some(&json!("completed"))
        );
        assert_eq!(
            result.pointer("/operations/1/status"),
            Some(&json!("completed"))
        );
        let report = fs::read_to_string(
            result
                .pointer("/snapshot/markdownPath")
                .unwrap()
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert!(report.contains("before the review"));
        assert!(report.contains(&"a".repeat(40)));
        let expected = if prefix.is_empty() {
            link.clone()
        } else {
            format!("{prefix}\n\n{link}")
        };
        assert_eq!(*provider.comment.borrow(), expected);
        assert_eq!(
            provider.requests.borrow().as_slice(),
            [
                "GET repos/owner/repo/pulls/42",
                "POST gists",
                "GET gists/abc123",
                "POST repos/owner/repo/issues/42/comments",
                "GET repos/owner/repo/issues/comments/99"
            ]
        );
        let count = provider.requests.borrow().len();
        let scenario = workspace.0.join(name);
        fs::write(
            scenario.join("overview.md"),
            format!("{source}\nRefreshed\n"),
        )
        .unwrap();
        let capture_path = scenario.join("capture.json");
        let mut capture: Value = serde_json::from_slice(&fs::read(&capture_path).unwrap()).unwrap();
        *capture
            .pointer_mut("/pullRequest/reviewedSourceCommit")
            .unwrap() = json!("c".repeat(40));
        fs::write(capture_path, serde_json::to_vec(&capture).unwrap()).unwrap();
        assert_eq!(publish(&args, &provider), result);
        assert_eq!(provider.requests.borrow().len(), count);
    }
}

/// Proven refusals may retry, but a lost response remains a journaled uncertainty.
#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "The bounded JSON result fixtures favor concise assertions"
)]
fn github_refusal_retry_preserves_receipts_and_ambiguous_results() {
    struct RetryClock {
        waits: RefCell<Vec<Duration>>,
        interrupted: bool,
    }

    impl Clock for RetryClock {
        fn now(&self) -> Timestamp {
            Timestamp::UNIX_EPOCH
        }

        fn wait(&self, duration: Duration) -> Result<(), AppError> {
            let mut waits = self.waits.borrow_mut();
            assert!(waits.len() < 2, "unexpected retry");
            waits.push(duration);
            if self.interrupted {
                return Err(fail("interrupted", "scripted interruption"));
            }
            Ok(())
        }
    }

    let workspace = Workspace::new();
    let args = workspace.scenario("rate-success", &overview(), "");
    let provider = Provider::default();
    provider.refuse_upload_once.set(true);
    provider.refuse_comment_once.set(true);
    let clock = RetryClock {
        waits: RefCell::new(Vec::new()),
        interrupted: false,
    };
    let result: Value =
        serde_json::from_str(&run(&args, &provider, &provider, &clock).unwrap()).unwrap();
    assert_eq!(result["status"], "succeeded");
    assert_eq!(result["operations"][0]["status"], "completed");
    assert_eq!(result["operations"][1]["status"], "completed");
    assert_eq!(result["artifact"]["id"], "abc123");
    assert_eq!(result["comment"]["id"], 99);
    assert_eq!(*clock.waits.borrow(), [Duration::from_secs(2); 2]);
    assert_eq!(
        provider.requests.borrow().as_slice(),
        [
            "GET repos/owner/repo/pulls/42",
            "POST gists",
            "POST gists",
            "GET gists/abc123",
            "POST repos/owner/repo/issues/42/comments",
            "POST repos/owner/repo/issues/42/comments",
            "GET repos/owner/repo/issues/comments/99",
        ]
    );
    let persisted: Value =
        serde_json::from_slice(&fs::read(result["resultPath"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(persisted, result);
    assert_eq!(
        serde_json::from_str::<Value>(&run(&args, &provider, &provider, &clock).unwrap()).unwrap(),
        result
    );
    assert_eq!(provider.requests.borrow().len(), 7);

    let args = workspace.scenario("rate-then-lost", &overview(), "");
    let provider = Provider::default();
    provider.refuse_comment_once.set(true);
    provider.fail_comment.set(true);
    let clock = RetryClock {
        waits: RefCell::new(Vec::new()),
        interrupted: false,
    };
    let result: Value =
        serde_json::from_str(&run(&args, &provider, &provider, &clock).unwrap()).unwrap();
    assert_eq!(result["status"], "incomplete");
    assert_eq!(result["operations"][0]["status"], "completed");
    assert_eq!(result["operations"][1]["status"], "ambiguous");
    assert_eq!(result["artifact"]["id"], "abc123");
    let count = provider.requests.borrow().len();
    assert_eq!(count, 5);
    let persisted: Value =
        serde_json::from_slice(&fs::read(result["resultPath"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(persisted, result);
    let resumed: Value =
        serde_json::from_str(&run(&args, &provider, &provider, &clock).unwrap()).unwrap();
    assert_eq!(resumed["status"], "needsReconciliation");
    assert_eq!(provider.requests.borrow().len(), count);

    let args = workspace.scenario("interrupted-rate", &overview(), "");
    let provider = Provider::default();
    provider.refuse_upload_once.set(true);
    let clock = RetryClock {
        waits: RefCell::new(Vec::new()),
        interrupted: true,
    };
    let result: Value =
        serde_json::from_str(&run(&args, &provider, &provider, &clock).unwrap()).unwrap();
    assert_eq!(result["operations"][0]["status"], "failed");
    assert_eq!(provider.requests.borrow().len(), 2);
    let persisted: Value =
        serde_json::from_slice(&fs::read(result["resultPath"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(persisted, result);
    let resumed: Value =
        serde_json::from_str(&run(&args, &provider, &provider, &clock).unwrap()).unwrap();
    assert_eq!(resumed["status"], "succeeded");
}

#[test]
fn ambiguous_comment_requires_reconciliation() {
    let workspace = Workspace::new();
    let source = overview();
    let args = workspace.scenario("partial", &source, "");
    let provider = Provider::default();
    provider.fail_comment.set(true);
    let result = publish(&args, &provider);
    assert_eq!(result.get("status"), Some(&json!("incomplete")));
    assert_eq!(result.pointer("/artifact/id"), Some(&json!("abc123")));
    assert_eq!(
        result.pointer("/operations/0/status"),
        Some(&json!("completed"))
    );
    assert_eq!(
        result.pointer("/operations/1/status"),
        Some(&json!("ambiguous"))
    );
    let count = provider.requests.borrow().len();
    assert_eq!(
        publish(&args, &provider).get("status"),
        Some(&json!("needsReconciliation"))
    );
    assert_eq!(provider.requests.borrow().len(), count);
}

/// An unreadable refusal body may conceal an accepted upload or comment.
#[test]
fn malformed_overview_write_response_requires_reconciliation_without_replay() {
    let workspace = Workspace::new();
    for (name, route, operation, count) in [
        ("upload-truncated", "gists", 0, 2),
        (
            "comment-truncated",
            "repos/owner/repo/issues/42/comments",
            1,
            4,
        ),
    ] {
        let args = workspace.scenario(name, &overview(), "");
        let provider = Provider {
            truncated_response: Some(route),
            ..Provider::default()
        };
        let result = publish(&args, &provider);
        assert_eq!(result.get("status"), Some(&json!("incomplete")));
        assert_eq!(
            result.pointer(&format!("/operations/{operation}/status")),
            Some(&json!("ambiguous"))
        );
        if operation == 1 {
            assert_eq!(
                result.pointer("/operations/0/status"),
                Some(&json!("completed"))
            );
            assert_eq!(result.pointer("/artifact/id"), Some(&json!("abc123")));
        }
        let persisted: Value = serde_json::from_slice(
            &fs::read(result.get("resultPath").unwrap().as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted, result);
        assert_eq!(provider.requests.borrow().len(), count);
        assert_eq!(
            publish(&args, &provider).get("status"),
            Some(&json!("needsReconciliation"))
        );
        assert_eq!(provider.requests.borrow().len(), count);
    }
}

/// A malformed overview is rejected before any remote write.
#[test]
fn missing_or_misordered_sections_never_upload() {
    let workspace = Workspace::new();
    let source = overview();
    for heading in ["Design changes", "Significant changes"] {
        let missing = source.replace(&format!("## {heading}\n"), "");
        for (suffix, input) in [
            ("missing", missing.clone()),
            ("misordered", format!("## {heading}\n\n{missing}")),
        ] {
            let name = format!("{}-{suffix}", heading.replace(' ', "-"));
            let args = workspace.scenario(&name, &input, "");
            let provider = Provider::default();
            let result = publish(&args, &provider);
            assert_eq!(result.get("status"), Some(&json!("failed")), "{result}");
            assert!(
                result
                    .get("error")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .contains(heading),
                "{result}"
            );
            assert!(
                !workspace
                    .0
                    .join(&name)
                    .join("output/overview-snapshot.json")
                    .exists()
            );
            assert!(
                provider
                    .requests
                    .borrow()
                    .iter()
                    .all(|request| !request.starts_with("POST ")),
                "{result}"
            );
        }
    }
}

/// Private output paths cannot be published as part of the authored overview.
#[test]
fn private_local_paths_never_upload() {
    let workspace = Workspace::new();
    for (name, forward_slashes) in [("native", false), ("forward", true)] {
        let path = workspace.0.join(name).join("output");
        let path = path.to_str().unwrap();
        let path = if forward_slashes {
            path.replace('\\', "/")
        } else {
            path.to_owned()
        };
        let source = format!("{}\n{path}\n", overview());
        let args = workspace.scenario(name, &source, "");
        let provider = Provider::default();
        let result = publish(&args, &provider);
        assert_eq!(result.get("status"), Some(&json!("failed")), "{result}");
        assert!(
            result
                .get("error")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("private"),
            "{result}"
        );
        assert!(
            provider
                .requests
                .borrow()
                .iter()
                .all(|request| !request.starts_with("POST ")),
            "{result}"
        );
    }
}

/// Best-effort publication records provider failures without stopping the review.
#[test]
fn provider_failures_are_reported_and_persisted_at_each_stage() {
    let workspace = Workspace::new();
    for (name, route) in [
        ("open", "repos/owner/repo/pulls/42"),
        ("upload", "gists"),
        ("verify-artifact", "gists/abc123"),
        ("comment", "repos/owner/repo/issues/42/comments"),
        ("verify-comment", "repos/owner/repo/issues/comments/99"),
    ] {
        let args = workspace.scenario(name, &overview(), "");
        let provider = Provider {
            fail_request: Some(route),
            ..Provider::default()
        };
        let result = publish(&args, &provider);
        assert_ne!(result.get("status"), Some(&json!("succeeded")), "{result}");
        assert!(
            result
                .get("error")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("scripted provider failure"),
            "{result}"
        );
        let saved: Value = serde_json::from_slice(
            &fs::read(result.get("resultPath").unwrap().as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(saved, result);
        if name == "verify-artifact" {
            assert!(
                provider
                    .requests
                    .borrow()
                    .iter()
                    .all(|request| !request.contains("/comments"))
            );
        }
    }
}

/// A bad recorded artifact must stop publication before provider writes.
#[test]
fn azure_manifest_failures_stop_before_attachment_upload() {
    let workspace = Workspace::new();
    for problem in ["html", "archive", "path"] {
        _ = workspace.scenario(problem, &overview(), "");
        let root = workspace.0.join(problem);
        let capture = azure_capture(&root);
        let source = root.join("overview.md");
        let output = root.join("output");
        let skill = root.join("skill");
        let input = OverviewInput {
            capture: &capture,
            overview_path: &source,
            output_directory: &output,
            skill_root: &skill,
            comment_prefix: "",
            tenant: None,
        };
        let provider = AzureProvider::default();
        let first = publish_overview(&input, &provider, &provider, &NoWait, |_| {
            Ok(OverviewRender::MissingTools {
                missing: vec!["pandoc".into()],
                diagnostics: vec![],
            })
        })
        .unwrap();
        let snapshot = first.get("snapshot").unwrap();
        let directory = output
            .join("overview-publication")
            .join(snapshot.get("runId").unwrap().as_str().unwrap());
        fs::create_dir_all(&directory).unwrap();
        let html = directory.join("report.html");
        let archive = directory.join("report.zip");
        fs::write(&html, b"html").unwrap();
        fs::write(&archive, b"archive").unwrap();
        let mut manifest = json!({
            "sourcePath":snapshot.get("markdownPath").unwrap(),
            "sourceHash":snapshot.get("markdownHash").unwrap(),
            "htmlPath":html,
            "htmlUri":"file:///report.html",
            "htmlHash":format!("{:x}", Sha256::digest(b"html")),
            "diagramCount":0,
            "archivePath":archive,
            "archiveHash":format!("{:x}", Sha256::digest(b"archive"))
        });
        match problem {
            "html" => fs::write(&html, b"modified").unwrap(),
            "archive" => fs::write(&archive, b"modified").unwrap(),
            "path" => {
                *manifest.get_mut("htmlPath").unwrap() = json!(root.join("other/report.html"));
            }
            _ => unreachable!(),
        }
        fs::write(
            directory.join("html.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        provider.requests.borrow_mut().clear();
        let result = publish_overview(&input, &provider, &provider, &NoWait, |snapshot| {
            let markdown = PathBuf::from(snapshot.get("markdownPath").unwrap().as_str().unwrap());
            let hash = snapshot.get("markdownHash").unwrap().as_str().unwrap();
            match freeze(
                &markdown,
                hash,
                &directory,
                &skill,
                &root,
                &provider,
                &NoWait,
                |_, _, _, _| panic!("an existing manifest must not render"),
            )? {
                Preparation::Ready(artifact) => Ok(OverviewRender::Ready {
                    html_path: artifact.html_path,
                    html_hash: artifact.html_hash,
                    archive_path: artifact.archive_path,
                    archive_hash: artifact.archive_hash,
                }),
                Preparation::MissingTools(_) => {
                    panic!("an existing manifest needs no tools")
                }
                _ => panic!("unexpected preparation result"),
            }
        })
        .unwrap();
        assert_eq!(result.get("status"), Some(&json!("failed")), "{result}");
        assert!(
            provider
                .requests
                .borrow()
                .iter()
                .all(|request| request.starts_with("GET ")),
            "{result}"
        );
    }
}

/// Matching legacy or generation manifests preserve the existing frozen files.
#[test]
fn matching_legacy_manifest_reuses_frozen_html() {
    let workspace = Workspace::new();
    for (name, legacy_matches) in [("legacy", true), ("generation", false)] {
        let args = workspace.scenario(name, &overview(), "");
        let root = workspace.0.join(name);
        let capture_path = root.join("capture.json");
        let capture = azure_capture(&root);
        fs::write(&capture_path, serde_json::to_vec(&capture).unwrap()).unwrap();
        let source = root.join("overview.md");
        let output = root.join("output");
        let skill = root.join("skill");
        let input = OverviewInput {
            capture: &capture,
            overview_path: &source,
            output_directory: &output,
            skill_root: &skill,
            comment_prefix: "",
            tenant: None,
        };
        let provider = AzureProvider {
            allow_writes: true,
            ..AzureProvider::default()
        };
        let first = publish_overview(&input, &provider, &provider, &NoWait, |_| {
            Ok(OverviewRender::MissingTools {
                missing: vec!["pandoc".into()],
                diagnostics: vec![],
            })
        })
        .unwrap();
        let snapshot = first.get("snapshot").unwrap();
        let publication = output.join("overview-publication");
        let directory = if legacy_matches {
            publication.clone()
        } else {
            let legacy = publication.join("html.json");
            fs::write(&legacy, br#"{"sourceHash":"old"}"#).unwrap();
            publication.join(snapshot.get("runId").unwrap().as_str().unwrap())
        };
        fs::create_dir_all(&directory).unwrap();
        let html = directory.join("report.html");
        let archive = directory.join("report.zip");
        fs::write(&html, b"legacy html").unwrap();
        fs::write(&archive, b"legacy archive").unwrap();
        let manifest = json!({
            "sourcePath":snapshot.get("markdownPath").unwrap(),
            "sourceHash":snapshot.get("markdownHash").unwrap(),
            "htmlPath":html,
            "htmlUri":"file:///report.html",
            "htmlHash":format!("{:x}", Sha256::digest(b"legacy html")),
            "diagramCount":0,
            "archiveHash":format!("{:x}", Sha256::digest(b"legacy archive"))
        });
        let manifest_path = directory.join("html.json");
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(&manifest_path, &manifest_bytes).unwrap();
        let result: Value =
            serde_json::from_str(&run(&args, &provider, &provider, &NoWait).unwrap()).unwrap();
        assert_eq!(result.get("status"), Some(&json!("succeeded")), "{result}");
        assert_eq!(provider.archive.borrow().as_slice(), b"legacy archive");
        assert_eq!(fs::read(&html).unwrap(), b"legacy html");
        assert_eq!(fs::read(&manifest_path).unwrap(), manifest_bytes);
        if !legacy_matches {
            assert_eq!(
                fs::read(publication.join("html.json")).unwrap(),
                br#"{"sourceHash":"old"}"#
            );
        }

        assert_eq!(
            result
                .pointer("/snapshot/htmlPath")
                .unwrap()
                .as_str()
                .unwrap(),
            html.to_str().unwrap()
        );
    }
}

/// Corrected local content can replace a snapshot before any remote write.
#[test]
fn corrected_overview_can_publish_after_render_failure() {
    let workspace = Workspace::new();
    _ = workspace.scenario("corrected", &overview(), "");
    let root = workspace.0.join("corrected");
    let capture = azure_capture(&root);
    let source = root.join("overview.md");
    let output = root.join("output");
    let skill = root.join("skill");
    let input = OverviewInput {
        capture: &capture,
        overview_path: &source,
        output_directory: &output,
        skill_root: &skill,
        comment_prefix: "",
        tenant: None,
    };
    let provider = AzureProvider {
        allow_writes: true,
        ..AzureProvider::default()
    };
    let first = publish_overview(&input, &provider, &provider, &NoWait, |_| {
        Err(fail("invalidInput", "Invalid diagram"))
    })
    .unwrap();
    assert_eq!(first.get("status"), Some(&json!("failed")));
    assert_eq!(first.get("operations"), Some(&json!([])));
    assert!(
        provider
            .requests
            .borrow()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
    fs::write(&source, format!("{}\nCorrected diagram\n", overview())).unwrap();
    let second = publish_overview(&input, &provider, &provider, &NoWait, |_| {
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
    .unwrap();
    assert_eq!(second.get("status"), Some(&json!("succeeded")), "{second}");
    assert_ne!(
        first.pointer("/snapshot/markdownHash"),
        second.pointer("/snapshot/markdownHash")
    );
}
