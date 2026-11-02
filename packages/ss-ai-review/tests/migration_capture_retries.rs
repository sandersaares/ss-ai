//! Exercises capture retry evidence without provider access or real waiting.
#![allow(
    clippy::indexing_slicing,
    reason = "The recorded provider fixture has a fixed response shape"
)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, classification,
    collect_capture,
};

/// Scripts refusals and records retries across the public capture boundary.
struct Provider {
    responses: RefCell<VecDeque<HttpResponse>>,
    requests: Cell<usize>,
    tokens: Cell<usize>,
    request_tokens: RefCell<Vec<String>>,
    waits: RefCell<Vec<Duration>>,
    interrupt_wait: bool,
}

impl Process for Provider {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "az");
        let next = self.tokens.get().checked_add(1).unwrap();
        self.tokens.set(next);
        Ok(ProcessOutput::new(
            true,
            format!("fixture-token-{next}").into_bytes(),
            vec![],
        ))
    }
}

impl Http for Provider {
    fn send(
        &self,
        method: &str,
        _: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(method, "GET");
        assert!(body.is_none());
        self.requests
            .set(self.requests.get().checked_add(1).unwrap());
        self.request_tokens.borrow_mut().push(token.to_owned());
        Ok(self.responses.borrow_mut().pop_front().unwrap())
    }
}

impl Clock for Provider {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        self.waits.borrow_mut().push(duration);
        if self.interrupt_wait {
            return Err(io::Error::other("interrupted test wait").into());
        }
        Ok(())
    }
}

fn response(status: u16, retry_after: Option<&str>, body: &[u8]) -> HttpResponse {
    let mut headers = BTreeMap::new();
    if let Some(value) = retry_after {
        headers.insert("retry-after".into(), value.into());
    }
    HttpResponse::new(status, headers, body.to_vec())
}

fn capture(provider: &Provider) -> AppError {
    let target = capture_target(&json!({
        "url":"https://dev.azure.com/org/project/_git/repo/pullrequest/17"
    }))
    .unwrap();
    collect_capture(&target, None, provider, provider, provider).unwrap_err()
}

#[test]
fn rate_refusals_do_not_consume_transient_or_authentication_recovery() {
    let mut responses = VecDeque::new();
    // Exceed the transient retry budget to prove throttling uses its own budget.
    for _ in 0..6 {
        responses.push_back(response(429, Some("240"), b"rate limit"));
    }
    responses.push_back(response(500, None, b"server error"));
    responses.push_back(response(401, None, b"expired"));
    responses.push_back(response(200, None, b"not-json"));
    let provider = Provider {
        responses: RefCell::new(responses),
        requests: Cell::new(0),
        tokens: Cell::new(0),
        request_tokens: RefCell::new(Vec::new()),
        waits: RefCell::new(Vec::new()),
        interrupt_wait: false,
    };
    let error = capture(&provider);
    assert_eq!(classification(&error), "service");
    assert_eq!(provider.requests.get(), 9);
    assert_eq!(provider.tokens.get(), 2);
    let waits = provider.waits.borrow();
    assert_eq!(waits.get(..6).unwrap(), [Duration::from_secs(240); 6]);
    assert_eq!(waits.len(), 7);
    assert!(provider.responses.borrow().is_empty());
}

#[test]
fn interrupted_rate_wait_stops_before_another_request() {
    let provider = Provider {
        responses: RefCell::new(VecDeque::from([response(429, Some("3"), b"rate limit")])),
        requests: Cell::new(0),
        tokens: Cell::new(0),
        request_tokens: RefCell::new(Vec::new()),
        waits: RefCell::new(Vec::new()),
        interrupt_wait: true,
    };
    let error = capture(&provider);
    assert!(error.to_string().contains("interrupted test wait"));
    assert_eq!(provider.requests.get(), 1);
    assert_eq!(provider.tokens.get(), 1);
    assert_eq!(*provider.waits.borrow(), [Duration::from_secs(3)]);
}

#[test]
fn throttled_read_and_token_refresh_recover_to_complete_capture() {
    let fixture: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/azure-devops.json"
    ))
    .unwrap();
    let expected: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/azure-devops-context.json"
    ))
    .unwrap();
    let responses = &fixture["responses"];
    let mut replies = VecDeque::from([
        response(429, Some("240"), b"rate limit"),
        response(401, None, b"expired"),
    ]);
    for key in [
        "connectionData",
        "pullRequest",
        "iterations",
        "reviewers",
        "threads",
    ] {
        replies.push_back(response(
            200,
            None,
            &serde_json::to_vec(&responses[key]).unwrap(),
        ));
    }
    let provider = Provider {
        responses: RefCell::new(replies),
        requests: Cell::new(0),
        tokens: Cell::new(0),
        request_tokens: RefCell::new(Vec::new()),
        waits: RefCell::new(Vec::new()),
        interrupt_wait: false,
    };
    let target = capture_target(&json!({
        "url":"https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242"
    })).unwrap();
    let actual = collect_capture(&target, None, &provider, &provider, &provider).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(provider.requests.get(), 7);
    assert_eq!(provider.tokens.get(), 2);
    assert_eq!(
        *provider.request_tokens.borrow(),
        [
            "fixture-token-1",
            "fixture-token-1",
            "fixture-token-2",
            "fixture-token-2",
            "fixture-token-2",
            "fixture-token-2",
            "fixture-token-2",
        ]
    );
    assert_eq!(*provider.waits.borrow(), [Duration::from_secs(240)]);
    assert!(provider.responses.borrow().is_empty());
}
