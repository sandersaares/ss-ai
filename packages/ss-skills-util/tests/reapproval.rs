//! Real isolated Git objects with injected service effects exercise the full command lifecycle.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Tests use bounded fixture arrays and counters"
)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_skills_util::{Clock, Http, HttpResponse, NativeProcess, Process, ProcessOutput, run_in};
use tempfile::TempDir;
use url::Url;

/// Process launches must not inherit another test's temporarily retained file locks.
static PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2026-09-28T10:00:00Z".parse().unwrap()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("No test response requires a timed retry")
    }
}

/// Keeps all service writes in memory while real Git fetches from a test-owned repository.
struct Provider {
    number: u32,
    candidates: Vec<u32>,
    repository: PathBuf,
    evidence: RefCell<Value>,
    writes: Cell<usize>,
    fetches: Cell<usize>,
    fault: Cell<&'static str>,
    target_reads: Cell<usize>,
    host_directory: RefCell<Option<PathBuf>>,
    ado: bool,
}

impl Provider {
    fn new(repository: &Path, old: &str, new: &str) -> Self {
        let fixture: Value =
            serde_json::from_str(include_str!("fixtures/github-approval.json")).unwrap();
        let mut evidence = fixture["evidence"].clone();
        evidence["pullRequest"]["head"]["sha"] = json!(new);
        for review in evidence["reviews"].as_array_mut().unwrap() {
            review["commit_id"] = json!(old);
        }
        evidence["timeline"][2]["pullRequestCommit"]["commit"]["oid"] = json!(new);
        // UI approvals require no saved target tip or full-review instrumentation.
        evidence["pullRequest"]["base"]
            .as_object_mut()
            .unwrap()
            .remove("sha");
        evidence["pullRequest"]["base"]
            .as_object_mut()
            .unwrap()
            .remove("ref");
        Self {
            number: 1,
            candidates: vec![1],
            repository: repository.to_owned(),
            evidence: RefCell::new(evidence),
            writes: Cell::new(0),
            fetches: Cell::new(0),
            fault: Cell::new(""),
            target_reads: Cell::new(0),
            host_directory: RefCell::new(None),
            ado: false,
        }
    }

    /// Synthetic fresh-iteration policy uses the sanitized service event shapes.
    fn ado(repository: &Path, old: &str, new: &str) -> Self {
        let mut provider = Self::new(repository, old, new);
        provider.ado = true;
        let fixture: Value =
            serde_json::from_str(include_str!("fixtures/ado-no-approval-policy.json")).unwrap();
        let mut evidence = fixture["stages"][2].clone();
        evidence["sourceCommit"] = json!(new);
        evidence["iterations"] = json!([
            {"id": 1, "createdDate": "2026-01-01T00:00:00Z", "sourceRefCommit": {"commitId": old}},
            {"id": 2, "createdDate": "2026-01-01T12:00:00Z", "sourceRefCommit": {"commitId": new}}
        ]);
        evidence["threads"]
            .as_array_mut()
            .unwrap()
            .retain(|thread| thread["id"] == 1 || thread["id"] == 2);
        let policy = json!({"id": 99, "isEnabled": true, "isBlocking": true,
            "type": {"id": "fa4e907d-c16b-4a4c-9dfa-4906e5d171dd"},
            "settings": {"minimumApproverCount": 1, "requireVoteOnEachIteration": true,
                "requireVoteOnLastIteration": true, "resetOnSourcePush": false}});
        evidence["policies"]
            .as_array_mut()
            .unwrap()
            .push(policy.clone());
        evidence["evaluations"] = json!([{"evaluationId": "reviewer-policy", "configuration": policy,
            "status": "queued", "context": {}}]);
        provider.evidence.replace(evidence);
        provider
    }

    fn url(&self) -> &str {
        if self.ado {
            "https://dev.azure.com/example/project/_git/widgets"
        } else {
            "https://github.com/example/widgets"
        }
    }

    /// Reuses structured account/initiator mappings instead of interpreting vote prose.
    fn ado_vote(&self, value: i64, id: u32, at: &str) {
        let mut evidence = self.evidence.borrow_mut();
        let mut event = evidence["threads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|thread| thread["properties"]["CodeReviewThreadType"]["$value"] == "VoteUpdate")
            .unwrap()
            .clone();
        event["id"] = json!(id);
        event["publishedDate"] = json!(at);
        event["lastUpdatedDate"] = json!(at);
        event["properties"]["CodeReviewVoteResult"]["$value"] = json!(value.to_string());
        evidence["threads"].as_array_mut().unwrap().push(event);
        evidence["reviewers"][0]["vote"] = json!(value);
    }

    fn later(&self, state: &str) {
        let mut evidence = self.evidence.borrow_mut();
        let source = evidence["pullRequest"]["head"]["sha"].clone();
        evidence["reviews"].as_array_mut().unwrap().push(json!({
            "id": 5, "node_id": "later-review", "user": {"id": 42, "node_id": "user-42"},
            "state": state, "submitted_at": "2026-09-28T08:00:00Z", "commit_id": source,
            "body": "Independent human decision."
        }));
        if state != "PENDING" {
            evidence["timeline"].as_array_mut().unwrap().push(
                json!({"__typename": "PullRequestReview", "id": "later-review", "state": state}),
            );
        }
    }

    /// Prevents atomic replacement without touching the active sibling lock.
    fn fail_journal_save(&self) {
        let path = host_journal(self.host_directory.borrow().as_ref().unwrap());
        fs::rename(&path, path.with_extension("saved")).unwrap();
        fs::create_dir_all(path).unwrap();
    }

    /// Places repeat notices after the fixture's intervening approval and source push.
    fn notice_time(&self) -> &'static str {
        if self.writes.get() > 1 {
            "2026-09-28T09:08:30Z"
        } else {
            "2026-09-28T09:05:00Z"
        }
    }

    /// Reporting faults begin only after the provider's terminal receipt is durable.
    fn observation_override(&self) -> Option<Value> {
        let fault = self.fault.get();
        if fault == "movingCapture" {
            self.target_reads.set(self.target_reads.get() + 1);
            let key = if self.ado { "description" } else { "body" };
            self.evidence.borrow_mut()["pullRequest"][key] = json!(format!(
                "Changed during capture {}",
                self.target_reads.get()
            ));
        }
        if fault == "preflightReadFailure" {
            return Some(Value::Null);
        }
        if self.writes.get() > 0
            && matches!(fault, "noticeClosed" | "noticeDraft" | "noticeAbandoned")
        {
            let mut evidence = self.evidence.borrow_mut();
            if self.ado {
                evidence["pullRequest"]["isDraft"] = json!(fault == "noticeDraft");
                evidence["pullRequest"]["status"] = json!(match fault {
                    "noticeClosed" => "completed",
                    "noticeAbandoned" => "abandoned",
                    _ => "active",
                });
                // A completed PR may have no source ref; recovery needs only its discussion.
                evidence["sourceCommit"] = Value::Null;
            } else {
                evidence["pullRequest"]["draft"] = json!(fault == "noticeDraft");
                evidence["pullRequest"]["state"] = json!(if fault == "noticeClosed" {
                    "closed"
                } else {
                    "open"
                });
                evidence["pullRequest"]["head"]["repo"] = Value::Null;
            }
        }
        if !matches!(fault, "reportReadFailure" | "reportStandingChanged") {
            return None;
        }
        let path = host_journal(self.host_directory.borrow().as_ref().unwrap());
        let record: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        if !record["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation["result"]["outcome"] == "alreadyValid")
        {
            return None;
        }
        if fault == "reportReadFailure" {
            return Some(Value::Null);
        }
        self.fault.set("");
        if self.ado {
            self.ado_vote(0, 41, "2026-09-28T09:07:00Z");
        } else {
            let mut evidence = self.evidence.borrow_mut();
            let source = evidence["pullRequest"]["head"]["sha"].clone();
            evidence["reviews"].as_array_mut().unwrap().push(json!({
                "id": 99, "node_id": "reporting-withdrawal", "user": {"id": 42, "node_id": "user-42"},
                "state": "CHANGES_REQUESTED", "submitted_at": "2026-09-28T09:07:00Z",
                "commit_id": source, "body": "Independent later decision."
            }));
            evidence["timeline"].as_array_mut().unwrap().push(json!({
                "__typename": "PullRequestReview", "id": "reporting-withdrawal",
                "state": "CHANGES_REQUESTED"
            }));
        }
        None
    }
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if self.ado {
            assert_eq!(program, "az");
            assert_eq!(&arguments[..2], ["account", "get-access-token"]);
            return Ok(ProcessOutput::new(
                true,
                b"fixture-oauth".to_vec(),
                Vec::new(),
            ));
        }
        assert_eq!(program, "gh");
        let path = arguments[1]
            .replace(&format!("/pulls/{}", self.number), "/pulls/1")
            .replace(&format!("/issues/{}", self.number), "/issues/1");
        let path = path.as_str();
        let method = arguments[3].as_str();
        if method == "POST" && path == "repos/example/widgets/issues/1/comments" {
            self.writes.set(self.writes.get() + 1);
            if matches!(
                self.fault.get(),
                "noticeRefused" | "noticeRefusalJournalFailure"
            ) {
                if self.fault.get() == "noticeRefusalJournalFailure" {
                    self.fail_journal_save();
                }
                return Ok(ProcessOutput::new(
                    false,
                    b"HTTP/2.0 403 Forbidden\r\n\r\n{}".to_vec(),
                    Vec::new(),
                ));
            }
            let request: Value = serde_json::from_slice(input).unwrap();
            let comment = json!({
                "id": 50 + self.writes.get(), "user": {"id": 42}, "body": request["body"],
                "created_at": self.notice_time(), "updated_at": self.notice_time()
            });
            if self.fault.get() != "noticeUnknown" {
                self.evidence.borrow_mut()["comments"]
                    .as_array_mut()
                    .unwrap()
                    .push(comment.clone());
            }
            if self.fault.get() == "noticeJournalFailure" {
                self.fail_journal_save();
            }
            return Ok(
                if matches!(self.fault.get(), "noticeUnknown" | "lostNoticeReply") {
                    ProcessOutput::new(false, Vec::new(), b"connection lost".to_vec())
                } else {
                    ProcessOutput::new(true, serde_json::to_vec(&comment).unwrap(), Vec::new())
                },
            );
        }
        if method == "POST" && path.ends_with("/reviews") {
            self.writes.set(self.writes.get() + 1);
            if self.fault.get() == "rateLimit" {
                return Ok(ProcessOutput::new(
                    false,
                    b"HTTP/2.0 429 Too Many Requests\r\nRetry-After: 1\r\n\r\n{}".to_vec(),
                    Vec::new(),
                ));
            }
            if self.fault.get() == "permission" {
                return Ok(ProcessOutput::new(
                    false,
                    b"HTTP/2.0 403 Forbidden\r\n\r\n{}".to_vec(),
                    Vec::new(),
                ));
            }
            let request: Value = serde_json::from_slice(input).unwrap();
            assert_eq!(request["event"], "APPROVE");
            let mut evidence = self.evidence.borrow_mut();
            assert_eq!(request["commit_id"], evidence["pullRequest"]["head"]["sha"]);
            let review = json!({
                "id": 4, "node_id": "renewal-review", "user": {"id": 42, "node_id": "user-42"},
                "state": "APPROVED", "submitted_at": "2026-09-28T09:00:00Z",
                "commit_id": request["commit_id"], "body": request["body"]
            });
            if self.fault.get() != "unknown" {
                evidence["reviews"]
                    .as_array_mut()
                    .unwrap()
                    .push(review.clone());
                evidence["timeline"].as_array_mut().unwrap().push(json!({"__typename": "PullRequestReview", "id": "renewal-review", "state": "APPROVED"}));
            }
            if self.fault.get() == "postPush" {
                evidence["pullRequest"]["head"]["sha"] = json!("f".repeat(40));
            }
            if self.fault.get() == "duplicate" {
                let mut duplicate = review.clone();
                duplicate["id"] = json!(6);
                duplicate["node_id"] = json!("duplicate-renewal");
                evidence["reviews"].as_array_mut().unwrap().push(duplicate);
                evidence["timeline"].as_array_mut().unwrap().push(json!({
                    "__typename": "PullRequestReview", "id": "duplicate-renewal", "state": "APPROVED"
                }));
            }
            drop(evidence);
            if self.fault.get() == "postWithdrawal" {
                self.later("CHANGES_REQUESTED");
            }
            if self.fault.get() == "journalFailure" {
                // A directory at the journal name deterministically refuses atomic file replacement.
                self.fail_journal_save();
            }
            return Ok(if matches!(self.fault.get(), "lostReply" | "unknown") {
                ProcessOutput::new(false, Vec::new(), b"connection lost".to_vec())
            } else {
                ProcessOutput::new(true, serde_json::to_vec(&review).unwrap(), Vec::new())
            });
        }
        assert!(
            method == "GET" || path == "graphql",
            "Unexpected service mutation"
        );
        if self.fault.get() == "postReadFailure" && self.writes.get() > 0 {
            return Ok(ProcessOutput::new(
                false,
                b"HTTP/2.0 403 Forbidden\r\n\r\n{}".to_vec(),
                Vec::new(),
            ));
        }
        if path == "repos/example/widgets/pulls/1"
            && let Some(value) = self.observation_override()
        {
            return Ok(ProcessOutput::new(
                true,
                serde_json::to_vec(&value).unwrap(),
                Vec::new(),
            ));
        }
        let evidence = self.evidence.borrow();
        let value = match path {
            "user" => {
                json!({"id": if self.fault.get() == "auth" { 99 } else { 42 }, "login": "reviewer", "type": "User"})
            }
            "repos/example/widgets" => json!({"id": 123}),
            "repos/example/widgets/pulls?state=open&per_page=100" => {
                let mut candidates = self.candidates.iter().map(|number| json!({
                    "number": number, "state": "open", "draft": false, "title": "Recent source"
                })).collect::<Vec<_>>();
                candidates.push(json!({
                    "number": self.candidates.iter().max().unwrap() + 1,
                    "state": "open", "draft": false, "title": "Excluded old"
                }));
                json!([candidates])
            }
            "repos/example/widgets/pulls/1" => {
                self.target_reads.set(self.target_reads.get() + 1);
                let mut value = evidence["pullRequest"].clone();
                // Base-tip advancement on every read must not fence otherwise identical source evidence.
                value["base"]["sha"] = json!(format!("{:040x}", self.target_reads.get()));
                value["updated_at"] = json!(self.target_reads.get());
                value
            }
            "repos/example/widgets/pulls/1/reviews?per_page=100" => json!([evidence["reviews"]]),
            "repos/example/widgets/issues/1/comments?per_page=100" => json!([evidence["comments"]]),
            "repos/example/widgets/pulls/1/comments?per_page=100" => {
                json!([evidence["reviewComments"]])
            }
            "graphql" => {
                let request: Value = serde_json::from_slice(input).unwrap();
                if request["query"].as_str().unwrap().contains("committedDate") {
                    let at = if self
                        .candidates
                        .iter()
                        .any(|number| request["variables"]["num"] == *number)
                    {
                        "2026-09-27T00:00:00Z"
                    } else {
                        "2020-01-01T00:00:00Z"
                    };
                    json!({"data": {"repository": {"pullRequest": {"commits": {
                        "nodes": [{"commit": {"committedDate": at}}],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }}}}})
                } else {
                    assert_eq!(request["variables"]["num"], self.number);
                    json!({"data": {"repository": {"pullRequest": {"timelineItems": {
                        "nodes": evidence["timeline"], "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }}}}})
                }
            }
            _ => panic!("Unexpected provider path {path}"),
        };
        Ok(ProcessOutput::new(
            true,
            serde_json::to_vec(&value).unwrap(),
            Vec::new(),
        ))
    }

    fn isolated_git(&self, directory: &Path, arguments: &[String]) -> io::Result<ProcessOutput> {
        let mut arguments = arguments.to_vec();
        if arguments.iter().any(|argument| argument == "fetch") {
            self.fetches.set(self.fetches.get() + 1);
            let remote = arguments
                .iter_mut()
                .find(|argument| argument.as_str() == self.url())
                .unwrap();
            self.repository.to_str().unwrap().clone_into(remote);
        }
        NativeProcess::new(directory).isolated_git(directory, &arguments)
    }

    fn isolated_git_oauth(
        &self,
        directory: &Path,
        arguments: &[String],
        url: &str,
        token: &str,
    ) -> io::Result<ProcessOutput> {
        assert!(self.ado);
        assert_eq!(url, self.url());
        assert_eq!(token, "fixture-oauth");
        self.isolated_git(directory, arguments)
    }
}

impl Http for Provider {
    #[expect(
        clippy::cognitive_complexity,
        reason = "Explicit fixture routes and fault branches stay together for evidence review"
    )]
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert!(self.ado, "GitHub must use the injected CLI");
        assert_eq!(token, "fixture-oauth");
        let url = Url::parse(url).unwrap();
        let path = url.path();
        let query: BTreeMap<_, _> = url.query_pairs().collect();
        let response = |status, value| {
            Ok(HttpResponse::new(
                status,
                BTreeMap::new(),
                serde_json::to_vec(&value).unwrap(),
            ))
        };
        if method != "GET" {
            self.writes.set(self.writes.get() + 1);
            let request: Value = serde_json::from_slice(body.unwrap()).unwrap();
            let vote = method == "PUT";
            assert!(vote || method == "POST");
            if self.fault.get() == "rateLimit"
                || self.fault.get() == "permission"
                || !vote
                    && matches!(
                        self.fault.get(),
                        "auditRefused" | "noticeRefused" | "noticeRefusalJournalFailure"
                    )
            {
                if self.fault.get() == "noticeRefusalJournalFailure" {
                    self.fail_journal_save();
                }
                return response(
                    if self.fault.get() == "rateLimit" {
                        429
                    } else {
                        403
                    },
                    json!({}),
                );
            }
            if vote {
                assert!(path.ends_with("/reviewers/10000000-0000-4000-8000-000000000001"));
                assert_eq!(
                    request,
                    json!({"id": "10000000-0000-4000-8000-000000000001",
                    "vote": self.evidence.borrow()["reviewers"][0]["vote"], "isReapprove": true})
                );
                if self.fault.get() != "unknown" {
                    if self.fault.get() == "sourceAtVote" {
                        self.ado_push();
                        self.evidence.borrow_mut()["iterations"][2]["createdDate"] =
                            json!("2026-09-28T08:59:00Z");
                    }
                    self.ado_vote(
                        request["vote"].as_i64().unwrap(),
                        20,
                        "2026-09-28T09:00:00Z",
                    );
                    if self.fault.get() == "duplicate" {
                        self.ado_vote(
                            request["vote"].as_i64().unwrap(),
                            21,
                            "2026-09-28T09:01:00Z",
                        );
                    }
                    if self.fault.get() == "postWithdrawal" {
                        self.ado_vote(0, 21, "2026-09-28T09:01:00Z");
                    }
                    if self.fault.get() == "postPush" {
                        self.ado_push();
                    }
                    if self.fault.get() == "policyCompleted" {
                        let mut evidence = self.evidence.borrow_mut();
                        evidence["evaluations"][0]["status"] = json!("approved");
                        evidence["evaluations"][0]["context"] =
                            json!({"reviewers": ["newly satisfied"]});
                        evidence["reviewers"][0]["isReapprove"] = json!(true);
                    }
                }
                if matches!(self.fault.get(), "lostVoteReply" | "unknown") {
                    return Err(io::Error::other("Lost vote reply").into());
                }
                return response(200, self.evidence.borrow()["reviewers"][0].clone());
            }
            assert!(path.ends_with("/threads"));
            assert_eq!(request["status"], 4);
            let notice = request["comments"][0]["content"]
                .as_str()
                .unwrap()
                .contains("<!-- ss-autoreapprove-needs-review ");
            let at = if notice {
                self.notice_time()
            } else {
                "2026-09-28T09:05:00Z"
            };
            let thread = json!({"id": if notice { 30 + self.writes.get() } else { 30 },
                "publishedDate": at, "lastUpdatedDate": at, "status": "closed",
                "comments": [{"id": 1, "parentCommentId": 0, "commentType": "text",
                    "author": {"id": "10000000-0000-4000-8000-000000000001"},
                    "content": request["comments"][0]["content"], "publishedDate": at,
                    "lastContentUpdatedDate": at, "lastUpdatedDate": at}], "isDeleted": false, "properties": {}});
            if !matches!(self.fault.get(), "auditUnknown" | "noticeUnknown") {
                self.evidence.borrow_mut()["threads"]
                    .as_array_mut()
                    .unwrap()
                    .push(thread.clone());
            }
            if self.fault.get() == "noticeJournalFailure" {
                self.fail_journal_save();
            }
            if matches!(
                self.fault.get(),
                "lostAuditReply" | "auditUnknown" | "lostNoticeReply" | "noticeUnknown"
            ) {
                return Err(io::Error::other("Lost audit reply").into());
            }
            return response(200, thread);
        }
        if self.fault.get() == "postReadFailure" && self.writes.get() > 0 {
            return response(403, json!({}));
        }
        if path.ends_with("/pullRequests/1") {
            self.target_reads.set(self.target_reads.get() + 1);
            if let Some(value) = self.observation_override() {
                return response(200, value);
            }
        }
        let evidence = self.evidence.borrow();
        let values =
            |value: &Value| json!({"count": value.as_array().unwrap().len(), "value": value});
        let value = if path.ends_with("/_apis/connectionData") {
            json!({"authenticatedUser": {"id": if self.fault.get() == "auth" { "another-account" }
                else { "10000000-0000-4000-8000-000000000001" }, "providerDisplayName": "Fixture"}})
        } else if path.ends_with("/repositories/widgets") {
            json!({"id": evidence["pullRequest"]["repository"]["id"]})
        } else if path.ends_with("/pullRequests") {
            if query.get("$skip").unwrap() == "0" {
                values(
                    &json!([{"pullRequestId": 1, "status": "active", "isDraft": false, "title": "Recent"},
                    {"pullRequestId": 2, "status": "active", "isDraft": false, "title": "Old"}]),
                )
            } else {
                values(&json!([]))
            }
        } else if path.ends_with("/commits") {
            values(
                &json!([{"committer": {"date": if path.ends_with("/1/commits") {
                "2026-09-27T00:00:00Z" } else { "2020-01-01T00:00:00Z" }}}]),
            )
        } else if path.ends_with("/pullRequests/1") {
            evidence["pullRequest"].clone()
        } else if path.ends_with("/refs") {
            values(
                &json!([{"name": evidence["pullRequest"]["sourceRefName"], "objectId": evidence["sourceCommit"]}]),
            )
        } else if path.ends_with("/reviewers") {
            values(&evidence["reviewers"])
        } else if path.ends_with("/iterations") {
            values(&evidence["iterations"])
        } else if path.ends_with("/threads") {
            values(&evidence["threads"])
        } else if path.ends_with("/policy/configurations") {
            values(&evidence["policies"])
        } else if path.ends_with("/policy/evaluations") {
            assert!(!query.contains_key("$top"));
            assert!(!query.contains_key("$skip"));
            values(&evidence["evaluations"])
        } else {
            panic!("Unexpected ADO route {url}");
        };
        response(200, value)
    }
}

impl Provider {
    fn ado_push(&self) {
        let mut evidence = self.evidence.borrow_mut();
        evidence["sourceCommit"] = json!("f".repeat(40));
        evidence["iterations"].as_array_mut().unwrap().push(json!({"id": 3,
            "createdDate": "2026-09-28T09:02:00Z", "sourceRefCommit": {"commitId": "f".repeat(40)}}));
    }
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .env("GIT_AUTHOR_DATE", "2026-09-27T12:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-09-27T12:00:00Z")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn sources(root: &Path) -> (String, String) {
    git(root, &["init", "--quiet"]);
    fs::write(root.join("notes.txt"), "An ordinary sentnce.\n").unwrap();
    git(root, &["add", "--", "notes.txt"]);
    git(root, &["commit", "--quiet", "-m", "Original source"]);
    let old = git(root, &["rev-parse", "HEAD"]);
    fs::write(root.join("notes.txt"), "An ordinary sentence.\n").unwrap();
    git(root, &["add", "--", "notes.txt"]);
    git(root, &["commit", "--quiet", "-m", "Editorial correction"]);
    (old, git(root, &["rev-parse", "HEAD"]))
}

fn invoke(
    root: &Path,
    provider: &Provider,
    operation: &str,
    args: &[String],
) -> Result<Value, AppError> {
    let mut arguments = vec![
        OsString::from("ss-skills-util"),
        "autoreapprove".into(),
        operation.into(),
        "--state-path".into(),
        root.join("run.json").into_os_string(),
    ];
    arguments.extend(args.iter().map(OsString::from));
    Ok(serde_json::from_str(&run_in(
        &arguments,
        provider,
        provider,
        &FixedClock,
        &root.parent().unwrap().join("host-store"),
    )?)?)
}

/// Starts a fresh run without requiring its inspection to need source assessment.
fn discover_inspect(root: &Path, provider: &Provider, dry_run: bool) -> Value {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("policy.md"),
        include_str!("../../../skills/ss-autoreapprove/significance.md"),
    )
    .unwrap();
    let mut args = vec![
        "--repository-url".to_owned(),
        provider.url().to_owned(),
        "--policy-path".to_owned(),
        root.join("policy.md").to_str().unwrap().to_owned(),
    ];
    if dry_run {
        args.push("--dry-run".to_owned());
    }
    let discovered = invoke(root, provider, "discover", &args).unwrap();
    assert_eq!(discovered["counts"]["noRecentCommit"], 1);
    invoke(
        root,
        provider,
        "inspect",
        &[
            "--repository-url".into(),
            provider.url().into(),
            "--pull-request-id".into(),
            provider.number.to_string(),
        ],
    )
    .unwrap()
}

fn prepare(root: &Path, provider: &Provider, dry_run: bool) -> (String, Value) {
    let inspected = discover_inspect(root, provider, dry_run);
    assert_eq!(
        inspected["items"][0]["status"], "assessmentRequired",
        "{inspected}"
    );
    let snapshot = inspected["items"][0]["snapshotDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    let prepared = work(root, provider, "prepare", &snapshot, &[]).unwrap();
    if prepared.get("requiredDocuments").is_some() {
        assert_eq!(
            prepared["requiredDocuments"],
            json!([
                "significance-policy",
                "assessment-contract",
                "approval-discussion"
            ])
        );
    } else {
        assert_eq!(prepared["items"][0]["status"], "failed");
    }
    (snapshot, prepared)
}

fn work(
    root: &Path,
    provider: &Provider,
    operation: &str,
    snapshot: &str,
    args: &[String],
) -> Result<Value, AppError> {
    let mut all = vec![
        "--pull-request-id".into(),
        provider.number.to_string(),
        "--snapshot-digest".into(),
        snapshot.to_owned(),
    ];
    all.extend_from_slice(args);
    invoke(root, provider, operation, &all)
}

fn assess(root: &Path, provider: &Provider, snapshot: &str, prepared: &Value) {
    assess_as(root, provider, snapshot, prepared, "insignificant");
}

/// Shares complete evidence coverage while varying the cumulative policy decision.
fn assess_as(
    root: &Path,
    provider: &Provider,
    snapshot: &str,
    prepared: &Value,
    disposition: &str,
) {
    // Small pages exercise contiguous evidence reads in the original lifecycle scenarios.
    assess_with_limit(root, provider, snapshot, prepared, disposition, 113);
}

/// Shares semantic coverage while allowing recovery tests to use native-sized pages.
fn assess_with_limit(
    root: &Path,
    provider: &Provider,
    snapshot: &str,
    prepared: &Value,
    disposition: &str,
    read_limit: usize,
) {
    let mut references = vec!["policy".to_owned(), "discussion".to_owned()];
    for change in prepared["changes"].as_array().unwrap() {
        let id = change["id"].as_str().unwrap();
        for side in ["old", "new"] {
            if !change[side].is_null() {
                references.push(format!("{id}:{side}"));
            }
        }
    }
    for reference in references {
        let mut offset = 0;
        loop {
            let read = work(
                root,
                provider,
                "read",
                snapshot,
                &[
                    "--reference".into(),
                    reference.clone(),
                    "--offset".into(),
                    offset.to_string(),
                    "--limit".into(),
                    read_limit.to_string(),
                ],
            )
            .unwrap();
            if read["complete"] == true {
                break;
            }
            let next = read["nextOffset"].as_u64().unwrap();
            assert!(next > offset);
            offset = next;
        }
    }
    for change in prepared["changes"].as_array().unwrap() {
        let id = change["id"].as_str().unwrap();
        let mut arguments = vec![
            "--change-id".into(), id.into(), "--disposition".into(), "insignificant".into(),
            "--rationale".into(), "The complete fixture change has no significant design change or likely performance/correctness regression.".into(),
        ];
        for side in ["old", "new"] {
            if !change[side].is_null() {
                arguments.extend(["--evidence".into(), format!("{id}:{side}")]);
            }
        }
        work(root, provider, "judge", snapshot, &arguments).unwrap();
    }
    let current = work(root, provider, "assessment", snapshot, &[]).unwrap();
    let mut cumulative = vec![
        "--change-id".into(),
        "cumulative".into(),
        "--disposition".into(),
        disposition.into(),
        "--rationale".into(),
        match disposition {
            "significant" => "A significant design change requires a separate new review.",
            "uncertain" => "Missing contract context prevents assessing correctness.",
            _ => "Combined fixture changes have no significant design change or likely performance/correctness regression; discussion has no new objection.",
        }.into(),
        "--evidence".into(),
        "policy".into(),
        "--evidence".into(),
        "discussion".into(),
    ];
    if let Some(previous) = current["cumulativeDigest"].as_str() {
        // Missing/stale expected versions cannot silently replace a concurrent judgment.
        work(root, provider, "judge", snapshot, &cumulative).unwrap_err();
        cumulative.extend(["--previous-digest".into(), previous.to_owned()]);
    }
    work(root, provider, "judge", snapshot, &cumulative).unwrap();
    work(root, provider, "complete", snapshot, &[]).unwrap();
}

/// Notice publication carries the same immutable snapshot and attribution as approval.
fn notify(root: &Path, provider: &Provider, snapshot: &str) -> Result<Value, AppError> {
    work(
        root,
        provider,
        "notify",
        snapshot,
        &[
            "--repository-url".into(),
            provider.url().into(),
            "--attribution".into(),
            "[Copilot speaking]".into(),
        ],
    )
}

/// Refresh remains an orchestrator operation and cannot manufacture semantic judgments.
fn refresh(root: &Path, provider: &Provider) -> Result<Value, AppError> {
    invoke(
        root,
        provider,
        "refresh",
        &[
            "--repository-url".into(),
            provider.url().into(),
            "--pull-request-id".into(),
            provider.number.to_string(),
        ],
    )
}

/// Source movement forces a new exact-source assessment on both publication paths.
#[expect(
    clippy::cognitive_complexity,
    reason = "The bounded provider/disposition table checks the same fresh-assessment boundary"
)]
fn stale_preflight_recovery_reassesses_source_before_any_effect() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    fs::write(
        repository.path().join("notes.txt"),
        "A further source change.\n",
    )
    .unwrap();
    git(repository.path(), &["add", "--", "notes.txt"]);
    git(
        repository.path(),
        &["commit", "--quiet", "-m", "Further source"],
    );
    let changed = git(repository.path(), &["rev-parse", "HEAD"]);
    for ado in [false, true] {
        for disposition in ["insignificant", "significant", "uncertain"] {
            let scope = TempDir::new().unwrap();
            let root = scope.path().join("run");
            let provider = if ado {
                Provider::ado(repository.path(), &old, &new)
            } else {
                Provider::new(repository.path(), &old, &new)
            };
            let (snapshot, prepared) = prepare(&root, &provider, false);
            // Recovery tests target freshness, so use the native default evidence page size.
            assess_with_limit(&root, &provider, &snapshot, &prepared, disposition, 4096);
            let unchanged = fs::read(root.join("run.json")).unwrap();
            assert_eq!(
                ss_skills_util::classification(&refresh(&root, &provider).unwrap_err()),
                "invalidState"
            );
            assert_eq!(fs::read(root.join("run.json")).unwrap(), unchanged);
            if ado {
                provider.ado_push();
                let mut evidence = provider.evidence.borrow_mut();
                evidence["sourceCommit"] = json!(changed);
                evidence["iterations"][2]["sourceRefCommit"]["commitId"] = json!(changed);
                // The new iteration must precede the fixture's later renewal vote.
                evidence["iterations"][2]["createdDate"] = json!("2026-09-28T08:59:00Z");
            } else {
                provider.evidence.borrow_mut()["pullRequest"]["head"]["sha"] = json!(changed);
            }
            if disposition == "insignificant" {
                assert_eq!(
                    publish(&root, &provider, &snapshot).unwrap()["items"][0]["status"],
                    "staleDuringExecution"
                );
            } else {
                assert_eq!(
                    ss_skills_util::classification(
                        &notify(&root, &provider, &snapshot).unwrap_err()
                    ),
                    "staleEvidence"
                );
            }
            assert_eq!(provider.writes.get(), 0);
            let stale = invoke(&root, &provider, "results", &[]).unwrap();
            assert_eq!(stale["items"][0]["status"], "staleDuringExecution");
            if disposition == "uncertain" {
                let record: Value =
                    serde_json::from_slice(&fs::read(root.join("run.json")).unwrap()).unwrap();
                assert_eq!(record["inventory"][0]["executionError"]["kind"], "service");
            }
            if disposition == "insignificant" {
                let path = host_journal(&scope.path().join("host-store").join("publications"));
                let retained = path.with_extension("retained");
                fs::rename(&path, &retained).unwrap();
                // A missing publishing-host journal cannot prove that the receipt preceded a write.
                assert_eq!(
                    ss_skills_util::classification(&refresh(&root, &provider).unwrap_err()),
                    "ambiguousWrite"
                );
                fs::rename(&retained, &path).unwrap();
            }
            let refreshed = refresh(&root, &provider).unwrap();
            assert_eq!(refreshed["items"][0]["status"], "assessmentRequired");
            assert_eq!(refreshed["items"][0]["staleRecovery"]["attempts"], 1);
            assert!(refreshed["items"][0]["assessmentDigest"].is_null());
            assert!(refreshed["items"][0]["publication"].is_null());
            let current = refreshed["items"][0]["snapshotDigest"].as_str().unwrap();
            assert_ne!(current, snapshot);
            for operation in ["prepare", "complete"] {
                work(&root, &provider, operation, &snapshot, &[]).unwrap_err();
            }
            let prepared = work(&root, &provider, "prepare", current, &[]).unwrap();
            assert_eq!(prepared["oldCommit"], old);
            assert_eq!(prepared["newCommit"], changed);
            work(&root, &provider, "complete", current, &[]).unwrap_err();
            assess_with_limit(&root, &provider, current, &prepared, disposition, 4096);
            publish(&root, &provider, &snapshot).unwrap_err();
            notify(&root, &provider, &snapshot).unwrap_err();
            assert_eq!(provider.writes.get(), 0);
            let result = if disposition == "insignificant" {
                publish(&root, &provider, current).unwrap()
            } else {
                notify(&root, &provider, current).unwrap()
            };
            let writes = provider.writes.get();
            assert_eq!(
                writes,
                if ado && disposition == "insignificant" {
                    2
                } else {
                    1
                }
            );
            if disposition == "insignificant" {
                assert_eq!(
                    result["items"][0]["status"], "reapproved",
                    "{ado}: {result}"
                );
                assert_ne!(result["items"][0]["publication"]["runId"], result["runId"]);
                publish(&root, &provider, current).unwrap();
            } else {
                assert_eq!(
                    result["items"][0]["status"],
                    if disposition == "uncertain" {
                        "failed"
                    } else {
                        "needsReview"
                    }
                );
                notify(&root, &provider, current).unwrap();
            }
            assert_eq!(provider.writes.get(), writes);
            refresh(&root, &provider).unwrap_err();
        }
    }
}

/// Continuous discussion movement consumes the persisted budget, including uncertainty.
fn stale_notification_recovery_exhausts_without_remote_writes() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        for disposition in ["significant", "uncertain"] {
            let scope = TempDir::new().unwrap();
            let root = scope.path().join("run");
            let provider = if ado {
                Provider::ado(repository.path(), &old, &new)
            } else {
                Provider::new(repository.path(), &old, &new)
            };
            let (mut snapshot, mut prepared) = prepare(&root, &provider, false);
            for attempt in 0..=2 {
                // Native-sized pages keep repeated assessment focused on freshness rather than paging.
                assess_with_limit(&root, &provider, &snapshot, &prepared, disposition, 4096);
                let key = if ado { "description" } else { "body" };
                provider.evidence.borrow_mut()["pullRequest"][key] =
                    json!(format!("Changed discussion {attempt}"));
                notify(&root, &provider, &snapshot).unwrap_err();
                if attempt == 2 {
                    assert_eq!(
                        ss_skills_util::classification(&refresh(&root, &provider).unwrap_err()),
                        "retryExhausted"
                    );
                    break;
                }
                let refreshed = refresh(&root, &provider).unwrap();
                assert_eq!(
                    refreshed["items"][0]["staleRecovery"]["attempts"],
                    attempt + 1
                );
                refreshed["items"][0]["snapshotDigest"]
                    .as_str()
                    .unwrap()
                    .clone_into(&mut snapshot);
                prepared = work(&root, &provider, "prepare", &snapshot, &[]).unwrap();
            }
            let result = invoke(&root, &provider, "results", &[]).unwrap();
            assert_eq!(result["state"], "partial");
            assert_eq!(result["items"][0]["status"], "failed");
            assert_eq!(result["items"][0]["error"]["kind"], "retryExhausted");
            assert_eq!(
                result["items"][0]["staleRecovery"],
                json!({"attempts":2, "limit":2, "exhausted":true})
            );
            assert_eq!(provider.writes.get(), 0);
            refresh(&root, &provider).unwrap_err();
            notify(&root, &provider, &snapshot).unwrap_err();
            assert_eq!(provider.writes.get(), 0);
        }
    }
}

/// A recapture must reject old workers even if evidence and clock time return to their prior values.
fn repeated_observation_has_a_distinct_snapshot() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());

    for ado in [false, true] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("run");
        let provider = if ado {
            Provider::ado(repository.path(), &old, &new)
        } else {
            Provider::new(repository.path(), &old, &new)
        };
        let (original, prepared) = prepare(&root, &provider, false);
        let initial: Value =
            serde_json::from_slice(&fs::read(root.join("run.json")).unwrap()).unwrap();
        assess_with_limit(&root, &provider, &original, &prepared, "significant", 4096);

        let key = if ado { "description" } else { "body" };
        let original_description = provider.evidence.borrow()["pullRequest"].get(key).cloned();
        provider.evidence.borrow_mut()["pullRequest"][key] = json!("Temporary discussion change");
        assert_eq!(
            ss_skills_util::classification(&notify(&root, &provider, &original).unwrap_err()),
            "staleEvidence"
        );
        let mut evidence = provider.evidence.borrow_mut();
        let pull_request = evidence["pullRequest"].as_object_mut().unwrap();
        if let Some(description) = original_description {
            pull_request.insert(key.to_owned(), description);
        } else {
            pull_request.remove(key);
        }
        drop(evidence);

        let refreshed = refresh(&root, &provider).unwrap();
        let snapshot = refreshed["items"][0]["snapshotDigest"].as_str().unwrap();
        assert_ne!(snapshot, original);
        assert_eq!(refreshed["items"][0]["staleRecovery"]["attempts"], 1);
        assert_eq!(provider.writes.get(), 0);

        // Earlier schema-8 records used the observation-only digest.
        let path = root.join("run.json");
        let current = fs::read(&path).unwrap();
        let mut legacy: Value = serde_json::from_slice(&current).unwrap();
        assert_eq!(
            legacy["inventory"][0]["inspection"]["evidence"],
            initial["inventory"][0]["inspection"]["evidence"]
        );
        legacy["inventory"][0]["inspection"]["snapshotDigest"] = json!(original);
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(
            invoke(&root, &provider, "results", &[]).unwrap()["items"][0]["snapshotDigest"],
            original
        );
        fs::write(&path, current).unwrap();

        let prepared = work(&root, &provider, "prepare", snapshot, &[]).unwrap();
        assess_with_limit(&root, &provider, snapshot, &prepared, "significant", 4096);
        assert_eq!(
            ss_skills_util::classification(&notify(&root, &provider, &original).unwrap_err()),
            "invalidState"
        );
        notify(&root, &provider, snapshot).unwrap();
        assert_eq!(provider.writes.get(), 1);
    }
}

/// Capture races recover in live and dry-run modes without bypassing authority.
fn stale_inspection_recovery_is_bounded_and_rechecks_authority() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        for dry_run in [false, true] {
            for settle in [false, true] {
                let scope = TempDir::new().unwrap();
                let root = scope.path().join("run");
                let provider = if ado {
                    Provider::ado(repository.path(), &old, &new)
                } else {
                    Provider::new(repository.path(), &old, &new)
                };
                provider.fault.set("movingCapture");
                let initial = discover_inspect(&root, &provider, dry_run);
                assert_eq!(initial["items"][0]["status"], "staleDuringExecution");
                if settle {
                    provider.fault.set("");
                    if ado {
                        provider.ado_vote(-10, 41, "2026-09-28T09:07:00Z");
                    } else {
                        provider.later("CHANGES_REQUESTED");
                    }
                    let result = refresh(&root, &provider).unwrap();
                    assert_eq!(result["items"][0]["status"], "ineligible");
                    assert_eq!(result["state"], "complete");
                } else {
                    for attempt in 1..=2 {
                        let result = refresh(&root, &provider).unwrap();
                        assert_eq!(result["items"][0]["status"], "staleDuringExecution");
                        assert_eq!(result["items"][0]["staleRecovery"]["attempts"], attempt);
                    }
                    assert_eq!(
                        ss_skills_util::classification(&refresh(&root, &provider).unwrap_err()),
                        "retryExhausted"
                    );
                    let reads = provider.target_reads.get();
                    refresh(&root, &provider).unwrap_err();
                    assert_eq!(provider.target_reads.get(), reads);
                }
                assert_eq!(provider.writes.get(), 0);
                assert_eq!(provider.fetches.get(), 0);
            }
        }
    }
}

/// Staleness after an attempted effect is never permission to authorize another write.
fn stale_refresh_preserves_accepted_and_unknown_effects() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        for fault in ["postPush", "unknown", "noticeUnknown"] {
            let scope = TempDir::new().unwrap();
            let root = scope.path().join("run");
            let provider = if ado {
                Provider::ado(repository.path(), &old, &new)
            } else {
                Provider::new(repository.path(), &old, &new)
            };
            let (snapshot, prepared) = prepare(&root, &provider, false);
            let disposition = if fault == "noticeUnknown" {
                "significant"
            } else {
                "insignificant"
            };
            // Page size matches native reads; this scenario targets remote-effect safety.
            assess_with_limit(&root, &provider, &snapshot, &prepared, disposition, 4096);
            let key = if ado { "description" } else { "body" };
            provider.evidence.borrow_mut()["pullRequest"][key] =
                json!("Changed before the first effect.");
            if fault == "noticeUnknown" {
                notify(&root, &provider, &snapshot).unwrap_err();
            } else {
                assert_eq!(
                    publish(&root, &provider, &snapshot).unwrap()["items"][0]["status"],
                    "staleDuringExecution"
                );
            }
            let refreshed = refresh(&root, &provider).unwrap();
            let snapshot = refreshed["items"][0]["snapshotDigest"].as_str().unwrap();
            let prepared = work(&root, &provider, "prepare", snapshot, &[]).unwrap();
            assess_with_limit(&root, &provider, snapshot, &prepared, disposition, 4096);
            provider.fault.set(fault);
            if fault == "noticeUnknown" {
                notify(&root, &provider, snapshot).unwrap_err();
            } else if fault == "postPush" {
                assert_eq!(
                    publish(&root, &provider, snapshot).unwrap()["items"][0]["status"],
                    "staleDuringExecution"
                );
            } else {
                assert_eq!(
                    publish(&root, &provider, snapshot).unwrap()["error"]["kind"],
                    "ambiguousWrite"
                );
            }
            let before = fs::read(root.join("run.json")).unwrap();
            let writes = provider.writes.get();
            assert!(writes > 0);
            assert_eq!(
                ss_skills_util::classification(&refresh(&root, &provider).unwrap_err()),
                "ambiguousWrite"
            );
            assert_eq!(fs::read(root.join("run.json")).unwrap(), before);
            assert_eq!(provider.writes.get(), writes);
            if fault == "postPush" {
                let path = host_journal(&scope.path().join("host-store").join("publications"));
                fs::rename(&path, path.with_extension("retained")).unwrap();
                assert_eq!(
                    ss_skills_util::classification(&refresh(&root, &provider).unwrap_err()),
                    "ambiguousWrite"
                );
                assert_eq!(fs::read(root.join("run.json")).unwrap(), before);
                assert_eq!(provider.writes.get(), writes);
            }
        }
    }
}

/// Exercises both providers' persistent refusal and independent-approval reset.
fn needs_review_notices_short_circuit_and_expire_only_after_external_approval() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("first");
        let provider = if ado {
            Provider::ado(repository.path(), &old, &new)
        } else {
            Provider::new(repository.path(), &old, &new)
        };
        let (snapshot, prepared) = prepare(&root, &provider, false);
        assess_as(&root, &provider, &snapshot, &prepared, "significant");
        let overlapping = scope.path().join("overlapping");
        let (overlapping_snapshot, overlapping_prepared) = prepare(&overlapping, &provider, false);
        assess_as(
            &overlapping,
            &provider,
            &overlapping_snapshot,
            &overlapping_prepared,
            "significant",
        );
        publish(&root, &provider, &snapshot).unwrap_err();
        let result = notify(&root, &provider, &snapshot).unwrap();
        assert_eq!(result["state"], "complete");
        assert_eq!(result["items"][0]["status"], "needsReview");
        assert_eq!(
            result["items"][0]["reviewNotice"]["createdAt"],
            "2026-09-28T09:05:00Z"
        );
        assert_eq!(provider.writes.get(), 1);
        notify(&root, &provider, &snapshot).unwrap();
        let shared = notify(&overlapping, &provider, &overlapping_snapshot).unwrap();
        assert_eq!(
            shared["items"][0]["reviewNotice"],
            result["items"][0]["reviewNotice"]
        );
        assert_eq!(provider.writes.get(), 1);
        let body = if ado {
            provider.evidence.borrow()["threads"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["comments"][0]["content"]
                .as_str()
                .unwrap()
                .to_owned()
        } else {
            provider.evidence.borrow()["comments"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["body"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert!(body.starts_with("[Copilot speaking]"));
        assert!(body.contains("<!-- ss-autoreapprove-needs-review "));
        assert!(body.contains("separate new review"));
        let fetches = provider.fetches.get();
        let retained = discover_inspect(&scope.path().join("retained"), &provider, false);
        assert_eq!(retained["items"][0]["status"], "needsReview");
        assert_eq!(
            retained["items"][0]["reviewNotice"],
            result["items"][0]["reviewNotice"]
        );
        assert_eq!(
            provider.fetches.get(),
            fetches,
            "Retained notices skip source work"
        );
        assert_eq!(provider.writes.get(), 1);
        if ado {
            // An external positive vote on the same source is a new approval event.
            provider.ado_vote(10, 40, "2026-09-28T09:06:00Z");
        } else {
            provider.later("APPROVED");
            provider.evidence.borrow_mut()["reviews"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()["submitted_at"] = json!("2026-09-28T09:06:00Z");
        }
        let reset = discover_inspect(&scope.path().join("reset"), &provider, false);
        assert_eq!(reset["items"][0]["status"], "alreadyValid");
        assert!(reset["items"][0]["automaticApproval"].is_null());
        if ado {
            provider.ado_push();
            provider.evidence.borrow_mut()["iterations"][2]["createdDate"] =
                json!("2026-09-28T09:08:00Z");
        } else {
            let mut evidence = provider.evidence.borrow_mut();
            evidence["reviews"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()["state"] = json!("DISMISSED");
            evidence["timeline"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()["state"] = json!("DISMISSED");
            evidence["timeline"].as_array_mut().unwrap().push(json!({
                "__typename": "ReviewDismissedEvent", "id": "later-dismissal",
                "review": {"id": "later-review"}, "previousReviewState": "APPROVED",
                "createdAt": "2026-09-28T09:08:00Z",
                "pullRequestCommit": {"commit": {"oid": new}}
            }));
        }
        let resumed = discover_inspect(&scope.path().join("resumed"), &provider, true);
        assert_eq!(resumed["items"][0]["status"], "assessmentRequired");
        assert_eq!(provider.writes.get(), 1);
    }
}

/// Verifies that dry runs and uncertain comment effects never issue another POST.
fn notice_dry_runs_refusals_and_uncertain_effects_do_not_duplicate_writes() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        for fault in [
            "dryRun",
            "uncertain",
            "noticeRefused",
            "lostNoticeReply",
            "noticeUnknown",
            "postReadFailure",
            "prePush",
        ] {
            let scope = TempDir::new().unwrap();
            let root = scope.path().join("first");
            let provider = if ado {
                Provider::ado(repository.path(), &old, &new)
            } else {
                Provider::new(repository.path(), &old, &new)
            };
            let (snapshot, prepared) = prepare(&root, &provider, fault == "dryRun");
            assess_as(
                &root,
                &provider,
                &snapshot,
                &prepared,
                if fault == "uncertain" {
                    "uncertain"
                } else {
                    "significant"
                },
            );
            provider.fault.set(fault);
            if fault == "prePush" {
                if ado {
                    provider.ado_push();
                } else {
                    provider.evidence.borrow_mut()["pullRequest"]["head"]["sha"] =
                        json!("f".repeat(40));
                }
            }
            let result = notify(&root, &provider, &snapshot);
            if fault == "lostNoticeReply" {
                assert_eq!(result.unwrap()["items"][0]["status"], "needsReview");
                notify(&root, &provider, &snapshot).unwrap();
            } else if fault == "uncertain" {
                let result = result.unwrap();
                assert_eq!(result["items"][0]["status"], "failed");
                assert_eq!(result["items"][0]["error"]["kind"], "service");
                assert_eq!(result["state"], "partial");
                let later = discover_inspect(&scope.path().join("later"), &provider, false);
                assert_eq!(later["items"][0]["status"], "needsReview");
                notify(&root, &provider, &snapshot).unwrap();
            } else if fault == "postReadFailure" {
                assert_eq!(
                    ss_skills_util::classification(&result.unwrap_err()),
                    "ambiguousWrite"
                );
                provider.fault.set("");
                assert_eq!(
                    notify(&root, &provider, &snapshot).unwrap()["items"][0]["status"],
                    "needsReview"
                );
            } else {
                result.unwrap_err();
                notify(&root, &provider, &snapshot).unwrap_err();
            }
            if fault == "noticeUnknown" {
                provider.fault.set("");
                let later = scope.path().join("later");
                let (snapshot, prepared) = prepare(&later, &provider, false);
                assess(&later, &provider, &snapshot, &prepared);
                assert_eq!(
                    ss_skills_util::classification(
                        &publish(&later, &provider, &snapshot).unwrap_err()
                    ),
                    "ambiguousWrite"
                );
            }
            assert_eq!(
                provider.writes.get(),
                usize::from(!matches!(fault, "dryRun" | "prePush")),
                "{ado}: {fault}"
            );
        }
    }
}

/// Keeps automatic timestamps distinct from external approvals across fresh runs.
fn valid_automatic_approval_timestamps_survive_later_invocations() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("first");
        let provider = if ado {
            Provider::ado(repository.path(), &old, &new)
        } else {
            Provider::new(repository.path(), &old, &new)
        };
        let (snapshot, prepared) = prepare(&root, &provider, false);
        assess(&root, &provider, &snapshot, &prepared);
        let overlapping = scope.path().join("overlapping");
        let (overlapping_snapshot, overlapping_prepared) = prepare(&overlapping, &provider, false);
        assess(
            &overlapping,
            &provider,
            &overlapping_snapshot,
            &overlapping_prepared,
        );
        let published = publish(&root, &provider, &snapshot).unwrap();
        let automatic = &published["items"][0]["automaticApproval"];
        assert_eq!(automatic["approvedAt"], "2026-09-28T09:00:00Z");
        assert_eq!(published["items"][0]["status"], "reapproved");
        let shared = publish(&overlapping, &provider, &overlapping_snapshot).unwrap();
        assert_eq!(shared["items"][0]["status"], "alreadyValid");
        assert_eq!(&shared["items"][0]["automaticApproval"], automatic);
        provider.fault.set("preflightReadFailure");
        assert_eq!(publish(&root, &provider, &snapshot).unwrap(), published);
        assert_eq!(
            publish(&overlapping, &provider, &overlapping_snapshot).unwrap(),
            shared
        );
        provider.fault.set("");
        let later = discover_inspect(&scope.path().join("later"), &provider, false);
        assert_eq!(later["items"][0]["status"], "alreadyValid");
        assert_eq!(&later["items"][0]["automaticApproval"], automatic);
        assert_eq!(
            later["items"][0]["title"],
            if ado { "Recent" } else { "Recent source" }
        );
        assert!(
            later["items"][0]["url"]
                .as_str()
                .unwrap()
                .starts_with(provider.url())
        );
        assert_eq!(
            later["items"].as_array().unwrap().len(),
            1,
            "Out-of-range PRs appear only in counts"
        );
    }
}

/// A terminal provider receipt cannot leave reporting errors absent from results.
fn publication_reporting_failures_are_persisted() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        for fault in ["reportReadFailure", "reportStandingChanged"] {
            let scope = TempDir::new().unwrap();
            let root = scope.path().join("first");
            let provider = if ado {
                Provider::ado(repository.path(), &old, &new)
            } else {
                Provider::new(repository.path(), &old, &new)
            };
            let (snapshot, prepared) = prepare(&root, &provider, false);
            assess(&root, &provider, &snapshot, &prepared);
            if ado {
                provider.ado_vote(10, 40, "2026-09-28T09:06:00Z");
            } else {
                provider.later("APPROVED");
            }
            provider
                .host_directory
                .replace(Some(scope.path().join("host-store/publications")));
            provider.fault.set(fault);
            let error = publish(&root, &provider, &snapshot).unwrap_err();
            let result = invoke(&root, &provider, "results", &[]).unwrap();
            assert_eq!(result["state"], "partial");
            assert_eq!(
                result["items"][0]["status"],
                if fault == "reportStandingChanged" {
                    "staleDuringExecution"
                } else {
                    "failed"
                }
            );
            assert_eq!(
                result["items"][0]["error"]["kind"],
                ss_skills_util::classification(&error)
            );
            assert!(result["items"][0]["publication"].is_null());
            assert!(result["items"][0]["automaticApproval"].is_null());
            assert_eq!(provider.writes.get(), 0);
            provider.fault.set("");
            publish(&root, &provider, &snapshot).unwrap_err();
            assert_eq!(invoke(&root, &provider, "results", &[]).unwrap(), result);
        }
    }
}

/// A completed run retains its observed result rather than mixing it with later service state.
fn persisted_already_valid_results_replay_without_refreshing_reporting() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        for fault in ["reportReadFailure", "reportStandingChanged"] {
            let scope = TempDir::new().unwrap();
            let root = scope.path().join("run");
            let provider = if ado {
                Provider::ado(repository.path(), &old, &new)
            } else {
                Provider::new(repository.path(), &old, &new)
            };
            let (snapshot, prepared) = prepare(&root, &provider, false);
            assess(&root, &provider, &snapshot, &prepared);
            if ado {
                provider.ado_vote(10, 40, "2026-09-28T09:06:00Z");
            } else {
                provider.later("APPROVED");
            }
            provider
                .host_directory
                .replace(Some(scope.path().join("host-store/publications")));
            let published = publish(&root, &provider, &snapshot).unwrap();
            assert_eq!(published["state"], "complete");
            assert_eq!(published["items"][0]["status"], "alreadyValid");
            assert!(published["items"][0]["publication"].is_object());
            assert!(published["items"][0]["automaticApproval"].is_null());
            let run_bytes = fs::read(root.join("run.json")).unwrap();
            if fault == "reportStandingChanged" {
                if ado {
                    provider.ado_vote(0, 41, "2026-09-28T09:07:00Z");
                } else {
                    let mut evidence = provider.evidence.borrow_mut();
                    evidence["reviews"].as_array_mut().unwrap().push(json!({
                        "id": 99, "node_id": "later-withdrawal",
                        "user": {"id": 42, "node_id": "user-42"},
                        "state": "CHANGES_REQUESTED", "submitted_at": "2026-09-28T09:07:00Z",
                        "commit_id": new, "body": "Independent later decision."
                    }));
                    evidence["timeline"].as_array_mut().unwrap().push(json!({
                        "__typename": "PullRequestReview", "id": "later-withdrawal",
                        "state": "CHANGES_REQUESTED"
                    }));
                }
            } else {
                provider.fault.set(fault);
            }
            let reads = provider.target_reads.get();
            let replayed = publish(&root, &provider, &snapshot)
                .unwrap_or_else(|error| panic!("{ado}: {fault}: {error}"));
            assert_eq!(replayed, published, "{ado}: {fault}");
            assert_eq!(invoke(&root, &provider, "results", &[]).unwrap(), published);
            assert_eq!(fs::read(root.join("run.json")).unwrap(), run_bytes);
            assert_eq!(provider.target_reads.get(), reads);
            assert_eq!(provider.writes.get(), 0);
        }
    }
}

/// Failed terminal saves must retain recoverable intent in both journals.
fn notice_terminal_save_failures_preserve_original_recovery() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        for fault in ["noticeJournalFailure", "noticeRefusalJournalFailure"] {
            let scope = TempDir::new().unwrap();
            let root = scope.path().join("first");
            let provider = if ado {
                Provider::ado(repository.path(), &old, &new)
            } else {
                Provider::new(repository.path(), &old, &new)
            };
            let (snapshot, prepared) = prepare(&root, &provider, false);
            assess_as(&root, &provider, &snapshot, &prepared, "significant");
            let directory = scope.path().join("host-store/review-notices");
            provider.host_directory.replace(Some(directory.clone()));
            provider.fault.set(fault);
            let error = notify(&root, &provider, &snapshot).unwrap_err();
            assert_eq!(ss_skills_util::classification(&error), "ambiguousWrite");
            let result = invoke(&root, &provider, "results", &[]).unwrap();
            assert_eq!(result["state"], "failed");
            assert_eq!(result["error"]["kind"], "ambiguousWrite");
            assert!(result["items"][0]["error"].is_null());
            let path = host_journal(&directory);
            let intent: Value =
                serde_json::from_slice(&fs::read(path.with_extension("saved")).unwrap()).unwrap();
            assert_eq!(intent["attempts"][0]["state"], "intent");
            fs::remove_dir(&path).unwrap();
            fs::rename(path.with_extension("saved"), &path).unwrap();
            provider.fault.set("");
            let recovered = notify(&root, &provider, &snapshot);
            if fault == "noticeJournalFailure" {
                assert_eq!(recovered.unwrap()["items"][0]["status"], "needsReview");
                let settled: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
                assert_eq!(settled["attempts"][0]["state"], "verified");
            } else {
                // The lost refusal is not durable evidence permitting another POST.
                assert_eq!(
                    ss_skills_util::classification(&recovered.unwrap_err()),
                    "ambiguousWrite"
                );
            }
            assert_eq!(provider.writes.get(), 1);
        }
    }
}

/// A resolved notification error cannot replace the assessment's uncertainty.
fn uncertain_assessment_diagnostic_survives_notification_retry() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("first");
        let provider = if ado {
            Provider::ado(repository.path(), &old, &new)
        } else {
            Provider::new(repository.path(), &old, &new)
        };
        let (snapshot, prepared) = prepare(&root, &provider, false);
        assess_as(&root, &provider, &snapshot, &prepared, "uncertain");
        let before = invoke(&root, &provider, "results", &[]).unwrap();
        provider.fault.set("preflightReadFailure");
        notify(&root, &provider, &snapshot).unwrap_err();
        let failed = invoke(&root, &provider, "results", &[]).unwrap();
        assert_eq!(failed["items"][0]["error"], before["items"][0]["error"]);
        provider.fault.set("");
        let recovered = notify(&root, &provider, &snapshot).unwrap();
        assert_eq!(recovered["items"][0]["error"], before["items"][0]["error"]);
        assert_eq!(recovered["items"][0]["status"], "failed");
        assert_eq!(recovered["state"], "partial");
        assert!(recovered["items"][0]["reviewNotice"].is_object());
        assert_eq!(provider.writes.get(), 1);
    }
}

/// Recovery cannot clear another PR's fatal write, even across a lost run save.
fn notice_recovery_is_bound_to_the_failed_pull_request() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for interrupted_run_save in [false, true] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("run");
        let mut first = Provider::new(repository.path(), &old, &new);
        first.candidates = vec![1, 2];
        let mut second = Provider::new(repository.path(), &old, &new);
        second.number = 2;
        second.candidates = vec![2];
        second.evidence.borrow_mut()["pullRequest"]["number"] = json!(2);
        let (first_snapshot, prepared) = prepare(&root, &first, false);
        assess_as(&root, &first, &first_snapshot, &prepared, "significant");
        let (second_snapshot, prepared) = prepare(&root, &second, false);
        assess_as(&root, &second, &second_snapshot, &prepared, "significant");
        let run_path = root.join("run.json");
        let before = fs::read(&run_path).unwrap();
        notify(&root, &first, &first_snapshot).unwrap();
        if interrupted_run_save {
            // Preserve the verified host effect while replaying the last durable run.
            fs::write(&run_path, before).unwrap();
        }
        second.fault.set("postReadFailure");
        notify(&root, &second, &second_snapshot).unwrap_err();
        let failed = fs::read(&run_path).unwrap();
        let record: Value = serde_json::from_slice(&failed).unwrap();
        assert_eq!(record["state"], "failed");
        assert_eq!(record["errorPullRequestId"], 2);
        notify(&root, &first, &first_snapshot).unwrap_err();
        assert_eq!(fs::read(&run_path).unwrap(), failed);
        second.fault.set("");
        notify(&root, &second, &second_snapshot).unwrap();
        let recovered = notify(&root, &first, &first_snapshot).unwrap();
        assert_eq!(recovered["state"], "complete");
        assert!(recovered["error"].is_null());
        assert_eq!(first.writes.get(), 1);
        assert_eq!(second.writes.get(), 1);
    }
}

/// A later unproven vote reset cannot prevent verification of an existing comment.
fn ado_notice_recovery_does_not_require_current_approval_eligibility() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    let scope = TempDir::new().unwrap();
    let root = scope.path().join("run");
    let provider = Provider::ado(repository.path(), &old, &new);
    let (snapshot, prepared) = prepare(&root, &provider, false);
    assess_as(&root, &provider, &snapshot, &prepared, "significant");
    provider.fault.set("postReadFailure");
    notify(&root, &provider, &snapshot).unwrap_err();
    provider.fault.set("");
    provider.ado_push();
    provider.evidence.borrow_mut()["reviewers"][0]["vote"] = json!(0);
    provider.evidence.borrow_mut()["pullRequest"]["pullRequestId"] = json!(999);
    assert_eq!(
        ss_skills_util::classification(&notify(&root, &provider, &snapshot).unwrap_err()),
        "ambiguousWrite"
    );
    provider.evidence.borrow_mut()["pullRequest"]["pullRequestId"] = json!(1);
    let recovered = notify(&root, &provider, &snapshot).unwrap();
    assert_eq!(recovered["items"][0]["status"], "needsReview");
    assert!(recovered["error"].is_null());
    assert_eq!(provider.writes.get(), 1);
}

/// Accepted comments remain recoverable after state changes without weakening ownership checks.
fn notice_readback_survives_closure_and_draft_conversion() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        for fault in ["noticeClosed", "noticeDraft", "noticeAbandoned"] {
            if !ado && fault == "noticeAbandoned" {
                continue;
            }
            for interrupted in [false, true] {
                let scope = TempDir::new().unwrap();
                let root = scope.path().join("run");
                let provider = if ado {
                    Provider::ado(repository.path(), &old, &new)
                } else {
                    Provider::new(repository.path(), &old, &new)
                };
                let (snapshot, prepared) = prepare(&root, &provider, false);
                assess_as(&root, &provider, &snapshot, &prepared, "significant");
                if interrupted {
                    provider.fault.set("postReadFailure");
                    assert_eq!(
                        ss_skills_util::classification(
                            &notify(&root, &provider, &snapshot).unwrap_err()
                        ),
                        "ambiguousWrite"
                    );
                    assert_eq!(
                        invoke(&root, &provider, "results", &[]).unwrap()["state"],
                        "failed"
                    );
                }
                provider.fault.set(fault);
                if interrupted {
                    assert_notice_readback_ownership(&root, &provider, &snapshot, fault);
                }
                let recovered = notify(&root, &provider, &snapshot)
                    .unwrap_or_else(|error| panic!("{ado}: {fault}: {interrupted}: {error}"));
                assert_eq!(recovered["state"], "complete");
                assert_eq!(recovered["items"][0]["status"], "needsReview");
                assert!(recovered["items"][0]["reviewNotice"].is_object());
                assert!(recovered["error"].is_null());
                let record: Value =
                    serde_json::from_slice(&fs::read(root.join("run.json")).unwrap()).unwrap();
                assert!(record["errorPullRequestId"].is_null());
                assert_eq!(provider.writes.get(), 1);
            }
        }
    }
}

/// Recovery of an inactive PR still requires a unique, exact, account-owned effect.
fn assert_notice_readback_ownership(
    root: &Path,
    provider: &Provider,
    snapshot: &str,
    fault: &'static str,
) {
    let ado = provider.ado;
    let original = provider.evidence.borrow().clone();
    // Replacement identities deliberately lie outside the fixture's original service scope.
    for mutation in ["account", "scope", "author", "body", "duplicate"] {
        let mut evidence = provider.evidence.borrow_mut();
        match mutation {
            "account" => provider.fault.set("auth"),
            "scope" => {
                evidence["pullRequest"][if ado { "pullRequestId" } else { "number" }] = json!(999);
            }
            _ => {
                let collection = evidence[if ado { "threads" } else { "comments" }]
                    .as_array_mut()
                    .unwrap();
                if mutation == "duplicate" {
                    let mut duplicate = collection.last().unwrap().clone();
                    duplicate["id"] = json!(999);
                    collection.push(duplicate);
                } else {
                    let comment = collection.last_mut().unwrap();
                    let comment = if ado {
                        &mut comment["comments"][0]
                    } else {
                        comment
                    };
                    if mutation == "author" {
                        comment[if ado { "author" } else { "user" }]["id"] =
                            json!("another-account");
                    } else {
                        let key = if ado { "content" } else { "body" };
                        comment[key] = json!(format!("{}\nEdited", comment[key].as_str().unwrap()));
                    }
                }
            }
        }
        drop(evidence);
        assert_eq!(
            ss_skills_util::classification(&notify(root, provider, snapshot).unwrap_err()),
            "ambiguousWrite",
            "{ado}: {fault}: {mutation}"
        );
        let failed = invoke(root, provider, "results", &[]).unwrap();
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error"]["kind"], "ambiguousWrite");
        assert_eq!(provider.writes.get(), 1);
        provider.evidence.replace(original.clone());
        provider.fault.set(fault);
    }
}

/// Superseded edited notices cannot obstruct a later operation's exact readback.
fn expired_edited_notices_do_not_block_new_notice_readback() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for ado in [false, true] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("first");
        let provider = if ado {
            Provider::ado(repository.path(), &old, &new)
        } else {
            Provider::new(repository.path(), &old, &new)
        };
        let (snapshot, prepared) = prepare(&root, &provider, false);
        assess_as(&root, &provider, &snapshot, &prepared, "significant");
        let original = notify(&root, &provider, &snapshot).unwrap();
        if ado {
            provider.ado_vote(10, 40, "2026-09-28T09:06:00Z");
            let mut evidence = provider.evidence.borrow_mut();
            evidence["threads"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|thread| thread["comments"][0]["commentType"] == "text")
                .unwrap()["comments"][0]["lastContentUpdatedDate"] = json!("2026-09-28T09:07:00Z");
            drop(evidence);
            provider.ado_push();
            let mut evidence = provider.evidence.borrow_mut();
            evidence["sourceCommit"] = json!(old);
            evidence["iterations"][2]["sourceRefCommit"]["commitId"] = json!(old);
            evidence["iterations"][2]["createdDate"] = json!("2026-09-28T09:08:00Z");
        } else {
            provider.later("APPROVED");
            let mut evidence = provider.evidence.borrow_mut();
            let review = evidence["reviews"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap();
            review["submitted_at"] = json!("2026-09-28T09:06:00Z");
            review["state"] = json!("DISMISSED");
            evidence["comments"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()["updated_at"] = json!("2026-09-28T09:07:00Z");
            evidence["pullRequest"]["head"]["sha"] = json!(old);
            evidence["timeline"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()["state"] = json!("DISMISSED");
            evidence["timeline"].as_array_mut().unwrap().push(json!({
                "__typename": "ReviewDismissedEvent", "id": "later-dismissal",
                "review": {"id": "later-review"}, "previousReviewState": "APPROVED",
                "createdAt": "2026-09-28T09:08:00Z",
                "pullRequestCommit": {"commit": {"oid": old}}
            }));
        }
        let later = scope.path().join("later");
        let (snapshot, prepared) = prepare(&later, &provider, false);
        assess_as(&later, &provider, &snapshot, &prepared, "significant");
        let result = notify(&later, &provider, &snapshot).unwrap();
        assert_eq!(result["items"][0]["status"], "needsReview");
        assert_ne!(
            result["items"][0]["reviewNotice"],
            original["items"][0]["reviewNotice"]
        );
        notify(&later, &provider, &snapshot).unwrap();
        // Even an approval that expires this notice cannot prevent its readback.
        if ado {
            provider.ado_vote(10, 41, "2026-09-28T09:09:00Z");
        } else {
            let mut evidence = provider.evidence.borrow_mut();
            evidence["reviews"].as_array_mut().unwrap().push(json!({
                "id": 99, "node_id": "final-approval", "user": {"id": 42, "node_id": "user-42"},
                "state": "APPROVED", "submitted_at": "2026-09-28T09:09:00Z",
                "commit_id": old, "body": "Independent approval."
            }));
            evidence["timeline"].as_array_mut().unwrap().push(json!({
                "__typename": "PullRequestReview", "id": "final-approval", "state": "APPROVED"
            }));
        }
        notify(&later, &provider, &snapshot).unwrap();
        assert_eq!(provider.writes.get(), 2);
    }
}

fn publish(root: &Path, provider: &Provider, snapshot: &str) -> Result<Value, AppError> {
    work(
        root,
        provider,
        "publish",
        snapshot,
        &[
            "--repository-url".into(),
            provider.url().into(),
            "--attribution".into(),
            "[Copilot speaking]".into(),
        ],
    )
}

#[test]
fn native_git_assessment_and_guarded_publication_have_exact_effect_boundaries() {
    let _guard = PROCESS_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    let baseline = git(repository.path(), &["status", "--porcelain=v1"]);
    for fault in [
        "",
        "dryRun",
        "lostReply",
        "unknown",
        "prePush",
        "postPush",
        "withdrawal",
        "pending",
        "discussion",
        "auth",
        "postWithdrawal",
        "postReadFailure",
        "duplicate",
        "rateLimit",
        "permission",
        "journalFailure",
        "otherUnknown",
    ] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("first");
        let provider = Provider::new(repository.path(), &old, &new);
        let (snapshot, prepared) = prepare(&root, &provider, fault == "dryRun");
        assert_eq!(prepared["oldCommit"], old);
        assert_eq!(prepared["newCommit"], new);
        assert_eq!(prepared["changeCount"], 1);
        work(&root, &provider, "complete", &snapshot, &[]).unwrap_err();
        work(
            &root,
            &provider,
            "read",
            &snapshot,
            &[
                "--reference".into(),
                "policy".into(),
                "--offset".into(),
                "10".into(),
            ],
        )
        .unwrap_err();
        assess(&root, &provider, &snapshot, &prepared);
        assert_eq!(provider.writes.get(), 0);
        provider.fault.set(fault);
        match fault {
            "prePush" => {
                provider.evidence.borrow_mut()["pullRequest"]["head"]["sha"] =
                    json!("e".repeat(40));
            }
            "withdrawal" => provider.later("CHANGES_REQUESTED"),
            "pending" => provider.later("PENDING"),
            "discussion" => {
                provider.evidence.borrow_mut()["pullRequest"]["body"] =
                    json!("New unresolved objection.");
            }
            _ => {}
        }
        if fault == "dryRun" {
            publish(&root, &provider, &snapshot).unwrap_err();
            let result = invoke(&root, &provider, "results", &[]).unwrap();
            assert_eq!(result["state"], "complete");
            assert_eq!(result["items"][0]["status"], "wouldReapprove");
            assert_eq!(provider.writes.get(), 0);
            continue;
        }
        if fault == "auth" {
            let error = publish(&root, &provider, &snapshot).unwrap_err();
            assert_eq!(ss_skills_util::classification(&error), "authentication");
            assert_eq!(provider.writes.get(), 0);
            assert_eq!(
                invoke(&root, &provider, "results", &[]).unwrap()["state"],
                "failed"
            );
            continue;
        }
        if fault == "otherUnknown" {
            let path = root.join("run.json");
            let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            record["state"] = json!("failed");
            record["error"] =
                json!({"kind": "ambiguousWrite", "message": "Another PR has unresolved work."});
            fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
            publish(&root, &provider, &snapshot).unwrap_err();
            assert_eq!(provider.writes.get(), 0);
            continue;
        }
        check_publication(&root, &provider, &snapshot, fault);
    }
    assert_eq!(
        git(repository.path(), &["status", "--porcelain=v1"]),
        baseline
    );
    assert_eq!(git(repository.path(), &["rev-parse", "HEAD"]), new);
    // Keep process launches under the guard so fork cannot inherit another test's file locks.
    force_push_inventory_preserves_every_path_kind_and_rejects_missing_objects();
    empty_source_diff_and_corrupted_assessments_never_bypass_discussion_or_coverage();
    risk_judgments_and_uncertainty_prevent_publication();
    prepared_run_cannot_bypass_another_runs_unknown_effect();
    ado_publication_preserves_authority_and_never_replays_unknown_effects();
    // Related subprocess scenarios share the guard against inherited file locks.
    needs_review_notices_short_circuit_and_expire_only_after_external_approval();
    notice_dry_runs_refusals_and_uncertain_effects_do_not_duplicate_writes();
    valid_automatic_approval_timestamps_survive_later_invocations();
    publication_reporting_failures_are_persisted();
    persisted_already_valid_results_replay_without_refreshing_reporting();
    notice_terminal_save_failures_preserve_original_recovery();
    uncertain_assessment_diagnostic_survives_notification_retry();
    notice_recovery_is_bound_to_the_failed_pull_request();
    ado_notice_recovery_does_not_require_current_approval_eligibility();
    notice_readback_survives_closure_and_draft_conversion();
    expired_edited_notices_do_not_block_new_notice_readback();
}

#[test]
fn native_stale_evidence_recovery_preserves_assessment_and_effect_boundaries() {
    // Unwinding drops test-owned journals before this guard, so poison cannot retain file locks.
    let _guard = PROCESS_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    stale_preflight_recovery_reassesses_source_before_any_effect();
    stale_notification_recovery_exhausts_without_remote_writes();
    repeated_observation_has_a_distinct_snapshot();
    stale_inspection_recovery_is_bounded_and_rechecks_authority();
    stale_refresh_preserves_accepted_and_unknown_effects();
}

/// Exercises real command dispatch and records while ADO effects remain entirely in memory.
#[expect(
    clippy::cognitive_complexity,
    reason = "The bounded scenario table verifies each distinct effect boundary"
)]
fn ado_publication_preserves_authority_and_never_replays_unknown_effects() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for fault in [
        "",
        "suggestions",
        "dryRun",
        "lostVoteReply",
        "lostAuditReply",
        "unknown",
        "auditUnknown",
        "auditRefused",
        "policyCompleted",
        "sourceAtVote",
        "prePush",
        "postPush",
        "withdrawal",
        "postWithdrawal",
        "pending",
        "declined",
        "pendingThread",
        "discussion",
        "auth",
        "duplicate",
        "postReadFailure",
        "permission",
        "rateLimit",
    ] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("ado");
        let provider = Provider::ado(repository.path(), &old, &new);
        if fault == "suggestions" {
            let mut evidence = provider.evidence.borrow_mut();
            evidence["reviewers"][0]["vote"] = json!(5);
            evidence["threads"][1]["properties"]["CodeReviewVoteResult"]["$value"] = json!("5");
        }
        let (snapshot, prepared) = prepare(&root, &provider, fault == "dryRun");
        assert_eq!(prepared["oldCommit"], old, "{fault}: {prepared}");
        assert_eq!(prepared["newCommit"], new);
        assess(&root, &provider, &snapshot, &prepared);
        let original_run = fs::read(root.join("run.json")).unwrap();
        let second = scope.path().join("overlapping");
        let second_snapshot = if matches!(fault, "unknown" | "auditUnknown" | "duplicate") {
            let (snapshot, prepared) = prepare(&second, &provider, false);
            assess(&second, &provider, &snapshot, &prepared);
            Some(snapshot)
        } else {
            None
        };
        provider.fault.set(fault);
        match fault {
            "prePush" => provider.ado_push(),
            "withdrawal" => provider.ado_vote(0, 19, "2026-09-28T08:00:00Z"),
            "pending" => provider.evidence.borrow_mut()["reviewers"][0]["isFlagged"] = json!(true),
            "declined" => provider.evidence.borrow_mut()["reviewers"][0]["hasDeclined"] = json!(true),
            "discussion" => provider.evidence.borrow_mut()["pullRequest"]["description"] = json!("New human objection."),
            "pendingThread" => provider.evidence.borrow_mut()["threads"].as_array_mut().unwrap().push(json!({
                "id": 19, "status": "pending", "comments": [{"commentType": "text",
                    "author": {"id": "10000000-0000-4000-8000-000000000001"}, "content": "Pending human review"}]})),
            _ => {}
        }
        if fault == "dryRun" || fault == "auth" {
            publish(&root, &provider, &snapshot).unwrap_err();
            assert_eq!(provider.writes.get(), 0);
            continue;
        }
        let result = publish(&root, &provider, &snapshot).unwrap();
        let expected = match fault {
            "unknown" | "auditUnknown" | "auditRefused" | "lostAuditReply" | "duplicate"
            | "postReadFailure" | "permission" | "rateLimit" => "failed",
            "prePush" | "postPush" | "sourceAtVote" | "postWithdrawal" | "discussion" => {
                "staleDuringExecution"
            }
            "withdrawal" | "pending" | "declined" | "pendingThread" => "ineligible",
            _ => "reapproved",
        };
        assert_eq!(result["items"][0]["status"], expected, "{fault}: {result}");
        let writes = provider.writes.get();
        if let Some(second_snapshot) = second_snapshot {
            let error = publish(&second, &provider, &second_snapshot).unwrap_err();
            assert_eq!(ss_skills_util::classification(&error), "ambiguousWrite");
            assert_eq!(
                provider.writes.get(),
                writes,
                "Unresolved effects block overlapping runs"
            );
        }
        if matches!(
            fault,
            "prePush" | "withdrawal" | "pending" | "declined" | "pendingThread" | "discussion"
        ) {
            assert_eq!(writes, 0, "{fault}");
        } else if matches!(
            fault,
            "unknown" | "duplicate" | "postReadFailure" | "permission" | "rateLimit"
        ) {
            assert_eq!(writes, 1, "{fault}");
        } else {
            assert_eq!(writes, 2, "{fault}");
        }
        provider.fault.set("");
        if fault.is_empty() {
            // Host persistence precedes run persistence. Restore the original run
            // to simulate interruption between those saves, then consume the receipt.
            fs::write(root.join("run.json"), original_run).unwrap();
        }
        if fault == "permission" {
            publish(&root, &provider, &snapshot).unwrap_err();
            assert_eq!(provider.writes.get(), writes);
            continue;
        }
        let recovered =
            publish(&root, &provider, &snapshot).unwrap_or_else(|error| panic!("{fault}: {error}"));
        if fault == "lostAuditReply" || fault == "postReadFailure" || fault == "auditRefused" {
            assert_eq!(
                recovered["items"][0]["status"], "reapproved",
                "{fault}: {recovered}"
            );
            assert_eq!(
                provider.writes.get(),
                writes + usize::from(fault != "lostAuditReply")
            );
        } else {
            assert_eq!(recovered["items"][0]["status"], expected, "{fault}");
            assert_eq!(provider.writes.get(), writes, "No replay for {fault}");
        }
        if matches!(
            fault,
            "" | "suggestions"
                | "lostVoteReply"
                | "lostAuditReply"
                | "postReadFailure"
                | "auditRefused"
        ) {
            // On the next push, cumulative authority must still name the external
            // approval, not the automatic vote or its audit thread.
            provider.ado_push();
            let later = scope.path().join("later");
            let (_, later_prepared) = prepare(&later, &provider, true);
            // The fake new source is deliberately unavailable locally. Inspection,
            // not successful object fetching, must already preserve the anchor.
            assert_eq!(later_prepared["items"][0]["status"], "failed");
            let saved: Value =
                serde_json::from_slice(&fs::read(later.join("run.json")).unwrap()).unwrap();
            assert_eq!(
                saved["inventory"][0]["inspection"]["decision"]["anchor"]["sourceCommit"],
                old
            );
            assert_eq!(
                saved["inventory"][0]["inspection"]["decision"]["anchor"]["reviewId"],
                "2"
            );
        }
    }
}

/// A saved preparation does not reserve permission to write after another run becomes uncertain.
fn prepared_run_cannot_bypass_another_runs_unknown_effect() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    let scope = TempDir::new().unwrap();
    let first = scope.path().join("first");
    let second = scope.path().join("second");
    let provider = Provider::new(repository.path(), &old, &new);
    let (snapshot, prepared) = prepare(&first, &provider, false);
    assess(&first, &provider, &snapshot, &prepared);
    let run_path = first.join("run.json");
    let before = fs::read(&run_path).unwrap();

    // Refuse preflight after preparation, then restore the run as if the process
    // stopped before saving its error. The host's prepared record remains intact.
    provider.fault.set("auth");
    assert_eq!(
        ss_skills_util::classification(&publish(&first, &provider, &snapshot).unwrap_err()),
        "authentication"
    );
    fs::write(&run_path, before).unwrap();
    provider.fault.set("");
    let (second_snapshot, prepared) = prepare(&second, &provider, false);
    assess(&second, &provider, &second_snapshot, &prepared);
    provider.fault.set("unknown");
    let result = publish(&second, &provider, &second_snapshot).unwrap();
    assert_eq!(result["state"], "failed");
    assert_eq!(provider.writes.get(), 1);

    provider.fault.set("");
    let error = publish(&first, &provider, &snapshot).unwrap_err();
    assert_eq!(ss_skills_util::classification(&error), "ambiguousWrite");
    assert_eq!(
        provider.writes.get(),
        1,
        "Prepared work must not replay an unknown effect"
    );
}

/// Each policy concern and insufficient evidence independently prevents a remote effect.
fn risk_judgments_and_uncertainty_prevent_publication() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for (disposition, rationale, status) in [
        (
            "significant",
            "Significant design change replaces the approved ownership model.",
            "needsReview",
        ),
        (
            "significant",
            "Likely performance regression adds a scan per item.",
            "needsReview",
        ),
        (
            "significant",
            "Likely correctness regression drops the empty-input case.",
            "needsReview",
        ),
        (
            "uncertain",
            "Missing contract context prevents assessing correctness.",
            "failed",
        ),
    ] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("risk");
        let provider = Provider::new(repository.path(), &old, &new);
        let (snapshot, prepared) = prepare(&root, &provider, false);
        let id = prepared["changes"][0]["id"].as_str().unwrap();
        work(
            &root,
            &provider,
            "judge",
            &snapshot,
            &[
                "--change-id".into(),
                id.into(),
                "--disposition".into(),
                disposition.into(),
                "--rationale".into(),
                rationale.into(),
                "--evidence".into(),
                format!("{id}:new"),
            ],
        )
        .unwrap();
        // A favorable aggregate must not hide either a significant or uncertain member.
        work(
            &root,
            &provider,
            "judge",
            &snapshot,
            &[
                "--change-id".into(),
                "cumulative".into(),
                "--disposition".into(),
                "insignificant".into(),
                "--rationale".into(),
                "Claimed acceptable aggregate.".into(),
                "--evidence".into(),
                "policy".into(),
                "--evidence".into(),
                "discussion".into(),
            ],
        )
        .unwrap();
        let completed = work(&root, &provider, "complete", &snapshot, &[]).unwrap();
        assert_eq!(completed["status"], status);
        if status == "failed" {
            assert_eq!(completed["error"]["kind"], "service");
            let saved: Value =
                serde_json::from_slice(&fs::read(root.join("run.json")).unwrap()).unwrap();
            assert_eq!(saved["state"], "partial");
            assert!(!saved["inventory"][0]["executionError"].is_null());
        }
        publish(&root, &provider, &snapshot).unwrap_err();
        assert_eq!(provider.writes.get(), 0);
    }
}

/// Checks terminal effects and recovery separately from scenario preparation.
fn check_publication(root: &Path, provider: &Provider, snapshot: &str, fault: &str) {
    let result = if fault == "journalFailure" {
        recover_failed_journal(root, provider, snapshot)
    } else {
        publish(root, provider, snapshot).unwrap()
    };
    let expected = match fault {
        "unknown" | "postReadFailure" | "duplicate" | "rateLimit" | "permission" => "failed",
        "prePush" | "postPush" | "discussion" | "postWithdrawal" => "staleDuringExecution",
        "withdrawal" | "pending" => "ineligible",
        _ => "reapproved",
    };
    assert_eq!(result["items"][0]["status"], expected, "{fault}: {result}");
    assert_eq!(
        provider.writes.get(),
        usize::from(!matches!(
            fault,
            "prePush" | "withdrawal" | "pending" | "discussion"
        ))
    );
    if fault == "permission" {
        assert_eq!(result["state"], "failed");
        publish(root, provider, snapshot).unwrap_err();
    } else {
        publish(root, provider, snapshot).unwrap();
    }
    assert_eq!(
        provider.writes.get(),
        usize::from(!matches!(
            fault,
            "prePush" | "withdrawal" | "pending" | "discussion"
        )),
        "Recovery must never duplicate a write"
    );
    let receipt = &result["items"][0]["publication"];
    let host_path = Path::new(receipt["journalPath"].as_str().unwrap());
    assert!(host_path.is_absolute() && host_path.is_file());
    if fault.is_empty() {
        reject_publication_corruption(host_path, root, provider, snapshot);
    }
    if fault == "postReadFailure" {
        fs::write(root.join("policy.md"), "Updated installed policy.").unwrap();
        provider.fault.set("");
        let run_path = root.join("run.json");
        let pending = fs::read(&run_path).unwrap();
        let recovered = publish(root, provider, snapshot).unwrap();
        assert_eq!(recovered["items"][0]["status"], "reapproved");
        assert_eq!(provider.writes.get(), 1);

        // Retain the terminal host record but replay the last durable run bytes,
        // as when execution stops between the host save and the run save.
        fs::write(&run_path, pending).unwrap();
        let reads = provider.target_reads.get();
        let repaired = publish(root, provider, snapshot).unwrap();
        assert_eq!(repaired["items"][0]["status"], "reapproved");
        assert_eq!(
            repaired["items"][0]["publication"],
            recovered["items"][0]["publication"]
        );
        assert_eq!(
            provider.target_reads.get(),
            reads,
            "Terminal recovery is local only"
        );
        assert_eq!(
            provider.writes.get(),
            1,
            "Receipt recovery must never replay a review"
        );
    }
    if fault == "unknown" {
        let second = root.parent().unwrap().join("second");
        let (snapshot, prepared) = prepare(&second, provider, false);
        assess(&second, provider, &snapshot, &prepared);
        let error = publish(&second, provider, &snapshot).unwrap_err();
        assert_eq!(ss_skills_util::classification(&error), "ambiguousWrite");
        assert_eq!(provider.writes.get(), 1);
    }
}

fn host_journal(directory: &Path) -> PathBuf {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap()
}

/// A failed acknowledgement save must leave a recoverable intent, not a blocked assessment error.
fn recover_failed_journal(root: &Path, provider: &Provider, snapshot: &str) -> Value {
    let directory = root
        .parent()
        .unwrap()
        .join("host-store")
        .join("publications");
    provider.host_directory.replace(Some(directory.clone()));
    let error = publish(root, provider, snapshot).unwrap_err();
    assert_eq!(ss_skills_util::classification(&error), "ambiguousWrite");
    assert_eq!(
        invoke(root, provider, "results", &[]).unwrap()["state"],
        "failed"
    );
    let path = host_journal(&directory);
    fs::remove_dir(&path).unwrap();
    fs::rename(path.with_extension("saved"), &path).unwrap();
    provider.fault.set("");
    let result = publish(root, provider, snapshot).unwrap();
    assert_eq!(provider.writes.get(), 1);
    result
}

/// Recovery validates the effect and readback instead of trusting a terminal status string.
fn reject_publication_corruption(
    host_path: &Path,
    root: &Path,
    provider: &Provider,
    snapshot: &str,
) {
    let bytes = fs::read(host_path).unwrap();
    let record: Value = serde_json::from_slice(&bytes).unwrap();
    for (path, replacement) in [
        ("/operations/0/state", json!("write-intent")),
        ("/operations/0/effect/state", json!("unknown")),
        (
            "/operations/0/effect/acknowledgement/commit_id",
            json!("a".repeat(40)),
        ),
        ("/operations/0/result/reviewId", json!("different-review")),
        (
            "/operations/0/verification/evidence/observation/pullRequest/head/sha",
            json!("a".repeat(40)),
        ),
    ] {
        let mut corrupt = record.clone();
        *corrupt.pointer_mut(path).unwrap() = replacement;
        fs::write(host_path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        publish(root, provider, snapshot).unwrap_err();
        assert_eq!(provider.writes.get(), 1);
    }
    fs::write(host_path, bytes).unwrap();
}

fn force_push_inventory_preserves_every_path_kind_and_rejects_missing_objects() {
    let repository = TempDir::new().unwrap();
    git(repository.path(), &["init", "--quiet"]);
    for (path, text) in [
        ("removed.txt", "Removed on the rewritten source.\n"),
        (
            "moved.txt",
            "An unchanged uniquely identifiable renamed document.\n",
        ),
        ("mode.txt", "An executable mode change.\n"),
        ("link", "A regular file becomes a symlink.\n"),
        ("binary", "An ordinary text becomes binary.\n"),
        ("large", "A small file becomes too large.\n"),
        ("mixed.txt", "An ordinary sentnce.\n"),
    ] {
        fs::write(repository.path().join(path), text).unwrap();
    }
    git(repository.path(), &["add", "."]);
    git(
        repository.path(),
        &["commit", "--quiet", "-m", "Original approved tree"],
    );
    let old = git(repository.path(), &["rev-parse", "HEAD"]);
    // An unrelated root commit models a force push; three-dot comparison cannot represent it.
    git(
        repository.path(),
        &["checkout", "--quiet", "--orphan", "rewritten"],
    );
    fs::remove_file(repository.path().join("removed.txt")).unwrap();
    git(repository.path(), &["mv", "--", "moved.txt", "renamed.txt"]);
    fs::write(repository.path().join("binary"), [0, 0xff]).unwrap();
    fs::write(repository.path().join("large"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    fs::write(repository.path().join("added.txt"), "Added content.\n").unwrap();
    fs::write(
        repository.path().join("mixed.txt"),
        "An ordinary sentence.\n",
    )
    .unwrap();
    git(repository.path(), &["add", "."]);
    git(
        repository.path(),
        &["update-index", "--chmod=+x", "--", "mode.txt"],
    );
    // Index-only entries work on filesystems without symlink privileges and never initialize a submodule.
    let blob = git(repository.path(), &["rev-parse", &format!("{old}:link")]);
    git(
        repository.path(),
        &[
            "update-index",
            "--cacheinfo",
            &format!("120000,{blob},link"),
        ],
    );
    git(
        repository.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{old},submodule"),
        ],
    );
    git(
        repository.path(),
        &["commit", "--quiet", "-m", "Unrelated replacement tree"],
    );
    let new = git(repository.path(), &["rev-parse", "HEAD"]);
    let original_status = git(repository.path(), &["status", "--porcelain=v1"]);
    let scope = TempDir::new().unwrap();
    let root = scope.path().join("force");
    let provider = Provider::new(repository.path(), &old, &new);
    let (snapshot, prepared) = prepare(&root, &provider, true);
    assert_eq!(prepared["changeCount"], 9);
    let page = work(
        &root,
        &provider,
        "assessment",
        &snapshot,
        &["--after".into(), "4".into(), "--limit".into(), "2".into()],
    )
    .unwrap();
    assert_eq!(page["nextOffset"], 6);
    assert_eq!(page["changes"][0], prepared["changes"][4]);
    assert_eq!(page["changes"].as_array().unwrap().len(), 2);
    let state: Value = serde_json::from_slice(&fs::read(root.join("run.json")).unwrap()).unwrap();
    let changes = state["inventory"][0]["assessment"]["inventory"]["changes"]
        .as_array()
        .unwrap();
    for (path, kind, content) in [
        ("removed.txt", "D", "text"),
        ("renamed.txt", "R", "text"),
        ("mode.txt", "M", "text"),
        ("link", "T", "text"),
        ("binary", "M", "binary"),
        ("large", "M", "tooLarge"),
        ("added.txt", "A", "text"),
        ("submodule", "A", "submodule"),
        ("mixed.txt", "M", "text"),
    ] {
        let change = changes
            .iter()
            .find(|change| change["new"]["path"] == path || change["old"]["path"] == path)
            .unwrap();
        assert_eq!(change["kind"], kind);
        let side = if change["new"].is_null() {
            "old"
        } else {
            "new"
        };
        assert_eq!(change[side]["content"]["kind"], content);
        let id = change["id"].as_str().unwrap();
        if matches!(content, "binary" | "tooLarge" | "submodule") {
            work(
                &root,
                &provider,
                "judge",
                &snapshot,
                &[
                    "--change-id".into(),
                    id.into(),
                    "--disposition".into(),
                    "insignificant".into(),
                    "--rationale".into(),
                    "Not permitted.".into(),
                    "--evidence".into(),
                    format!("{id}:{side}"),
                ],
            )
            .unwrap_err();
        }
        work(
            &root,
            &provider,
            "judge",
            &snapshot,
            &[
                "--change-id".into(),
                id.into(),
                "--disposition".into(),
                "significant".into(),
                "--rationale".into(),
                "Unsupported content or structural change requires review.".into(),
                "--evidence".into(),
                format!("{id}:{side}"),
            ],
        )
        .unwrap();
    }
    // A cumulative insignificant judgment cannot erase significant member decisions.
    work(
        &root,
        &provider,
        "judge",
        &snapshot,
        &[
            "--change-id".into(),
            "cumulative".into(),
            "--disposition".into(),
            "insignificant".into(),
            "--rationale".into(),
            "Insufficient cumulative claim.".into(),
            "--evidence".into(),
            "policy".into(),
            "--evidence".into(),
            "discussion".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        work(&root, &provider, "complete", &snapshot, &[]).unwrap()["status"],
        "needsReview"
    );
    assert_eq!(provider.writes.get(), 0);
    assert_eq!(
        git(repository.path(), &["status", "--porcelain=v1"]),
        original_status
    );
    let missing = Provider::new(repository.path(), &"f".repeat(40), &new);
    let (_, result) = prepare(&scope.path().join("missing"), &missing, true);
    assert_eq!(result["items"][0]["status"], "failed");
    assert!(!result["items"][0]["error"].is_null());
    assert_eq!(missing.writes.get(), 0);

    // Structural edits and a behavioral correction are eligible when all present content is readable.
    // Restore only opaque sides; the path, mode, type, addition and removal cases remain.
    for path in ["binary", "large"] {
        fs::write(
            repository.path().join(path),
            git(repository.path(), &["show", &format!("{old}:{path}")]),
        )
        .unwrap();
    }
    git(
        repository.path(),
        &["update-index", "--force-remove", "--", "submodule"],
    );
    fs::write(
        repository.path().join("added.txt"),
        "fn clamp(value: i32) -> i32 { value.max(0) }\n",
    )
    .unwrap();
    git(
        repository.path(),
        &["add", "--", "binary", "large", "added.txt"],
    );
    git(
        repository.path(),
        &["commit", "--quiet", "-m", "Complete readable changes"],
    );
    let readable = git(repository.path(), &["rev-parse", "HEAD"]);
    let root = scope.path().join("readable");
    let provider = Provider::new(repository.path(), &old, &readable);
    let (snapshot, prepared) = prepare(&root, &provider, false);
    assert!(
        prepared["changes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|change| change["contentAvailable"] == true)
    );
    assess(&root, &provider, &snapshot, &prepared);
    assert_eq!(
        publish(&root, &provider, &snapshot).unwrap()["items"][0]["status"],
        "reapproved"
    );
    assert_eq!(provider.writes.get(), 1);
}

fn empty_source_diff_and_corrupted_assessments_never_bypass_discussion_or_coverage() {
    let repository = TempDir::new().unwrap();
    let (old, _) = sources(repository.path());
    git(
        repository.path(),
        &["checkout", "--quiet", "--detach", &old],
    );
    git(
        repository.path(),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Same source tree",
        ],
    );
    let new = git(repository.path(), &["rev-parse", "HEAD"]);
    let scope = TempDir::new().unwrap();
    let root = scope.path().join("empty");
    let provider = Provider::new(repository.path(), &old, &new);
    let (snapshot, prepared) = prepare(&root, &provider, true);
    assert_eq!(prepared["changeCount"], 0);
    work(
        &root,
        &provider,
        "judge",
        &snapshot,
        &[
            "--change-id".into(),
            "cumulative".into(),
            "--disposition".into(),
            "insignificant".into(),
            "--rationale".into(),
            "Exact trees are equal.".into(),
            "--evidence".into(),
            "policy".into(),
            "--evidence".into(),
            "discussion".into(),
        ],
    )
    .unwrap();
    work(&root, &provider, "complete", &snapshot, &[]).unwrap_err();
    assess(&root, &provider, &snapshot, &prepared);
    let path = root.join("run.json");
    let original = fs::read(&path).unwrap();
    let record: Value = serde_json::from_slice(&original).unwrap();
    for (pointer, replacement) in [
        ("/schemaVersion", json!(4)),
        (
            "/inventory/0/assessment/documents",
            json!(["significance-policy", "ss-review-audit"]),
        ),
        ("/inventory/0/assessment/reads", json!({})),
        ("/inventory/0/assessment/cumulative", Value::Null),
        ("/inventory/0/assessment/inventory/complete", json!(false)),
        (
            "/inventory/0/assessment/snapshotDigest",
            json!("f".repeat(64)),
        ),
    ] {
        let mut corrupt = record.clone();
        *corrupt.pointer_mut(pointer).unwrap() = replacement;
        fs::write(&path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        invoke(&root, &provider, "results", &[]).unwrap_err();
    }
    fs::write(path, original).unwrap();
    assert_eq!(
        invoke(&root, &provider, "results", &[]).unwrap()["items"][0]["status"],
        "wouldReapprove"
    );
    assert_eq!(provider.writes.get(), 0);
    let output = Command::new(env!("CARGO_BIN_EXE_ss-skills-util"))
        .args(["autoreapprove", "results", "--state-path"])
        .arg(root.join("run.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["items"][0]["status"], "wouldReapprove");
}
