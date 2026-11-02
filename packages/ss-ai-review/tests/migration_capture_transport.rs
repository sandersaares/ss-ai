//! Exercises capture transport failures through the public provider boundary.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::json;
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, classification,
    collect_capture,
};

/// Supplies only scripted OAuth and HTTP responses; no provider can be reached.
struct Provider {
    responses: RefCell<VecDeque<HttpResponse>>,
    tokens: Cell<usize>,
    requests: RefCell<Vec<(String, String, String)>>,
}

impl Provider {
    fn new(responses: Vec<HttpResponse>) -> Self {
        Self {
            responses: RefCell::new(responses.into()),
            tokens: Cell::new(0),
            requests: RefCell::new(Vec::new()),
        }
    }
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "az");
        assert_eq!(arguments.get(..2).unwrap(), ["account", "get-access-token"]);
        assert!(input.is_empty());
        let next = self.tokens.get().checked_add(1).unwrap();
        self.tokens.set(next);
        Ok(ProcessOutput::new(
            true,
            format!("fixture-token-{next}").into_bytes(),
            Vec::new(),
        ))
    }
}

impl Http for Provider {
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert!(body.is_none());
        self.requests
            .borrow_mut()
            .push((method.into(), url.into(), token.into()));
        Ok(self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("unexpected provider request"))
    }
}

/// Refuses real waiting so failure tests cannot depend on elapsed time.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("this scenario must not wait")
    }
}

fn capture(provider: &Provider) -> AppError {
    let target = capture_target(&json!({
        "url": "https://dev.azure.com/org/project/_git/repo/pullrequest/17"
    }))
    .unwrap();
    collect_capture(&target, None, provider, provider, &NoWait).unwrap_err()
}

#[test]
fn signin_documents_fail_capture_without_followup_reads() {
    for (headers, body) in [
        (
            BTreeMap::from([("content-type".into(), "text/html; charset=utf-8".into())]),
            b"sign-in".as_slice(),
        ),
        (BTreeMap::new(), b"  <html>sign-in</html>".as_slice()),
    ] {
        let provider = Provider::new(vec![HttpResponse::new(200, headers, body.to_vec())]);
        let error = capture(&provider);
        assert_eq!(classification(&error), "authentication");
        assert_eq!(provider.tokens.get(), 1);
        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 1);
        let request = requests.first().unwrap();
        assert_eq!(request.0, "GET");
        assert!(request.1.contains("_apis/connectionData"));
        assert!(!error.to_string().contains("fixture-token"));
    }
}

#[test]
fn repeated_authentication_refusal_refreshes_once_and_redacts_token() {
    let provider = Provider::new(vec![
        HttpResponse::new(401, BTreeMap::new(), b"expired".to_vec()),
        HttpResponse::new(401, BTreeMap::new(), b"fixture-token-2 rejected".to_vec()),
    ]);
    let error = capture(&provider);
    assert_eq!(classification(&error), "authentication");
    assert!(error.to_string().contains("401"));
    assert!(!error.to_string().contains("fixture-token-2"));
    assert_eq!(provider.tokens.get(), 2);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2);
    let first = requests.first().unwrap();
    let second = requests.get(1).unwrap();
    assert_eq!(first.0, "GET");
    assert_eq!(first.1, second.1);
    assert_eq!(first.2, "fixture-token-1");
    assert_eq!(second.2, "fixture-token-2");
}
