//! Exercises GitHub authorship evidence through complete offline captures.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_slice, from_str, json, to_vec};
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, classification,
    collect_capture,
};

/// Routes the recorded capture once per endpoint without allowing remote access.
struct Provider {
    responses: Value,
    seen: RefCell<BTreeSet<String>>,
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        assert_eq!(arguments.first().map(String::as_str), Some("api"));
        let route = arguments.get(1).expect("missing provider route");
        let key = if route == "graphql" {
            let request: Value = from_slice(input).unwrap();
            assert!(request.pointer("/variables/cur").unwrap().is_null());
            let query = request.get("query").and_then(Value::as_str).unwrap();
            if query.contains("reviewThreads(") {
                "reviewThreads"
            } else if query.contains("timelineItems(") {
                "timeline"
            } else if query.contains("reviews(") {
                assert!(query.contains("totalCount"));
                "reviewEdits"
            } else if query.contains("comments(") {
                "issueEdits"
            } else {
                panic!("unexpected GraphQL query")
            }
        } else {
            assert!(input.is_empty());
            match route.as_str() {
                "repos/octo/widget/pulls/7" => "pullRequest",
                "repos/octo/widget/pulls/7/reviews?per_page=100" => "reviews",
                "repos/octo/widget/issues/7/comments?per_page=100" => "issueComments",
                "repos/octo/widget/pulls/7/requested_reviewers?per_page=100" => {
                    "requestedReviewers"
                }
                "user" => "user",
                "repos/octo/widget/compare/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb...aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" => {
                    "compare"
                }
                _ => panic!("unexpected provider request: {route}"),
            }
        };
        assert!(self.seen.borrow_mut().insert(key.to_owned()));
        let response = self.responses.get(key).expect("missing fixture response");
        let payload = if ["reviews", "issueComments", "requestedReviewers"].contains(&key) {
            json!([response])
        } else {
            response.clone()
        };
        Ok(ProcessOutput::new(true, to_vec(&payload).unwrap(), vec![]))
    }
}

impl Http for Provider {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub capture must not use Azure DevOps HTTP")
    }
}

/// Makes unexpected retry waits fail immediately.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("complete recorded capture must not wait")
    }
}

/// Captures a modified fixture while asserting every expected read completed.
fn capture(responses: Value) -> Value {
    let provider = Provider {
        responses,
        seen: RefCell::new(BTreeSet::new()),
    };
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let result = collect_capture(&target, None, &provider, &provider, &NoWait).unwrap();
    assert_eq!(provider.seen.borrow().len(), 10);
    result
}

#[test]
fn identity_and_marker_evidence_determine_comment_authorship() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let comment_path =
        "/reviewThreads/data/repository/pullRequest/reviewThreads/nodes/0/comments/nodes/0";
    for (account_type, node_id, url, login, body, expected) in [
        ("Bot", "U_1", "https://github.com/a", "a", "text", "agent"),
        (
            "User",
            "BOT_1",
            "https://github.com/a",
            "a",
            "text",
            "agent",
        ),
        (
            "User",
            "U_1",
            "https://github.com/apps/a",
            "a",
            "text",
            "agent",
        ),
        (
            "User",
            "U_1",
            "https://github.com/a",
            "a[bot]",
            "text",
            "indeterminate",
        ),
        (
            "User",
            "U_1",
            "https://github.com/a",
            "a",
            "text",
            "indeterminate",
        ),
        (
            "User",
            "U_1",
            "https://github.com/a",
            "a",
            "2026-08-04T10:33:54Z",
            "indeterminate",
        ),
        (
            "User",
            "U_1",
            "https://github.com/a",
            "a",
            " \n[Copilot speaking] text",
            "agent",
        ),
        (
            "User",
            "U_1",
            "https://github.com/a",
            "a",
            "mentions [Copilot speaking] text",
            "indeterminate",
        ),
    ] {
        let mut responses = fixture.get("responses").unwrap().clone();
        let comment = responses.pointer_mut(comment_path).unwrap();
        *comment.pointer_mut("/author/__typename").unwrap() = json!(account_type);
        *comment.pointer_mut("/author/id").unwrap() = json!(node_id);
        *comment.pointer_mut("/author/url").unwrap() = json!(url);
        *comment.pointer_mut("/author/login").unwrap() = json!(login);
        *comment.get_mut("body").unwrap() = json!(body);
        let result = capture(responses);
        let captured = result.pointer("/threads/0/comments/0").unwrap();
        assert_eq!(captured.get("authorship").unwrap(), expected);
        assert_eq!(captured.get("content").unwrap(), body);
        let evidence = captured
            .pointer("/author/evidence")
            .unwrap()
            .as_array()
            .unwrap();
        let (kind, identity_evidence) = if account_type == "Bot" {
            ("service", "accountType:Bot")
        } else if node_id.starts_with("BOT_") {
            ("service", "nodeId:BOT_")
        } else if url.starts_with("https://github.com/apps/") {
            ("service", "url:apps")
        } else {
            ("person", "accountType:User")
        };
        assert_eq!(captured.pointer("/author/kind").unwrap(), kind);
        assert!(evidence.contains(&json!(identity_evidence)));
        assert_eq!(captured.pointer("/author/uniqueName").unwrap(), login);
        let authorship_evidence = captured
            .get("authorshipEvidence")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(
            authorship_evidence.contains(&json!("content:agentMarker")),
            body.trim_start().starts_with("[Copilot speaking]")
        );
    }
}

#[test]
fn vote_only_reviews_use_vote_and_identity_evidence() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    for (state, body, account_type, count, expected) in [
        ("APPROVED", "", "User", 0, Some("human")),
        ("CHANGES_REQUESTED", "", "User", 0, Some("human")),
        ("DISMISSED", "", "User", 0, Some("human")),
        ("APPROVED", "", "Bot", 0, Some("agent")),
        (
            "COMMENTED",
            "Automated findings follow.",
            "Bot",
            0,
            Some("agent"),
        ),
        ("APPROVED", "", "User", 1, Some("indeterminate")),
        ("APPROVED", "feedback", "User", 0, Some("indeterminate")),
        (
            "CHANGES_REQUESTED",
            "feedback",
            "User",
            0,
            Some("indeterminate"),
        ),
        ("COMMENTED", "feedback", "User", 0, Some("indeterminate")),
        ("DISMISSED", "feedback", "User", 0, Some("indeterminate")),
        ("COMMENTED", "", "User", 0, None),
        ("PENDING", "", "User", 0, None),
    ] {
        let mut responses = fixture.get("responses").unwrap().clone();
        *responses.get_mut("reviews").unwrap() = json!([{
            "id": 9, "node_id": "R1", "state": state, "body": body,
            "user": {"id": 7, "login": "alice", "node_id": "U_7",
                "type": account_type, "html_url": "https://github.com/alice"},
            "submitted_at": "2026-01-02T03:04:06Z",
            "html_url": "https://github.com/review", "author_association": "MEMBER",
            "commit_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }]);
        *responses
            .pointer_mut("/reviewEdits/data/repository/pullRequest/reviews/nodes")
            .unwrap() = json!([{"id":"R1","lastEditedAt":null,"comments":{"totalCount":count}}]);
        let result = capture(responses);
        let thread = result
            .get("threads")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|thread| thread.get("id") == Some(&json!("R1")));
        match (thread, expected) {
            (Some(thread), Some(expected)) => {
                assert_eq!(thread.get("kind").unwrap(), "reviewSummary");
                assert_eq!(thread.pointer("/properties/state").unwrap(), state);
                let comment = thread.pointer("/comments/0").unwrap();
                assert_eq!(comment.get("authorship").unwrap(), expected);
                assert_eq!(comment.get("content").unwrap(), body);
                if account_type == "Bot" {
                    assert_eq!(comment.pointer("/author/kind").unwrap(), "service");
                    assert!(
                        comment
                            .pointer("/author/evidence")
                            .unwrap()
                            .as_array()
                            .unwrap()
                            .contains(&json!("accountType:Bot"))
                    );
                }
                let evidence = comment
                    .get("authorshipEvidence")
                    .unwrap()
                    .as_array()
                    .unwrap();
                assert_eq!(
                    evidence.contains(&json!("review:voteOnly")),
                    expected == "human"
                );
            }
            (None, None) => {}
            _ => panic!("unexpected review summary for {state}"),
        }
    }
}

#[test]
fn unknown_review_state_refuses_instead_of_inventing_publication_status() {
    // The discussion contract excludes unsubmitted drafts and requires a
    // failed retrieval rather than incomplete evidence (design.md,
    // "Deterministic discussion capture"). A submission timestamp does not
    // establish whether an unknown state represents a withdrawn review.
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let mut responses = fixture.get("responses").unwrap().clone();
    *responses.get_mut("reviews").unwrap() = json!([{
        "id": 9, "node_id": "R1", "state": "SOMETHING_NEW",
        "body": "Evidence from an unknown review state",
        "user": {"id": 7, "login": "alice", "node_id": "U_7",
            "type": "User", "html_url": "https://github.com/alice"},
        "submitted_at": "2026-01-02T03:04:06Z",
        "html_url": "https://github.com/review", "author_association": "MEMBER",
        "commit_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }]);
    let provider = Provider {
        responses,
        seen: RefCell::new(BTreeSet::new()),
    };
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let error = collect_capture(&target, None, &provider, &provider, &NoWait).unwrap_err();
    assert_eq!(classification(&error), "service");
    assert!(error.to_string().contains("Unknown GitHub review state"));
}

#[test]
fn vote_only_review_rejects_non_integer_or_missing_comment_counts() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    for count in [Value::Null, json!(-1), json!("0")] {
        let mut responses = fixture.get("responses").unwrap().clone();
        *responses.get_mut("reviews").unwrap() = json!([{
            "id": 9, "node_id": "R1", "state": "APPROVED", "body": "",
            "user": {"id": 7, "login": "alice", "node_id": "U_7",
                "type": "User", "html_url": "https://github.com/alice"},
            "submitted_at": "2026-01-02T03:04:06Z",
            "html_url": "https://github.com/review", "author_association": "MEMBER",
            "commit_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }]);
        *responses
            .pointer_mut("/reviewEdits/data/repository/pullRequest/reviews/nodes")
            .unwrap() = json!([{"id":"R1","lastEditedAt":null,"comments":{"totalCount":count}}]);
        let provider = Provider {
            responses,
            seen: RefCell::new(BTreeSet::new()),
        };
        let target =
            capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
        let error = collect_capture(&target, None, &provider, &provider, &NoWait).unwrap_err();
        assert_eq!(classification(&error), "service");
        assert_eq!(provider.seen.borrow().len(), 8);
    }
}

#[test]
fn pending_reviews_and_inline_drafts_do_not_leak_into_discussion_or_roster() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    for all_draft in [false, true] {
        let mut responses = fixture.get("responses").unwrap().clone();
        *responses.get_mut("reviews").unwrap() = json!([
            {"id": 8, "node_id": "draft-review", "state": "PENDING", "body": "unsent",
             "user":{"id":8,"login":"draft-user","node_id":"U_8","type":"User"}},
            {"id": 9, "node_id": "empty-review", "state": "COMMENTED", "body": "",
             "user":{"id":7,"login":"alice","node_id":"U_7","type":"User"}},
            {"id": 10, "node_id": "submitted-review", "state": "COMMENTED", "body": "feedback",
             "user":{"id":7,"login":"alice","node_id":"U_7","type":"User"},
             "submitted_at":"2026-01-02T03:04:06Z", "html_url":null,
             "author_association":"MEMBER", "commit_id":null}
        ]);
        *responses
            .pointer_mut("/reviewEdits/data/repository/pullRequest/reviews/nodes")
            .unwrap() = json!([
            {"id":"empty-review","lastEditedAt":null},
            {"id":"submitted-review","lastEditedAt":null}
        ]);
        let comments = responses
            .pointer_mut(
                "/reviewThreads/data/repository/pullRequest/reviewThreads/nodes/0/comments/nodes",
            )
            .unwrap()
            .as_array_mut()
            .unwrap();
        let mut draft = comments.first().unwrap().clone();
        *draft.get_mut("id").unwrap() = json!("draft-comment");
        *draft.get_mut("state").unwrap() = json!("PENDING");
        *draft.get_mut("replyTo").unwrap() = json!({"id":"PRRC_1"});
        if all_draft {
            comments.clear();
            *draft.get_mut("replyTo").unwrap() = Value::Null;
        }
        comments.push(draft);
        let result = capture(responses);
        let threads = result.get("threads").unwrap().as_array().unwrap();
        assert!(
            threads
                .iter()
                .any(|thread| thread.get("id") == Some(&json!("submitted-review")))
        );
        assert!(!threads.iter().any(|thread| {
            ["draft-review", "empty-review"]
                .iter()
                .any(|id| thread.get("id") == Some(&json!(id)))
        }));
        let inline = threads
            .iter()
            .find(|thread| thread.get("id") == Some(&json!("PRRT_1")));
        if all_draft {
            assert!(inline.is_none());
        } else {
            let comments = inline.unwrap().get("comments").unwrap().as_array().unwrap();
            assert_eq!(comments.len(), 1);
            assert_eq!(comments.first().unwrap().get("id").unwrap(), "PRRC_1");
        }
        assert!(
            result
                .get("reviewers")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .all(|reviewer| {
                    reviewer.pointer("/identity/uniqueName") != Some(&json!("draft-user"))
                })
        );
    }
}

#[test]
fn service_edit_evidence_controls_review_and_comment_timestamps() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    for edited in [false, true] {
        let mut responses = fixture.get("responses").unwrap().clone();
        let account = json!({"id":7,"login":"alice","node_id":"U_7","type":"User"});
        *responses.get_mut("reviews").unwrap() = json!([{
            "id":9,"node_id":"R1","state":"COMMENTED","body":"review",
            "user":account,"submitted_at":"2026-01-02T03:04:06Z",
            "html_url":null,"author_association":"MEMBER","commit_id":null
        }]);
        *responses.get_mut("issueComments").unwrap() = json!([{
            "id":10,"node_id":"I1","body":"issue","user":account,
            "created_at":"2026-01-02T03:04:06Z","updated_at":"2026-01-02T03:04:07Z",
            "html_url":null,"author_association":"MEMBER"
        }]);
        let timestamp = if edited {
            json!("2026-08-03T09:15:00Z")
        } else {
            Value::Null
        };
        *responses
            .pointer_mut("/reviewEdits/data/repository/pullRequest/reviews/nodes")
            .unwrap() = json!([{"id":"R1","lastEditedAt":timestamp,"comments":{"totalCount":0}}]);
        *responses
            .pointer_mut("/issueEdits/data/repository/pullRequest/comments/nodes")
            .unwrap() = json!([{"id":"I1","lastEditedAt":timestamp}]);
        let result = capture(responses);
        let threads = result.get("threads").unwrap().as_array().unwrap();
        for id in ["R1", "I1"] {
            let thread = threads
                .iter()
                .find(|thread| thread.get("id").unwrap() == id)
                .unwrap();
            let comment = thread.pointer("/comments/0").unwrap();
            assert_eq!(comment.get("isEdited").unwrap(), edited);
            assert_eq!(
                comment.get("publishedUtc").unwrap(),
                "2026-01-02T03:04:06.0000000Z"
            );
            if edited {
                assert_eq!(
                    comment.get("lastEditedUtc").unwrap(),
                    "2026-08-03T09:15:00.0000000Z"
                );
                assert_eq!(
                    thread.get("lastUpdatedUtc").unwrap(),
                    "2026-08-03T09:15:00.0000000Z"
                );
            } else {
                assert!(comment.get("lastEditedUtc").unwrap().is_null());
                assert_eq!(
                    thread.get("lastUpdatedUtc").unwrap(),
                    "2026-01-02T03:04:06.0000000Z"
                );
            }
        }
    }
}

#[test]
fn thread_anchors_and_sides_preserve_service_evidence() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    for (side, outdated) in [("LEFT", false), ("RIGHT", false), ("RIGHT", true)] {
        let mut responses = fixture.get("responses").unwrap().clone();
        let thread = responses
            .pointer_mut("/reviewThreads/data/repository/pullRequest/reviewThreads/nodes/0")
            .unwrap();
        *thread.get_mut("diffSide").unwrap() = json!(side);
        *thread.get_mut("isOutdated").unwrap() = json!(outdated);
        *thread.get_mut("isResolved").unwrap() = json!(outdated);
        *thread.get_mut("originalLine").unwrap() = json!(12);
        *thread.get_mut("originalStartLine").unwrap() = json!(10);
        if outdated {
            *thread.get_mut("line").unwrap() = Value::Null;
            *thread.get_mut("startLine").unwrap() = Value::Null;
        }

        let result = capture(responses);
        let thread = result.pointer("/threads/0").unwrap();
        assert_eq!(
            thread.pointer("/context/side").unwrap().as_str().unwrap(),
            side.to_ascii_lowercase()
        );
        assert_eq!(thread.pointer("/properties/diffSide").unwrap(), side);
        assert_eq!(thread.get("isOutdated").unwrap(), outdated);
        assert_eq!(thread.get("anchorKey").unwrap(), "PRRT_1");
        assert_eq!(thread.pointer("/context/filePath").unwrap(), "src/café.rs");
        assert_eq!(thread.pointer("/context/originalEndLine").unwrap(), 12);
        assert_eq!(thread.pointer("/context/originalStartLine").unwrap(), 10);
        if outdated {
            assert_eq!(thread.get("resolution").unwrap(), "resolved");
            assert_eq!(thread.get("rawStatus").unwrap(), "resolved");
            assert!(thread.pointer("/context/endLine").unwrap().is_null());
            assert!(thread.pointer("/context/startLine").unwrap().is_null());
        } else {
            assert_eq!(thread.get("resolution").unwrap(), "active");
            assert_eq!(thread.get("rawStatus").unwrap(), "unresolved");
        }
    }
}

#[test]
fn empty_review_bodies_preserve_vote_only_evidence() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    for (state, body) in [
        ("APPROVED", Value::Null),
        ("CHANGES_REQUESTED", json!("")),
        ("DISMISSED", json!(" \r\n\t")),
    ] {
        let mut responses = fixture.get("responses").unwrap().clone();
        *responses.get_mut("reviews").unwrap() = json!([{
            "id":9,"node_id":"R1","state":state,"body":body,
            "user":{"id":7,"login":"alice","node_id":"U_7","type":"User"},
            "submitted_at":"2026-01-02T03:04:06Z","html_url":null,
            "author_association":"MEMBER","commit_id":null
        }]);
        *responses
            .pointer_mut("/reviewEdits/data/repository/pullRequest/reviews/nodes")
            .unwrap() = json!([{"id":"R1","lastEditedAt":null,"comments":{"totalCount":0}}]);
        let result = capture(responses);
        let comment = result
            .get("threads")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|thread| thread.get("id").unwrap() == "R1")
            .unwrap()
            .pointer("/comments/0")
            .unwrap();
        assert_eq!(comment.get("authorship").unwrap(), "human");
        assert!(
            comment
                .get("authorshipEvidence")
                .unwrap()
                .as_array()
                .unwrap()
                .contains(&json!("review:voteOnly"))
        );
    }
}

#[test]
fn pull_request_capture_keeps_identity_commits_and_merge_shape() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    for (state, merged) in [("open", false), ("closed", true)] {
        let mut responses = fixture.get("responses").unwrap().clone();
        *responses.pointer_mut("/pullRequest/state").unwrap() = json!(state);
        *responses.pointer_mut("/pullRequest/merged").unwrap() = json!(merged);
        let result = capture(responses);
        assert_eq!(
            result.pointer("/target/webUrl").unwrap(),
            "https://github.com/octo/widget/pull/7"
        );
        assert_eq!(result.pointer("/authenticatedIdentity/id").unwrap(), "701");
        assert_eq!(
            result.pointer("/authenticatedIdentity/kind").unwrap(),
            "person"
        );
        assert_eq!(
            result.pointer("/authenticatedIdentity/uniqueName").unwrap(),
            "runner"
        );
        assert_eq!(result.pointer("/pullRequest/id").unwrap(), 7);
        assert_eq!(
            result.pointer("/pullRequest/title").unwrap(),
            "Unicode capture"
        );
        assert_eq!(result.pointer("/pullRequest/sourceRef").unwrap(), "topic");
        assert_eq!(result.pointer("/pullRequest/targetRef").unwrap(), "main");
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
            result.pointer("/pullRequest/mergeState/status").unwrap(),
            "clean"
        );
        assert_eq!(
            result.pointer("/pullRequest/author/uniqueName").unwrap(),
            "author"
        );
        let merge = result
            .pointer("/pullRequest/mergeState")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(merge.len(), 3);
        assert!(merge.contains_key("status"));
        assert!(merge.contains_key("isMergeable"));
        assert!(merge.contains_key("mergeCommit"));
        assert!(!merge.contains_key("isMerged"));
    }
}

#[test]
fn submitted_requested_and_team_reviewers_are_combined_once() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let mut responses = fixture.get("responses").unwrap().clone();
    let account = json!({"id":7,"login":"alice","node_id":"U_7","type":"User"});
    let review = |id, state| {
        json!({"id":id,"node_id":format!("R{id}"),"state":state,
        "body":"feedback","user":account,"submitted_at":"2026-01-02T03:04:06Z",
        "html_url":null,"author_association":"MEMBER","commit_id":null})
    };
    *responses.get_mut("reviews").unwrap() =
        json!([review(1, "CHANGES_REQUESTED"), review(2, "APPROVED")]);
    *responses
        .pointer_mut("/reviewEdits/data/repository/pullRequest/reviews/nodes")
        .unwrap() = json!([{"id":"R1","lastEditedAt":null,"comments":{"totalCount":0}},
        {"id":"R2","lastEditedAt":null,"comments":{"totalCount":0}}]);
    *responses.get_mut("requestedReviewers").unwrap() = json!({
        "users":[account,{"id":8,"login":"pending","node_id":"U_8","type":"User"}],
        "teams":[{"id":55,"node_id":"T_1","name":"Widget Owners","slug":"widget-owners",
            "html_url":"https://github.com/orgs/octo/teams/widget-owners"}]
    });
    let result = capture(responses);
    let reviewers = result.get("reviewers").unwrap().as_array().unwrap();
    assert_eq!(reviewers.len(), 3);
    for (name, team) in [
        ("alice", false),
        ("pending", false),
        ("widget-owners", true),
    ] {
        let reviewer = reviewers
            .iter()
            .find(|reviewer| reviewer.pointer("/identity/uniqueName").unwrap() == name)
            .unwrap();
        assert_eq!(reviewer.get("isTeam").unwrap(), team);
        assert_eq!(reviewer.as_object().unwrap().len(), 3);
        assert!(reviewer.get("isRequired").is_some());
    }
}
