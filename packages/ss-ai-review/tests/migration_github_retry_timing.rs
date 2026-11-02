//! Tests provider retry timing and terminal errors without real time or network access.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::json;
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, classification,
    collect_capture,
};

/// Replays a finite read script and records waits without real time.
struct Provider {
    replies: RefCell<VecDeque<ProcessOutput>>,
    calls: Cell<usize>,
    waits: RefCell<Vec<Duration>>,
}

impl Process for Provider {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        self.calls.set(self.calls.get().checked_add(1).unwrap());
        Ok(self
            .replies
            .borrow_mut()
            .pop_front()
            .expect("unexpected retry"))
    }
}

impl Http for Provider {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub capture must not access Azure DevOps")
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

/// Starts capture until the scripted terminal response refuses the operation.
fn capture(outputs: Vec<ProcessOutput>) -> (AppError, Provider) {
    let provider = Provider {
        replies: RefCell::new(outputs.into()),
        calls: Cell::new(0),
        waits: RefCell::new(Vec::new()),
    };
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let error = collect_capture(&target, None, &provider, &provider, &provider).unwrap_err();
    (error, provider)
}

/// Creates synthetic CLI diagnostics without accessing credentials.
fn failure(text: &str) -> ProcessOutput {
    ProcessOutput::new(false, text.as_bytes().to_vec(), vec![])
}

#[test]
fn reset_headers_and_secondary_limits_use_the_expected_wait() {
    for (headers, message, expected) in [
        (
            "X-RateLimit-Reset: 240\nX-RateLimit-Remaining: 0",
            "API rate limit exceeded",
            240,
        ),
        (
            "X-RateLimit-Reset: 0\nX-RateLimit-Remaining: 0",
            "API rate limit exceeded",
            60,
        ),
        (
            "X-RateLimit-Reset: 240\nX-RateLimit-Remaining: 25",
            "secondary rate limit",
            60,
        ),
        ("Retry-After: 30", "secondary rate limit", 30),
    ] {
        let response = format!("HTTP/2 403\n{headers}\n\n{{\"message\":\"{message}\"}}");
        let (error, provider) = capture(vec![
            failure(&response),
            failure("HTTP/2 400\n\n{\"message\":\"stop\"}"),
        ]);
        assert_eq!(classification(&error), "service");
        assert_eq!(*provider.waits.borrow(), [Duration::from_secs(expected)]);
        assert_eq!(provider.calls.get(), 2);
    }
}

#[test]
fn transient_failures_stop_after_the_bounded_backoff() {
    let replies =
        std::iter::repeat_with(|| failure("HTTP/2 503\n\n{\"message\":\"service unavailable\"}"))
            .take(5)
            .collect();
    let (error, provider) = capture(replies);
    assert_eq!(classification(&error), "service");
    assert_eq!(provider.calls.get(), 5);
    assert_eq!(provider.waits.borrow().len(), 4);
    let (error, provider) = capture(vec![failure("HTTP/2 404\n\n{\"message\":\"Not Found\"}")]);
    assert_eq!(classification(&error), "service");
    assert!(error.to_string().contains("Not Found"));
    assert_eq!(provider.calls.get(), 1);
    assert!(provider.waits.borrow().is_empty());
}

#[test]
fn diagnostics_redact_bearer_credentials() {
    let secret = "gho_abcdefghijklmnopqrstuvwxyz012345";
    let diagnostic = format!("HTTP/2 400\n\nAuthorization: bearer {secret}");
    let (error, provider) = capture(vec![failure(&diagnostic)]);
    assert!(!error.to_string().contains(secret));
    assert!(error.to_string().contains("<redacted>"));
    assert_eq!(provider.calls.get(), 1);
}
