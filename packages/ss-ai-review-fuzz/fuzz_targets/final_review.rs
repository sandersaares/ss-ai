//! Exercises final publication and replay with offline, stateful provider effects.
#![no_main]
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "The generated plans and provider request budget are bounded"
)]

use std::cell::{Cell, RefCell};
use std::fs::remove_file;
use std::io;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use jiff::Timestamp;
use libfuzzer_sys::fuzz_target;
use ohno::AppError;
use serde_json::{Value, from_slice, from_str, json, to_vec};
use ss_ai_review::{
    Clock, FinalReviewInput, Http, HttpResponse, Process, ProcessOutput, publish_final_review,
};
use ss_ai_runtime::FileTransaction;

/// Shares immutable capture data while each case owns its provider state.
static FIXTURES: LazyLock<(Value, Value)> = LazyLock::new(|| {
    (
        from_str(include_str!(
            "../../../tests/fixtures/ss-review-native-capture/github.json"
        ))
        .unwrap(),
        from_str(include_str!(
            "../../../tests/fixtures/ss-review-native-capture/github-expected.json"
        ))
        .unwrap(),
    )
});

/// Models accepted writes separately from the response returned to the caller.
struct Provider<'a> {
    responses: &'a Value,
    fault: u8,
    fault_index: usize,
    requests: Cell<usize>,
    starts: Cell<usize>,
    submissions: Cell<usize>,
    deleted: Cell<bool>,
    findings: RefCell<Vec<String>>,
    summary: RefCell<Option<String>>,
}

impl Provider<'_> {
    /// Bounds all provider activity, including erroneous retries and pagination.
    fn request(&self) {
        let count = self.requests.get() + 1;
        // An arbitrary workload guard with headroom for capture, writes, and
        // readback of the bounded plan; this is not a provider protocol limit.
        assert!(count <= 48, "provider request budget exhausted");
        self.requests.set(count);
    }

    /// Loses a write response after the fake provider has accepted the write.
    fn accepted(&self, stage: u8) -> io::Result<()> {
        if self.fault == stage {
            Err(io::Error::other("accepted write response lost"))
        } else {
            Ok(())
        }
    }

    /// Supplies finite readback of every accepted finding in publication order.
    fn review(&self) -> Value {
        let summary = self.summary.borrow();
        let findings = self.findings.borrow();
        let submitted = summary.is_some();
        let comments: Vec<_> = findings
            .iter()
            .enumerate()
            .map(|(index, body)| {
                json!({"id":format!("C{index}"),"state":if submitted {"SUBMITTED"} else {"PENDING"},
                "path":format!("src/{index}.rs"),"line":null,"startLine":null,
                "subjectType":"FILE","body":body})
            })
            .collect();
        json!({"data":{"node":{"id":"review","state":if submitted {"COMMENTED"} else {"PENDING"},
            "url":"https://github.com/octo/widget/pull/7#review",
            "body":summary.as_deref().unwrap_or(""),
            "comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":comments}}}})
    }

    /// Reports thread membership for the review-scoped comments.
    fn threads(&self) -> Value {
        let nodes: Vec<_> = self
            .findings
            .borrow()
            .iter()
            .enumerate()
            .map(|(index, _)| {
                json!({"id":format!("T{index}"),"diffSide":null,"subjectType":"FILE",
                "comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},
                    "nodes":[{"id":format!("C{index}")}]}})
            })
            .collect();
        json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
            "pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":nodes}}}}})
    }

    /// Implements only the routes used by this publication; all others fail.
    fn response(&self, route: &str, input: &[u8]) -> io::Result<Value> {
        let responses = self.responses;
        if route == "graphql" {
            let request: Value = from_slice(input).unwrap();
            let query = request["query"].as_str().unwrap();
            let variables = &request["variables"];
            if query.contains("states:[PENDING]") {
                return Ok(json!({"data":{"repository":{"pullRequest":{"reviews":{"nodes":[]}}}}}));
            }
            if query.contains("addPullRequestReviewThread(") {
                let index = self.findings.borrow().len();
                assert_eq!(variables["path"], format!("src/{index}.rs"));
                self.findings
                    .borrow_mut()
                    .push(variables["body"].as_str().unwrap().to_owned());
                if self.fault == 2 && index == self.fault_index {
                    self.accepted(2)?;
                }
                return Ok(json!({"data":{"addPullRequestReviewThread":{"thread":{
                    "id":format!("T{index}"),"comments":{"nodes":[{"id":format!("C{index}")}]}}}}}));
            }
            if query.contains("addPullRequestReview(") {
                assert_eq!(self.starts.replace(self.starts.get() + 1), 0);
                self.accepted(1)?;
                return Ok(json!({"data":{"addPullRequestReview":{"pullRequestReview":{
                    "id":"review","state":"PENDING"}}}}));
            }
            if query.contains("submitPullRequestReview(") {
                assert_eq!(self.submissions.replace(self.submissions.get() + 1), 0);
                assert_eq!(variables["event"], "COMMENT");
                *self.summary.borrow_mut() = Some(variables["body"].as_str().unwrap().to_owned());
                self.accepted(4)?;
                return Ok(
                    json!({"data":{"submitPullRequestReview":{"pullRequestReview":{
                    "id":"review","state":"COMMENTED",
                    "url":"https://github.com/octo/widget/pull/7#review"}}}}),
                );
            }
            if query.contains("deletePullRequestReview(") {
                assert!(!self.deleted.replace(true));
                return Ok(
                    json!({"data":{"deletePullRequestReview":{"pullRequestReview":{"id":"review"}}}}),
                );
            }
            if query.contains("node(id:$reviewId)") {
                let submitted = self.summary.borrow().is_some();
                if (self.fault == 3 && !submitted) || (self.fault == 5 && submitted) {
                    return Ok(json!({"data":{"node":null}}));
                }
                return Ok(self.review());
            }
            if query.contains("reviewThreads(") && self.starts.get() > 0 {
                return Ok(self.threads());
            }
            return Ok(if query.contains("reviewThreads(") {
                responses["reviewThreads"].clone()
            } else if query.contains("timelineItems(") {
                responses["timeline"].clone()
            } else if query.contains("reviews(") {
                responses["reviewEdits"].clone()
            } else if query.contains("comments(") {
                responses["issueEdits"].clone()
            } else {
                panic!("unexpected GraphQL request: {query}")
            });
        }
        Ok(match route {
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
            _ => panic!("unexpected route: {route}"),
        })
    }
}

impl Process for Provider<'_> {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        self.request();
        assert_eq!(program, "gh");
        assert_eq!(arguments[0], "api");
        let response = self.response(&arguments[1], input)?;
        Ok(ProcessOutput::new(true, to_vec(&response).unwrap(), vec![]))
    }
}

impl Http for Provider<'_> {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub publication must not use HTTP")
    }
}

/// Refuses retries instead of consulting or waiting on a real clock.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2026-08-04T12:00:00Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected wait")
    }
}

/// Runs one transaction and checks that its durable result blocks replay.
fn exercise(bytes: &[u8]) {
    // An arbitrary small workload that still exercises ordered findings and
    // failures at different positions in a publication.
    let count = usize::from(bytes[0] % 3) + 1;
    let fault = bytes[1] % 6;
    let fault_index = usize::from(bytes[2]) % count;
    let (fixture, expected) = &*FIXTURES;
    let provider = Provider {
        responses: &fixture["responses"],
        fault,
        fault_index,
        requests: Cell::new(0),
        starts: Cell::new(0),
        submissions: Cell::new(0),
        deleted: Cell::new(false),
        findings: RefCell::new(Vec::new()),
        summary: RefCell::new(None),
    };
    let plan: Vec<_> = (0..count)
        .map(|index| {
            json!({"operation":"newFileThread","findingId":format!("F{index}"),
            "body":format!("Finding {index} λ"),"anchor":{"path":format!("src/{index}.rs")},
            "wasDowngraded":false,"requiresReopen":false})
        })
        .collect();
    let input = FinalReviewInput {
        target: &fixture["target"],
        plan: &plan,
        summary_body: "Summary λ",
        expected_evidence_hash: "capture",
        expected_identity: &expected["authenticatedIdentity"],
        source_commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        merge_base_commit: "cccccccccccccccccccccccccccccccccccccccc",
        discard_pending_review: false,
        publish_closed: false,
        approve_merge: false,
        tenant: None,
    };
    // The build directory already exists; the process-specific name avoids worker collisions.
    // The sibling lock is retained for other owners, including after this process exits.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!("final-review-fuzz-{}.json", std::process::id()));
    let mut journal = FileTransaction::open(&path, &FixedClock).unwrap();
    match remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => panic!("cannot reset journal: {error}"),
    }
    let run = || Ok("capture".to_owned());
    let result = publish_final_review(
        &mut journal,
        &path,
        &input,
        &provider,
        &provider,
        &FixedClock,
        |_| run(),
    );
    if fault == 0 {
        assert_eq!(result.unwrap()["status"], "succeeded");
        assert_eq!(provider.findings.borrow().len(), count);
        assert_eq!(provider.submissions.get(), 1);
    } else {
        assert!(result.is_err(), "fault {fault} was not exercised");
    }
    assert_eq!(provider.starts.get(), 1);
    assert!(provider.submissions.get() <= 1);
    let requests = provider.requests.get();
    publish_final_review(
        &mut journal,
        &path,
        &input,
        &provider,
        &provider,
        &FixedClock,
        |_| run(),
    )
    .unwrap_err();
    assert_eq!(
        provider.requests.get(),
        requests,
        "replay contacted provider"
    );
    remove_file(&path).unwrap();
}

fuzz_target!(|input: &[u8]| {
    // A small fixed prefix selects a bounded plan and one publication failure.
    // The upper length is an arbitrary input-size cap; trailing bytes are ignored.
    if input.len() < 3 || input.len() > 64 {
        return;
    }
    exercise(input);
});
