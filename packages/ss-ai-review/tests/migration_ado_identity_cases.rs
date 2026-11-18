//! Exercises account and authorship evidence through offline Azure DevOps captures.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, collect_capture,
};

/// Replays provider reads without a live account or external requests.
struct Provider(RefCell<VecDeque<Value>>);

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
        _: &str,
        _: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(method, "GET");
        assert!(body.is_none());
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            serde_json::to_vec(&self.0.borrow_mut().pop_front().unwrap()).unwrap(),
        ))
    }
}

impl Clock for Provider {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("complete fixture must not wait")
    }
}

/// Uses real capture normalization while retaining control of account evidence.
fn capture(descriptor: Option<&str>, content: &str, self_descriptor: &str) -> Value {
    let fixture: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/azure-devops.json"
    ))
    .unwrap();
    let mut responses = fixture.get("responses").unwrap().clone();
    *responses
        .pointer_mut("/connectionData/authenticatedUser/subjectDescriptor")
        .unwrap() = json!(self_descriptor);
    let comment = responses
        .pointer_mut("/threads/value/0/comments/0")
        .unwrap();
    *comment.pointer_mut("/author/descriptor").unwrap() = json!(descriptor);
    *comment.get_mut("content").unwrap() = json!(content);
    let provider = Provider(RefCell::new(
        [
            "connectionData",
            "pullRequest",
            "iterations",
            "reviewers",
            "threads",
        ]
        .into_iter()
        .map(|name| responses.get(name).unwrap().clone())
        .collect(),
    ));
    let target = capture_target(&json!({"url":"https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242"})).unwrap();
    let result = collect_capture(&target, None, &provider, &provider, &provider).unwrap();
    assert!(provider.0.borrow().is_empty());
    result
}

#[test]
fn descriptor_kinds_and_evidence_do_not_guess_personal_authorship() {
    for (descriptor, kind, evidence, authorship) in [
        ("aadsp.ABC", "service", "descriptor:aadsp", "agent"),
        ("s2s.DEF", "service", "descriptor:s2s", "agent"),
        ("svc.GHI", "service", "descriptor:svc", "agent"),
        ("aad.ABC", "person", "descriptor:aad", "indeterminate"),
        ("msa.DEF", "person", "descriptor:msa", "indeterminate"),
    ] {
        let result = capture(Some(descriptor), "text", "aad.runner");
        let comment = result.pointer("/threads/0/comments/0").unwrap();
        assert_eq!(comment.pointer("/author/kind").unwrap(), kind);
        assert!(
            comment
                .pointer("/author/evidence")
                .unwrap()
                .as_array()
                .unwrap()
                .contains(&json!(evidence))
        );
        assert_eq!(comment.get("authorship").unwrap(), authorship);
        assert_ne!(comment.get("authorship").unwrap(), "human");
    }
    let result = capture(None, "", "aad.runner");
    assert_eq!(
        result.pointer("/threads/0/comments/0/author/kind").unwrap(),
        "indeterminate"
    );
    assert!(
        result
            .pointer("/threads/0/comments/0/author/evidence")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let result = capture(Some("aad.ABC"), "", "aad.runner");
    assert_eq!(
        result.pointer("/threads/0/comments/0/authorship").unwrap(),
        "indeterminate"
    );
}

#[test]
fn marker_is_case_sensitive_and_must_start_the_comment() {
    for (content, expected) in [
        ("[Copilot speaking] Addressed.", true),
        ("\r\n  [Copilot speaking] Addressed.", true),
        ("I disagree:\n\n> [Copilot speaking] Addressed.", false),
        ("[copilot speaking] addressed.", false),
    ] {
        let result = capture(Some("aad.SELF"), content, "aad.self");
        let comment = result.pointer("/threads/0/comments/0").unwrap();
        assert_eq!(
            comment.get("authorship").unwrap(),
            if expected { "agent" } else { "indeterminate" }
        );
        assert_eq!(
            comment
                .get("authorshipEvidence")
                .unwrap()
                .as_array()
                .unwrap()
                .contains(&json!("content:agentMarker")),
            expected
        );
    }
}
