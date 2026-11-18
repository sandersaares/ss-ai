//! Offline tracker command transactions through named file inputs.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "Fixtures use known JSON shapes and assert rejected commands"
)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, from_slice, from_str, json, to_vec};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput, run};
use tempfile::{Builder, TempDir};

/// Isolates named inputs and the durable journal within the checkout.
struct Fixture(TempDir);
impl Fixture {
    fn new(count: usize) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
        fs::create_dir_all(&root).unwrap();
        let fixture = Self(
            Builder::new()
                .prefix("tracker 雪 ")
                .tempdir_in(root)
                .unwrap(),
        );
        let findings: Vec<Value> = (0..count).map(|i| {
            let id = format!("F{i}");
            json!({"schemaVersion":1,"id":id,"sourceFindingIds":[id],"origin":"reviewer",
                "title":format!("Finding {i} 雪 😀"),"description":"Problem café 雪 with enough context for a meaningful description.",
                "reasoning":"Reason", "consequence":"Impact", "recommendedAction":"Fix",
                "anchor":{"kind":"file","path":"src/lib.rs"}})
        }).collect();
        fixture.write(
            "approved 雪.json",
            &json!({"schemaVersion":2,"target":null,
            "reviewedSourceCommit":null,"reviewedMergeBaseCommit":null,"reviewedContextHash":null,
            "findings":findings}),
        );
        fixture.write("diagnostics 雪.json", &json!({}));
        fixture
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }
    fn write(&self, name: &str, value: &Value) {
        fs::write(self.path(name), to_vec(value).unwrap()).unwrap();
    }
    fn journal(&self) -> Value {
        from_slice(&fs::read(self.path("output 雪/tracker-publication-result.json")).unwrap())
            .unwrap()
    }
    fn args(&self, ado: bool) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "ss-ai",
            "review",
            "publish-tracker-items",
            "--url",
            if ado {
                "https://dev.azure.com/org/Project"
            } else {
                "https://github.com/acme/tracker"
            },
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        for (flag, name) in [
            ("--approved-findings-path", "approved 雪.json"),
            ("--diagnostics-path", "diagnostics 雪.json"),
            ("--output-directory", "output 雪"),
        ] {
            args.push(flag.into());
            args.push(self.path(name).into_os_string());
        }
        if ado {
            args.extend(["--work-item-type", "Bug", "--area-path", "Project"].map(OsString::from));
        }
        args
    }
}

/// Records effects and checks that every mutation has a persisted intent.
struct Provider<'a> {
    fixture: &'a Fixture,
    ado: bool,
    fail_second: bool,
    rewrite: bool,
    calls: RefCell<Vec<String>>,
    items: RefCell<Vec<Value>>,
}
impl<'a> Provider<'a> {
    fn new(fixture: &'a Fixture, ado: bool) -> Self {
        Self {
            fixture,
            ado,
            fail_second: false,
            rewrite: false,
            calls: RefCell::new(Vec::new()),
            items: RefCell::new(Vec::new()),
        }
    }
    fn create(&self, title: &str, body: &str) -> Option<Value> {
        let count = self.items.borrow().len();
        let journal = self.fixture.journal();
        assert_eq!(journal["status"], "inProgress");
        assert_eq!(journal["operations"][count]["outcome"], "ambiguous");
        if count > 0 {
            assert_eq!(journal["operations"][0]["outcome"], "succeeded");
        }
        self.calls.borrow_mut().push("create".into());
        if self.fail_second && count == 1 {
            return None;
        }
        let number = count.checked_add(1).unwrap();
        let body = if self.rewrite && count == 0 {
            format!("{body}!")
        } else {
            body.to_owned()
        };
        let item = json!({"id":format!("node-{number}"),"number":number,
            "url":format!("https://github.com/acme/tracker/issues/{number}"),"title":title,"body":body});
        self.items.borrow_mut().push(item.clone());
        Some(item)
    }
    fn read(&self, index: usize) -> Value {
        let journal = self.fixture.journal();
        assert_eq!(journal["operations"][index]["outcome"], "succeeded");
        self.calls.borrow_mut().push("read".into());
        self.items.borrow()[index].clone()
    }
}
impl Process for Provider<'_> {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if self.ado {
            assert_eq!(program, "az");
            self.calls.borrow_mut().push("auth".into());
            return Ok(ProcessOutput::new(true, b"token".to_vec(), vec![]));
        }
        assert_eq!(program, "gh");
        assert_eq!(arguments[1], "graphql");
        let request: Value = from_slice(input).unwrap();
        let query = request["query"].as_str().unwrap();
        let response = if query.contains("createIssue(") {
            let vars = &request["variables"];
            let Some(item) = self.create(
                vars["title"].as_str().unwrap(),
                vars["body"].as_str().unwrap(),
            ) else {
                return Ok(ProcessOutput::new(
                    false,
                    vec![],
                    b"connection lost".to_vec(),
                ));
            };
            json!({"data":{"createIssue":{"issue":item}}})
        } else if query.contains("node(id:") {
            let id = request["variables"]["issueId"].as_str().unwrap();
            let index = self
                .items
                .borrow()
                .iter()
                .position(|v| v["id"] == id)
                .unwrap();
            json!({"data":{"node":self.read(index)}})
        } else {
            self.calls.borrow_mut().push("preflight".into());
            json!({"data":{"repository":{"id":"repo","nameWithOwner":"acme/tracker",
                "hasIssuesEnabled":true,"isArchived":false,"viewerPermission":"WRITE"}}})
        };
        Ok(ProcessOutput::new(
            true,
            format!("HTTP/2 200\n\n{response}").into_bytes(),
            vec![],
        ))
    }
}
impl Http for Provider<'_> {
    fn send(
        &self,
        method: &str,
        url: &str,
        _: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert!(self.ado, "GitHub must not use HTTP");
        let response = if method == "POST" {
            let patch: Value = from_slice(body.unwrap()).unwrap();
            let field = |name: &str| {
                patch
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|p| p["path"] == name)
                    .unwrap()["value"]
                    .as_str()
                    .unwrap()
            };
            let item = self
                .create(
                    field("/fields/System.Title"),
                    field("/fields/System.Description"),
                )
                .unwrap();
            ado_item(&item)
        } else if url.contains("workitems/1?") {
            ado_item(&self.read(0))
        } else if url.contains("workitemtypes/Bug?") {
            json!({"fields":[{"referenceName":"System.Description"}]})
        } else if url.contains("workitemtypes?") {
            json!({"count":1,"value":[{"name":"Bug"}]})
        } else if url.contains("wit/fields/") {
            json!({"referenceName":"System.Description","type":"html"})
        } else if url.contains("classificationnodes/areas") || url.contains("_apis/projects/") {
            json!({"name":"Project"})
        } else {
            panic!("unexpected request {method} {url}")
        };
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            to_vec(&response).unwrap(),
        ))
    }
    fn send_json_patch(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        self.send(method, url, token, body)
    }
}
/// Models the Azure work item representation returned after a JSON Patch.
fn ado_item(item: &Value) -> Value {
    json!({"id":item["number"],"fields":{"System.Title":item["title"],"System.Description":item["body"]},
        "multilineFieldsFormat":{"System.Description":"Markdown"}})
}
/// Keeps retries and pacing independent of the wall clock.
struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Ok(())
    }
}
/// Rejects all provider access when local validation should fail.
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
        Err(io::Error::other("unexpected HTTP").into())
    }
}

#[test]
fn github_transaction_reads_back_exact_utf8_and_persists_receipts() {
    let fixture = Fixture::new(1);
    let provider = Provider::new(&fixture, false);
    let result: Value =
        from_str(&run(&fixture.args(false), &provider, &provider, &FixedClock).unwrap()).unwrap();
    assert_eq!(result["status"], "succeeded");
    assert_eq!(result["publishedItemCount"], 1);
    assert_eq!(
        *provider.calls.borrow(),
        ["preflight", "create", "read", "read"]
    );
    let item = &provider.items.borrow()[0];
    assert!(item["title"].as_str().unwrap().contains("雪 😀"));
    assert!(item["body"].as_str().unwrap().contains("café 雪"));
    let journal = fixture.journal();
    assert_eq!(journal["status"], "succeeded");
    assert_eq!(journal["verification"][0]["itemId"], "1");
}

#[test]
fn github_partial_ambiguous_failure_retains_receipt_and_blocks_replay() {
    let fixture = Fixture::new(2);
    let mut provider = Provider::new(&fixture, false);
    provider.fail_second = true;
    let result: Value =
        from_str(&run(&fixture.args(false), &provider, &provider, &FixedClock).unwrap()).unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(result["publishedItemCount"], 1);
    assert_eq!(result["ambiguousOperationCount"], 1);
    assert!(result["summary"].is_null());
    let journal = fixture.journal();
    assert_eq!(journal["operations"][0]["outcome"], "succeeded");
    assert_eq!(journal["operations"][1]["outcome"], "ambiguous");
    let offline = Offline(Cell::new(false));
    assert!(run(&fixture.args(false), &offline, &offline, &FixedClock).is_err());
    assert!(!offline.0.get());
}

#[test]
fn invalid_local_inputs_never_authenticate_or_contact_a_provider() {
    for contents in [b"".as_slice(), b"{", b"{}", b"[]"] {
        let fixture = Fixture::new(1);
        fs::write(fixture.path("approved 雪.json"), contents).unwrap();
        for ado in [false, true] {
            let offline = Offline(Cell::new(false));
            assert!(run(&fixture.args(ado), &offline, &offline, &FixedClock).is_err());
            assert!(!offline.0.get());
        }
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn executable_rejects_bad_named_input_on_a_bounded_stack() {
    let fixture = Fixture::new(1);
    fs::write(fixture.path("approved 雪.json"), b"{").unwrap();
    #[cfg(target_os = "linux")]
    let mut command = {
        // Match the constrained executable budget used by final_review_cli;
        // disable core dumps so a failure does not leave large artifacts.
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "ulimit -c 0; ulimit -s 768; exec \"$@\"",
            "sh",
            env!("CARGO_BIN_EXE_ss-ai"),
        ]);
        command
    };
    #[cfg(not(target_os = "linux"))]
    let mut command = Command::new(env!("CARGO_BIN_EXE_ss-ai"));
    let output = command
        .env("PATH", fixture.0.path())
        .args(fixture.args(false).iter().skip(1))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.status.code().is_some());
    assert!(output.stdout.is_empty());
}

#[test]
fn ado_explicit_acceptance_resumes_only_the_exact_rewrite_without_recreation() {
    let fixture = Fixture::new(1);
    let mut provider = Provider::new(&fixture, true);
    provider.rewrite = true;
    let mut args = fixture.args(true);
    let paused: Value = from_str(&run(&args, &provider, &provider, &FixedClock).unwrap()).unwrap();
    assert_eq!(paused["status"], "needsAdjudication");
    assert_eq!(paused["publishedItemCount"], 1);
    let a = &paused["adjudication"];
    args.extend([
        "--accept-stored-rewrite".into(),
        "--accept-work-item-id".into(),
        a["itemId"].as_str().unwrap().into(),
        "--accept-expected-body-hash".into(),
        a["expectedBodyHash"].as_str().unwrap().into(),
        "--accept-stored-body-hash".into(),
        a["storedBodyHash"].as_str().unwrap().into(),
    ]);
    let mut bad = args.clone();
    *bad.last_mut().unwrap() = "0".repeat(64).into();
    let offline = Offline(Cell::new(false));
    assert!(run(&bad, &offline, &offline, &FixedClock).is_err());
    assert!(!offline.0.get());
    let response = run(&args, &provider, &provider, &FixedClock)
        .unwrap_or_else(|error| panic!("{error}; journal: {}", fixture.journal()));
    let response: Value = from_str(&response).unwrap();
    assert_eq!(response["status"], "succeeded");
    assert_eq!(
        provider
            .calls
            .borrow()
            .iter()
            .filter(|c| *c == "create")
            .count(),
        1
    );
    assert_eq!(fixture.journal()["status"], "succeeded");
}
