//! Exercises independent GitHub connection cursors through complete captures.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_slice, from_str, json, to_vec};
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, collect_capture,
};

/// Serves each independent connection from a finite cursor script.
struct Provider {
    responses: Value,
    pages: RefCell<BTreeMap<String, VecDeque<(Value, Value)>>>,
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        let route = arguments.get(1).unwrap();
        let result = if route == "graphql" {
            let request: Value = from_slice(input).unwrap();
            let query = request.get("query").unwrap().as_str().unwrap();
            let key = if query.contains("reviewThreads(") {
                "threads"
            } else if query.contains("node(id:$threadId)") {
                assert_eq!(request.pointer("/variables/threadId").unwrap(), "PRRT_1");
                "comments"
            } else if query.contains("timelineItems(") {
                "timeline"
            } else if query.contains("reviews(") {
                "reviews"
            } else if query.contains("comments(") {
                "issues"
            } else {
                panic!("unexpected GraphQL query")
            };
            let (cursor, response) = self
                .pages
                .borrow_mut()
                .get_mut(key)
                .unwrap()
                .pop_front()
                .expect("unexpected page request");
            assert_eq!(request.pointer("/variables/cur").unwrap(), &cursor);
            response
        } else {
            let key = match route.as_str() {
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
            };
            let response = self.responses.get(key).unwrap();
            if key == "issueComments" && self.responses.get("issueCommentPages").is_some() {
                assert!(arguments.iter().any(|arg| arg == "--paginate"));
                assert!(arguments.iter().any(|arg| arg == "--slurp"));
                self.responses.get("issueCommentPages").unwrap().clone()
            } else if ["reviews", "issueComments", "requestedReviewers"].contains(&key) {
                json!([response])
            } else {
                response.clone()
            }
        };
        Ok(ProcessOutput::new(true, to_vec(&result).unwrap(), vec![]))
    }
}

impl Http for Provider {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub capture must not access Azure DevOps")
    }
}

/// Keeps offline captures independent of elapsed time.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected retry")
    }
}

/// Builds a GraphQL connection with a stated continuation.
#[expect(
    clippy::needless_pass_by_value,
    reason = "The JSON macro builds the response from owned fixture values"
)]
fn page(nodes: Value, next: Option<&str>) -> Value {
    json!({"nodes":nodes, "pageInfo":{"hasNextPage":next.is_some(),"endCursor":next}})
}

/// Wraps a pull request connection in its GraphQL response.
fn connection(key: &str, nodes: Value, next: Option<&str>) -> Value {
    json!({"data":{"repository":{"pullRequest":{key:page(nodes,next)}}}})
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "The complete capture contract covers independent provider connections"
)]
fn independently_paged_discussion_retains_all_evidence() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let mut responses = fixture.get("responses").unwrap().clone();
    let account = json!({"id":7,"login":"alice","node_id":"U_7","type":"User","html_url":"https://github.com/alice"});
    *responses.get_mut("reviews").unwrap() = json!([
        {"id":1,"node_id":"R1","state":"COMMENTED","body":"first","user":account,
         "submitted_at":"2026-01-02T03:04:06Z","html_url":null,"author_association":"MEMBER","commit_id":null},
        {"id":2,"node_id":"R2","state":"APPROVED","body":"","user":account,
         "submitted_at":"2026-01-02T03:04:07Z","html_url":null,"author_association":"MEMBER","commit_id":null}
    ]);
    *responses.get_mut("issueComments").unwrap() = json!([
        {"id":1,"node_id":"I1","body":"issue one","user":account,"created_at":"2026-01-02T03:04:06Z","html_url":null,"author_association":"MEMBER"},
        {"id":2,"node_id":"I2","body":"issue two","user":account,"created_at":"2026-01-02T03:04:07Z","html_url":null,"author_association":"MEMBER"}
    ]);
    let mut thread = responses
        .pointer("/reviewThreads/data/repository/pullRequest/reviewThreads/nodes/0")
        .unwrap()
        .clone();
    let first_comment = thread.pointer("/comments/nodes/0").unwrap().clone();
    *thread.get_mut("comments").unwrap() = page(json!([first_comment]), Some("comment-1"));
    let mut second_thread = thread.clone();
    *second_thread.get_mut("id").unwrap() = json!("PRRT_2");
    let mut other_comment = first_comment.clone();
    *other_comment.get_mut("id").unwrap() = json!("PRRC_4");
    *second_thread.get_mut("comments").unwrap() = page(json!([other_comment]), None);
    let mut second_comment = first_comment.clone();
    *second_comment.get_mut("id").unwrap() = json!("PRRC_2");
    *second_comment.get_mut("replyTo").unwrap() = json!({"id":"PRRC_1"});
    *second_comment.get_mut("createdAt").unwrap() = json!("2026-08-03T12:05:00Z");
    let mut third_comment = first_comment.clone();
    *third_comment.get_mut("id").unwrap() = json!("PRRC_3");
    *third_comment.get_mut("replyTo").unwrap() = json!({"id":"PRRC_1"});
    *third_comment.get_mut("createdAt").unwrap() = json!("2026-08-03T12:10:00Z");
    let actor = first_comment.get("author").unwrap().clone();
    let push = |id, commit| {
        json!({"__typename":"HeadRefForcePushedEvent","id":id,
        "createdAt":"2026-08-03T07:00:00Z","actor":actor,
        "beforeCommit":{"oid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"afterCommit":{"oid":commit}})
    };
    let pages = BTreeMap::from([
        (
            "threads".to_owned(),
            VecDeque::from([
                (
                    Value::Null,
                    connection("reviewThreads", json!([thread]), Some("thread-1")),
                ),
                (
                    json!("thread-1"),
                    connection("reviewThreads", json!([second_thread]), None),
                ),
            ]),
        ),
        (
            "comments".to_owned(),
            VecDeque::from([
                (
                    json!("comment-1"),
                    json!({"data":{"node":{"comments":page(json!([second_comment]),Some("comment-2"))}}}),
                ),
                (
                    json!("comment-2"),
                    json!({"data":{"node":{"comments":page(json!([third_comment]),None)}}}),
                ),
            ]),
        ),
        (
            "issues".to_owned(),
            VecDeque::from([
                (
                    Value::Null,
                    connection(
                        "comments",
                        json!([{"id":"I1","lastEditedAt":"2026-08-03T09:15:00Z"}]),
                        Some("issue-1"),
                    ),
                ),
                (
                    json!("issue-1"),
                    connection(
                        "comments",
                        json!([{"id":"I2","lastEditedAt":"2026-08-04T09:15:00Z"}]),
                        None,
                    ),
                ),
            ]),
        ),
        (
            "reviews".to_owned(),
            VecDeque::from([
                (
                    Value::Null,
                    connection(
                        "reviews",
                        json!([{"id":"R1","lastEditedAt":"2026-08-03T09:15:00Z","comments":{"totalCount":0}}]),
                        Some("review-1"),
                    ),
                ),
                (
                    json!("review-1"),
                    connection(
                        "reviews",
                        json!([{"id":"R2","lastEditedAt":"2026-08-04T09:15:00Z","comments":{"totalCount":0}}]),
                        None,
                    ),
                ),
            ]),
        ),
        (
            "timeline".to_owned(),
            VecDeque::from([
                (
                    Value::Null,
                    connection(
                        "timelineItems",
                        json!([push("F1", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")]),
                        Some("timeline-1"),
                    ),
                ),
                (
                    json!("timeline-1"),
                    connection(
                        "timelineItems",
                        json!([push("F2", "cccccccccccccccccccccccccccccccccccccccc")]),
                        None,
                    ),
                ),
            ]),
        ),
    ]);
    let provider = Provider {
        responses,
        pages: RefCell::new(pages),
    };
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let result = collect_capture(&target, None, &provider, &provider, &NoWait).unwrap();
    assert!(provider.pages.borrow().values().all(VecDeque::is_empty));
    let threads = result.get("threads").unwrap().as_array().unwrap();
    for id in ["PRRT_1", "PRRT_2", "R1", "R2", "I1", "I2", "F1", "F2"] {
        assert_eq!(
            threads
                .iter()
                .filter(|thread| thread.get("id").unwrap() == id)
                .count(),
            1
        );
    }
    let thread = |id: &str| {
        threads
            .iter()
            .find(|thread| thread.get("id").unwrap() == id)
            .unwrap()
    };
    assert_eq!(
        thread("PRRT_1").get("eventUtc").unwrap(),
        "2026-08-03T12:00:00.0000000Z"
    );
    assert_eq!(
        thread("R1").get("eventUtc").unwrap(),
        "2026-01-02T03:04:06.0000000Z"
    );
    assert_eq!(
        thread("I1").get("eventUtc").unwrap(),
        "2026-01-02T03:04:06.0000000Z"
    );
    let comments = thread("PRRT_1")
        .get("comments")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        comments
            .iter()
            .map(|comment| comment.get("id").unwrap())
            .collect::<Vec<_>>(),
        vec![&json!("PRRC_1"), &json!("PRRC_2"), &json!("PRRC_3")]
    );
    assert_eq!(
        thread("PRRT_1").get("publishedUtc").unwrap(),
        "2026-08-03T12:00:00.0000000Z"
    );
    assert_eq!(
        thread("PRRT_1").get("lastUpdatedUtc").unwrap(),
        "2026-08-03T12:10:00.0000000Z"
    );
    assert!(
        threads
            .iter()
            .any(|thread| thread.get("id").unwrap() == "PRRT_2")
    );
    for (id, edited) in [
        ("I1", "2026-08-03T09:15:00.0000000Z"),
        ("I2", "2026-08-04T09:15:00.0000000Z"),
        ("R1", "2026-08-03T09:15:00.0000000Z"),
        ("R2", "2026-08-04T09:15:00.0000000Z"),
    ] {
        assert_eq!(
            thread(id).pointer("/comments/0/lastEditedUtc").unwrap(),
            edited
        );
    }

    assert_eq!(
        thread("R2").pointer("/comments/0/authorship").unwrap(),
        "human"
    );
    assert_eq!(thread("R2").get("kind").unwrap(), "reviewSummary");
    assert!(thread("R2").get("rawStatus").unwrap().is_null());
    assert!(thread("R2").get("decision").unwrap().is_null());
    assert_eq!(
        thread("R2").pointer("/properties/state").unwrap(),
        "APPROVED"
    );
    assert_eq!(thread("R2").pointer("/comments/0/content").unwrap(), "");
    assert_eq!(
        thread("R2")
            .pointer("/comments/0/author/uniqueName")
            .unwrap(),
        "alice"
    );
    assert_eq!(
        thread("R2").get("publishedUtc").unwrap(),
        "2026-01-02T03:04:07.0000000Z"
    );
    assert!(
        thread("R2")
            .pointer("/comments/0/authorshipEvidence")
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&json!("review:voteOnly"))
    );
    assert_eq!(thread("R1").pointer("/comments/0/isEdited").unwrap(), true);
    assert_eq!(
        thread("R1").get("lastUpdatedUtc").unwrap(),
        "2026-08-03T09:15:00.0000000Z"
    );
    assert_eq!(thread("I1").get("kind").unwrap(), "text");
    assert_eq!(thread("I1").get("resolution").unwrap(), "indeterminate");
    assert!(thread("I1").get("rawStatus").unwrap().is_null());
    assert_eq!(
        thread("I1").pointer("/comments/0/content").unwrap(),
        "issue one"
    );
    assert_eq!(thread("I1").pointer("/comments/0/isEdited").unwrap(), true);
    assert_eq!(thread("PRRT_1").get("kind").unwrap(), "text");
    assert_eq!(thread("PRRT_1").get("resolution").unwrap(), "active");
    assert_eq!(thread("PRRT_1").get("rawStatus").unwrap(), "unresolved");
    assert_eq!(thread("F1").get("kind").unwrap(), "refUpdate");
    assert_eq!(
        thread("F1").pointer("/properties/beforeCommit").unwrap(),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert_eq!(
        thread("F1").pointer("/decision/by/uniqueName").unwrap(),
        "reviewer"
    );
    assert_eq!(
        thread("F1").pointer("/decision/utc").unwrap(),
        "2026-08-03T07:00:00.0000000Z"
    );
    assert!(
        thread("F1")
            .get("comments")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        result
            .get("revisions")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        thread("F1").pointer("/properties/afterCommit").unwrap(),
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    );
    assert_eq!(
        thread("F2").pointer("/properties/afterCommit").unwrap(),
        "cccccccccccccccccccccccccccccccccccccccc"
    );
}

#[test]
fn nested_comments_with_missing_cursor_fail_without_truncating() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let responses = fixture.get("responses").unwrap().clone();
    let mut thread = responses
        .pointer("/reviewThreads/data/repository/pullRequest/reviewThreads/nodes/0")
        .unwrap()
        .clone();
    *thread.pointer_mut("/comments/pageInfo").unwrap() =
        json!({"hasNextPage":true,"endCursor":null});
    let provider = Provider {
        responses,
        pages: RefCell::new(BTreeMap::from([(
            "threads".to_owned(),
            VecDeque::from([(
                Value::Null,
                connection("reviewThreads", json!([thread]), None),
            )]),
        )])),
    };
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let error = collect_capture(&target, None, &provider, &provider, &NoWait).unwrap_err();
    assert_eq!(ss_ai_review::classification(&error), "service");
    assert!(provider.pages.borrow().values().all(VecDeque::is_empty));
}

#[test]
fn rest_pagination_rejects_an_unwrapped_object() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let mut responses = fixture.get("responses").unwrap().clone();
    responses.as_object_mut().unwrap().insert(
        "issueCommentPages".into(),
        json!({"message":"not a page list"}),
    );
    let provider = Provider {
        responses,
        pages: RefCell::new(BTreeMap::new()),
    };
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let error = collect_capture(&target, None, &provider, &provider, &NoWait).unwrap_err();
    assert_eq!(ss_ai_review::classification(&error), "service");
}

#[test]
fn rest_pagination_flattens_every_page_into_the_capture() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let mut responses = fixture.get("responses").unwrap().clone();
    let account = json!({"id":7,"login":"alice","node_id":"U_7","type":"User"});
    let comment = |id| {
        json!({"id":id,"node_id":format!("I{id}"),"body":format!("text {id}"),
        "user":account,"created_at":"2026-01-02T03:04:06Z","html_url":null,"author_association":"MEMBER"})
    };
    responses.as_object_mut().unwrap().insert(
        "issueCommentPages".into(),
        json!([
            [comment(1), comment(2)],
            [comment(3), comment(4)],
            [comment(5)]
        ]),
    );
    let metadata = (1..=5)
        .map(|id| json!({"id":format!("I{id}"),"lastEditedAt":null}))
        .collect::<Vec<_>>();
    let initial_threads = responses.get("reviewThreads").unwrap().clone();
    let pages = BTreeMap::from([
        (
            "threads".to_owned(),
            VecDeque::from([(Value::Null, initial_threads)]),
        ),
        (
            "issues".to_owned(),
            VecDeque::from([(Value::Null, connection("comments", json!(metadata), None))]),
        ),
        (
            "reviews".to_owned(),
            VecDeque::from([(Value::Null, connection("reviews", json!([]), None))]),
        ),
        (
            "timeline".to_owned(),
            VecDeque::from([(Value::Null, connection("timelineItems", json!([]), None))]),
        ),
    ]);
    let provider = Provider {
        responses,
        pages: RefCell::new(pages),
    };
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let result = collect_capture(&target, None, &provider, &provider, &NoWait).unwrap();
    let threads = result.get("threads").unwrap().as_array().unwrap();
    for id in 1..=5 {
        let expected = format!("I{id}");
        let thread = threads
            .iter()
            .find(|thread| thread.get("id").unwrap() == &expected)
            .unwrap();
        assert_eq!(
            thread.pointer("/comments/0/content").unwrap(),
            &format!("text {id}")
        );
    }
    assert!(provider.pages.borrow().values().all(VecDeque::is_empty));
}

/// Injects one GraphQL refusal ahead of the complete recorded capture.
struct RateLimitedGraphql {
    inner: Provider,
    refused: std::cell::Cell<bool>,
    waits: RefCell<Vec<Duration>>,
}

impl Process for RateLimitedGraphql {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if arguments.get(1).is_some_and(|route| route == "graphql") && !self.refused.replace(true) {
            return Ok(ProcessOutput::new(false,
                b"HTTP/2 403\nRetry-After: 30\n\n{\"errors\":[{\"message\":\"API rate limit exceeded\"}]}".to_vec(),
                vec![]));
        }
        self.inner.execute(program, arguments, input)
    }
}

impl Http for RateLimitedGraphql {
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        self.inner.send(method, url, token, body)
    }
}

impl Clock for RateLimitedGraphql {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }
    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        self.waits.borrow_mut().push(duration);
        Ok(())
    }
}

#[test]
fn graphql_read_rate_refusal_retries_without_losing_capture() {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    let responses = fixture.get("responses").unwrap().clone();
    let pages = BTreeMap::from([
        (
            "threads".to_owned(),
            VecDeque::from([(Value::Null, responses.get("reviewThreads").unwrap().clone())]),
        ),
        (
            "issues".to_owned(),
            VecDeque::from([(Value::Null, responses.get("issueEdits").unwrap().clone())]),
        ),
        (
            "reviews".to_owned(),
            VecDeque::from([(Value::Null, responses.get("reviewEdits").unwrap().clone())]),
        ),
        (
            "timeline".to_owned(),
            VecDeque::from([(Value::Null, responses.get("timeline").unwrap().clone())]),
        ),
    ]);
    let provider = RateLimitedGraphql {
        inner: Provider {
            responses,
            pages: RefCell::new(pages),
        },
        refused: std::cell::Cell::new(false),
        waits: RefCell::new(Vec::new()),
    };
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    let result = collect_capture(&target, None, &provider, &provider, &provider).unwrap();
    assert_eq!(
        result.pointer("/threads/0/comments/0/content").unwrap(),
        "Alpha — “gamma” 🤖 café"
    );
    assert_eq!(*provider.waits.borrow(), [Duration::from_secs(30)]);
    assert!(
        provider
            .inner
            .pages
            .borrow()
            .values()
            .all(VecDeque::is_empty)
    );
}
