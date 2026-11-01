//! Preserves GitHub discussion when commit comparisons are explicitly absent.

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_slice, from_str, json, to_vec};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput, classification, run};
use ss_ai_review::{capture_target, collect_capture};
use ss_ai_review_context::{canonical_capture_from_evidence, evidence_hash, read_capture};
use tempfile::tempdir_in;

/// Records each offline read and returns scripted comparison failures in order.
struct Provider {
    responses: Value,
    comparisons: RefCell<VecDeque<ProcessOutput>>,
    comparison_routes: RefCell<Vec<String>>,
    seen: RefCell<BTreeSet<String>>,
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        assert_eq!(arguments.first().map(String::as_str), Some("api"));
        let route = arguments.get(1).unwrap();
        if route.starts_with("repos/octo/widget/compare/") {
            assert!(arguments.iter().any(|argument| argument == "--include"));
            self.comparison_routes.borrow_mut().push(route.clone());
            return Ok(self
                .comparisons
                .borrow_mut()
                .pop_front()
                .expect("unexpected comparison"));
        }
        let key = if route == "graphql" {
            let request: Value = from_slice(input).unwrap();
            assert!(request.pointer("/variables/cur").unwrap().is_null());
            let query = request.get("query").and_then(Value::as_str).unwrap();
            if query.contains("reviewThreads(") {
                "reviewThreads"
            } else if query.contains("timelineItems(") {
                "timeline"
            } else if query.contains("reviews(") {
                "reviewEdits"
            } else if query.contains("comments(") {
                "issueEdits"
            } else {
                panic!("unexpected GraphQL query")
            }
        } else {
            assert!(input.is_empty());
            match route.as_str() {
                "repos/octo/widget/pulls/7" => "pullRequest",
                "repos/octo/widget/pulls/7/reviews?per_page=100" => "reviews",
                "repos/octo/widget/issues/7/comments?per_page=100" => "issueComments",
                "repos/octo/widget/pulls/7/requested_reviewers?per_page=100" => {
                    "requestedReviewers"
                }
                "user" => "user",
                _ => panic!("unexpected provider request: {route}"),
            }
        };
        assert!(self.seen.borrow_mut().insert(key.to_owned()));
        let response = self.responses.get(key).unwrap();
        let payload = if ["reviews", "issueComments", "requestedReviewers"].contains(&key) {
            json!([response])
        } else {
            response.clone()
        };
        Ok(ProcessOutput::new(true, to_vec(&payload).unwrap(), vec![]))
    }
}

impl Http for Provider {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub capture must not use Azure DevOps HTTP")
    }
}

/// Rejects retry waits so unexpected failures cannot delay the test.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("scripted comparison must not wait")
    }
}

/// Builds the shared complete fixture with a local or forked head repository.
fn provider(fork: bool, comparisons: Vec<ProcessOutput>) -> Provider {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let mut responses = fixture.get("responses").unwrap().clone();
    if fork {
        *responses.pointer_mut("/pullRequest/head/label").unwrap() = json!("contributor:topic");
        *responses
            .pointer_mut("/pullRequest/head/repo/full_name")
            .unwrap() = json!("contributor/widget");
        *responses
            .pointer_mut("/pullRequest/head/repo/owner/login")
            .unwrap() = json!("contributor");
    }
    Provider {
        responses,
        comparisons: RefCell::new(comparisons.into()),
        comparison_routes: RefCell::new(Vec::new()),
        seen: RefCell::new(BTreeSet::new()),
    }
}

/// Makes HTTP status evidence distinct from error text that only mentions it.
fn failure(status: Option<u16>, diagnostic: &str) -> ProcessOutput {
    let body = json!({"message":diagnostic}).to_string();
    let stdout = status
        .map(|code| format!("HTTP/2 {code} refusal\r\n\r\n{body}"))
        .unwrap_or(body);
    ProcessOutput::new(false, stdout.into_bytes(), diagnostic.as_bytes().to_vec())
}

/// Collects one pull request through injected boundaries.
fn collect(provider: &Provider) -> Result<Value, AppError> {
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    collect_capture(&target, None, provider, provider, &NoWait)
}

#[test]
fn explicit_absence_preserves_canonical_capture_and_discussion() {
    for (fork, responses) in [
        (false, vec![failure(Some(404), "not found")]),
        (
            true,
            vec![
                failure(Some(404), "not found"),
                failure(Some(404), "not found"),
            ],
        ),
    ] {
        let provider = provider(fork, responses);
        let evidence = collect(&provider).unwrap();
        assert!(provider.comparisons.borrow().is_empty());
        assert_eq!(provider.seen.borrow().len(), 9);
        let routes = provider.comparison_routes.borrow();
        assert_eq!(routes.len(), if fork { 2 } else { 1 });
        if fork {
            assert!(routes.first().unwrap().contains("...contributor:"));
            assert_eq!(
                evidence.pointer("/pullRequest/sourceRef").unwrap(),
                "contributor:topic"
            );
        }
        assert!(!routes.last().unwrap().contains("...contributor:"));
        let capture = canonical_capture_from_evidence(&evidence).unwrap();
        assert!(
            capture
                .pointer("/pullRequest/reviewedMergeBaseCommit")
                .unwrap()
                .is_null()
        );
        assert_eq!(
            capture.pointer("/threads/0/comments/0/content").unwrap(),
            "Alpha — “gamma” 🤖 café"
        );
        assert!(!evidence_hash(&capture).unwrap().is_empty());
    }
}

#[test]
fn authentication_permission_and_ambiguous_failures_remain_errors() {
    for (status, diagnostic, expected) in [
        (Some(401), "authentication required", "authentication"),
        (Some(403), "resource forbidden", "permissionDenied"),
        (None, "Not Found (HTTP 404)", "service"),
    ] {
        for after_fallback in [false, true] {
            let responses = if after_fallback {
                vec![failure(Some(404), "not found"), failure(status, diagnostic)]
            } else {
                vec![failure(status, diagnostic)]
            };
            let provider = provider(true, responses);
            let error = collect(&provider).unwrap_err();
            assert_eq!(classification(&error), expected);
            assert!(provider.comparisons.borrow().is_empty());
            assert_eq!(
                provider.comparison_routes.borrow().len(),
                if after_fallback { 2 } else { 1 }
            );
        }
    }
}

#[test]
fn published_capture_and_refresh_accept_an_absent_merge_base() {
    let output = tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let directory = output.path().to_str().unwrap();
    let first = provider(
        true,
        vec![
            failure(Some(404), "not found"),
            failure(Some(404), "not found"),
        ],
    );
    let arguments = [
        "ss-ai",
        "context",
        "create",
        "--url",
        "https://github.com/octo/widget/pull/7",
        "--output-directory",
        directory,
        "--output-prefix",
        "first-",
    ]
    .map(OsString::from);
    let summary: Value = from_str(&run(&arguments, &first, &first, &NoWait).unwrap()).unwrap();
    assert!(summary.get("reviewedMergeBaseCommit").unwrap().is_null());
    let first_path = output.path().join("first-pr-context.json");
    let document = read_capture(&first_path, &NoWait).unwrap();
    assert!(
        document
            .pointer("/pullRequest/reviewedMergeBaseCommit")
            .unwrap()
            .is_null()
    );
    let second = provider(
        true,
        vec![
            failure(Some(404), "not found"),
            failure(Some(404), "not found"),
        ],
    );
    let mut arguments = arguments.to_vec();
    arguments.extend([
        OsString::from("--compare-path"),
        first_path.into_os_string(),
        OsString::from("--output-prefix"),
        OsString::from("second-"),
    ]);
    // Replace the first prefix rather than passing duplicate options.
    let prefix = arguments
        .iter()
        .position(|argument| argument == "--output-prefix")
        .unwrap();
    arguments.drain(prefix..prefix + 2);
    let refreshed: Value = from_str(&run(&arguments, &second, &second, &NoWait).unwrap()).unwrap();
    assert!(refreshed.get("reviewedMergeBaseCommit").unwrap().is_null());
    assert_eq!(
        refreshed
            .pointer("/delta/reviewedMergeBaseCommitChanged")
            .unwrap(),
        false
    );
    assert!(output.path().join("second-pr-context.json").is_file());
}
