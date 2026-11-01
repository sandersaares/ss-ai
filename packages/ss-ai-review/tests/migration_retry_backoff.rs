//! Exercises bounded retry delays through offline provider capture.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::json;
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, classification,
    collect_capture,
};

/// Returns a finite run of refusals and records waits without real time.
struct Provider {
    refusals: Cell<usize>,
    calls: Cell<usize>,
    waits: RefCell<Vec<Duration>>,
}

impl Provider {
    /// Bounds the fake independently of the retry loop under test.
    fn refuses(&self) -> bool {
        let calls = self.calls.get().checked_add(1).unwrap();
        assert!(calls <= self.refusals.get().checked_add(1).unwrap());
        self.calls.set(calls);
        calls <= self.refusals.get()
    }
}

impl Process for Provider {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        if program == "az" {
            return Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]));
        }
        assert_eq!(program, "gh");
        if self.refuses() {
            Ok(ProcessOutput::new(
                false,
                b"HTTP/2 429 refusal\r\n\r\n{\"message\":\"rate limit\"}".to_vec(),
                vec![],
            ))
        } else {
            Ok(ProcessOutput::new(true, b"not-json".to_vec(), vec![]))
        }
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
        assert_eq!(token, "fixture-token");
        assert!(body.is_none());
        if self.refuses() {
            Ok(HttpResponse::new(
                429,
                BTreeMap::new(),
                b"rate limit".to_vec(),
            ))
        } else {
            Ok(HttpResponse::new(
                200,
                BTreeMap::new(),
                b"not-json".to_vec(),
            ))
        }
    }
}

impl Clock for Provider {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        assert!(self.waits.borrow().len() < self.refusals.get());
        self.waits.borrow_mut().push(duration);
        Ok(())
    }
}

#[test]
fn repeated_provider_rate_refusals_use_bounded_exponential_backoff() {
    for (url, first, cap) in [
        (
            "https://github.com/octo/widget/pull/7",
            Duration::from_secs(60),
            Duration::from_secs(120),
        ),
        (
            "https://dev.azure.com/org/project/_git/repo/pullrequest/17",
            Duration::from_millis(500),
            Duration::from_secs(30),
        ),
    ] {
        // Continue well past both caps to exercise repeated capped waits.
        let provider = Provider {
            refusals: Cell::new(20),
            calls: Cell::new(0),
            waits: RefCell::new(Vec::new()),
        };
        let target = capture_target(&json!({"url":url})).unwrap();
        let error = collect_capture(&target, None, &provider, &provider, &provider).unwrap_err();
        assert_eq!(classification(&error), "service");
        let waits = provider.waits.borrow();
        assert_eq!(waits.len(), provider.refusals.get());
        assert_eq!(waits.first(), Some(&first));
        assert_eq!(waits.last(), Some(&cap));
        assert!(waits.iter().all(|wait| *wait <= cap));
        assert!(waits.windows(2).all(|pair| pair.first() <= pair.get(1)));
    }
}
