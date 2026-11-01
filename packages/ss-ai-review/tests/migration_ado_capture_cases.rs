//! Exercises Azure DevOps capture selection through offline provider reads.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_str, json, to_vec};
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, classification,
    collect_capture,
};

/// Routes the recorded reads once each and forbids network access.
struct Provider {
    responses: Value,
    seen: RefCell<BTreeSet<String>>,
    token_calls: Cell<usize>,
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
        self.token_calls
            .set(self.token_calls.get().checked_add(1).unwrap());
        Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]))
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
        assert_eq!(method, "GET");
        assert_eq!(token, "fixture-token");
        assert!(body.is_none());
        let key = if url.contains("_apis/git/pullrequests/4242?") {
            assert_eq!(
                url,
                "https://example.visualstudio.com/_apis/git/pullrequests/4242?api-version=7.1"
            );
            "resolve"
        } else if url.contains("_apis/connectionData?") {
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

/// Rejects waits so malformed evidence fails without elapsed time.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("recorded capture must not wait")
    }
}

/// Builds independent fixture state for each capture.
fn provider() -> Provider {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/azure-devops.json"
    ))
    .unwrap();
    Provider {
        responses: fixture.get("responses").unwrap().clone(),
        seen: RefCell::new(BTreeSet::new()),
        token_calls: Cell::new(0),
    }
}

/// Captures through injected boundaries without resolving a live account.
fn capture(provider: &Provider) -> Result<Value, AppError> {
    let target = capture_target(&json!({
        "url":"https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242"
    }))
    .unwrap();
    collect_capture(&target, None, provider, provider, &NoWait)
}

#[test]
fn malformed_list_envelopes_fail_but_empty_and_single_collections_survive() {
    for (endpoint, payload) in [
        ("threads", json!({})),
        ("threads", json!({"count":12,"value":[]})),
        ("threads", json!({"count":"not a number","value":[]})),
        ("iterations", json!({})),
        ("connectionData", json!(true)),
    ] {
        let mut provider = provider();
        *provider.responses.get_mut(endpoint).unwrap() = payload;
        assert_eq!(classification(&capture(&provider).unwrap_err()), "service");
    }
    for threads in [
        json!({"value":[]}),
        json!({"count":0,"value":[]}),
        json!({"count":1,"value":[{"id":301,"comments":[
            {"id":1,"author":{"id":"a"},"content":"Only comment."}
        ]}]}),
    ] {
        let mut provider = provider();
        let count = threads.get("value").unwrap().as_array().unwrap().len();
        *provider.responses.get_mut("threads").unwrap() = threads;
        let result = capture(&provider).unwrap();
        assert_eq!(
            result.get("threads").unwrap().as_array().unwrap().len(),
            count
        );
        if count == 1 {
            assert_eq!(result.pointer("/threads/0/id").unwrap(), 301);
            assert_eq!(
                result.pointer("/threads/0/comments/0/content").unwrap(),
                "Only comment."
            );
        }
        assert_eq!(provider.seen.borrow().len(), 5);
    }
}

#[test]
fn empty_iterations_preserve_null_commits_and_discussion() {
    let mut provider = provider();
    *provider.responses.get_mut("iterations").unwrap() = json!({"count":0,"value":[]});
    let result = capture(&provider).unwrap();
    assert!(
        result
            .pointer("/pullRequest/reviewedSourceCommit")
            .unwrap()
            .is_null()
    );
    assert!(
        result
            .pointer("/pullRequest/reviewedMergeBaseCommit")
            .unwrap()
            .is_null()
    );
    assert!(
        result
            .get("revisions")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !result
            .get("threads")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(result.pointer("/threads/0/isOutdated").unwrap(), false);
    assert_eq!(provider.seen.borrow().len(), 5);
    assert_eq!(provider.token_calls.get(), 1);

    *provider.responses.get_mut("threads").unwrap() = json!({"count":0,"value":[]});
    *provider.responses.get_mut("reviewers").unwrap() = json!({"count":0,"value":[]});
    provider.seen.borrow_mut().clear();
    let result = capture(&provider).unwrap();
    assert!(
        result
            .get("threads")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        result
            .get("reviewers")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// Returns explicit provider refusals only for the iteration collection.
struct IterationRefusal {
    provider: Provider,
    status: u16,
    calls: Cell<usize>,
}

impl Process for IterationRefusal {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        self.provider.execute(program, arguments, input)
    }
}

impl Http for IterationRefusal {
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        if url.contains("/iterations?") {
            let calls = self.calls.get().checked_add(1).unwrap();
            // A bounded fake makes a mutated retry loop fail immediately.
            assert!(calls <= 5);
            self.calls.set(calls);
            // Sentinels inject transport errors that cannot be represented as HTTP statuses.
            if self.status == 0 {
                return Err(io::Error::other("connection lost").into());
            }
            if self.status == 2 && calls > 1 {
                return self.provider.send(method, url, token, body);
            }
            if self.status == 1 || self.status == 2 {
                // A failed request build yields a reqwest error without making a network request.
                let error = reqwest::blocking::Client::new()
                    .get("http://[")
                    .build()
                    .unwrap_err();
                return Err(error.into());
            }
            return Ok(HttpResponse::new(
                self.status,
                BTreeMap::from([("x-private".to_owned(), "header-secret".to_owned())]),
                b"refused fixture-token".to_vec(),
            ));
        }
        self.provider.send(method, url, token, body)
    }
}

/// Advances retry logic without consulting or waiting for the real clock.
struct InstantRetry(RefCell<Vec<Duration>>);

impl Clock for InstantRetry {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        self.0.borrow_mut().push(duration);
        Ok(())
    }
}

#[test]
fn refused_iteration_reads_do_not_become_empty_iterations() {
    for (status, expected, calls) in [
        (401, "authentication", 2),
        (403, "permissionDenied", 1),
        (503, "service", 5),
        (0, "service", 1),
        (1, "service", 5),
    ] {
        let provider = IterationRefusal {
            provider: provider(),
            status,
            calls: Cell::new(0),
        };
        let target = capture_target(&json!({
            "url":"https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242"
        })).unwrap();
        let clock = InstantRetry(RefCell::new(Vec::new()));
        let error = collect_capture(&target, None, &provider, &provider, &clock).unwrap_err();
        assert_eq!(classification(&error), expected);
        assert_eq!(provider.calls.get(), calls);
        assert!(!error.to_string().contains("fixture-token"));
        assert!(!error.to_string().contains("header-secret"));
        if status == 503 || status == 1 {
            assert_eq!(
                *clock.0.borrow(),
                [500, 1000, 2000, 4000].map(Duration::from_millis)
            );
        } else {
            assert!(clock.0.borrow().is_empty());
        }
    }
}

#[test]
fn transient_transport_error_recovers_to_complete_capture() {
    let provider = IterationRefusal {
        provider: provider(),
        status: 2,
        calls: Cell::new(0),
    };
    let target = capture_target(&json!({
        "url":"https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242"
    }))
    .unwrap();
    let clock = InstantRetry(RefCell::new(Vec::new()));
    let result = collect_capture(&target, None, &provider, &provider, &clock).unwrap();
    assert_eq!(provider.calls.get(), 2);
    assert_eq!(*clock.0.borrow(), [Duration::from_millis(500)]);
    assert_eq!(provider.provider.token_calls.get(), 1);
    assert_eq!(
        result.pointer("/threads/0/comments/0/content").unwrap(),
        "Alpha — “gamma” 🤖 café"
    );
}

#[test]
fn deleted_and_vote_events_do_not_replace_actionable_discussion() {
    let mut provider = provider();
    let threads = provider
        .responses
        .pointer_mut("/threads/value")
        .unwrap()
        .as_array_mut()
        .unwrap();
    let thread = threads.first_mut().unwrap();
    thread
        .get_mut("comments")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(json!({"id":3,"isDeleted":true,"author":{"id":"a"},"content":"Withdrawn."}));
    threads.push(json!({"id":102,"isDeleted":true}));
    threads.push(json!({"id":103,"properties":{"CodeReviewThreadType":"VoteUpdate"}}));
    *provider.responses.pointer_mut("/threads/count").unwrap() = json!(3);
    let result = capture(&provider).unwrap();
    let threads = result.get("threads").unwrap().as_array().unwrap();
    assert_eq!(threads.len(), 1);
    let thread = threads.first().unwrap();
    assert_eq!(thread.get("id").unwrap(), 101);
    assert!(thread.get("isDeleted").is_none());
    let comments = thread.get("comments").unwrap().as_array().unwrap();
    assert_eq!(comments.len(), 2);
    assert!(
        comments
            .iter()
            .all(|comment| comment.get("isDeleted").is_none())
    );
    assert_eq!(
        comments.first().unwrap().get("content").unwrap(),
        "Alpha — “gamma” 🤖 café"
    );
    assert_eq!(
        comments.get(1).unwrap().get("content").unwrap(),
        "2026-08-04T10:07:33.677Z"
    );
}

#[test]
fn thread_status_kinds_and_wrapped_properties_preserve_evidence() {
    for (status, resolution) in [
        ("active", "active"),
        ("pending", "active"),
        ("fixed", "resolved"),
        ("closed", "resolved"),
        ("byDesign", "resolved"),
        ("wontFix", "resolved"),
        ("unknown", "indeterminate"),
    ] {
        let mut provider = provider();
        *provider
            .responses
            .pointer_mut("/threads/value/0/status")
            .unwrap() = json!(status);
        let result = capture(&provider).unwrap();
        assert_eq!(result.pointer("/threads/0/rawStatus").unwrap(), status);
        assert_eq!(result.pointer("/threads/0/resolution").unwrap(), resolution);
    }

    let mut absent_status = provider();
    absent_status
        .responses
        .pointer_mut("/threads/value/0")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("status");
    let result = capture(&absent_status).unwrap();
    assert!(result.pointer("/threads/0/rawStatus").unwrap().is_null());
    assert_eq!(
        result.pointer("/threads/0/resolution").unwrap(),
        "indeterminate"
    );
    for (thread_type, kind) in [
        ("RefUpdate", "refUpdate"),
        ("ReviewersUpdate", "reviewersUpdated"),
        ("PolicyStatusUpdate", "policy"),
        ("SomeFutureUpdate", "system"),
    ] {
        let mut provider = provider();
        *provider
            .responses
            .pointer_mut("/threads/value/0")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .entry("properties")
            .or_insert(Value::Null) = json!({
        "CodeReviewThreadType":{"$type":"String","$value":thread_type},
            "SupportsMarkdown":{"$type":"Int32","$value":1},
            "Microsoft.TeamFoundation.Discussion.UniqueID":{"$type":"String","$value":"example-id"},
            "LLMModel":{"$type":"String","$value":"example-model"}
        });
        let result = capture(&provider).unwrap();
        assert_eq!(result.pointer("/threads/0/kind").unwrap(), kind);
        assert_eq!(
            result
                .pointer("/threads/0/properties/SupportsMarkdown")
                .unwrap(),
            1
        );
        assert_eq!(
            result
                .pointer("/threads/0/properties/CodeReviewThreadType")
                .unwrap(),
            thread_type
        );
        assert_eq!(
            result
                .pointer("/threads/0/properties/Microsoft.TeamFoundation.Discussion.UniqueID")
                .unwrap(),
            "example-id"
        );
        assert_eq!(
            result.pointer("/threads/0/properties/LLMModel").unwrap(),
            "example-model"
        );
    }
}

#[test]
fn automated_thread_markers_apply_only_to_the_structural_opener() {
    let mut provider = provider();
    let thread = provider
        .responses
        .pointer_mut("/threads/value/0")
        .unwrap()
        .as_object_mut()
        .unwrap();
    thread.insert(
        "properties".into(),
        json!({"LLMModel":{"$type":"String","$value":"model"},"Focus_Area":{"$type":"String","$value":"quality"}}),
    );
    thread.insert("comments".into(), json!([
        {"id":2,"author":{"id":"b","displayName":"Real Person","descriptor":"aad.person"},"content":"reply"},
        {"id":1,"author":{"id":"a","displayName":"Automation","descriptor":"aad.person"},"content":"finding"}
    ]));
    let result = capture(&provider).unwrap();
    assert_eq!(result.pointer("/threads/0/openingCommentId").unwrap(), 1);
    for (index, expected) in [(0, "indeterminate"), (1, "agent")] {
        let pointer = format!("/threads/0/comments/{index}");
        let comment = result.pointer(&pointer).unwrap();
        assert_eq!(
            comment.pointer("/author/displayName").unwrap(),
            if expected == "agent" {
                "Automation"
            } else {
                "Real Person"
            }
        );
        assert_eq!(comment.get("authorship").unwrap(), expected);
        for marker in ["property:LLMModel", "property:Focus_Area"] {
            assert_eq!(
                comment
                    .get("authorshipEvidence")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .contains(&json!(marker)),
                expected == "agent"
            );
        }
    }
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "The complete capture contract covers independent provider fields"
)]
fn complete_capture_preserves_revision_reviewer_and_thread_evidence() {
    let mut provider = provider();
    let iterations = provider
        .responses
        .pointer_mut("/iterations/value")
        .unwrap()
        .as_array_mut()
        .unwrap();
    let mut first = iterations.first().unwrap().clone();
    *first.get_mut("id").unwrap() = json!(1);
    *first.get_mut("reason").unwrap() = Value::Null;
    *first.pointer_mut("/commonRefCommit/commitId").unwrap() = json!("base-one");
    iterations.push(first);
    *provider.responses.pointer_mut("/iterations/count").unwrap() = json!(2);
    *provider.responses.get_mut("reviewers").unwrap() = json!({"count":2,"value":[
        {"id":"reviewer","isRequired":true,"isContainer":false,"vote":10},
        {"id":"team","vote":-10}
    ]});
    let thread = provider.responses.pointer_mut("/threads/value/0").unwrap();
    thread
        .pointer_mut("/comments/1")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("commentType");
    *thread
        .pointer_mut("/comments/1")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .entry("lastContentUpdatedDate")
        .or_insert(Value::Null) = json!("2026-08-03T12:20:00Z");
    *thread
        .pointer_mut("/threadContext")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .entry("leftFileStart")
        .or_insert(Value::Null) = json!({"line":2});
    *thread
        .pointer_mut("/threadContext")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .entry("leftFileEnd")
        .or_insert(Value::Null) = json!({"line":3});
    *thread
        .as_object_mut()
        .unwrap()
        .entry("pullRequestThreadContext")
        .or_insert(Value::Null) =
        json!({"changeTrackingId":14,"iterationContext":{"secondComparingIteration":1}});
    provider
        .responses
        .pointer_mut("/threads/value")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(json!({"id":104,"comments":[],"pullRequestThreadContext":{"changeTrackingId":21}}));
    *provider.responses.pointer_mut("/threads/count").unwrap() = json!(2);
    let result = capture(&provider).unwrap();
    assert_eq!(
        result.pointer("/target/project").unwrap(),
        "Example Project"
    );
    assert_eq!(
        result.pointer("/target/webUrl").unwrap(),
        "https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242"
    );
    assert_eq!(
        result.pointer("/authenticatedIdentity/descriptor").unwrap(),
        "aad.runner"
    );
    assert_eq!(
        result.pointer("/authenticatedIdentity/uniqueName").unwrap(),
        "runner@example.com"
    );
    assert_eq!(result.pointer("/pullRequest/id").unwrap(), 4242);
    assert_eq!(
        result.pointer("/pullRequest/title").unwrap(),
        "Unicode capture"
    );
    assert_eq!(
        result.pointer("/pullRequest/description").unwrap(),
        "Review café — “design” 🤖"
    );
    assert_eq!(result.pointer("/pullRequest/isDraft").unwrap(), false);
    assert_eq!(
        result.pointer("/pullRequest/author/kind").unwrap(),
        "person"
    );
    assert_eq!(
        result.pointer("/pullRequest/createdUtc").unwrap(),
        "2026-08-03T11:44:10.0000000Z"
    );
    assert_eq!(
        result.pointer("/pullRequest/sourceRef").unwrap(),
        "refs/heads/topic"
    );
    assert_eq!(
        result.pointer("/pullRequest/targetRef").unwrap(),
        "refs/heads/main"
    );
    assert_eq!(
        result.pointer("/pullRequest/reviewedSourceCommit").unwrap(),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert_eq!(
        result
            .pointer("/pullRequest/reviewedMergeBaseCommit")
            .unwrap(),
        "cccccccccccccccccccccccccccccccccccccccc"
    );
    assert_eq!(
        result
            .pointer("/pullRequest/mergeState/lastMergeTargetCommit")
            .unwrap(),
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    );
    assert_eq!(
        result.pointer("/pullRequest/mergeState/status").unwrap(),
        "succeeded"
    );
    assert_eq!(
        result.pointer("/revisions/0/mergeBaseCommit").unwrap(),
        "cccccccccccccccccccccccccccccccccccccccc"
    );
    assert_eq!(result.pointer("/revisions/0/reason").unwrap(), "push");
    assert_eq!(
        result.pointer("/revisions/1/mergeBaseCommit").unwrap(),
        "base-one"
    );
    assert!(result.pointer("/revisions/1/reason").unwrap().is_null());
    assert_eq!(
        result.pointer("/revisions/1/createdUtc").unwrap(),
        "2026-08-04T11:41:20.0000000Z"
    );
    assert_eq!(
        result.pointer("/reviewers/0/identity/id").unwrap(),
        "reviewer"
    );
    assert_eq!(result.pointer("/reviewers/0/isRequired").unwrap(), true);
    assert_eq!(result.pointer("/reviewers/0/isTeam").unwrap(), false);
    assert_eq!(result.pointer("/reviewers/1/identity/id").unwrap(), "team");
    assert_eq!(result.pointer("/reviewers/1/isRequired").unwrap(), false);
    assert_eq!(result.pointer("/reviewers/1/isTeam").unwrap(), false);
    assert!(result.pointer("/reviewers/0/vote").is_none());
    assert_eq!(result.pointer("/threads/0/kind").unwrap(), "text");
    assert_eq!(
        result.pointer("/threads/0/context/filePath").unwrap(),
        "src/café.rs"
    );
    assert_eq!(result.pointer("/threads/0/context/side").unwrap(), "right");
    assert_eq!(result.pointer("/threads/0/context/startLine").unwrap(), 4);
    assert_eq!(result.pointer("/threads/0/context/endLine").unwrap(), 5);
    assert_eq!(
        result
            .pointer("/threads/0/context/originalStartLine")
            .unwrap(),
        2
    );
    assert_eq!(
        result
            .pointer("/threads/0/context/originalEndLine")
            .unwrap(),
        3
    );
    assert_eq!(result.pointer("/threads/0/anchorKey").unwrap(), 14);
    assert_eq!(result.pointer("/threads/0/isOutdated").unwrap(), true);
    assert!(result.pointer("/threads/1/context").unwrap().is_null());
    assert_eq!(result.pointer("/threads/1/anchorKey").unwrap(), 21);
    assert_eq!(result.pointer("/threads/1/isOutdated").unwrap(), false);
    assert_eq!(
        result.pointer("/threads/0/eventUtc").unwrap(),
        "2026-08-03T12:00:00.0000000Z"
    );
    assert_eq!(
        result.pointer("/threads/0/lastUpdatedUtc").unwrap(),
        "2026-08-03T12:30:00.0000000Z"
    );
    assert!(result.pointer("/threads/0/decision").unwrap().is_null());
    assert!(
        result
            .pointer("/threads/0/comments/0/parentId")
            .unwrap()
            .is_null()
    );
    assert_eq!(
        result.pointer("/threads/0/comments/0/isEdited").unwrap(),
        false
    );
    assert_eq!(result.pointer("/threads/0/comments/1/parentId").unwrap(), 1);
    assert_eq!(
        result.pointer("/threads/0/comments/1/isEdited").unwrap(),
        true
    );
    assert_eq!(
        result
            .pointer("/threads/0/comments/1/lastEditedUtc")
            .unwrap(),
        "2026-08-03T12:20:00.0000000Z"
    );
    assert_eq!(
        result.pointer("/threads/0/url").unwrap(),
        "https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242?discussionId=101"
    );
    assert_eq!(
        result.pointer("/threads/0/comments/0/url").unwrap(),
        result.pointer("/threads/0/url").unwrap()
    );
}

#[test]
fn single_entry_and_empty_comments_preserve_collection_shape() {
    let mut provider = provider();
    *provider.responses.get_mut("reviewers").unwrap() =
        json!({"count":1,"value":[{"id":"only-reviewer-id"}]});
    provider
        .responses
        .pointer_mut("/threads/value/0/comments")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .truncate(1);
    let result = capture(&provider).unwrap();
    assert_eq!(
        result.get("revisions").unwrap().as_array().unwrap().len(),
        1
    );
    assert_eq!(
        result.get("reviewers").unwrap().as_array().unwrap().len(),
        1
    );
    assert_eq!(result.get("threads").unwrap().as_array().unwrap().len(), 1);
    assert_eq!(
        result.pointer("/reviewers/0/identity/id").unwrap(),
        "only-reviewer-id"
    );
    assert_eq!(result.pointer("/reviewers/0/isRequired").unwrap(), false);
    assert_eq!(
        result
            .pointer("/threads/0/comments")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        result.pointer("/threads/0/comments/0/content").unwrap(),
        "Alpha — “gamma” 🤖 café"
    );
    assert_eq!(
        result.pointer("/threads/0/comments/0/authorship").unwrap(),
        "indeterminate"
    );
    assert_eq!(result.pointer("/threads/0/resolution").unwrap(), "active");
    assert_eq!(result.pointer("/revisions/0/id").unwrap(), 2);
    provider.seen.borrow_mut().clear();
    *provider
        .responses
        .pointer_mut("/threads/value/0/comments")
        .unwrap() = json!([]);
    *provider
        .responses
        .pointer_mut("/threads/value/0/status")
        .unwrap() = json!("closed");
    let result = capture(&provider).unwrap();
    assert!(
        result
            .pointer("/threads/0/comments")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(result.pointer("/threads/0/resolution").unwrap(), "resolved");
    assert!(result.pointer("/threads/0/decision").unwrap().is_null());
}

#[test]
fn reviewer_votes_do_not_change_roster_or_add_vote_fields() {
    let mut provider = provider();
    *provider.responses.get_mut("reviewers").unwrap() = json!({"count":6,"value":[
        {"id":"a","vote":-10},{"id":"b","vote":-5},{"id":"c","vote":0},
        {"id":"d","vote":5},{"id":"e","vote":10},{"id":"f"}
    ]});
    let result = capture(&provider).unwrap();
    let reviewers = result.get("reviewers").unwrap().as_array().unwrap();
    assert_eq!(reviewers.len(), 6);
    for reviewer in reviewers {
        let fields = reviewer.as_object().unwrap();
        assert_eq!(fields.len(), 3);
        assert!(fields.contains_key("identity"));
        assert!(fields.contains_key("isRequired"));
        assert!(fields.contains_key("isTeam"));
    }
}

#[test]
fn target_resolution_uses_service_only_when_coordinates_are_missing() {
    let mut missing = provider();
    missing.responses.as_object_mut().unwrap().insert(
        "resolve".into(),
        json!({"repository":{"name":"example repo","project":{"name":"Example Project"}}}),
    );
    let target = capture_target(&json!({
        "platform":"azureDevOps","organization":"example","pullRequestId":4242,
        "collectionUrl":"https://example.visualstudio.com/"
    }))
    .unwrap();
    let result = collect_capture(&target, None, &missing, &missing, &NoWait).unwrap();
    assert_eq!(
        result.pointer("/target/project").unwrap(),
        "Example Project"
    );
    assert_eq!(
        result.pointer("/target/repository").unwrap(),
        "example repo"
    );
    assert_eq!(
        result.pointer("/target/webUrl").unwrap(),
        "https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242"
    );
    assert!(missing.seen.borrow().contains("resolve"));
    assert_eq!(missing.seen.borrow().len(), 6);

    let complete = provider();
    capture(&complete).unwrap();
    assert!(!complete.seen.borrow().contains("resolve"));
    assert_eq!(complete.seen.borrow().len(), 5);
}
