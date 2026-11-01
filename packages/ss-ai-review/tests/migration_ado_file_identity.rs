//! Exercises Azure iteration-change paging before any publication write.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_str, json, to_vec};
use ss_ai_review::{
    Clock, FinalReviewInput, Http, HttpResponse, Process, ProcessOutput, capture_target,
    classification, publish_final_review,
};
use ss_ai_runtime::FileTransaction;
use tempfile::tempdir_in;

/// Serves finite iteration pages and stops before publication can write.
struct Provider {
    responses: Value,
    pages: RefCell<VecDeque<Value>>,
    change_urls: RefCell<Vec<String>>,
    connections: Cell<usize>,
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
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(method, "GET", "preflight must not write");
        assert!(body.is_none());
        let value = if url.contains("/iterations/2/changes?") {
            self.change_urls.borrow_mut().push(url.to_owned());
            self.pages
                .borrow_mut()
                .pop_front()
                .expect("unexpected change page")
        } else if url.contains("/iterations?") {
            self.responses.get("iterations").unwrap().clone()
        } else if url.contains("/pullRequests/4242?") {
            self.responses.get("pullRequest").unwrap().clone()
        } else if url.contains("_apis/connectionData?") {
            let count = self.connections.get().checked_add(1).unwrap();
            self.connections.set(count);
            if count > 1 {
                return Ok(HttpResponse::new(
                    403,
                    BTreeMap::new(),
                    b"stop after paging".to_vec(),
                ));
            }
            self.responses.get("connectionData").unwrap().clone()
        } else {
            panic!("unexpected provider request: {url}")
        };
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            to_vec(&value).unwrap(),
        ))
    }
}

/// Refuses real-time waits while exercising the normal transaction boundary.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected retry or contention")
    }
}

/// Runs the public publication preflight with isolated checkout-local state.
fn preflight(pages: Vec<Value>, paths: &[&str], empty_iterations: bool) -> (AppError, Provider) {
    let fixture: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/azure-devops.json"
    ))
    .unwrap();
    let expected: Value = from_str(include_str!(
        "../../../tests/fixtures/ss-review-native-capture/azure-devops-context.json"
    ))
    .unwrap();
    let mut responses = fixture.get("responses").unwrap().clone();
    if empty_iterations {
        *responses.get_mut("iterations").unwrap() = json!({"count":0,"value":[]});
    }
    let provider = Provider {
        responses,
        pages: RefCell::new(pages.into()),
        change_urls: RefCell::new(Vec::new()),
        connections: Cell::new(0),
    };
    let target = capture_target(&json!({"url":"https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242"})).unwrap();
    let plan = paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            json!({
                "operation":"newFileThread","findingId":format!("F{index}"),"body":"finding",
                "anchor":{"kind":"file","path":path}
            })
        })
        .collect::<Vec<_>>();
    let input = FinalReviewInput {
        target: &target,
        plan: &plan,
        summary_body: "summary",
        expected_evidence_hash: "hash",
        expected_identity: expected.get("authenticatedIdentity").unwrap(),
        source_commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        merge_base_commit: "cccccccccccccccccccccccccccccccccccccccc",
        discard_pending_review: false,
        publish_closed: false,
        approve_merge: false,
        tenant: None,
    };
    let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let path = directory.path().join("result.json");
    let mut journal = FileTransaction::open(&path, &NoWait).unwrap();
    let error = publish_final_review(
        &mut journal,
        &path,
        &input,
        &provider,
        &provider,
        &NoWait,
        |_| panic!("capture is refused before hashing"),
    )
    .unwrap_err();
    (error, provider)
}

#[test]
fn cumulative_change_pages_keep_cursor_and_page_size() {
    let (error, provider) = preflight(
        vec![
            json!({"changeEntries":[{"changeTrackingId":1,"item":{"path":"/src/a.rs"}},{"changeTrackingId":2,"item":{"path":"/src/b.rs"}}],"nextSkip":2,"nextTop":1}),
            json!({"changeEntries":[{"changeTrackingId":3,"item":{"path":"\\src\\c.rs"}},{"changeTrackingId":4,"item":{"path":"/src/d.rs"}}],"nextSkip":4,"nextTop":1}),
            json!({"changeEntries":[{"changeTrackingId":5,"item":{"path":"/src/e.rs"}}]}),
        ],
        &["src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs", "src/e.rs"],
        false,
    );
    assert_eq!(classification(&error), "permissionDenied");
    assert!(error.to_string().contains("stop after paging"));
    let urls = provider.change_urls.borrow();
    assert_eq!(urls.len(), 3);
    assert!(
        urls.iter()
            .all(|url| url.contains("%24top=1000") && !url.contains("compareTo"))
    );
    assert!(!urls.first().unwrap().contains("%24skip"));
    assert!(urls.get(1).unwrap().contains("%24skip=2"));
    assert!(urls.get(2).unwrap().contains("%24skip=4"));
    assert!(provider.pages.borrow().is_empty());
}

#[test]
fn repeated_missing_or_empty_pages_do_not_fabricate_file_identity() {
    for (pages, expected) in [
        (
            vec![
                json!({"changeEntries":[{"item":{"path":"/other"},"changeTrackingId":1}],"nextSkip":1}),
                json!({"changeEntries":[],"nextSkip":1}),
            ],
            "cursor",
        ),
        (
            vec![json!({"message":"iteration missing"})],
            "changeEntries",
        ),
        (vec![json!({"changeEntries":[]})], "src/a.rs"),
    ] {
        let (error, provider) = preflight(pages, &["src/a.rs"], false);
        assert_eq!(classification(&error), "service");
        assert!(error.to_string().contains(expected), "{error}");
        assert_eq!(provider.connections.get(), 1);
    }
}

#[test]
fn publication_preflight_requires_an_iteration() {
    let (error, provider) = preflight(vec![], &["src/a.rs"], true);
    assert_eq!(classification(&error), "service");
    assert!(error.to_string().contains("No Azure DevOps iteration"));
    assert!(provider.change_urls.borrow().is_empty());
}
