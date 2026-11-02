//! Ensures presentation names do not prove GitHub account kind or authorship.
#![allow(
    clippy::indexing_slicing,
    reason = "The recorded provider response fixture has fixed keys"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, collect_capture,
};

/// Replays GitHub evidence without provider requests or a real clock.
struct Provider(RefCell<VecDeque<Value>>);
impl Process for Provider {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        Ok(ProcessOutput::new(
            true,
            serde_json::to_vec(&self.0.borrow_mut().pop_front().unwrap()).unwrap(),
            vec![],
        ))
    }
}
impl Http for Provider {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub capture must not send Azure DevOps HTTP")
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

#[test]
fn bot_suffix_without_account_evidence_remains_indeterminate() {
    let fixture: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let mut responses = fixture.get("responses").unwrap().clone();
    let author = responses.pointer_mut("/reviewThreads/data/repository/pullRequest/reviewThreads/nodes/0/comments/nodes/0/author").unwrap();
    author["login"] = json!("copilot-pull-request-reviewer[bot]");
    author["__typename"] = Value::Null;
    author["id"] = Value::Null;
    author["url"] = Value::Null;
    let replies = VecDeque::from([
        responses["pullRequest"].clone(),
        json!([responses["reviews"]]),
        json!([responses["issueComments"]]),
        json!([responses["requestedReviewers"]]),
        responses["reviewThreads"].clone(),
        responses["issueEdits"].clone(),
        responses["reviewEdits"].clone(),
        responses["timeline"].clone(),
        responses["user"].clone(),
        responses["compare"].clone(),
    ]);
    let provider = Provider(RefCell::new(replies));
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let result = collect_capture(&target, None, &provider, &provider, &provider).unwrap();
    let comment = result.pointer("/threads/0/comments/0").unwrap();
    assert_eq!(comment.pointer("/author/kind").unwrap(), "indeterminate");
    assert!(
        comment
            .pointer("/author/evidence")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(comment.get("authorship").unwrap(), "indeterminate");
    assert!(provider.0.borrow().is_empty());
}
