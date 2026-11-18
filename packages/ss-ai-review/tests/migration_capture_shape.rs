//! Compares populated provider captures through the public offline capture API.
#![allow(
    clippy::indexing_slicing,
    reason = "The recorded fixtures have a fixed provider response shape"
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, collect_capture,
};

/// Replays recorded provider responses without launching a process or using a network.
struct Provider {
    github: bool,
    replies: RefCell<VecDeque<Value>>,
}

impl Process for Provider {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        if self.github {
            assert_eq!(program, "gh");
            Ok(ProcessOutput::new(
                true,
                serde_json::to_vec(&self.replies.borrow_mut().pop_front().unwrap()).unwrap(),
                Vec::new(),
            ))
        } else {
            assert_eq!(program, "az");
            Ok(ProcessOutput::new(
                true,
                b"fixture-token".to_vec(),
                Vec::new(),
            ))
        }
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
        assert!(!self.github);
        assert_eq!(method, "GET");
        assert!(body.is_none());
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            serde_json::to_vec(&self.replies.borrow_mut().pop_front().unwrap()).unwrap(),
        ))
    }
}

impl Clock for Provider {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("successful capture must not wait")
    }
}

/// Produces both providers' captures with discussion, reference updates and reviewers.
fn captures() -> (Value, Value) {
    let mut ado: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/azure-devops.json"
    ))
    .unwrap();
    let responses = &mut ado["responses"];
    responses["reviewers"]["value"] = json!([{
        "id":"reviewer","displayName":"Reviewer","uniqueName":"reviewer@example.com",
        "descriptor":"aad.reviewer","vote":10
    }]);
    responses["reviewers"]["count"] = json!(1);
    responses["threads"]["value"][0]["comments"][0]["publishedDate"] = json!("2025-11-05T10:00:00");
    responses["threads"]["value"][0]["comments"][1]["publishedDate"] =
        json!("2025-11-05T10:00:00.1234567+02:00");
    let mut service = responses["threads"]["value"][0]["comments"][0].clone();
    service["id"] = json!(3);
    service["author"]["descriptor"] = json!("aadsp.SERVICE");
    let mut marker = responses["threads"]["value"][0]["comments"][0].clone();
    marker["id"] = json!(4);
    marker["content"] = json!("[Copilot speaking] Updated.");
    responses["threads"]["value"][0]["comments"]
        .as_array_mut()
        .unwrap()
        .extend([service, marker]);
    let mut update = responses["threads"]["value"][0].clone();
    update["id"] = json!(103);
    update["comments"] = json!([]);
    update["properties"] =
        json!({"CodeReviewThreadType":{"$type":"System.String","$value":"RefUpdate"}});
    let mut vote = update.clone();
    vote["id"] = json!(104);
    vote["properties"]["CodeReviewThreadType"]["$value"] = json!("VoteUpdate");
    responses["threads"]["value"]
        .as_array_mut()
        .unwrap()
        .extend([update, vote]);
    responses["threads"]["count"] = json!(3);
    let provider = Provider {
        github: false,
        replies: RefCell::new(
            [
                "connectionData",
                "pullRequest",
                "iterations",
                "reviewers",
                "threads",
            ]
            .into_iter()
            .map(|name| responses[name].clone())
            .collect(),
        ),
    };
    let target = capture_target(&json!({"url":"https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242"})).unwrap();
    let ado_capture = collect_capture(&target, None, &provider, &provider, &provider).unwrap();
    assert!(provider.replies.borrow().is_empty());

    let mut github: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let responses = &mut github["responses"];
    responses["requestedReviewers"]["users"] = json!([responses["pullRequest"]["user"]]);
    responses["timeline"]["data"]["repository"]["pullRequest"]["timelineItems"]["nodes"] = json!([{
        "__typename":"HeadRefForcePushedEvent","id":"E1","actor":null,
        "createdAt":"2026-08-03T13:00:00Z",
        "beforeCommit":{"oid":"a".repeat(40)},"afterCommit":{"oid":"b".repeat(40)}
    }]);
    let provider = Provider {
        github: true,
        replies: RefCell::new(VecDeque::from([
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
        ])),
    };
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let github_capture = collect_capture(&target, None, &provider, &provider, &provider).unwrap();
    assert!(provider.replies.borrow().is_empty());
    (ado_capture, github_capture)
}

/// Returns an object's keys for cross-provider shape comparison.
fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

/// Checks each thread against the shape and vocabulary shared by both providers.
fn check_thread(
    thread: &Value,
    thread_keys: &BTreeSet<&str>,
    context_keys: &BTreeSet<&str>,
    comment_keys: &BTreeSet<&str>,
    author_keys: &BTreeSet<&str>,
    decision_keys: &BTreeSet<&str>,
) {
    assert_eq!(&keys(thread), thread_keys);
    assert!(
        [
            "text",
            "reviewSummary",
            "refUpdate",
            "reviewersUpdated",
            "policy",
            "system"
        ]
        .contains(&thread["kind"].as_str().unwrap())
    );
    assert!(
        ["active", "resolved", "indeterminate"].contains(&thread["resolution"].as_str().unwrap())
    );
    let comments = thread["comments"].as_array().unwrap();
    if !thread["context"].is_null() {
        assert_eq!(&keys(&thread["context"]), context_keys);
        assert!(["left", "right"].contains(&thread["context"]["side"].as_str().unwrap()));
    }
    for comment in comments {
        assert_eq!(&keys(comment), comment_keys);
        assert_eq!(&keys(&comment["author"]), author_keys);
        assert!(
            ["person", "service", "indeterminate"]
                .contains(&comment["author"]["kind"].as_str().unwrap())
        );
        assert!(
            ["agent", "human", "indeterminate"].contains(&comment["authorship"].as_str().unwrap())
        );
    }
    if comments.is_empty() {
        assert!(thread["openingCommentId"].is_null());
    } else {
        assert!(
            comments
                .iter()
                .any(|comment| comment["id"] == thread["openingCommentId"])
        );
    }
    if !thread["decision"].is_null() {
        assert_eq!(&keys(&thread["decision"]), decision_keys);
        assert_eq!(thread["decision"]["type"], "refUpdate");
    }
}

/// Confirms reviewer records omit provider-specific approval votes.
fn check_reviewer(reviewer: &Value, reviewer_keys: &BTreeSet<&str>) {
    assert_eq!(&keys(reviewer), reviewer_keys);
    for name in ["vote", "platformVote", "voteLabel"] {
        assert!(!reviewer.as_object().unwrap().contains_key(name));
    }
}

#[test]
fn populated_provider_captures_share_schema_and_exclude_votes() {
    let (ado, github) = captures();
    let thread_keys = keys(&ado["threads"][0]);
    let comment_keys = keys(&ado["threads"][0]["comments"][0]);
    let author_keys = keys(&ado["threads"][0]["comments"][0]["author"]);
    let context_keys = keys(&ado["threads"][0]["context"]);
    let reviewer_keys = keys(&ado["reviewers"][0]);
    let decision = github["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|thread| !thread["decision"].is_null())
        .unwrap();
    let decision_keys = keys(&decision["decision"]);
    for field in ["target", "pullRequest"] {
        assert_eq!(keys(&ado[field]), keys(&github[field]), "{field}");
    }

    assert_eq!(keys(&ado), keys(&github));
    for capture in [&ado, &github] {
        let threads = capture["threads"].as_array().unwrap();
        let reviewers = capture["reviewers"].as_array().unwrap();
        assert!(!reviewers.is_empty());
        assert!(threads.iter().any(|thread| thread["kind"] == "text"));
        assert!(threads.iter().any(|thread| thread["kind"] == "refUpdate"));
        assert!(
            !capture["pullRequest"]
                .as_object()
                .unwrap()
                .contains_key("state")
        );
        assert!(
            !capture["pullRequest"]
                .as_object()
                .unwrap()
                .contains_key("platformState")
        );
        for reviewer in reviewers {
            check_reviewer(reviewer, &reviewer_keys);
        }
        assert!(
            ["person", "service", "indeterminate"]
                .contains(&capture["authenticatedIdentity"]["kind"].as_str().unwrap())
        );
        for thread in threads {
            check_thread(
                thread,
                &thread_keys,
                &context_keys,
                &comment_keys,
                &author_keys,
                &decision_keys,
            );
        }
    }
    assert!(
        ado["threads"]
            .as_array()
            .unwrap()
            .iter()
            .all(|thread| thread["id"] != 104)
    );
    assert!(
        github["threads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|thread| thread["decision"]["type"] == "refUpdate")
    );
    let ado_thread = &ado["threads"][0];
    let github_thread = &github["threads"][0];
    assert_eq!(keys(ado_thread), keys(github_thread));
    assert_eq!(
        keys(&ado_thread["comments"][0]),
        keys(&github_thread["comments"][0])
    );
    assert_eq!(
        keys(&ado_thread["comments"][0]["author"]),
        keys(&github_thread["comments"][0]["author"])
    );
    assert_eq!(
        keys(&ado_thread["comments"][0]["author"]),
        keys(&ado["authenticatedIdentity"])
    );
    assert_eq!(
        keys(&ado_thread["context"]),
        keys(&github_thread["context"])
    );
    assert_eq!(keys(&ado["reviewers"][0]), keys(&github["reviewers"][0]));
    assert_eq!(
        keys(&ado["pullRequest"]["mergeState"]),
        [
            "status",
            "mergeId",
            "lastMergeSourceCommit",
            "lastMergeTargetCommit",
            "lastMergeCommit"
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        keys(&github["pullRequest"]["mergeState"]),
        ["status", "isMergeable", "mergeCommit"]
            .into_iter()
            .collect()
    );
}

#[test]
fn capture_preserves_comment_body_and_normalizes_provider_timestamps() {
    let (ado, _) = captures();
    let comments = ado["threads"][0]["comments"].as_array().unwrap();
    let first = comments.iter().find(|comment| comment["id"] == 1).unwrap();
    let second = comments.iter().find(|comment| comment["id"] == 2).unwrap();
    assert_eq!(first["publishedUtc"], "2025-11-05T10:00:00.0000000Z");
    assert_eq!(second["publishedUtc"], "2025-11-05T08:00:00.1234567Z");
    assert_eq!(first["content"], "Alpha — “gamma” 🤖 café");
    assert_eq!(second["content"], "2026-08-04T10:07:33.677Z");
    for id in [3, 4] {
        let comment = comments.iter().find(|comment| comment["id"] == id).unwrap();
        assert_eq!(comment["authorship"], "agent");
    }
}
