//! Exercises GitHub capture retry and refusal evidence without launching gh.
#![allow(
    clippy::indexing_slicing,
    reason = "The recorded provider fixture has a fixed response shape"
)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, classification,
    collect_capture,
};

/// Replays GitHub CLI replies and records only injected retry waits.
struct Provider {
    replies: RefCell<VecDeque<ProcessOutput>>,
    requests: Cell<usize>,
    waits: RefCell<Vec<Duration>>,
}

impl Process for Provider {
    fn execute(&self, program: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        assert!(arguments.iter().any(|argument| argument == "--include"));
        self.requests
            .set(self.requests.get().checked_add(1).unwrap());
        Ok(self.replies.borrow_mut().pop_front().unwrap())
    }
}

impl Http for Provider {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub capture must not use the Azure HTTP boundary")
    }
}

impl Clock for Provider {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        self.waits.borrow_mut().push(duration);
        Ok(())
    }
}

fn output(success: bool, text: &str) -> ProcessOutput {
    ProcessOutput::new(success, text.as_bytes().to_vec(), Vec::new())
}

fn capture(provider: &Provider) -> AppError {
    let target = capture_target(&json!({"url":"https://github.com/org/repo/pull/17"})).unwrap();
    collect_capture(&target, None, provider, provider, provider).unwrap_err()
}

#[test]
fn rate_headers_survive_cli_and_use_a_separate_transient_budget() {
    let mut replies = VecDeque::new();
    // More throttles than the transient retry budget expose shared-counter errors.
    for _ in 0..6 {
        replies.push_back(output(
            false,
            "HTTP/2 429\nRetry-After: 240\n\n{\"message\":\"rate limit\"}",
        ));
    }
    replies.push_back(output(
        false,
        "HTTP/2 500\n\n{\"message\":\"server error\"}",
    ));
    replies.push_back(output(true, "HTTP/2 200\n\nnot-json"));
    let provider = Provider {
        replies: RefCell::new(replies),
        requests: Cell::new(0),
        waits: RefCell::new(Vec::new()),
    };
    assert_eq!(classification(&capture(&provider)), "service");
    assert_eq!(provider.requests.get(), 8);
    let waits = provider.waits.borrow();
    assert_eq!(waits.get(..6).unwrap(), [Duration::from_secs(240); 6]);
    assert_eq!(waits.get(6), Some(&Duration::from_secs(1)));
    assert_eq!(waits.len(), 7);
    assert!(provider.replies.borrow().is_empty());
}

#[test]
fn ordinary_forbidden_response_is_not_a_rate_limit_retry() {
    let provider = Provider {
        replies: RefCell::new(VecDeque::from([output(
            false,
            "HTTP/2 403\nX-RateLimit-Remaining: 25\n\n{\"message\":\"Resource not accessible\"}",
        )])),
        requests: Cell::new(0),
        waits: RefCell::new(Vec::new()),
    };
    let error = capture(&provider);
    assert_eq!(classification(&error), "permissionDenied");
    assert!(error.to_string().contains("Resource not accessible"));
    assert_eq!(provider.requests.get(), 1);
    assert!(provider.waits.borrow().is_empty());
}

/// Replays the complete recorded capture after a single scripted refusal.
struct RecordedProvider {
    replies: RefCell<VecDeque<(String, ProcessOutput)>>,
    waits: RefCell<Vec<Duration>>,
}

impl Process for RecordedProvider {
    fn execute(&self, program: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        let (expected, output) = self.replies.borrow_mut().pop_front().unwrap();
        assert_eq!(arguments.get(1).unwrap(), &expected);
        Ok(output)
    }
}

impl Http for RecordedProvider {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub capture must not use the Azure HTTP boundary")
    }
}

impl Clock for RecordedProvider {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        self.waits.borrow_mut().push(duration);
        Ok(())
    }
}

/// Adds scripted first-request failures before a complete recorded capture.
fn recorded_provider(failures: Vec<ProcessOutput>) -> (RecordedProvider, Value) {
    let fixture: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let expected: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-context.json"
    ))
    .unwrap();
    let responses = &fixture["responses"];
    let mut replies: VecDeque<_> = failures
        .into_iter()
        .map(|failure| ("repos/octo/widget/pulls/7".into(), failure))
        .collect();
    for (route, value) in [
        (
            "repos/octo/widget/pulls/7",
            responses["pullRequest"].clone(),
        ),
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
    ] {
        replies.push_back((
            route.into(),
            ProcessOutput::new(true, serde_json::to_vec(&value).unwrap(), vec![]),
        ));
    }
    (
        RecordedProvider {
            replies: RefCell::new(replies),
            waits: RefCell::new(Vec::new()),
        },
        expected,
    )
}

#[test]
fn forbidden_rate_limit_header_recovers_to_the_complete_recorded_capture() {
    let (provider, expected) = recorded_provider(vec![output(
        false,
        "HTTP/2 403\nRetry-After: 240\n\n{\"message\":\"secondary rate limit\"}",
    )]);
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let actual = collect_capture(&target, None, &provider, &provider, &provider).unwrap();
    assert_eq!(actual, expected);
    assert!(provider.replies.borrow().is_empty());
    assert_eq!(*provider.waits.borrow(), [Duration::from_secs(240)]);
}

#[test]
fn multiple_rate_refusals_preserve_transient_retries_through_success() {
    let mut failures = Vec::new();
    // Exceed the transient retry budget before injecting a separate server fault.
    for _ in 0..6 {
        failures.push(output(
            false,
            "HTTP/2 403\nRetry-After: 240\n\n{\"message\":\"secondary rate limit\"}",
        ));
    }
    failures.push(output(
        false,
        "HTTP/2 502\n\n{\"message\":\"server error\"}",
    ));
    let (provider, expected) = recorded_provider(failures);
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let actual = collect_capture(&target, None, &provider, &provider, &provider).unwrap();
    assert_eq!(actual, expected);
    assert!(provider.replies.borrow().is_empty());
    let waits = provider.waits.borrow();
    assert_eq!(waits.get(..6).unwrap(), [Duration::from_secs(240); 6]);
    assert_eq!(waits.get(6), Some(&Duration::from_secs(1)));
    assert_eq!(waits.len(), 7);
}
