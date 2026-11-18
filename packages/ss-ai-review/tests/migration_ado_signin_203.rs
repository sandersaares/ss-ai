//! Exercises the Azure DevOps sign-in response through the capture boundary.

use std::cell::Cell;
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

/// Makes one offline provider response available and rejects unexpected requests.
struct SignInProvider {
    requests: Cell<usize>,
    tokens: Cell<usize>,
}

impl Process for SignInProvider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "az");
        assert_eq!(arguments.get(..2).unwrap(), ["account", "get-access-token"]);
        assert!(input.is_empty());
        assert_eq!(self.tokens.replace(1), 0);
        Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]))
    }
}

impl Http for SignInProvider {
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(self.requests.replace(1), 0);
        assert_eq!(method, "GET");
        assert!(url.contains("_apis/connectionData"));
        assert_eq!(token, "fixture-token");
        assert!(body.is_none());
        Ok(HttpResponse::new(
            203,
            BTreeMap::from([("content-type".into(), "application/octet-stream".into())]),
            b"  <!DOCTYPE html><html><body>Sign in</body></html>".to_vec(),
        ))
    }
}

/// Prevents a sign-in response from causing a real or repeated wait.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("authentication refusal must not wait")
    }
}

#[test]
fn non_authoritative_signin_document_is_not_accepted_as_capture_data() {
    let target = capture_target(&json!({
        "url": "https://dev.azure.com/org/project/_git/repo/pullrequest/17"
    }))
    .unwrap();
    let provider = SignInProvider {
        requests: Cell::new(0),
        tokens: Cell::new(0),
    };
    let error = collect_capture(&target, None, &provider, &provider, &NoWait).unwrap_err();
    assert_eq!(classification(&error), "authentication");
    assert!(error.to_string().contains("tenant"));
    assert!(!error.to_string().contains("fixture-token"));
    assert_eq!(provider.requests.get(), 1);
    assert_eq!(provider.tokens.get(), 1);
}
