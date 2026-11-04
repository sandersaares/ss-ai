//! Exercises complete capture commands with recorded provider responses.
#![allow(
    clippy::indexing_slicing,
    reason = "Recorded fixtures have fixed, known shapes"
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs::{read, remove_file};
use std::io::Result as IoResult;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::id as process_id;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_slice, from_str, json, to_vec};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput, classification, run};
use ss_ai_review_context::document_hash;

/// Routes recorded provider reads without touching external services.
struct Provider {
    github: RefCell<BTreeMap<String, VecDeque<(Value, Value)>>>,
    azure: RefCell<BTreeMap<String, VecDeque<AzurePage>>>,
}

/// Holds a page and its request and response continuation tokens.
type AzurePage = (Option<String>, Value, Option<String>);

impl Provider {
    /// Keeps pagination order within each connection, not across unrelated reads.
    fn github_response(&self, route: &str, cursor: Value, response: Value) {
        self.github
            .borrow_mut()
            .entry(route.into())
            .or_default()
            .push_back((cursor, response));
    }

    fn azure_response(
        &self,
        route: &str,
        continuation: Option<&str>,
        response: Value,
        next: Option<&str>,
    ) {
        self.azure
            .borrow_mut()
            .entry(route.into())
            .or_default()
            .push_back((
                continuation.map(str::to_owned),
                response,
                next.map(str::to_owned),
            ));
    }

    fn assert_complete(&self) {
        assert!(self.github.borrow().values().all(VecDeque::is_empty));
        assert!(self.azure.borrow().values().all(VecDeque::is_empty));
    }
}

/// Identifies the independently paginated GraphQL connection and thread.
fn graphql_route(request: &Value) -> String {
    let query = request["query"].as_str().unwrap();
    assert!(query.starts_with("query"));
    let connection = if query.contains("reviewThreads(") {
        "reviewThreads"
    } else if query.contains("timelineItems(") {
        "timeline"
    } else if query.contains("reviews(") {
        "reviewEdits"
    } else if query.contains("node(id:$threadId)") {
        return format!(
            "threadComments:{}",
            request["variables"]["threadId"].as_str().unwrap()
        );
    } else if query.contains("comments(") {
        "issueEdits"
    } else {
        panic!("unexpected GraphQL query: {query}");
    };
    connection.into()
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> IoResult<ProcessOutput> {
        if program == "az" {
            assert_eq!(&arguments[..2], ["account", "get-access-token"]);
            return Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]));
        }
        assert_eq!(program, "gh");
        assert_eq!(arguments[0], "api");
        let route = &arguments[1];
        let method = &arguments
            .windows(2)
            .find(|pair| pair[0] == "--method")
            .unwrap()[1];
        assert_eq!(method, if route == "graphql" { "POST" } else { "GET" });
        let (key, cursor) = if route == "graphql" {
            let request: Value = from_slice(input).unwrap();
            let cursor = request["variables"]
                .get("cur")
                .expect("missing GraphQL cursor");
            assert!(cursor.is_null() || cursor.is_string());
            (graphql_route(&request), cursor.clone())
        } else {
            assert!(input.is_empty());
            (route.clone(), Value::Null)
        };
        let (expected_cursor, response) = self
            .github
            .borrow_mut()
            .get_mut(&key)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| panic!("unexpected GitHub request: {key}"));
        assert_eq!(cursor, expected_cursor, "unexpected cursor for {key}");
        Ok(ProcessOutput::new(true, to_vec(&response).unwrap(), vec![]))
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
        let mut routes = self.azure.borrow_mut();
        let matches: Vec<_> = routes
            .keys()
            .filter(|route| url.contains(route.as_str()))
            .cloned()
            .collect();
        assert_eq!(matches.len(), 1, "unexpected or ambiguous URL: {url}");
        let (expected, response, next) = routes
            .get_mut(&matches[0])
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected Azure request: {url}"));
        let continuation = url
            .split(['?', '&'])
            .find_map(|part| part.strip_prefix("continuationToken="));
        assert_eq!(
            continuation,
            expected.as_deref(),
            "unexpected continuation for {url}"
        );
        let mut headers = BTreeMap::new();
        if let Some(next) = next {
            headers.insert("x-ms-continuationtoken".into(), next);
        }
        Ok(HttpResponse::new(200, headers, to_vec(&response).unwrap()))
    }
}

/// Uses the fixture instant and rejects unexpected retry waits.
struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2026-08-04T12:00:00Z".parse().unwrap()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected retry")
    }
}

/// Removes only test-owned files, leaving no shared lock in use.
struct Output {
    prefix: String,
    path: PathBuf,
}

impl Output {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The atomic provides unique file names and does not synchronize data.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let prefix = format!(".cli-capture-{}-{id}-", process_id());
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("{prefix}pr-context.json"));
        Self { prefix, path }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        for suffix in ["", ".lock", ".authorship.json", ".authorship.json.lock"] {
            let mut path = self.path.as_os_str().to_os_string();
            path.push(suffix);
            drop(remove_file(path));
        }
    }
}

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ss-review-native-capture")
        .join(name);
    from_slice(&read(path).unwrap()).unwrap()
}

/// Independent endpoints may interleave, while each connection keeps its cursor.
#[test]
fn provider_routes_independent_reads_and_checks_continuations() {
    let provider = Provider {
        github: RefCell::new(BTreeMap::new()),
        azure: RefCell::new(BTreeMap::new()),
    };
    provider.github_response("first", Value::Null, json!("first"));
    provider.github_response("second", Value::Null, json!("second"));
    provider.github_response("reviewThreads", Value::Null, json!("threads"));
    provider.github_response("issueEdits", Value::Null, json!("edits"));
    provider.github_response("threadComments:A", json!("a1"), json!("A1"));
    provider.github_response("threadComments:A", json!("a2"), json!("A2"));
    provider.github_response("threadComments:B", json!("b1"), json!("B1"));
    provider.azure_response(
        "/iterations?",
        None,
        json!("iterations"),
        Some("next+token"),
    );
    provider.azure_response("/iterations?", Some("next%2Btoken"), json!("next"), None);
    provider.azure_response("/threads?", None, json!("threads"), None);

    let rest = |route: &str| {
        let args = ["api", route, "--method", "GET"].map(String::from);
        let result = provider.execute("gh", &args, &[]).unwrap();
        from_slice::<Value>(&result.stdout).unwrap()
    };
    assert_eq!(rest("second"), "second");
    assert_eq!(rest("first"), "first");
    let connection = |query: &str| {
        let args = ["api", "graphql", "--method", "POST"].map(String::from);
        let request = json!({"query":query,"variables":{"cur":null}});
        let result = provider
            .execute("gh", &args, &to_vec(&request).unwrap())
            .unwrap();
        from_slice::<Value>(&result.stdout).unwrap()
    };
    assert_eq!(
        connection("query { comments(first:1) { nodes { id } } }"),
        "edits"
    );
    assert_eq!(
        connection("query { reviewThreads(first:1) { nodes { id } } }"),
        "threads"
    );
    let thread = |id: &str, cursor: &str| {
        let args = ["api", "graphql", "--method", "POST"].map(String::from);
        let request = json!({"query":"query { node(id:$threadId) { comments(first:1) { nodes { id } } } }",
            "variables":{"threadId":id,"cur":cursor}});
        let result = provider
            .execute("gh", &args, &to_vec(&request).unwrap())
            .unwrap();
        from_slice::<Value>(&result.stdout).unwrap()
    };
    assert_eq!(thread("B", "b1"), "B1");
    assert_eq!(thread("A", "a1"), "A1");
    assert_eq!(thread("A", "a2"), "A2");
    let azure = |url: &str| {
        let result = provider.send("GET", url, "fixture-token", None).unwrap();
        if url.contains("/iterations?") && !url.contains("continuationToken=") {
            assert_eq!(result.headers["x-ms-continuationtoken"], "next+token");
        }
        from_slice::<Value>(&result.body).unwrap()
    };
    assert_eq!(azure("https://example/threads?api-version=7.1"), "threads");
    assert_eq!(
        azure("https://example/iterations?api-version=7.1"),
        "iterations"
    );
    assert_eq!(
        azure("https://example/iterations?api-version=7.1&continuationToken=next%2Btoken"),
        "next"
    );
    provider.assert_complete();

    catch_unwind(AssertUnwindSafe(|| rest("first"))).unwrap_err();

    provider.github_response("threadComments:A", json!("expected"), json!(null));
    catch_unwind(AssertUnwindSafe(|| thread("A", "wrong"))).unwrap_err();
    provider.azure_response("/iterations?", Some("expected"), json!(null), None);
    catch_unwind(AssertUnwindSafe(|| {
        azure("https://example/iterations?continuationToken=wrong")
    }))
    .unwrap_err();
}

/// Rejects malformed caller input before provider access or publication.
#[test]
fn malformed_capture_inputs_are_classified_as_invalid_input() {
    let provider = Provider {
        github: RefCell::new(BTreeMap::new()),
        azure: RefCell::new(BTreeMap::new()),
    };
    let output = Output::new();
    let create = [
        "ss-ai",
        "context",
        "create",
        "--url",
        "https://github.com/octo/widget/pull/7",
        "--output-directory",
        env!("CARGO_MANIFEST_DIR"),
        "--output-prefix",
        &output.prefix,
        "--now-utc",
        "not-a-timestamp",
    ]
    .map(OsString::from);
    let error = run(&create, &provider, &provider, &FixedClock).unwrap_err();
    assert_eq!(classification(&error), "invalidInput");
    assert!(!output.path.exists());

    let zero_id = [
        "ss-ai",
        "context",
        "create",
        "--url",
        "https://github.com/octo/widget/pull/7",
        "--pull-request-id",
        "0",
        "--output-directory",
        env!("CARGO_MANIFEST_DIR"),
        "--output-prefix",
        &output.prefix,
    ]
    .map(OsString::from);
    let error = run(&zero_id, &provider, &provider, &FixedClock).unwrap_err();
    assert_eq!(classification(&error), "invalidInput");
    assert!(!output.path.exists());

    let authorship = [
        "ss-ai",
        "context",
        "set-authorship",
        "--path",
        output.path.to_str().unwrap(),
        "--snapshot-id",
        "unused",
        "--classification",
        "{",
    ]
    .map(OsString::from);
    let error = run(&authorship, &provider, &provider, &FixedClock).unwrap_err();
    assert_eq!(classification(&error), "invalidInput");
    assert!(!output.path.exists());
    provider.assert_complete();
}

/// Tests CLI target resolution, provider collection, and immutable publication together.
#[test]
fn create_matches_powershell_captures_for_both_providers() {
    for (name, platform) in [("github", "gitHub"), ("azure-devops", "azureDevOps")] {
        let data = fixture(&format!("{name}.json"));
        let expected = fixture(&format!("{name}-expected.json"));
        let responses = &data["responses"];
        let provider = Provider {
            github: RefCell::new(BTreeMap::new()),
            azure: RefCell::new(BTreeMap::new()),
        };
        if platform == "gitHub" {
            for (route, response) in [
                (
                    "repos/octo/widget/pulls/7",
                    responses["pullRequest"].clone(),
                ),
                (
                    "repos/octo/widget/pulls/7/reviews?per_page=100",
                    json!([responses["reviews"]]),
                ),
                (
                    "repos/octo/widget/issues/7/comments?per_page=100",
                    json!([responses["issueComments"]]),
                ),
                (
                    "repos/octo/widget/pulls/7/requested_reviewers?per_page=100",
                    json!([responses["requestedReviewers"]]),
                ),
                ("reviewThreads", responses["reviewThreads"].clone()),
                ("issueEdits", responses["issueEdits"].clone()),
                ("reviewEdits", responses["reviewEdits"].clone()),
                ("timeline", responses["timeline"].clone()),
                ("user", responses["user"].clone()),
                (
                    "repos/octo/widget/compare/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb...aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    responses["compare"].clone(),
                ),
            ] {
                provider.github_response(route, Value::Null, response);
            }
        } else {
            for (route, response) in [
                ("_apis/connectionData", responses["connectionData"].clone()),
                ("pullRequests/4242?", responses["pullRequest"].clone()),
                ("/iterations?", responses["iterations"].clone()),
                ("/reviewers?", responses["reviewers"].clone()),
                ("/threads?", responses["threads"].clone()),
            ] {
                provider.azure_response(route, None, response, None);
            }
        }
        let output = Output::new();
        let url = expected["pullRequestUrl"].as_str().unwrap();
        let arguments = [
            "ss-ai",
            "context",
            "create",
            "--url",
            url,
            "--platform",
            platform,
            "--output-directory",
            env!("CARGO_MANIFEST_DIR"),
            "--output-prefix",
            &output.prefix,
        ]
        .map(OsString::from);
        let result: Value =
            from_str(&run(&arguments, &provider, &provider, &FixedClock).unwrap()).unwrap();
        let capture: Value = from_slice(&read(&output.path).unwrap()).unwrap();
        assert_eq!(capture, expected, "{name}");
        assert_eq!(
            document_hash(&capture).unwrap(),
            document_hash(&expected).unwrap(),
            "{name}"
        );
        assert_eq!(result["contextPath"], output.path.to_str().unwrap());
        provider.assert_complete();
        run(&arguments, &provider, &provider, &FixedClock).unwrap_err();
        let other_output = Output::new();
        let other_url = if platform == "gitHub" {
            "https://github.com/octo/widget/pull/8"
        } else {
            "https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4243"
        };
        let baseline = output.path.to_str().unwrap();
        let other_arguments = [
            "ss-ai",
            "context",
            "create",
            "--url",
            other_url,
            "--platform",
            platform,
            "--output-directory",
            env!("CARGO_MANIFEST_DIR"),
            "--output-prefix",
            &other_output.prefix,
            "--compare-path",
            baseline,
        ]
        .map(OsString::from);
        run(&other_arguments, &provider, &provider, &FixedClock).unwrap_err();
        assert!(!other_output.path.exists());
    }
}
