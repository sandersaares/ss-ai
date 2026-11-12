//! Offline command-boundary checks for final review publication.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixtures use known record shapes and assert rejected commands"
)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_slice, from_str, json, to_vec};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput, SystemClock, run};
use ss_ai_review_context::evidence_hash;
use tempfile::{Builder, TempDir};

const URL: &str = "https://github.com/owner/repo/pull/42";

/// Allows local Git comparisons but records and refuses all provider access.
#[derive(Default)]
struct Offline {
    provider_called: Cell<bool>,
}

impl Process for Offline {
    fn execute(&self, program: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        if program != "git" {
            self.provider_called.set(true);
            return Err(io::Error::other("offline provider boundary"));
        }
        let output = if arguments.iter().any(|arg| arg == "cat-file") {
            "commit\n"
        } else if arguments.iter().any(|arg| arg == "diff") {
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n context\n"
        } else {
            ".git\n"
        };
        Ok(ProcessOutput::new(true, output.as_bytes().to_vec(), vec![]))
    }
}

impl Http for Offline {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        self.provider_called.set(true);
        Err(io::Error::other("offline provider boundary").into())
    }
}

/// Keeps inputs and publication state isolated, including Unicode and spaces.
struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
        fs::create_dir_all(&root).unwrap();
        let directory = Builder::new()
            .prefix("final review 雪 ")
            .tempdir_in(root)
            .unwrap();
        let fixture = Self { directory };
        fs::create_dir_all(fixture.path("repository 雪")).unwrap();
        let capture = capture();
        fixture.write("context 雪.json", &capture);
        fixture.write("approved 雪.json", &approved(&capture, true));
        fixture.write("diagnostics 雪.json", &json!({}));
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn write(&self, name: &str, value: &Value) {
        fs::write(self.path(name), to_vec(value).unwrap()).unwrap();
    }

    /// Uses paths relative to the process cwd without mutating global cwd.
    fn args(&self) -> Vec<OsString> {
        let cwd = env::current_dir().unwrap();
        let mut args: Vec<OsString> = ["ss-ai", "review", "publish-final-review", "--url", URL]
            .into_iter()
            .map(Into::into)
            .collect();
        for (option, name) in [
            ("--approved-findings-path", "approved 雪.json"),
            ("--context-path", "context 雪.json"),
            ("--diagnostics-path", "diagnostics 雪.json"),
            ("--repository-path", "repository 雪"),
            ("--output-directory", "output 雪"),
        ] {
            let path = self.path(name);
            let path = path.strip_prefix(&cwd).unwrap_or(&path);
            args.push(option.into());
            args.push(path.as_os_str().to_owned());
        }
        args
    }

    fn merge_arg(&self, args: &mut Vec<OsString>) {
        args.push("--merge-approval-path".into());
        args.push(self.path("merge 雪.json").into_os_string());
    }
}

/// Provides capture evidence and immutable revisions for binding approvals.
fn capture() -> Value {
    json!({
        "schemaVersion": 3, "platform": "gitHub",
        "repositoryUrl": "https://github.com/owner/repo",
        "pullRequestUrl": URL,
        "target": {"platform":"gitHub", "collectionUrl":null, "organization":"owner",
            "project":null, "repository":"repo", "pullRequestId":42, "webUrl":URL},
        "authenticatedIdentity": {"platform":"gitHub", "id":"123", "displayName":"Reviewer",
            "uniqueName":"reviewer", "descriptor":null, "accountType":null,
            "kind":"person", "evidence":[]},
        "pullRequest": {"id":42, "title":"Review 雪", "description":null,
            "reviewedSourceCommit":"b".repeat(40), "reviewedMergeBaseCommit":"a".repeat(40),
            "isAuthoredBySelf":false},
        "reviewers":[], "revisions":[], "threads":[]
    })
}

/// Represents audited findings bound to exactly this capture.
fn approved(context: &Value, with_finding: bool) -> Value {
    let findings = if with_finding {
        json!([{"schemaVersion":1, "id":"F", "sourceFindingIds":["F"], "origin":"reviewer",
            "title":"Finding 雪", "description":"Problem", "reasoning":"Reason",
            "consequence":"Impact", "recommendedAction":"Fix",
            "anchor":{"kind":"file","path":"src/lib.rs"}}])
    } else {
        json!([])
    };
    json!({"schemaVersion":2, "target":context["target"],
        "reviewedSourceCommit":context["pullRequest"]["reviewedSourceCommit"],
        "reviewedMergeBaseCommit":context["pullRequest"]["reviewedMergeBaseCommit"],
        "reviewedContextHash":evidence_hash(context).unwrap(), "findings":findings})
}

/// Builds an independent merge decision for the captured pull request.
fn merge(approve: bool) -> Value {
    json!({"schemaVersion":1, "pullRequestUrl":URL, "approveMerge":approve,
        "approvedSourceCommit":"b".repeat(40)})
}

/// Rejects invalid local inputs before crossing either provider boundary.
fn reject(args: &[OsString], case: &str) {
    let offline = Offline::default();
    let result = run(args, &offline, &offline, &SystemClock::default());
    assert!(
        result.is_err(),
        "{case}: expected rejection, got {result:?}"
    );
    assert!(
        !offline.provider_called.get(),
        "{case}: contacted a provider"
    );
}

/// Verifies that a complete fixture can reach the deliberately blocked provider.
fn reaches_provider(args: &[OsString], case: &str) {
    let offline = Offline::default();
    let result = run(args, &offline, &offline, &SystemClock::default());
    assert!(result.is_err(), "{case}: provider must refuse access");
    assert!(offline.provider_called.get(), "{case}: {result:?}");
}

/// Exercises the installed entry point with the same stack budget as other CLI tests.
fn command() -> Command {
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "ulimit -c 0; ulimit -s 768; exec \"$@\"",
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

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn executable_dispatches_on_a_bounded_stack() {
    let fixture = Fixture::new();
    let output = command()
        .current_dir(fixture.directory.path())
        .env("PATH", fixture.directory.path())
        .args(["review", "publish-final-review", "--help"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8(output.stdout).unwrap();
    for option in [
        "--url",
        "--approved-findings-path",
        "--context-path",
        "--diagnostics-path",
        "--repository-path",
        "--output-directory",
        "--merge-approval-path",
        "--discard-pending-review",
        "--publish-to-closed-pull-request",
        "--tenant",
    ] {
        assert!(help.contains(option), "missing {option}: {help}");
    }
    fs::remove_file(fixture.path("approved 雪.json")).unwrap();
    let output = command()
        .current_dir(fixture.directory.path())
        .env("PATH", fixture.directory.path())
        .args([
            "review",
            "publish-final-review",
            "--url",
            URL,
            "--approved-findings-path",
            "approved 雪.json",
            "--context-path",
            "context 雪.json",
            "--diagnostics-path",
            "diagnostics 雪.json",
            "--repository-path",
            "repository 雪",
            "--output-directory",
            "output 雪",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.status.code().is_some());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .to_lowercase()
            .contains("approved"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn valid_records_reach_only_the_offline_provider() {
    let fixture = Fixture::new();
    reaches_provider(&fixture.args(), "finding without merge approval");
    fixture.write("merge 雪.json", &merge(false));
    let mut args = fixture.args();
    fixture.merge_arg(&mut args);
    args.extend(
        [
            "--discard-pending-review",
            "--publish-to-closed-pull-request",
            "--tenant",
            "tenant",
        ]
        .into_iter()
        .map(OsString::from),
    );
    reaches_provider(&args, "finding with a negative merge decision and options");
    fixture.write("approved 雪.json", &approved(&capture(), false));
    fixture.write("merge 雪.json", &merge(true));
    let mut args = fixture.args();
    fixture.merge_arg(&mut args);
    reaches_provider(&args, "empty finding set with merge approval");
}

#[test]
fn missing_and_malformed_local_inputs_never_reach_a_provider() {
    for name in ["context 雪.json", "approved 雪.json", "diagnostics 雪.json"] {
        let fixture = Fixture::new();
        fs::remove_file(fixture.path(name)).unwrap();
        reject(&fixture.args(), &format!("missing {name}"));
        for bytes in [b"".as_slice(), b"{", b"null", b"1", b"[]"] {
            fs::write(fixture.path(name), bytes).unwrap();
            reject(&fixture.args(), &format!("malformed {name}: {bytes:?}"));
        }
    }
    let fixture = Fixture::new();
    let mut args = fixture.args();
    fixture.merge_arg(&mut args);
    reject(&args, "missing merge decision");
    for bytes in [b"".as_slice(), b"{", b"null", b"true", b"[]", b"{}"] {
        fs::write(fixture.path("merge 雪.json"), bytes).unwrap();
        reject(&args, &format!("malformed merge decision: {bytes:?}"));
    }
    let mut decision = merge(true);
    decision["approveMerge"] = json!("true");
    fixture.write("merge 雪.json", &decision);
    reject(&args, "merge decision with a non-boolean approval");
}

#[test]
fn malformed_record_fields_never_reach_a_provider() {
    let fixture = Fixture::new();
    for findings in [json!(null), json!("finding"), json!({}), json!([{}])] {
        let mut document = approved(&capture(), true);
        document["findings"] = findings;
        fixture.write("approved 雪.json", &document);
        reject(&fixture.args(), "malformed findings");
    }
    fixture.write("approved 雪.json", &approved(&capture(), true));
    fixture.write(
        "diagnostics 雪.json",
        &json!({"userInstructionFileCount":-1}),
    );
    reject(&fixture.args(), "malformed diagnostics field");
    fixture.write("diagnostics 雪.json", &json!({}));
    fs::write(fixture.path("context 雪.json.authorship.json"), b"{").unwrap();
    reject(&fixture.args(), "malformed capture sidecar");
}

#[test]
fn stale_records_and_mismatched_target_never_reach_a_provider() {
    let fixture = Fixture::new();
    let mut stale = approved(&capture(), true);
    stale["reviewedContextHash"] = json!("0".repeat(64));
    fixture.write("approved 雪.json", &stale);
    reject(&fixture.args(), "stale capture hash");
    stale = approved(&capture(), true);
    stale["reviewedSourceCommit"] = json!("c".repeat(40));
    fixture.write("approved 雪.json", &stale);
    reject(&fixture.args(), "stale reviewed commit");
    fixture.write("approved 雪.json", &approved(&capture(), true));
    let mut args = fixture.args();
    args[4] = "https://github.com/owner/other/pull/42".into();
    reject(&args, "different requested pull request");

    fixture.write("approved 雪.json", &approved(&capture(), false));
    let mut args = fixture.args();
    fixture.merge_arg(&mut args);
    let mut stale_merge = merge(true);
    stale_merge["approvedSourceCommit"] = json!("c".repeat(40));
    fixture.write("merge 雪.json", &stale_merge);
    reject(&args, "stale merge approval");
    stale_merge = merge(true);
    stale_merge["pullRequestUrl"] = json!("https://github.com/owner/repo/pull/43");
    fixture.write("merge 雪.json", &stale_merge);
    reject(&args, "merge approval for another pull request");

    let mut own_capture = capture();
    own_capture["pullRequest"]["isAuthoredBySelf"] = json!(true);
    fixture.write("context 雪.json", &own_capture);
    fixture.write("approved 雪.json", &approved(&own_capture, false));
    fixture.write("merge 雪.json", &merge(true));
    reject(&args, "self-authored pull request");
}

#[test]
fn publication_options_do_not_authorize_an_empty_review() {
    let fixture = Fixture::new();
    fixture.write("approved 雪.json", &approved(&capture(), false));
    for options in [
        &[][..],
        &["--discard-pending-review"][..],
        &["--publish-to-closed-pull-request"][..],
        &[
            "--discard-pending-review",
            "--publish-to-closed-pull-request",
        ][..],
    ] {
        let mut args = fixture.args();
        args.extend(options.iter().map(OsString::from));
        reject(&args, &format!("no merge decision: {options:?}"));
        fixture.write("merge 雪.json", &merge(false));
        fixture.merge_arg(&mut args);
        reject(&args, &format!("negative merge decision: {options:?}"));
    }
}

/// Replays captured provider evidence and records publication effects in memory.
struct PublicationProvider {
    responses: Value,
    github: bool,
    finding: RefCell<Option<String>>,
    ado_findings: RefCell<Vec<String>>,
    ado_replies: RefCell<Vec<String>>,
    summary: RefCell<Option<String>>,
    writes: RefCell<Vec<String>>,
    readbacks: RefCell<Vec<String>>,
    approval_expected: Cell<bool>,
    approval_written: Cell<bool>,
    reverse_replies: Cell<bool>,
    fail_final_readback: bool,
}

impl PublicationProvider {
    fn new(name: &str, fail_final_readback: bool) -> Self {
        let fixture = recorded(name);
        Self {
            responses: fixture["responses"].clone(),
            github: name == "github",
            finding: RefCell::new(None),
            ado_findings: RefCell::new(Vec::new()),
            ado_replies: RefCell::new(Vec::new()),
            summary: RefCell::new(None),
            writes: RefCell::new(Vec::new()),
            readbacks: RefCell::new(Vec::new()),
            approval_expected: Cell::new(false),
            approval_written: Cell::new(false),
            reverse_replies: Cell::new(false),
            fail_final_readback,
        }
    }

    /// Recorded GitHub fixtures establish preflight evidence; writes supply later readbacks.
    fn github_response(&self, route: &str, input: &[u8]) -> Value {
        let responses = &self.responses;
        if route == "graphql" {
            let request: Value = from_slice(input).unwrap();
            let query = request["query"].as_str().unwrap();
            let variables = &request["variables"];
            if query.contains("states:[PENDING]") {
                return json!({"data":{"repository":{"pullRequest":{"reviews":{"nodes":[]}}}}});
            }
            if query.contains("addPullRequestReviewThread(") {
                self.writes.borrow_mut().push("finding".into());
                assert_eq!(variables["path"], "src/lib.rs");
                assert_eq!(variables["startLine"], 1);
                assert_eq!(variables["line"], 2);
                assert_eq!(variables["side"], "RIGHT");
                *self.finding.borrow_mut() = Some(variables["body"].as_str().unwrap().to_owned());
                return json!({"data":{"addPullRequestReviewThread":{"thread":{"id":"thread",
                        "comments":{"nodes":[{"id":"comment"}]}}}}});
            }
            if query.contains("addPullRequestReview(") {
                self.writes.borrow_mut().push("start".into());
                assert_eq!(variables["commitOid"], "a".repeat(40));
                return json!({"data":{"addPullRequestReview":{"pullRequestReview":{"id":"review","state":"PENDING"}}}});
            }
            if query.contains("submitPullRequestReview(") {
                self.writes.borrow_mut().push("complete".into());
                assert_eq!(
                    variables["event"],
                    if self.approval_expected.get() {
                        "APPROVE"
                    } else {
                        "COMMENT"
                    }
                );
                *self.summary.borrow_mut() = Some(variables["body"].as_str().unwrap().to_owned());
                return json!({"data":{"submitPullRequestReview":{"pullRequestReview":{
                        "id":"review","state":if self.approval_expected.get() {"APPROVED"} else {"COMMENTED"},
                        "url":"https://github.com/octo/widget/pull/7#review"}}}});
            }
            if query.contains("node(id:$reviewId)") {
                let summary = self.summary.borrow();
                let submitted = summary.is_some();
                self.readbacks
                    .borrow_mut()
                    .push(if submitted { "completed" } else { "draft" }.into());
                let body = if submitted && self.fail_final_readback {
                    "different"
                } else {
                    summary.as_deref().unwrap_or("")
                };
                return json!({"data":{"node":{"id":"review",
                        "state":if !submitted {"PENDING"} else if self.approval_expected.get() {"APPROVED"} else {"COMMENTED"},
                        "url":"https://github.com/octo/widget/pull/7#review","body":body,
                        "comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},
                        "nodes":[{"id":"comment","state":if submitted {"SUBMITTED"} else {"PENDING"},
                            "path":"src/lib.rs","line":2,"startLine":1,"subjectType":"LINE",
                            "body":self.finding.borrow().as_ref().unwrap()}]}}}});
            }
            if query.contains("reviewThreads(") && self.finding.borrow().is_some() {
                return json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                    "pageInfo":{"hasNextPage":false,"endCursor":null},
                    "nodes":[{"id":"thread","diffSide":"RIGHT","subjectType":"LINE",
                            "comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},
                                "nodes":[{"id":"comment"}]}}]}}}}});
            }
            return if query.contains("reviewThreads(") {
                responses["reviewThreads"].clone()
            } else if query.contains("timelineItems(") {
                responses["timeline"].clone()
            } else if query.contains("reviews(") {
                responses["reviewEdits"].clone()
            } else if query.contains("comments(") {
                responses["issueEdits"].clone()
            } else {
                panic!("unexpected GraphQL request: {query}")
            };
        }
        match route {
            "user" => responses["user"].clone(),
            "repos/octo/widget/pulls/7" => {
                let mut pr = responses["pullRequest"].clone();
                pr["html_url"] = json!("https://github.com/octo/widget/pull/7");
                pr
            }
            "repos/octo/widget/pulls/7/reviews?per_page=100" => json!([responses["reviews"]]),
            "repos/octo/widget/issues/7/comments?per_page=100" => {
                json!([responses["issueComments"]])
            }
            "repos/octo/widget/pulls/7/requested_reviewers?per_page=100" => {
                json!([responses["requestedReviewers"]])
            }
            route if route.starts_with("repos/octo/widget/compare/") => {
                responses["compare"].clone()
            }
            _ => panic!("unexpected GitHub route: {route}"),
        }
    }

    /// Builds a thread-grouped response without imposing cross-thread ordering.
    fn ado_threads(&self) -> Value {
        let mut threads = self.responses["threads"]["value"]
            .as_array()
            .unwrap()
            .clone();
        let replies = self.ado_replies.borrow();
        let mut comments = replies
            .iter()
            .enumerate()
            .map(|(index, content)| json!({"id":index.checked_add(3).unwrap(),"content":content}))
            .collect::<Vec<_>>();
        if self.reverse_replies.get() {
            comments.reverse();
        }
        threads[0]["comments"]
            .as_array_mut()
            .unwrap()
            .extend(comments);
        let findings = self.ado_findings.borrow();
        if !findings.is_empty() {
            self.readbacks.borrow_mut().push(
                if self.summary.borrow().is_some() {
                    "completed"
                } else {
                    "finding"
                }
                .into(),
            );
            // Existing threads precede new threads in this listing, even
            // though the CLI publishes new threads before follow-ups.
            for (index, finding) in findings.iter().enumerate().rev() {
                threads.push(json!({"id":index.checked_add(201).unwrap(),
                    "threadContext":{"filePath":"/src/lib.rs",
                    "rightFileStart":{"line":1,"offset":1},"rightFileEnd":{"line":2,"offset":2}},
                    "status":"active","comments":[{"id":1,"content":finding}]}));
            }
        }
        if let Some(summary) = self.summary.borrow().as_ref() {
            threads.push(json!({"id":300,"status":"closed",
                "comments":[{"id":1,"content":summary}]}));
        }
        json!({"count":threads.len(),"value":threads})
    }
}

impl Process for PublicationProvider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if program == "git" {
            return Offline::default().execute(program, arguments, input);
        }
        if program == "az" {
            assert!(!self.github);
            assert_eq!(&arguments[..2], ["account", "get-access-token"]);
            return Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]));
        }
        assert!(self.github);
        assert_eq!(program, "gh");
        assert_eq!(arguments[0], "api");
        let response = self.github_response(&arguments[1], input);
        Ok(ProcessOutput::new(true, to_vec(&response).unwrap(), vec![]))
    }
}

impl Http for PublicationProvider {
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert!(!self.github, "GitHub must not use ADO HTTP");
        assert_eq!(token, "fixture-token");
        // Recorded ADO lists establish preflight evidence; writes supply later readbacks.
        let responses = &self.responses;
        let response = if method == "PUT" && url.contains("/reviewers/runner?") {
            assert!(self.approval_expected.get());
            let request: Value = from_slice(body.unwrap()).unwrap();
            assert_eq!(request, json!({"id":"runner","vote":10,"isRequired":false}));
            self.writes.borrow_mut().push("approval".into());
            self.approval_written.set(true);
            request
        } else if method == "POST" && url.contains("/threads/101/comments?") {
            let request: Value = from_slice(body.unwrap()).unwrap();
            assert_eq!(request["parentCommentId"], 1);
            let content = request["content"].as_str().unwrap().to_owned();
            let mut replies = self.ado_replies.borrow_mut();
            let id = replies.len().checked_add(3).unwrap();
            replies.push(content);
            self.writes.borrow_mut().push("reply".into());
            json!({"id":id})
        } else if method == "POST" && url.contains("/threads?") {
            let request: Value = from_slice(body.unwrap()).unwrap();
            let content = request["comments"][0]["content"]
                .as_str()
                .unwrap()
                .to_owned();
            if request.get("threadContext").is_some() {
                assert_eq!(request["threadContext"]["filePath"], "/src/lib.rs");
                assert_eq!(request["threadContext"]["rightFileStart"]["line"], 1);
                assert_eq!(request["threadContext"]["rightFileEnd"]["line"], 2);
                assert_eq!(request["pullRequestThreadContext"]["changeTrackingId"], 17);
                self.writes.borrow_mut().push("finding".into());
                *self.finding.borrow_mut() = Some(content.clone());
                let mut findings = self.ado_findings.borrow_mut();
                let id = findings.len().checked_add(201).unwrap();
                findings.push(content);
                json!({"id":id,"comments":[{"id":1}]})
            } else {
                self.writes.borrow_mut().push("summary".into());
                *self.summary.borrow_mut() = Some(content);
                json!({"id":300,"comments":[{"id":1}]})
            }
        } else {
            assert_eq!(method, "GET", "unexpected request: {url}");
            assert!(body.is_none());
            if url.contains("_apis/connectionData") {
                responses["connectionData"].clone()
            } else if url.contains("/reviewers/runner?") {
                assert!(self.approval_written.get());
                self.readbacks.borrow_mut().push("approval".into());
                json!({"id":"runner","vote":10,"isRequired":false})
            } else if url.contains("/iterations/2/changes?") {
                json!({"changeEntries":[{"item":{"path":"/src/lib.rs"},"changeTrackingId":17}]})
            } else if url.contains("/iterations?") {
                responses["iterations"].clone()
            } else if url.contains("/reviewers?") {
                responses["reviewers"].clone()
            } else if url.contains("/threads?") {
                self.ado_threads()
            } else if url.contains("/pullRequests/4242?") {
                responses["pullRequest"].clone()
            } else {
                panic!("unexpected ADO URL: {url}")
            }
        };
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            to_vec(&response).unwrap(),
        ))
    }
}

/// Avoids wall-clock waits while exercising the same injected clock boundary.
struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2026-08-04T12:00:00Z".parse().unwrap()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected retry wait")
    }
}

/// Reads the same immutable provider captures used by the capture parity tests.
fn recorded(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ss-review-native-capture")
        .join(format!("{name}.json"));
    from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Binds the finding to a recorded capture and its provider URL.
fn publication_args(fixture: &Fixture, name: &str) -> Vec<OsString> {
    let capture = recorded(&format!("{name}-expected"));
    fixture.write("context 雪.json", &capture);
    let mut document = approved(&capture, true);
    document["findings"][0]["anchor"] =
        json!({"kind":"line","path":"src/lib.rs","startLine":1,"endLine":2,"side":"right"});
    fixture.write("approved 雪.json", &document);
    let mut args = fixture.args();
    args[4] = capture["pullRequestUrl"].as_str().unwrap().into();
    args
}

/// Adds a commit-bound user decision to the recorded capture's command.
fn authorize(fixture: &Fixture, args: &mut Vec<OsString>, name: &str) {
    let capture = recorded(&format!("{name}-expected"));
    fixture.write(
        "merge 雪.json",
        &json!({
            "schemaVersion":1, "pullRequestUrl":capture["pullRequestUrl"],
            "approveMerge":true,
            "approvedSourceCommit":capture["pullRequest"]["reviewedSourceCommit"]
        }),
    );
    fixture.merge_arg(args);
}

/// Interleaves new findings and follow-ups to exercise the publication order.
fn multi_ado_args(fixture: &Fixture) -> Vec<OsString> {
    let args = publication_args(fixture, "azure-devops");
    let mut document: Value =
        from_slice(&fs::read(fixture.path("approved 雪.json")).unwrap()).unwrap();
    let first = document["findings"][0].clone();
    let mut second = first.clone();
    second["id"] = json!("F2");
    second["sourceFindingIds"] = json!(["F2"]);
    second["title"] = json!("Second new finding");
    let reply = |id: &str| {
        let mut finding = first.clone();
        finding["id"] = json!(id);
        finding["sourceFindingIds"] = json!([id]);
        finding["title"] = json!(format!("Follow-up {id}"));
        finding["followUp"] = json!({"threadId":"101","commentId":"1"});
        finding
    };
    document["findings"] = json!([first, reply("R1"), second, reply("R2")]);
    fixture.write("approved 雪.json", &document);
    args
}

#[test]
fn github_cli_publishes_and_reads_back_a_draft_finding_and_summary() {
    let fixture = Fixture::new();
    let provider = PublicationProvider::new("github", false);
    let mut args = publication_args(&fixture, "github");
    args.extend(
        [
            "--discard-pending-review",
            "--publish-to-closed-pull-request",
        ]
        .map(OsString::from),
    );
    let result: Value = from_str(&run(&args, &provider, &provider, &FixedClock).unwrap()).unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["publishedFindingCount"], 1);
    assert_eq!(*provider.writes.borrow(), ["start", "finding", "complete"]);
    assert_eq!(*provider.readbacks.borrow(), ["draft", "completed"]);
    assert!(
        provider
            .finding
            .borrow()
            .as_ref()
            .unwrap()
            .contains("Finding 雪")
    );
    assert!(!provider.summary.borrow().as_ref().unwrap().is_empty());
}

#[test]
fn ado_cli_publishes_and_reads_back_a_finding_and_summary() {
    let fixture = Fixture::new();
    let provider = PublicationProvider::new("azure-devops", false);
    let args = publication_args(&fixture, "azure-devops");
    let result: Value = from_str(&run(&args, &provider, &provider, &FixedClock).unwrap()).unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["publishedFindingCount"], 1);
    assert_eq!(*provider.writes.borrow(), ["finding", "summary"]);
    assert_eq!(*provider.readbacks.borrow(), ["finding", "completed"]);
    assert!(
        provider
            .finding
            .borrow()
            .as_ref()
            .unwrap()
            .contains("Finding 雪")
    );
    assert!(!provider.summary.borrow().as_ref().unwrap().is_empty());
}

#[test]
fn ado_multiple_findings_verify_in_per_thread_order() {
    let fixture = Fixture::new();
    let provider = PublicationProvider::new("azure-devops", false);
    let args = multi_ado_args(&fixture);
    let result: Value = from_str(&run(&args, &provider, &provider, &FixedClock).unwrap()).unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["publishedFindingCount"], 4);
    assert_eq!(
        *provider.writes.borrow(),
        ["finding", "finding", "reply", "reply", "summary"]
    );
    assert_eq!(*provider.readbacks.borrow(), ["finding", "completed"]);
    let replies = provider.ado_replies.borrow();
    assert!(replies[0].contains("Follow-up R1"));
    assert!(replies[1].contains("Follow-up R2"));
}

#[test]
fn ado_reordered_replies_fail_readback_and_cannot_be_replayed() {
    let fixture = Fixture::new();
    let provider = PublicationProvider::new("azure-devops", false);
    provider.reverse_replies.set(true);
    let args = multi_ado_args(&fixture);
    let error = run(&args, &provider, &provider, &FixedClock).unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("order"),
        "{error}"
    );
    assert_eq!(
        *provider.writes.borrow(),
        ["finding", "finding", "reply", "reply"]
    );
    let offline = Offline::default();
    run(&args, &offline, &offline, &FixedClock).unwrap_err();
    assert!(!offline.provider_called.get());
}

#[test]
fn cli_approval_is_read_back_for_both_providers() {
    for name in ["github", "azure-devops"] {
        let fixture = Fixture::new();
        let provider = PublicationProvider::new(name, false);
        provider.approval_expected.set(true);
        let mut args = publication_args(&fixture, name);
        authorize(&fixture, &mut args, name);
        let result: Value =
            from_str(&run(&args, &provider, &provider, &FixedClock).unwrap()).unwrap();
        assert_eq!(result["status"], "succeeded", "{name}: {result}");
        if name == "github" {
            assert_eq!(*provider.writes.borrow(), ["start", "finding", "complete"]);
            assert_eq!(*provider.readbacks.borrow(), ["draft", "completed"]);
        } else {
            assert_eq!(
                *provider.writes.borrow(),
                ["finding", "approval", "summary"]
            );
            assert!(
                provider
                    .readbacks
                    .borrow()
                    .iter()
                    .any(|entry| entry == "approval")
            );
        }
    }
}

#[test]
fn accepted_github_submission_with_failed_readback_is_not_replayed() {
    let fixture = Fixture::new();
    let provider = PublicationProvider::new("github", true);
    let args = publication_args(&fixture, "github");
    let error = run(&args, &provider, &provider, &FixedClock).unwrap_err();
    assert!(error.to_string().contains("summary"));
    assert_eq!(*provider.writes.borrow(), ["start", "finding", "complete"]);
    assert_eq!(*provider.readbacks.borrow(), ["draft", "completed"]);
    let result: Value =
        from_slice(&fs::read(fixture.path("output 雪/publication-result.json")).unwrap()).unwrap();
    assert_eq!(result["status"], "failed");
    assert!(
        result["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation["operation"] == "completeReview"
                && operation["outcome"] == "succeeded")
    );
    let offline = Offline::default();
    assert!(run(&args, &offline, &offline, &FixedClock).is_err());
    assert!(!offline.provider_called.get());
    assert_eq!(*provider.writes.borrow(), ["start", "finding", "complete"]);
}
