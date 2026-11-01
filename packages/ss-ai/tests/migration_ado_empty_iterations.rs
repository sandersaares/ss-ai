//! Verifies that an explicitly empty iteration list is canonical capture evidence.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_str, json, to_vec};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput};
use ss_ai_review::{capture_target, collect_capture};
use ss_ai_review_context::{canonical_capture_from_evidence, evidence_hash};

/// Serves the complete fixture while recording every provider read.
struct Provider {
    responses: Value,
    seen: RefCell<BTreeSet<String>>,
}

impl Process for Provider {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "az");
        Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]))
    }
}

impl Http for Provider {
    fn send(
        &self,
        method: &str,
        url: &str,
        _: &str,
        _: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(method, "GET");
        let key = if url.contains("_apis/connectionData?") {
            "connectionData"
        } else if url.contains("/iterations?") {
            "iterations"
        } else if url.contains("/reviewers?") {
            "reviewers"
        } else if url.contains("/threads?") {
            "threads"
        } else if url.contains("/pullRequests/4242?") {
            "pullRequest"
        } else {
            panic!("unexpected provider request: {url}")
        };
        assert!(self.seen.borrow_mut().insert(key.to_owned()));
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            to_vec(self.responses.get(key).unwrap()).unwrap(),
        ))
    }
}

/// Rejects waits so the test cannot rely on real time.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected retry")
    }
}

#[test]
fn empty_iterations_are_valid_canonical_evidence() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/azure-devops.json"
    ))
    .unwrap();
    let mut responses = fixture.get("responses").unwrap().clone();
    *responses.get_mut("iterations").unwrap() = json!({"count":0,"value":[]});
    let provider = Provider {
        responses,
        seen: RefCell::new(BTreeSet::new()),
    };
    let target = capture_target(&json!({
        "url":"https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242"
    })).unwrap();
    let evidence = collect_capture(&target, None, &provider, &provider, &NoWait).unwrap();
    let capture = canonical_capture_from_evidence(&evidence).unwrap();
    assert!(
        capture
            .pointer("/pullRequest/reviewedSourceCommit")
            .unwrap()
            .is_null()
    );
    assert!(
        capture
            .pointer("/pullRequest/reviewedMergeBaseCommit")
            .unwrap()
            .is_null()
    );
    assert!(
        capture
            .get("revisions")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !capture
            .get("threads")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!evidence_hash(&capture).unwrap().is_empty());
    assert_eq!(provider.seen.borrow().len(), 5);
}
