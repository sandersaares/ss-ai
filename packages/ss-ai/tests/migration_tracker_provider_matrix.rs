//! Offline tracker provider evidence through the native command boundary.
#![allow(
    clippy::indexing_slicing,
    reason = "The tests index fixed JSON fixtures and check their shapes through assertions"
)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput, run};
use tempfile::{Builder, TempDir};

/// Keeps file-based inputs and their journal inside the checkout.
struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dir = Builder::new()
            .prefix(".tracker-provider-migration-")
            .tempdir_in(root)
            .unwrap();
        let fixture = Self(dir);
        fixture.write(
            "approved 雪.json",
            &json!({"schemaVersion":2,"target":null,"reviewedSourceCommit":null,
                "reviewedMergeBaseCommit":null,"reviewedContextHash":null,
                "findings":[{"schemaVersion":1,"id":"F","sourceFindingIds":["F"],
                    "origin":"reviewer","title":"A title 雪","description":"Description",
                    "reasoning":"Reason","consequence":"Impact","recommendedAction":"Fix",
                    "anchor":{"kind":"file","path":"src/lib.rs"}}]}),
        );
        fixture.write("diagnostics.json", &json!({}));
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }

    fn write(&self, name: &str, value: &Value) {
        fs::write(self.path(name), serde_json::to_vec(value).unwrap()).unwrap();
    }

    fn journal(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("out/tracker-publication-result.json")).unwrap())
            .unwrap()
    }

    fn args(&self, azure: bool) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "ss-ai",
            "review",
            "publish-tracker-items",
            "--url",
            if azure {
                "https://dev.azure.com/org/Project"
            } else {
                "https://github.com/acme/tracker"
            },
            "--now-utc",
            "2026-01-01T00:00:00Z",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        for (flag, name) in [
            ("--approved-findings-path", "approved 雪.json"),
            ("--diagnostics-path", "diagnostics.json"),
            ("--output-directory", "out"),
        ] {
            args.push(flag.into());
            args.push(self.path(name).into_os_string());
        }
        if azure {
            args.extend(["--work-item-type", "task", "--area-path", "project"].map(Into::into));
        }
        args
    }
}

/// Prohibits unexpected waiting during deterministic provider scenarios.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected retry or pacing")
    }
}

/// Reports one GitHub mutation response while recording its durable intent.
struct GitHub<'a> {
    fixture: &'a Fixture,
    success: bool,
    body: Value,
    calls: RefCell<Vec<String>>,
}

impl Process for GitHub<'_> {
    fn execute(&self, program: &str, _: &[String], input: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        let request: Value = serde_json::from_slice(input).unwrap();
        let query = request.get("query").unwrap().as_str().unwrap();
        let response = if query.contains("createIssue(") {
            assert!(!self.calls.borrow().iter().any(|call| call == "create"));
            self.calls.borrow_mut().push("create".into());
            assert_eq!(
                self.fixture.journal()["operations"][0]["outcome"],
                "ambiguous"
            );
            return Ok(ProcessOutput::new(
                self.success,
                self.body.to_string().into_bytes(),
                Vec::new(),
            ));
        } else {
            assert!(
                self.calls.borrow().is_empty(),
                "unexpected repeated preflight"
            );
            self.calls.borrow_mut().push("preflight".into());
            json!({"data":{"repository":{"id":"repo","nameWithOwner":"acme/tracker",
                "hasIssuesEnabled":true,"isArchived":false,"viewerPermission":"WRITE"}}})
        };
        Ok(ProcessOutput::new(
            true,
            format!("HTTP/2 200\n\n{response}").into_bytes(),
            Vec::new(),
        ))
    }
}

impl Http for GitHub<'_> {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub uses the CLI")
    }
}

/// Refuses any provider effect when checking the replay guard.
struct Offline(Cell<bool>);

impl Process for Offline {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        self.0.set(true);
        Err(io::Error::other("unexpected provider"))
    }
}

impl Http for Offline {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        self.0.set(true);
        Err(io::Error::other("unexpected provider").into())
    }
}

/// Uncertain GraphQL responses retain intent and never authorize replay.
#[test]
fn github_graphql_errors_without_http_refusal_keep_ambiguous_intent() {
    for (success, body) in [
        (false, json!({"errors":[{"message":"Body too long"}]})),
        (
            true,
            json!({"errors":[{"message":"Resource not accessible"}]}),
        ),
        (
            true,
            json!({"data":{"createIssue":null},"errors":[{"message":"Execution failed"}]}),
        ),
    ] {
        let fixture = Fixture::new();
        let provider = GitHub {
            fixture: &fixture,
            success,
            body,
            calls: RefCell::new(Vec::new()),
        };
        let output = run(&fixture.args(false), &provider, &provider, &NoWait).unwrap();
        let outcome: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(outcome["status"], "failed");
        assert_eq!(outcome["ambiguousOperationCount"], 1);
        assert_eq!(fixture.journal()["operations"][0]["outcome"], "ambiguous");
        assert_eq!(*provider.calls.borrow(), ["preflight", "create"]);
        let offline = Offline(Cell::new(false));
        run(&fixture.args(false), &offline, &offline, &NoWait).unwrap_err();
        assert!(!offline.0.get());
    }
}

/// Injects one lost read response without repeating the completed creation.
struct GitHubReadRetry {
    item: RefCell<Option<Value>>,
    calls: RefCell<Vec<String>>,
    first_read: Cell<bool>,
    changed_field: Option<&'static str>,
    reads: Cell<usize>,
    waits: Cell<usize>,
}

impl Process for GitHubReadRetry {
    fn execute(&self, _: &str, _: &[String], input: &[u8]) -> io::Result<ProcessOutput> {
        let request: Value = serde_json::from_slice(input).unwrap();
        let query = request.get("query").unwrap().as_str().unwrap();
        let response = if query.contains("createIssue(") {
            assert!(!self.calls.borrow().iter().any(|call| call == "create"));
            self.calls.borrow_mut().push("create".into());
            let vars = request.get("variables").unwrap();
            let item = json!({"id":"node","number":1,"url":"https://github.com/acme/tracker/issues/1",
                "title":vars["title"],"body":vars["body"]});
            *self.item.borrow_mut() = Some(item.clone());
            json!({"data":{"createIssue":{"issue":item}}})
        } else if query.contains("node(id:") {
            // One failed read, its retry, and final verification exhaust this fixture.
            assert!(self.reads.get() < 3, "unexpected repeated read");
            self.reads.set(self.reads.get().checked_add(1).unwrap());
            self.calls.borrow_mut().push("read".into());
            if self.first_read.replace(false) {
                return Ok(ProcessOutput::new(
                    false,
                    Vec::new(),
                    b"connection reset".to_vec(),
                ));
            }
            let mut item = self.item.borrow().as_ref().unwrap().clone();
            if let Some(field) = self.changed_field {
                item[field] = json!("different");
            }
            json!({"data":{"node":item}})
        } else {
            assert!(
                self.calls.borrow().is_empty(),
                "unexpected repeated preflight"
            );
            self.calls.borrow_mut().push("preflight".into());
            json!({"data":{"repository":{"id":"repo","nameWithOwner":"acme/tracker",
                "hasIssuesEnabled":true,"isArchived":false,"viewerPermission":"WRITE"}}})
        };
        Ok(ProcessOutput::new(
            true,
            format!("HTTP/2 200\n\n{response}").into_bytes(),
            Vec::new(),
        ))
    }
}

impl Http for GitHubReadRetry {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub uses the CLI")
    }
}

impl Clock for GitHubReadRetry {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        assert_eq!(self.waits.get(), 0, "unexpected repeated retry");
        self.waits.set(1);
        Ok(())
    }
}

/// Read-only retries retain the original creation and complete verification.
#[test]
fn github_lost_read_retries_without_recreating_the_issue() {
    let fixture = Fixture::new();
    let provider = GitHubReadRetry {
        item: RefCell::new(None),
        calls: RefCell::new(Vec::new()),
        first_read: Cell::new(true),
        changed_field: None,
        reads: Cell::new(0),
        waits: Cell::new(0),
    };
    let output = run(&fixture.args(false), &provider, &provider, &provider).unwrap();
    let outcome: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(outcome["status"], "succeeded");
    assert_eq!(provider.waits.get(), 1);
    assert_eq!(
        *provider.calls.borrow(),
        ["preflight", "create", "read", "read", "read"]
    );
    assert_eq!(fixture.journal()["operations"][0]["outcome"], "succeeded");
}

/// A changed readback stops the transaction without discarding the receipt.
#[test]
fn github_changed_title_or_body_stops_before_the_next_creation() {
    for field in ["title", "body"] {
        let fixture = Fixture::new();
        let path = fixture.path("approved 雪.json");
        let mut approved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let mut second = approved["findings"][0].clone();
        second["id"] = json!("F-2");
        second["sourceFindingIds"] = json!(["F-2"]);
        approved["findings"].as_array_mut().unwrap().push(second);
        fixture.write("approved 雪.json", &approved);
        let provider = GitHubReadRetry {
            item: RefCell::new(None),
            calls: RefCell::new(Vec::new()),
            first_read: Cell::new(false),
            changed_field: Some(field),
            reads: Cell::new(0),
            waits: Cell::new(0),
        };
        let output = run(&fixture.args(false), &provider, &provider, &NoWait).unwrap();
        let outcome: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(outcome["status"], "failed", "{field}");
        assert!(outcome["summary"].is_null());
        assert_eq!(*provider.calls.borrow(), ["preflight", "create", "read"]);
        let journal = fixture.journal();
        assert_eq!(journal["operations"][0]["outcome"], "succeeded");
        assert_eq!(journal["operations"][1]["outcome"], "pending");
        let offline = Offline(Cell::new(false));
        run(&fixture.args(false), &offline, &offline, &NoWait).unwrap_err();
        assert!(!offline.0.get());
    }
}

/// Date-shaped titles remain text through file preparation and provider calls.
#[test]
fn github_timestamp_shaped_finding_title_is_preserved() {
    let fixture = Fixture::new();
    let path = fixture.path("approved 雪.json");
    let mut approved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let title = "2026-01-01T00:00:00Z";
    approved["findings"][0]["title"] = json!(title);
    fixture.write("approved 雪.json", &approved);
    let provider = GitHubReadRetry {
        item: RefCell::new(None),
        calls: RefCell::new(Vec::new()),
        first_read: Cell::new(false),
        changed_field: None,
        reads: Cell::new(0),
        waits: Cell::new(0),
    };
    let output = run(&fixture.args(false), &provider, &provider, &NoWait).unwrap();
    let outcome: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(outcome["status"], "succeeded");
    assert_eq!(provider.item.borrow().as_ref().unwrap()["title"], title);
    assert_eq!(fixture.journal()["operations"][0]["title"], title);
}

/// Answers Azure preflight from an in-memory project and records every request.
struct Azure {
    fail: &'static str,
    status: u16,
    requests: RefCell<Vec<(String, String)>>,
    patch: RefCell<Option<Value>>,
    authenticated: Cell<bool>,
}

impl Azure {
    fn new(fail: &'static str, status: u16) -> Self {
        Self {
            fail,
            status,
            requests: RefCell::new(Vec::new()),
            patch: RefCell::new(None),
            authenticated: Cell::new(false),
        }
    }

    fn response(status: u16, body: &Value) -> HttpResponse {
        HttpResponse::new(status, BTreeMap::new(), serde_json::to_vec(body).unwrap())
    }
}

impl Process for Azure {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "az");
        assert!(
            !self.authenticated.replace(true),
            "unexpected repeated authentication"
        );
        Ok(ProcessOutput::new(true, b"token".to_vec(), Vec::new()))
    }
}

impl Http for Azure {
    fn send(
        &self,
        method: &str,
        url: &str,
        _: &str,
        _: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        // These fixtures return no transient read failure, so repeating one cannot make progress.
        assert!(
            !self
                .requests
                .borrow()
                .iter()
                .any(|(m, u)| m == method && u == url),
            "unexpected repeated Azure read"
        );
        self.requests.borrow_mut().push((method.into(), url.into()));
        assert_eq!(method, "GET");
        let (point, value) = if url.contains("_apis/projects/") {
            ("project", json!({"name":"Project"}))
        } else if url.contains("workitemtypes/Task?") {
            (
                "type-detail",
                if self.fail == "missing-field" {
                    json!({"fields":[]})
                } else {
                    json!({"fields":[{"referenceName":"System.Description"}]})
                },
            )
        } else if url.contains("workitemtypes?") {
            (
                "types",
                if self.fail == "missing-type" {
                    json!({"count":0,"value":[]})
                } else {
                    json!({"count":1,"value":[{"name":"Task"}]})
                },
            )
        } else if url.contains("wit/fields/") {
            (
                "field",
                json!({"referenceName":"System.Description","type":"html"}),
            )
        } else if url.contains("classificationnodes/areas") {
            ("area", json!({"name":"Project"}))
        } else if url.contains("_apis/wit/workitems/9?") {
            (
                "parent",
                if self.fail == "parent-url" {
                    json!({"id":9,"fields":{"System.TeamProject":"Project","System.Title":"Parent"}})
                } else {
                    json!({"id":9,"url":"https://dev.azure.com/org/_apis/wit/workItems/9",
                        "fields":{"System.TeamProject":if self.fail == "foreign-parent" { "Other" } else { "Project" },
                            "System.Title":"Parent"}})
                },
            )
        } else {
            panic!("unexpected Azure request: {url}")
        };
        if point == self.fail {
            return Ok(Self::response(404, &json!({"message":"not found"})));
        }
        Ok(Self::response(200, &value))
    }

    fn send_json_patch(
        &self,
        method: &str,
        url: &str,
        _: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert!(
            self.patch.borrow().is_none(),
            "unexpected repeated Azure creation"
        );
        self.requests.borrow_mut().push((method.into(), url.into()));
        *self.patch.borrow_mut() = Some(serde_json::from_slice(body.unwrap()).unwrap());
        Ok(Self::response(
            self.status,
            &json!({"message":"creation failed"}),
        ))
    }
}

/// Missing Azure metadata must never reach an irreversible creation.
#[test]
fn azure_missing_type_field_area_and_parent_stop_before_creation() {
    for point in [
        "types",
        "missing-type",
        "type-detail",
        "missing-field",
        "field",
        "area",
        "parent",
        "foreign-parent",
        "parent-url",
    ] {
        let fixture = Fixture::new();
        let provider = Azure::new(point, 500);
        let mut args = fixture.args(true);
        if ["parent", "foreign-parent", "parent-url"].contains(&point) {
            args.extend(["--parent-work-item-id", "9"].map(Into::into));
        }
        run(&args, &provider, &provider, &NoWait).unwrap_err();
        let requests = provider.requests.borrow();
        assert!(
            requests.iter().all(|(method, _)| method == "GET"),
            "{point}"
        );
        assert!(provider.patch.borrow().is_none());
    }
}

/// A definite refusal differs from a lost outcome; neither reaches another write.
#[test]
fn azure_creation_classifies_refusal_and_service_failure_without_replay() {
    for (status, expected) in [(400, "failed"), (500, "ambiguous")] {
        let fixture = Fixture::new();
        let provider = Azure::new("", status);
        let output = run(&fixture.args(true), &provider, &provider, &NoWait).unwrap();
        let outcome: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(outcome["status"], "failed");
        assert_eq!(fixture.journal()["operations"][0]["outcome"], expected);
        let requests = provider.requests.borrow();
        assert_eq!(
            requests
                .iter()
                .filter(|(method, _)| method == "POST")
                .count(),
            1
        );
        assert!(
            requests
                .iter()
                .any(|(_, url)| url.contains("classificationnodes/areas?"))
        );
        let patch = provider.patch.borrow();
        let patch = patch.as_ref().unwrap().as_array().unwrap();
        assert_eq!(patch.len(), 4);
        assert_eq!(patch[1]["path"], "/fields/System.Description");
        assert_eq!(patch[2]["value"], "Project");
        assert_eq!(patch[3]["value"], "Markdown");
    }
}

/// Returns a work item with the format Azure reports after creation.
struct AzureFormat {
    preflight: Azure,
    format: Option<&'static str>,
    item: RefCell<Option<Value>>,
    reads: Cell<usize>,
}

impl Process for AzureFormat {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        self.preflight.execute(program, arguments, input)
    }
}

impl Http for AzureFormat {
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        if url.contains("_apis/wit/workitems/12?") {
            // Successful publication reads once after creation and once for final verification.
            assert!(self.reads.get() < 2, "unexpected repeated Azure read");
            self.reads.set(self.reads.get().checked_add(1).unwrap());
            return Ok(Azure::response(200, self.item.borrow().as_ref().unwrap()));
        }
        self.preflight.send(method, url, token, body)
    }

    fn send_json_patch(
        &self,
        method: &str,
        _: &str,
        _: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(method, "POST");
        assert!(
            self.item.borrow().is_none(),
            "unexpected repeated Azure creation"
        );
        let patch: Value = serde_json::from_slice(body.unwrap()).unwrap();
        let mut item = json!({"id":12,"fields":{
            "System.Title":patch[0]["value"],"System.Description":patch[1]["value"]}});
        if let Some(format) = self.format {
            item["multilineFieldsFormat"] = json!({"System.Description":format});
        }
        *self.item.borrow_mut() = Some(item.clone());
        Ok(Azure::response(200, &item))
    }
}

/// Azure format verification must not advance a partially published review.
#[test]
fn azure_readback_requires_markdown_case_insensitively_before_next_creation() {
    for (format, expected) in [
        (Some("mArKdOwN"), "succeeded"),
        (None, "failed"),
        (Some("Html"), "failed"),
    ] {
        let fixture = Fixture::new();
        if expected == "failed" {
            let path = fixture.path("approved 雪.json");
            let mut approved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            let mut second = approved["findings"][0].clone();
            second["id"] = json!("F-2");
            second["sourceFindingIds"] = json!(["F-2"]);
            approved["findings"].as_array_mut().unwrap().push(second);
            fixture.write("approved 雪.json", &approved);
        }
        let provider = AzureFormat {
            preflight: Azure::new("", 200),
            format,
            item: RefCell::new(None),
            reads: Cell::new(0),
        };
        let output = run(&fixture.args(true), &provider, &provider, &NoWait).unwrap();
        let outcome: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(outcome["status"], expected, "{format:?}");
        let journal = fixture.journal();
        assert_eq!(journal["operations"][0]["outcome"], "succeeded");
        if expected == "failed" {
            assert_eq!(journal["operations"][1]["outcome"], "pending");
            assert!(outcome["summary"].is_null());
            let offline = Offline(Cell::new(false));
            run(&fixture.args(true), &offline, &offline, &NoWait).unwrap_err();
            assert!(!offline.0.get());
        }
    }
}
