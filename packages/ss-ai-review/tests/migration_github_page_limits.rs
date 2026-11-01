//! Preserves complete GitHub pagination and refuses malformed connections.

use std::cell::Cell;
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_slice, from_str, json, to_vec};
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, classification,
    collect_capture,
};

/// Serves a complete initial fixture and an explicitly controlled thread page.
struct Provider {
    responses: Value,
    page: Value,
    advancing: bool,
    pages: Cell<usize>,
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
            assert!(
                request
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap()
                    .contains("reviewThreads(")
            );
            let previous = self.pages.get();
            let page = previous.checked_add(1).unwrap();
            // The producer must fail by the documented bound; a changed loop
            // cannot make the fake or mutation test wait indefinitely.
            assert!(page <= 1000, "capture exceeded its page bound");
            self.pages.set(page);
            if self.advancing {
                let expected = if page == 1 {
                    Value::Null
                } else {
                    json!(previous.to_string())
                };
                assert_eq!(request.pointer("/variables/cur").unwrap(), &expected);
                json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                    "nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":page.to_string()}
                }}}}})
            } else {
                self.page.clone()
            }
        } else {
            let key = match route.as_str() {
                "repos/octo/widget/pulls/7" => "pullRequest",
                "repos/octo/widget/pulls/7/reviews?per_page=100" => "reviews",
                "repos/octo/widget/issues/7/comments?per_page=100" => "issueComments",
                "repos/octo/widget/pulls/7/requested_reviewers?per_page=100" => {
                    "requestedReviewers"
                }
                _ => panic!("unexpected read after malformed connection: {route}"),
            };
            let response = self.responses.get(key).unwrap();
            if ["reviews", "issueComments", "requestedReviewers"].contains(&key) {
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

/// Prevents unexpected retry behavior from using real time.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("malformed page must not be retried")
    }
}

/// Captures through offline boundaries and returns the refusal.
fn capture(provider: &Provider) -> AppError {
    let target = capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap();
    collect_capture(&target, None, provider, provider, &NoWait).unwrap_err()
}

/// Creates the recorded initial responses independently for each scenario.
fn provider(page: Value, advancing: bool) -> Provider {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/github.json"
    ))
    .unwrap();
    Provider {
        responses: fixture.get("responses").unwrap().clone(),
        page,
        advancing,
        pages: Cell::new(0),
    }
}

#[test]
fn malformed_thread_connections_fail_instead_of_truncating() {
    let no_page = provider(Value::Null, false);
    assert_eq!(classification(&capture(&no_page)), "service");
    assert_eq!(no_page.pages.get(), 1);

    for connection in [
        Value::Null,
        json!({"pageInfo":{"hasNextPage":false,"endCursor":null}}),
        json!({"nodes":[]}),
        json!({"nodes":[],"pageInfo":{"endCursor":"cursor"}}),
        json!({"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":null}}),
    ] {
        let page = if connection.is_null() {
            json!({"data":{"repository":{"pullRequest":{}}}})
        } else {
            json!({"data":{"repository":{"pullRequest":{"reviewThreads":connection}}}})
        };
        let provider = provider(page, false);
        assert_eq!(classification(&capture(&provider)), "service");
        assert_eq!(provider.pages.get(), 1);
    }
}

#[test]
fn advancing_cursors_cannot_exceed_the_connection_page_limit() {
    let provider = provider(Value::Null, true);
    let error = capture(&provider);
    assert_eq!(classification(&error), "service");
    assert!(error.to_string().contains("page limit"));
    assert_eq!(provider.pages.get(), 1000);
}
