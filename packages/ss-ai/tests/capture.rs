//! Exercises complete capture commands with recorded provider responses.
#![allow(
    clippy::indexing_slicing,
    reason = "Recorded fixtures have fixed, known shapes"
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput, run};
use ss_ai_review_context::document_hash;

/// Routes recorded provider reads without touching external services.
struct Provider {
    github: RefCell<VecDeque<(String, Value)>>,
    azure: RefCell<VecDeque<(String, Value)>>,
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if program == "az" {
            return Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]));
        }
        assert_eq!(program, "gh");
        let (route, response) = self
            .github
            .borrow_mut()
            .pop_front()
            .expect("unexpected GitHub request");
        assert_eq!(arguments[1], route);
        let method = &arguments
            .windows(2)
            .find(|pair| pair[0] == "--method")
            .unwrap()[1];
        assert_eq!(method, if route == "graphql" { "POST" } else { "GET" });
        if route == "graphql" {
            let request: Value = serde_json::from_slice(input).unwrap();
            assert!(request["query"].as_str().unwrap().starts_with("query"));
        } else {
            assert!(input.is_empty());
        }
        Ok(ProcessOutput::new(
            true,
            serde_json::to_vec(&response).unwrap(),
            vec![],
        ))
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
        assert_eq!(method, "GET");
        assert!(body.is_none());
        let (route, response) = self
            .azure
            .borrow_mut()
            .pop_front()
            .expect("unexpected Azure request");
        assert!(url.contains(&route), "unexpected URL: {url}");
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            serde_json::to_vec(&response).unwrap(),
        ))
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
        let prefix = format!(".cli-capture-{}-{id}-", std::process::id());
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
            drop(fs::remove_file(path));
        }
    }
}

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ss-review-native-capture")
        .join(name);
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Tests CLI target resolution, provider collection, and immutable publication together.
#[test]
fn create_matches_powershell_captures_for_both_providers() {
    for (name, platform) in [("github", "gitHub"), ("azure-devops", "azureDevOps")] {
        let data = fixture(&format!("{name}.json"));
        let expected = fixture(&format!("{name}-expected.json"));
        let responses = &data["responses"];
        let mut provider = Provider {
            github: RefCell::new(VecDeque::new()),
            azure: RefCell::new(VecDeque::new()),
        };
        if platform == "gitHub" {
            provider.github = RefCell::new(VecDeque::from([
                ("repos/octo/widget/pulls/7".into(), responses["pullRequest"].clone()),
                ("repos/octo/widget/pulls/7/reviews?per_page=100".into(), json!([responses["reviews"]])),
                ("repos/octo/widget/issues/7/comments?per_page=100".into(), json!([responses["issueComments"]])),
                ("repos/octo/widget/pulls/7/requested_reviewers?per_page=100".into(), json!([responses["requestedReviewers"]])),
                ("graphql".into(), responses["reviewThreads"].clone()),
                ("graphql".into(), responses["issueEdits"].clone()),
                ("graphql".into(), responses["reviewEdits"].clone()),
                ("graphql".into(), responses["timeline"].clone()),
                ("user".into(), responses["user"].clone()),
                ("repos/octo/widget/compare/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb...aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(), responses["compare"].clone()),
            ]));
        } else {
            provider.azure = RefCell::new(VecDeque::from([
                (
                    "_apis/connectionData".into(),
                    responses["connectionData"].clone(),
                ),
                (
                    "pullRequests/4242?".into(),
                    responses["pullRequest"].clone(),
                ),
                ("/iterations?".into(), responses["iterations"].clone()),
                ("/reviewers?".into(), responses["reviewers"].clone()),
                ("/threads?".into(), responses["threads"].clone()),
            ]));
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
            serde_json::from_str(&run(&arguments, &provider, &provider, &FixedClock).unwrap())
                .unwrap();
        let capture: Value = serde_json::from_slice(&fs::read(&output.path).unwrap()).unwrap();
        assert_eq!(capture, expected, "{name}");
        assert_eq!(
            document_hash(&capture).unwrap(),
            document_hash(&expected).unwrap(),
            "{name}"
        );
        assert_eq!(result["contextPath"], output.path.to_str().unwrap());
        assert!(provider.github.borrow().is_empty());
        assert!(provider.azure.borrow().is_empty());
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
