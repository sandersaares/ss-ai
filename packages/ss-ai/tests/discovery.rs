//! Read-only provider fixtures and real persisted records for recency discovery.
//! Child-process coverage lives in `native.rs`: a fork can temporarily retain
//! another test thread's file locks until exec, defeating controlled contention.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Tests use bounded and explicit provider fixture shapes"
)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput, run};
use tempfile::TempDir;
use url::Url;

/// Holds a fixed cutoff, with unexpected retry requests failing immediately.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2026-09-28T10:00:00Z".parse().unwrap()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("No fixture response is transient")
    }
}

/// Allows only discovery reads and records every native provider operation.
struct GitHub {
    calls: RefCell<Vec<String>>,
    account: Cell<u64>,
    invalid_listing: bool,
    empty: bool,
    draft_only: bool,
}

impl GitHub {
    fn new() -> Self {
        Self {
            calls: RefCell::default(),
            account: Cell::new(42),
            invalid_listing: false,
            empty: false,
            draft_only: false,
        }
    }
}

impl Process for GitHub {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        let path = &arguments[1];
        let method = &arguments[3];
        self.calls.borrow_mut().push(path.clone());
        let response = match (method.as_str(), path.as_str()) {
            ("GET", "user") => {
                json!({"id": self.account.get(), "login": "reviewer", "type": "User"})
            }
            ("GET", "repos/example/widgets") => json!({"id": 123}),
            ("GET", "repos/example/widgets/pulls?state=open&per_page=100") => {
                if self.empty {
                    json!([[]])
                } else if self.draft_only {
                    json!([[{"number": 5, "draft": true}]])
                } else if self.invalid_listing {
                    json!([[{"number": 1}], "truncated page"])
                } else {
                    json!([
                        [{"number": 1, "state": "open", "draft": false, "title": "excluded old"},
                         {"number": 2, "state": "open", "draft": false, "title": "cutoff"}],
                        [{"number": 3, "state": "open", "draft": false, "title": "excluded future"},
                         {"number": 4, "state": "open", "draft": false, "title": "malformed"},
                         {"number": 5, "state": "open", "draft": true, "title": "draft"},
                         {"number": 6, "state": "open", "draft": false, "title": "paged recent"}]
                    ])
                }
            }
            ("POST", "graphql") => {
                let request: Value = serde_json::from_slice(input).unwrap();
                assert!(request["query"].as_str().unwrap().contains("committedDate"));
                assert!(!request["query"].as_str().unwrap().contains("review"));
                let variables = &request["variables"];
                let id = variables["num"].as_u64().unwrap();
                let next = id == 6 && variables["cur"].is_null();
                let dates = match id {
                    1 => vec![json!("2026-09-01T00:00:00Z")],
                    2 => vec![json!("2026-09-14T12:00:00+02:00")],
                    4 => vec![json!(17)],
                    6 if next => vec![
                        json!("2026-09-01T00:00:00Z"),
                        json!("2026-09-28T11:00:00+02:00"),
                    ],
                    3 | 6 => vec![json!("2026-10-01T00:00:00Z")],
                    _ => panic!("Unexpected PR"),
                };
                json!({"data": {"repository": {"pullRequest": {"commits": {
                    "nodes": dates.into_iter().map(|date| json!({"commit": {"committedDate": date}})).collect::<Vec<_>>(),
                    "pageInfo": {"hasNextPage": next, "endCursor": if next { Some("next") } else { None }}
                }}}}})
            }
            _ => panic!("Discovery attempted forbidden history, content or write: {method} {path}"),
        };
        Ok(ProcessOutput::new(
            true,
            serde_json::to_vec(&response).unwrap(),
            Vec::new(),
        ))
    }
}

/// No GitHub fixture can fall back to direct HTTP or obtain another identity.
struct NoHttp;

impl Http for NoHttp {
    fn send(
        &self,
        method: &str,
        url: &str,
        _: &str,
        _: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        panic!("Unexpected HTTP {method} {url}")
    }
}

fn arguments(root: &Path, repository: &str) -> Vec<OsString> {
    [
        "ss-ai".into(),
        "autoreapprove".into(),
        "discover".into(),
        "--repository-url".into(),
        repository.into(),
        "--state-path".into(),
        root.join("run.json").into_os_string(),
        "--policy-path".into(),
        root.join("policy.md").into_os_string(),
        "--dry-run".into(),
        "--limit".into(),
        "1".into(),
    ]
    .into()
}

fn fixture() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("policy.md"),
        "Trusted significance policy fixture.\n",
    )
    .unwrap();
    root
}

fn record(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(root.join("run.json")).unwrap()).unwrap()
}

fn result_arguments(root: &Path) -> Vec<OsString> {
    vec![
        "ss-ai".into(),
        "autoreapprove".into(),
        "results".into(),
        "--state-path".into(),
        root.join("run.json").into_os_string(),
    ]
}

/// Results and invalid invocations have no reason to run even a read-only provider.
struct NoProcess;

impl Process for NoProcess {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        panic!("Unexpected process: {program}")
    }
}

#[test]
fn github_discovery_accounts_for_every_page_without_reading_excluded_content() {
    let root = fixture();
    let provider = GitHub::new();
    let args = arguments(root.path(), "https://github.com/example/widgets");
    let summary: Value =
        serde_json::from_str(&run(&args, &provider, &NoHttp, &FixedClock).unwrap()).unwrap();
    assert_eq!(summary["state"], "partial");
    assert_eq!(
        summary["counts"],
        json!({"open": 5, "noRecentCommit": 2, "assessmentRequired": 2, "failed": 1, "pending": 0,
            "alreadyValid": 0, "noPriorApproval": 0, "ineligible": 0, "indeterminate": 0,
            "staleDuringExecution": 0, "needsReview": 0, "wouldReapprove": 0,
            "readyToReapprove": 0, "reapproved": 0})
    );
    assert_eq!(summary["items"].as_array().unwrap().len(), 1);
    assert_eq!(summary["items"][0]["pullRequestId"], 2);
    assert_eq!(summary["items"][0]["status"], "assessmentRequired");
    assert_eq!(summary["nextAfter"], 2);
    assert!(!summary.to_string().contains("excluded"));
    let state = record(root.path());
    assert_eq!(state["inventory"].as_array().unwrap().len(), 5);
    assert!(
        !state["inventory"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["pullRequestId"] == 5)
    );
    assert_eq!(
        state["inventory"][1]["recency"]["latestNonfutureCommit"],
        "2026-09-14T10:00:00.000000000Z"
    );
    assert_eq!(
        state["inventory"][4]["recency"]["latestNonfutureCommit"],
        "2026-09-28T09:00:00.000000000Z"
    );
    assert_eq!(state["inventory"][3]["recency"]["state"], "failed");
    assert_eq!(state["inventory"][3]["recency"]["evidenceComplete"], false);
    assert!(!state["inventory"][3]["recency"]["error"].is_null());
    assert_eq!(
        provider
            .calls
            .borrow()
            .iter()
            .filter(|call| *call == "graphql")
            .count(),
        6
    );

    // Recovery rereads identity/scope, but never repeats completed recency work.
    provider.calls.borrow_mut().clear();
    let resumed: Value =
        serde_json::from_str(&run(&args, &provider, &NoHttp, &FixedClock).unwrap()).unwrap();
    assert_eq!(resumed, summary);
    assert_eq!(
        provider.calls.borrow().as_slice(),
        ["user", "repos/example/widgets"]
    );
    let mut args = result_arguments(root.path());
    args.extend(["--after".into(), "2".into(), "--limit".into(), "2".into()]);
    let page: Value =
        serde_json::from_str(&run(&args, &NoProcess, &NoHttp, &FixedClock).unwrap()).unwrap();
    assert_eq!(page["items"][0]["pullRequestId"], 4);
    assert_eq!(page["items"][1]["pullRequestId"], 6);
    assert!(page["nextAfter"].is_null());
}

#[test]
fn draft_only_discovery_has_no_inventory_counts_or_provider_work() {
    let root = fixture();
    let mut provider = GitHub::new();
    provider.draft_only = true;
    let summary: Value = serde_json::from_str(
        &run(
            &arguments(root.path(), "https://github.com/example/widgets"),
            &provider,
            &NoHttp,
            &FixedClock,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(summary["state"], "complete");
    assert_eq!(summary["counts"]["open"], 0);
    assert!(summary["counts"].get("draft").is_none());
    assert_eq!(summary["items"], json!([]));
    assert_eq!(record(root.path())["inventory"], json!([]));
    assert!(!provider.calls.borrow().iter().any(|call| call == "graphql"));
}

#[test]
fn legacy_draft_inventory_is_removed_after_validation() {
    let root = fixture();
    let provider = GitHub::new();
    run(
        &arguments(root.path(), "https://github.com/example/widgets"),
        &provider,
        &NoHttp,
        &FixedClock,
    )
    .unwrap();
    let mut legacy = record(root.path());
    let mut draft = legacy["inventory"][1].clone();
    draft["pullRequestId"] = json!(5);
    draft["url"] = json!("https://github.com/example/widgets/pull/5");
    draft["title"] = json!("draft to discard");
    draft["draft"] = json!(true);
    legacy["inventory"].as_array_mut().unwrap().insert(4, draft);
    fs::write(
        root.path().join("run.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();

    let result: Value = serde_json::from_str(
        &run(
            &result_arguments(root.path()),
            &NoProcess,
            &NoHttp,
            &FixedClock,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["counts"]["open"], 5);
    assert!(result["counts"].get("draft").is_none());
    assert!(!record(root.path()).to_string().contains("draft to discard"));
}

#[test]
fn changed_identity_policy_and_malformed_records_do_not_overwrite_evidence() {
    let root = fixture();
    let provider = GitHub::new();
    let args = arguments(root.path(), "https://github.com/example/widgets");
    run(&args, &provider, &NoHttp, &FixedClock).unwrap();
    let original = fs::read(root.path().join("run.json")).unwrap();
    provider.account.set(43);
    run(&args, &provider, &NoHttp, &FixedClock).unwrap_err();
    assert_eq!(fs::read(root.path().join("run.json")).unwrap(), original);
    provider.account.set(42);
    fs::write(root.path().join("policy.md"), "Different policy").unwrap();
    run(&args, &provider, &NoHttp, &FixedClock).unwrap_err();
    assert_eq!(fs::read(root.path().join("run.json")).unwrap(), original);
    fs::write(
        root.path().join("policy.md"),
        "Trusted significance policy fixture.\n",
    )
    .unwrap();
    let mut malformed: Value = serde_json::from_slice(&original).unwrap();
    malformed["inventory"][0]["recency"]
        .as_object_mut()
        .unwrap()
        .remove("checkedAt");
    fs::write(
        root.path().join("run.json"),
        serde_json::to_vec(&malformed).unwrap(),
    )
    .unwrap();
    run(&args, &provider, &NoHttp, &FixedClock).unwrap_err();
    assert_eq!(record(root.path()), malformed);
}

#[test]
fn incompatible_assessment_schemas_and_malformed_recovery_budgets_preserve_records() {
    let root = fixture();
    let provider = GitHub::new();
    run(
        &arguments(root.path(), "https://github.com/example/widgets"),
        &provider,
        &NoHttp,
        &FixedClock,
    )
    .unwrap();
    let current = record(root.path());
    assert_eq!(current["schemaVersion"], 14);
    let path = root.path().join("run.json");
    for version in [7, 8, 9, 10, 11, 12, 13] {
        let mut legacy = current.clone();
        legacy["schemaVersion"] = json!(version);
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let error = run(
            &result_arguments(root.path()),
            &NoProcess,
            &NoHttp,
            &FixedClock,
        )
        .unwrap_err();
        assert_eq!(ss_ai::classification(&error), "invalidState");
        assert!(error.to_string().contains("Unsupported discovery schema"));
        assert_eq!(record(root.path()), legacy);
    }

    let id = uuid::Uuid::new_v4().to_string();
    for recovery in [
        json!({"attemptIds":[id, id], "stale":null, "error":null}),
        json!({"attemptIds":[current["runId"]], "stale":null, "error":null}),
        json!({"attemptIds":["invalid"], "stale":null, "error":null}),
        json!({"attemptIds":[], "stale":{"kind":"service", "message":"Not staleness"}, "error":null}),
        json!({"attemptIds":[], "stale":null, "error":{"kind":"retryExhausted", "message":"Not exhausted"}}),
    ] {
        let mut malformed = current.clone();
        malformed["inventory"][0]["recovery"] = recovery;
        fs::write(&path, serde_json::to_vec(&malformed).unwrap()).unwrap();
        assert_eq!(
            ss_ai::classification(
                &run(
                    &result_arguments(root.path()),
                    &NoProcess,
                    &NoHttp,
                    &FixedClock,
                )
                .unwrap_err()
            ),
            "invalidState"
        );
    }
    let mut malformed = current;
    malformed["inventory"][0]
        .as_object_mut()
        .unwrap()
        .remove("recovery");
    fs::write(&path, serde_json::to_vec(&malformed).unwrap()).unwrap();
    assert_eq!(
        ss_ai::classification(
            &run(
                &result_arguments(root.path()),
                &NoProcess,
                &NoHttp,
                &FixedClock,
            )
            .unwrap_err()
        ),
        "invalidState"
    );
}

#[test]
fn records_require_complete_consistent_fields_before_any_results_are_exposed() {
    let root = fixture();
    run(
        &arguments(root.path(), "https://github.com/example/widgets"),
        &GitHub::new(),
        &NoHttp,
        &FixedClock,
    )
    .unwrap();
    let original = record(root.path());
    let mut changed_policy = original["policy"].clone();
    changed_policy["content"] = json!("Replaced content with an unchanged digest");
    for (pointer, replacement) in [
        ("/schemaVersion", json!(1)),
        ("/schemaVersion", json!(2)),
        ("/schemaVersion", json!(6)),
        ("/errorPullRequestId", json!(0)),
        ("/errorPullRequestId", json!(1)),
        ("/errorPullRequestId", json!(999)),
        ("/runId", json!("not-a-uuid")),
        ("/accountId", json!("")),
        ("/utilityVersion", json!("")),
        ("/policy", changed_policy),
        ("/enumerationComplete", json!(false)),
        ("/state", json!("complete")),
        ("/state", json!("failed")),
        ("/inventory/1/pullRequestId", json!(0)),
        ("/inventory/1", original["inventory"][0].clone()),
        (
            "/inventory/0/url",
            json!("https://github.com/another/repo/pull/1"),
        ),
        ("/inventory/0/recency/state", json!("pending")),
        ("/inventory/0/recency/evidenceComplete", json!(false)),
        ("/inventory/0/recency/checkedAt", json!("not-a-timestamp")),
        (
            "/inventory/1/recency/latestNonfutureCommit",
            json!("2027-01-01T00:00:00Z"),
        ),
        ("/inventory/3/recency/error", Value::Null),
        ("/inventory/3/recency/error/kind", json!("unknown")),
    ] {
        let mut malformed = original.clone();
        *malformed.pointer_mut(pointer).unwrap() = replacement;
        fs::write(
            root.path().join("run.json"),
            serde_json::to_vec(&malformed).unwrap(),
        )
        .unwrap();
        let error = run(
            &result_arguments(root.path()),
            &NoProcess,
            &NoHttp,
            &FixedClock,
        )
        .unwrap_err();
        assert_eq!(ss_ai::classification(&error), "invalidState");
        assert_eq!(record(root.path()), malformed);
    }
    for field in ["tenant", "error", "errorPullRequestId"] {
        let mut malformed = original.clone();
        malformed.as_object_mut().unwrap().remove(field);
        fs::write(
            root.path().join("run.json"),
            serde_json::to_vec(&malformed).unwrap(),
        )
        .unwrap();
        run(
            &result_arguments(root.path()),
            &NoProcess,
            &NoHttp,
            &FixedClock,
        )
        .unwrap_err();
    }
    let mut malformed = original.clone();
    malformed["inventory"].as_array_mut().unwrap().swap(0, 1);
    fs::write(
        root.path().join("run.json"),
        serde_json::to_vec(&malformed).unwrap(),
    )
    .unwrap();
    run(
        &result_arguments(root.path()),
        &NoProcess,
        &NoHttp,
        &FixedClock,
    )
    .unwrap_err();
    let mut malformed = original;
    malformed["extra"] = json!("unrecognized field");
    fs::write(
        root.path().join("run.json"),
        serde_json::to_vec(&malformed).unwrap(),
    )
    .unwrap();
    run(
        &result_arguments(root.path()),
        &NoProcess,
        &NoHttp,
        &FixedClock,
    )
    .unwrap_err();
}

#[test]
fn invalid_policy_or_relative_paths_fail_before_provider_access() {
    let root = fixture();
    let args = arguments(root.path(), "https://github.com/example/widgets");
    for bytes in [Vec::new(), vec![0xFF]] {
        fs::write(root.path().join("policy.md"), bytes).unwrap();
        run(&args, &NoProcess, &NoHttp, &FixedClock).unwrap_err();
        assert!(!root.path().join("run.json").exists());
    }
    for flag in ["--state-path", "--policy-path", "--limit"] {
        let mut invalid = args.clone();
        let value = invalid.iter().position(|arg| arg == flag).unwrap() + 1;
        invalid[value] = if flag == "--limit" { "0" } else { "relative" }.into();
        run(&invalid, &NoProcess, &NoHttp, &FixedClock).unwrap_err();
    }
}

/// Interrupts after one persisted recency read, not through a production-only hook.
struct Interrupted {
    provider: GitHub,
}

impl Process for Interrupted {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if arguments[1] == "graphql" {
            let request: Value = serde_json::from_slice(input).unwrap();
            assert_ne!(request["variables"]["num"], 2, "Simulated interruption");
        }
        self.provider.execute(program, arguments, input)
    }
}

#[test]
fn interrupted_discovery_resumes_only_pending_entries() {
    let root = fixture();
    let args = arguments(root.path(), "https://github.com/example/widgets");
    std::panic::catch_unwind(|| {
        run(
            &args,
            &Interrupted {
                provider: GitHub::new(),
            },
            &NoHttp,
            &FixedClock,
        )
        .unwrap();
    })
    .unwrap_err();
    let before = record(root.path());
    assert_eq!(before["state"], "processing");
    assert_eq!(before["inventory"][0]["recency"]["state"], "noRecentCommit");
    assert_eq!(before["inventory"][1]["recency"]["state"], "pending");
    let summary: Value = serde_json::from_str(
        &run(
            &result_arguments(root.path()),
            &NoProcess,
            &NoHttp,
            &FixedClock,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(summary["counts"]["pending"], 4);
    assert!(summary["items"].as_array().unwrap().is_empty());
    let provider = GitHub::new();
    run(&args, &provider, &NoHttp, &FixedClock).unwrap();
    assert_eq!(record(root.path())["inventory"][0], before["inventory"][0]);
    assert_eq!(
        provider
            .calls
            .borrow()
            .iter()
            .filter(|call| *call == "graphql")
            .count(),
        5
    );
    assert!(
        !provider
            .calls
            .borrow()
            .iter()
            .any(|call| call.contains("pulls?"))
    );
}

#[test]
fn failed_enumeration_is_distinct_from_complete_empty_inventory() {
    for invalid in [false, true] {
        let root = fixture();
        let provider = GitHub {
            invalid_listing: invalid,
            empty: !invalid,
            ..GitHub::new()
        };
        let result = run(
            &arguments(root.path(), "https://github.com/example/widgets"),
            &provider,
            &NoHttp,
            &FixedClock,
        );
        if invalid {
            result.unwrap_err();
        } else {
            result.unwrap();
        }
        let state = record(root.path());
        assert_eq!(state["state"], if invalid { "failed" } else { "complete" });
        assert_eq!(state["enumerationComplete"], !invalid);
        assert_eq!(state["error"].is_null(), !invalid);
        assert!(state["inventory"].as_array().unwrap().is_empty());
        assert!(!provider.calls.borrow().iter().any(|call| call == "graphql"));
    }
}

/// Malformed provider evidence must not become a successful old-PR exclusion.
#[derive(Clone, Copy)]
enum Fault {
    EmptyTitle,
    RepeatedPr,
    EmptyCommits,
    MissingTimestamp,
    RepeatedCursor,
    Authentication,
}

struct BrokenEvidence {
    fault: Fault,
    provider: GitHub,
}

impl Process for BrokenEvidence {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        let output = self.provider.execute(program, arguments, input)?;
        let mut value: Value = serde_json::from_slice(&output.stdout).unwrap();
        if arguments[1].contains("pulls?") {
            match self.fault {
                Fault::EmptyTitle => value[0][0]["title"] = json!(" "),
                Fault::RepeatedPr => value[1] = value[0].clone(),
                _ => {}
            }
        } else if arguments[1] == "graphql" {
            let request: Value = serde_json::from_slice(input).unwrap();
            let id = request["variables"]["num"].as_u64().unwrap();
            let commits = &mut value["data"]["repository"]["pullRequest"]["commits"];
            match self.fault {
                Fault::EmptyCommits if id == 1 => commits["nodes"] = json!([]),
                Fault::MissingTimestamp if id == 1 => commits["nodes"][0]["commit"] = json!({}),
                Fault::RepeatedCursor if id == 6 => {
                    commits["pageInfo"] = json!({"hasNextPage": true, "endCursor": "next"});
                }
                Fault::Authentication if id == 2 => {
                    return Ok(ProcessOutput::new(
                        false,
                        Vec::new(),
                        b"HTTP 401 Unauthorized".to_vec(),
                    ));
                }
                _ => {}
            }
        }
        Ok(ProcessOutput::new(
            true,
            serde_json::to_vec(&value).unwrap(),
            Vec::new(),
        ))
    }
}

#[test]
fn malformed_pages_and_scope_failures_retain_explicit_diagnostics() {
    for fault in [
        Fault::EmptyTitle,
        Fault::RepeatedPr,
        Fault::EmptyCommits,
        Fault::MissingTimestamp,
        Fault::RepeatedCursor,
        Fault::Authentication,
    ] {
        let root = fixture();
        let provider = BrokenEvidence {
            fault,
            provider: GitHub::new(),
        };
        let args = arguments(root.path(), "https://github.com/example/widgets");
        let output = run(&args, &provider, &NoHttp, &FixedClock);
        let state = record(root.path());
        match fault {
            Fault::EmptyTitle | Fault::RepeatedPr => {
                output.unwrap_err();
                assert_eq!(state["state"], "failed");
                assert_eq!(state["enumerationComplete"], false);
                assert_eq!(state["error"]["kind"], "service");
                assert!(state["inventory"].as_array().unwrap().is_empty());
            }
            Fault::Authentication => {
                output.unwrap_err();
                assert_eq!(state["state"], "failed");
                assert_eq!(state["error"]["kind"], "authentication");
                assert_eq!(state["inventory"][0]["recency"]["state"], "noRecentCommit");
                assert_eq!(state["inventory"][1]["recency"]["state"], "failed");
                assert_eq!(state["inventory"][2]["recency"]["state"], "pending");
                let bytes = fs::read(root.path().join("run.json")).unwrap();
                run(&args, &provider, &NoHttp, &FixedClock).unwrap_err();
                assert_eq!(fs::read(root.path().join("run.json")).unwrap(), bytes);
            }
            Fault::EmptyCommits | Fault::MissingTimestamp | Fault::RepeatedCursor => {
                let summary: Value = serde_json::from_str(&output.unwrap()).unwrap();
                assert_eq!(summary["state"], "partial");
                assert_eq!(summary["counts"]["failed"], 2);
                assert_eq!(summary["counts"]["pending"], 0);
            }
        }
    }
}

/// OAuth fixture records tenant forwarding but never launches the real CLI.
struct AzureToken;

impl Process for AzureToken {
    fn execute(&self, program: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "az");
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--tenant", "fixture-tenant"])
        );
        Ok(ProcessOutput::new(
            true,
            b"fixture-token".to_vec(),
            Vec::new(),
        ))
    }
}

/// Official metadata/commit endpoints, without any approval-history fields.
struct AzureMetadata {
    calls: RefCell<Vec<String>>,
}

impl Http for AzureMetadata {
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
        self.calls.borrow_mut().push(url.to_owned());
        let url = Url::parse(url).unwrap();
        let query: BTreeMap<_, _> = url.query_pairs().collect();
        let mut headers = BTreeMap::new();
        let value = if url.path().ends_with("/_apis/connectionData") {
            json!({"authenticatedUser": {"id": "account", "providerDisplayName": "Reviewer"}})
        } else if url.path().ends_with("/repositories/widgets") {
            json!({"id": "repository"})
        } else if url.path().ends_with("/pullRequests") {
            let entries = if query.get("$skip").unwrap() == "0" {
                vec![
                    json!({"pullRequestId": 2, "status": "active", "isDraft": false, "title": "cutoff"}),
                    json!({"pullRequestId": 3, "isDraft": true}),
                ]
            } else {
                Vec::new()
            };
            json!({"count": entries.len(), "value": entries})
        } else if url.path().ends_with("/pullRequests/2/commits") {
            let date = if query.contains_key("continuationToken") {
                "2026-10-01T00:00:00Z"
            } else {
                headers.insert("x-ms-continuationtoken".to_owned(), "next".to_owned());
                "2026-09-14T10:00:00Z"
            };
            json!({"count": 1, "value": [{"committer": {"date": date}}]})
        } else {
            panic!("Unexpected Azure history, content or write endpoint: {url}")
        };
        Ok(HttpResponse::new(
            200,
            headers,
            serde_json::to_vec(&value).unwrap(),
        ))
    }
}

#[test]
fn azure_metadata_and_commit_pages_preserve_tenant_and_do_not_collect_vote_history() {
    let root = fixture();
    let mut args = arguments(
        root.path(),
        "https://dev.azure.com/org/project/_git/widgets",
    );
    args.extend(["--tenant".into(), "fixture-tenant".into()]);
    let provider = AzureMetadata {
        calls: RefCell::default(),
    };
    let summary: Value =
        serde_json::from_str(&run(&args, &AzureToken, &provider, &FixedClock).unwrap()).unwrap();
    assert_eq!(summary["state"], "processing");
    assert_eq!(summary["counts"]["assessmentRequired"], 1);
    assert_eq!(summary["counts"]["failed"], 0);
    let calls = provider.calls.borrow();
    assert_eq!(calls.len(), 6);
    assert!(
        calls
            .iter()
            .any(|call| call.contains("continuationToken=next"))
    );
    assert!(calls.iter().any(|call| call.contains("%24skip=2")));
    assert_eq!(summary["counts"]["open"], 1);
    assert!(summary["counts"].get("draft").is_none());
    assert_eq!(
        record(root.path())["inventory"].as_array().unwrap().len(),
        1
    );
    assert!(!calls.iter().any(|call| call.contains("/pullRequests/3/")));
}

/// Real captured provider shapes with injected transport, never live approvals.
struct AzureApproval {
    fixture: Value,
    stage: usize,
    calls: RefCell<Vec<String>>,
    source_reads: Cell<usize>,
    identity_reads: Cell<usize>,
    move_source: bool,
    repeat_policy: bool,
    evaluation_continuation: bool,
    deny_threads: bool,
    change_identity: bool,
}

impl AzureApproval {
    fn new(stage: usize) -> Self {
        Self {
            fixture: serde_json::from_str(include_str!(
                "../../ss-ai-review/tests/fixtures/ado-no-approval-policy.json"
            ))
            .unwrap(),
            stage,
            calls: RefCell::default(),
            source_reads: Cell::new(0),
            identity_reads: Cell::new(0),
            move_source: false,
            repeat_policy: false,
            evaluation_continuation: false,
            deny_threads: false,
            change_identity: false,
        }
    }

    /// Adds a synthetic applicable policy without importing private repository evidence.
    fn require_iteration_approval(&mut self) {
        let snapshot = &mut self.fixture["stages"][self.stage];
        // An arbitrary fixture identity distinguishes this policy from captured settings.
        let policy = json!({
            "id": 99, "isEnabled": true, "isBlocking": true,
            "type": {"id": "fa4e907d-c16b-4a4c-9dfa-4906e5d171dd"},
            "settings": {"minimumApproverCount": 1, "requireVoteOnEachIteration": true, "requireVoteOnLastIteration": true, "resetOnSourcePush": false}
        });
        snapshot["policies"]
            .as_array_mut()
            .unwrap()
            .push(policy.clone());
        snapshot["evaluations"] = json!([{"evaluationId": "reviewer-policy", "configuration": policy, "status": "queued", "context": {}}]);
    }
}

impl Http for AzureApproval {
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(method, "GET", "Inspection must not write remotely");
        assert_eq!(token, "fixture-token");
        assert!(body.is_none());
        self.calls.borrow_mut().push(url.to_owned());
        let url = Url::parse(url).unwrap();
        let query: BTreeMap<_, _> = url.query_pairs().collect();
        let mut headers = BTreeMap::new();
        let stage = &self.fixture["stages"][self.stage];
        let values =
            |value: &Value| json!({"count": value.as_array().unwrap().len(), "value": value});
        let value = if url.path().ends_with("/_apis/connectionData") {
            self.identity_reads.set(self.identity_reads.get() + 1);
            // Discovery and inspection preflight retain the account; postflight observes the change.
            let account = if self.change_identity && self.identity_reads.get() > 2 {
                json!("another-account")
            } else {
                self.fixture["accountId"].clone()
            };
            json!({"authenticatedUser": {"id": account, "providerDisplayName": "Fixture account"}})
        } else if url.path().ends_with("/repositories/widgets") {
            json!({"id": stage["pullRequest"]["repository"]["id"]})
        } else if url.path().ends_with("/pullRequests") {
            if query.get("$skip").unwrap() == "0" {
                values(&json!([
                    {"pullRequestId": 1, "status": "active", "isDraft": false, "title": "recent"},
                    {"pullRequestId": 2, "status": "active", "isDraft": false, "title": "excluded old"}
                ]))
            } else {
                values(&json!([]))
            }
        } else if url.path().ends_with("/commits") {
            assert!(
                !url.path().contains("/iterations/"),
                "There is no per-iteration reviewer endpoint"
            );
            let date = if url.path().ends_with("/1/commits") {
                "2026-09-27T00:00:00Z"
            } else {
                "2026-08-01T00:00:00Z"
            };
            values(&json!([{"committer": {"date": date}}]))
        } else if url.path().ends_with("/pullRequests/1") {
            stage["pullRequest"].clone()
        } else if url.path().ends_with("/refs") {
            let source = query.get("filter").unwrap() == "heads/fixture-source";
            assert!(source, "Reapproval must not read target commit authority");
            let (name, mut commit) = if source {
                self.source_reads.set(self.source_reads.get() + 1);
                (
                    stage["pullRequest"]["sourceRefName"].clone(),
                    stage["sourceCommit"].clone(),
                )
            } else {
                (
                    stage["pullRequest"]["targetRefName"].clone(),
                    stage["targetCommit"].clone(),
                )
            };
            if self.move_source && source && self.source_reads.get() > 1 {
                commit = json!("e".repeat(40));
            }
            values(&json!([{"name": name, "objectId": commit}]))
        } else if url.path().ends_with("/reviewers") {
            values(&stage["reviewers"])
        } else if url.path().ends_with("/iterations") {
            values(&stage["iterations"])
        } else if url.path().ends_with("/threads") {
            if self.deny_threads {
                return Ok(HttpResponse::new(
                    403,
                    headers,
                    b"{\"message\":\"Forbidden\"}".to_vec(),
                ));
            }
            values(&stage["threads"])
        } else if url.path().ends_with("/policy/configurations") {
            let index: usize = query
                .get("continuationToken")
                .map_or(0, |value| value.parse().unwrap());
            if index + 1 < stage["policies"].as_array().unwrap().len() {
                headers.insert("x-ms-continuationtoken".to_owned(), (index + 1).to_string());
            }
            let index = if self.repeat_policy && index > 0 {
                0
            } else {
                index
            };
            values(&json!([stage["policies"][index]]))
        } else if url.path().ends_with("/policy/evaluations") {
            // ADO can page underlying records before omitting missing evaluations.
            // Returned evaluation counts therefore cannot supply the next offset.
            let mut records = vec![None];
            records.extend(stage["evaluations"].as_array().unwrap().iter().map(Some));
            let skip = query.get("$skip").map_or(0, |value| value.parse().unwrap());
            let top = query
                .get("$top")
                .map_or(records.len(), |value| value.parse().unwrap());
            let evaluations: Vec<_> = records.into_iter().skip(skip).take(top).flatten().collect();
            if self.evaluation_continuation {
                headers.insert("x-ms-continuationtoken".to_owned(), "unexpected".to_owned());
            }
            values(&json!(evaluations))
        } else {
            panic!("Unexpected ADO inspection endpoint {url}")
        };
        Ok(HttpResponse::new(
            200,
            headers,
            serde_json::to_vec(&value).unwrap(),
        ))
    }
}

fn inspect_arguments(root: &Path, number: u32) -> Vec<OsString> {
    vec![
        "ss-ai".into(),
        "autoreapprove".into(),
        "inspect".into(),
        "--repository-url".into(),
        "https://dev.azure.com/org/project/_git/widgets".into(),
        "--state-path".into(),
        root.join("run.json").into_os_string(),
        "--pull-request-id".into(),
        number.to_string().into(),
        "--tenant".into(),
        "fixture-tenant".into(),
    ]
}

fn discover_azure_approval(root: &Path, provider: &AzureApproval, dry_run: bool) {
    let mut args = arguments(root, "https://dev.azure.com/org/project/_git/widgets");
    if !dry_run {
        args.retain(|argument| argument != "--dry-run");
    }
    args.extend(["--tenant".into(), "fixture-tenant".into()]);
    run(&args, &AzureToken, provider, &FixedClock).unwrap();
    assert!(
        provider
            .calls
            .borrow()
            .iter()
            .all(|url| !url.contains("/threads") && !url.contains("/reviewers"))
    );
    provider.calls.borrow_mut().clear();
}

#[test]
fn ado_pr_converted_to_draft_is_removed_during_inspection() {
    let root = fixture();
    let mut provider = AzureApproval::new(1);
    discover_azure_approval(root.path(), &provider, true);
    provider.fixture["stages"][1]["pullRequest"]["isDraft"] = json!(true);
    let result: Value = serde_json::from_str(
        &run(
            &inspect_arguments(root.path(), 1),
            &AzureToken,
            &provider,
            &FixedClock,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["state"], "complete");
    assert_eq!(result["counts"]["open"], 1);
    assert_eq!(result["items"], json!([]));
    assert_eq!(record(root.path())["inventory"][0]["pullRequestId"], 2);
    assert!(
        !provider
            .calls
            .borrow()
            .iter()
            .any(|url| url.contains("/threads"))
    );
}

#[test]
fn ado_inspection_preserves_verified_retention_and_removal_in_both_modes() {
    for dry_run in [true, false] {
        for (stage, expected) in [
            (0, "noPriorApproval"),
            (1, "alreadyValid"),
            (2, "alreadyValid"),
            (3, "alreadyValid"),
            (4, "ineligible"),
        ] {
            let root = fixture();
            let provider = AzureApproval::new(stage);
            discover_azure_approval(root.path(), &provider, dry_run);
            let output: Value = serde_json::from_str(
                &run(
                    &inspect_arguments(root.path(), 1),
                    &AzureToken,
                    &provider,
                    &FixedClock,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(output["state"], "complete");
            assert_eq!(output["items"][0]["status"], expected);
            assert_eq!(output["counts"][expected], 1);
            assert_eq!(output["counts"]["noRecentCommit"], 1);
            assert_eq!(output["counts"]["assessmentRequired"], 0);
            assert_eq!(output["dryRun"], dry_run);
            assert_eq!(provider.source_reads.get(), 2);
            let captured = record(root.path());
            assert!(!captured["inventory"][0]["inspection"]["snapshotDigest"].is_null());
            assert!(captured["inventory"][1]["inspection"].is_null());
            if expected == "alreadyValid" {
                assert_eq!(output["items"][0]["approvalEventId"], "2");
            }
            // Immutable recovery refreshes identity/scope, but never replaces the snapshot.
            provider.calls.borrow_mut().clear();
            run(
                &inspect_arguments(root.path(), 1),
                &AzureToken,
                &provider,
                &FixedClock,
            )
            .unwrap();
            assert_eq!(provider.calls.borrow().len(), 2);
            assert_eq!(record(root.path()), captured);
            run(
                &inspect_arguments(root.path(), 2),
                &NoProcess,
                &NoHttp,
                &FixedClock,
            )
            .unwrap_err();
            run(
                &result_arguments(root.path()),
                &NoProcess,
                &NoHttp,
                &FixedClock,
            )
            .unwrap();
        }
    }
}

#[test]
fn ado_inspection_ignores_deleted_history_and_persists_deterministic_results() {
    for dry_run in [true, false] {
        for delete_approval in [false, true] {
            let root = fixture();
            let mut provider = AzureApproval::new(2);
            let stage = &mut provider.fixture["stages"][2];
            stage["threads"].as_array_mut().unwrap().push(json!({
                "id": "deleted-discussion", "isDeleted": true
            }));
            if delete_approval {
                stage["threads"][1] = json!({"id": 2, "isDeleted": true});
            }
            discover_azure_approval(root.path(), &provider, dry_run);
            let output: Value = serde_json::from_str(
                &run(
                    &inspect_arguments(root.path(), 1),
                    &AzureToken,
                    &provider,
                    &FixedClock,
                )
                .unwrap(),
            )
            .unwrap();
            let expected = if delete_approval {
                "noPriorApproval"
            } else {
                "alreadyValid"
            };
            assert_eq!(output["state"], "complete");
            assert_eq!(output["counts"]["failed"], 0);
            assert_eq!(output["counts"][expected], 1);
            assert_eq!(output["items"][0]["status"], expected);
            assert!(output["items"][0]["error"].is_null());
            let captured = record(root.path());
            let inspection = &captured["inventory"][0]["inspection"];
            assert!(!inspection["snapshotDigest"].is_null());
            let captured_threads = inspection["evidence"]["observation"]["threads"]
                .as_array()
                .unwrap();
            // Routine source-update notifications are redundant with exact source iterations.
            assert!(
                captured_threads
                    .iter()
                    .any(|thread| thread["id"] == "deleted-discussion"
                        && thread["isDeleted"] == true)
            );
            assert!(captured_threads.iter().any(|thread| thread["id"] == 2));
            assert!(!captured_threads.iter().any(|thread| thread["id"] == 3));
            let results: Value = serde_json::from_str(
                &run(
                    &result_arguments(root.path()),
                    &NoProcess,
                    &NoHttp,
                    &FixedClock,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(results["items"][0]["status"], expected);
            provider.calls.borrow_mut().clear();
            run(
                &inspect_arguments(root.path(), 1),
                &AzureToken,
                &provider,
                &FixedClock,
            )
            .unwrap();
            assert_eq!(record(root.path()), captured);
            assert_eq!(provider.calls.borrow().len(), 2);
        }
    }
}

#[test]
fn ado_inspection_omits_untyped_notifications_without_losing_approval_authority() {
    for properties in [Some(Value::Null), None] {
        let root = fixture();
        // A retained vote after a push exercises both typed votes and routine notices.
        let mut provider = AzureApproval::new(2);
        let mut notification = json!({
            "id": "routine-notification",
            "isDeleted": false,
            "status": "closed",
            "publishedDate": "2026-01-01T12:00:00Z",
            "comments": [{"commentType": "system", "content": "No code change or vote reset."}]
        });
        if let Some(properties) = properties {
            notification["properties"] = properties;
        }
        provider.fixture["stages"][2]["threads"]
            .as_array_mut()
            .unwrap()
            .push(notification.clone());
        discover_azure_approval(root.path(), &provider, true);
        let output: Value = serde_json::from_str(
            &run(
                &inspect_arguments(root.path(), 1),
                &AzureToken,
                &provider,
                &FixedClock,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(output["state"], "complete");
        assert_eq!(output["items"][0]["status"], "alreadyValid");
        assert_eq!(output["items"][0]["approvalEventId"], "2");
        assert_eq!(output["counts"]["failed"], 0);
        let saved = record(root.path());
        let inspection = &saved["inventory"][0]["inspection"];
        assert!(inspection["error"].is_null());
        assert!(inspection["snapshotDigest"].is_string());
        assert!(
            !inspection["evidence"]["observation"]["threads"]
                .as_array()
                .unwrap()
                .contains(&notification)
        );
        // Result consumption revalidates the persisted observation and decision.
        run(
            &result_arguments(root.path()),
            &NoProcess,
            &NoHttp,
            &FixedClock,
        )
        .unwrap();
    }
}

#[test]
fn ado_inspection_validates_routine_events_before_omitting_them() {
    for malformed in [false, true] {
        let root = fixture();
        let mut provider = AzureApproval::new(2);
        provider.fixture["stages"][2]["threads"].as_array_mut().unwrap().push(json!({
            "id": "invalid-notification", "status": "closed",
            "publishedDate": if malformed { "2026-01-01T00:00:00Z" } else { "2999-01-01T00:00:00Z" },
            "properties": {"CodeReviewThreadType": {
                "$type": if malformed { "System.Int32" } else { "System.String" },
                "$value": "PolicyStatusUpdate"
            }},
            "comments": [{"commentType": "system", "content": "Routine notification"}]
        }));
        discover_azure_approval(root.path(), &provider, true);
        let output: Value = serde_json::from_str(
            &run(
                &inspect_arguments(root.path(), 1),
                &AzureToken,
                &provider,
                &FixedClock,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(output["counts"]["failed"], 1, "{output}");
        assert_eq!(output["items"][0]["error"]["kind"], "service", "{output}");
    }
}

#[test]
fn ado_inspection_reads_all_evaluations_without_count_based_offsets() {
    // Cover empty responses and both sides of the former page-size boundary.
    for count in [1, 0, 100, 101] {
        let root = fixture();
        let mut provider = AzureApproval::new(2);
        provider.require_iteration_approval();
        let evaluations = provider.fixture["stages"][2]["evaluations"]
            .as_array_mut()
            .unwrap();
        let reviewer = evaluations.pop().unwrap();
        for index in 1..count {
            evaluations.push(json!({
                "evaluationId": format!("merge-check-{index}"),
                "configuration": {
                    "id": index,
                    "type": {"id": "0609b952-1397-4640-95ec-e00a01b2c241"}
                },
                "status": "approved"
            }));
        }
        if count == 0 {
            // With no reviewer policy, an empty evaluation list is complete evidence.
            provider.fixture["stages"][2]["policies"]
                .as_array_mut()
                .unwrap()
                .pop();
        } else {
            // The final record must still enforce fresh iteration approval.
            evaluations.push(reviewer);
        }
        discover_azure_approval(root.path(), &provider, true);
        let output: Value = serde_json::from_str(
            &run(
                &inspect_arguments(root.path(), 1),
                &AzureToken,
                &provider,
                &FixedClock,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(output["counts"]["failed"], 0, "{output}");
        assert_eq!(
            output["items"][0]["status"],
            if count == 0 {
                "alreadyValid"
            } else {
                "assessmentRequired"
            }
        );
        let saved = record(root.path());
        assert_eq!(
            saved["inventory"][0]["inspection"]["evidence"]["observation"]["evaluations"]
                .as_array()
                .unwrap()
                .len(),
            usize::from(count > 0)
        );
        let calls = provider.calls.borrow();
        let evaluations: Vec<_> = calls
            .iter()
            .filter(|url| url.contains("/policy/evaluations?"))
            .collect();
        // Inspection fences the same complete evidence with two observations.
        assert_eq!(evaluations.len(), 2);
        for url in evaluations {
            let url = Url::parse(url).unwrap();
            let query: BTreeMap<_, _> = url.query_pairs().collect();
            assert!(!query.contains_key("$top"));
            assert!(!query.contains_key("$skip"));
            assert_eq!(query["includeNotApplicable"], "true");
            assert_eq!(query["api-version"], "7.1-preview.1");
        }
    }
}

#[test]
fn ado_inspection_rejects_duplicate_or_incomplete_evaluations() {
    for (continuation, diagnostic) in [
        (false, "Repeated ADO evidence identifier"),
        (true, "Unexpected ADO evaluation continuation"),
    ] {
        let root = fixture();
        let mut provider = AzureApproval::new(2);
        provider.require_iteration_approval();
        provider.evaluation_continuation = continuation;
        if !continuation {
            let evaluations = provider.fixture["stages"][2]["evaluations"]
                .as_array_mut()
                .unwrap();
            evaluations.push(evaluations[0].clone());
        }
        discover_azure_approval(root.path(), &provider, true);
        let output: Value = serde_json::from_str(
            &run(
                &inspect_arguments(root.path(), 1),
                &AzureToken,
                &provider,
                &FixedClock,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(output["state"], "partial");
        assert_eq!(output["counts"]["failed"], 1);
        let saved = record(root.path());
        let inspection = &saved["inventory"][0]["inspection"];
        assert!(inspection["evidence"].is_null());
        assert!(inspection["decision"].is_null());
        assert_eq!(inspection["error"]["kind"], "service");
        assert!(
            inspection["error"]["message"]
                .as_str()
                .unwrap()
                .contains(diagnostic)
        );
    }
}

#[test]
fn ado_ordinary_policies_preserve_individual_votes_and_bind_assessment_anchors() {
    for (stage, expected) in [
        (0, "noPriorApproval"),
        (1, "alreadyValid"),
        (2, "assessmentRequired"),
        (4, "ineligible"),
    ] {
        let root = fixture();
        let mut provider = AzureApproval::new(stage);
        provider.require_iteration_approval();
        let snapshot = &mut provider.fixture["stages"][stage];
        if stage != 0 && stage != 4 {
            snapshot["reviewers"][0]["votedFor"] = json!([{"id": "required-group", "vote": 10}]);
        }
        discover_azure_approval(root.path(), &provider, true);
        let output: Value = serde_json::from_str(
            &run(
                &inspect_arguments(root.path(), 1),
                &AzureToken,
                &provider,
                &FixedClock,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(output["items"][0]["status"], expected);
        assert_eq!(output["counts"]["failed"], 0);
        assert_eq!(output["counts"]["indeterminate"], 0);
        assert_eq!(
            output["state"],
            if expected == "assessmentRequired" {
                "processing"
            } else {
                "complete"
            }
        );
        if expected == "assessmentRequired" {
            let saved = record(root.path());
            assert_eq!(
                saved["inventory"][0]["inspection"]["decision"]["anchor"],
                json!({
                    "reviewId": "2",
                    "sourceCommit": provider.fixture["stages"][stage]["iterations"][0]["sourceRefCommit"]["commitId"]
                })
            );
        }
    }
}

/// Stops at an injected Git refusal while checking the authenticated fetch boundary.
struct AzureObjectFailure {
    old: String,
    new: String,
}

impl Process for AzureObjectFailure {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        AzureToken.execute(program, arguments, input)
    }

    fn isolated_git(&self, _: &Path, arguments: &[String]) -> io::Result<ProcessOutput> {
        assert!(arguments.iter().any(|arg| arg == "init"));
        Ok(ProcessOutput::new(true, Vec::new(), Vec::new()))
    }

    fn isolated_git_oauth(
        &self,
        _: &Path,
        arguments: &[String],
        url: &str,
        token: &str,
    ) -> io::Result<ProcessOutput> {
        assert_eq!(url, "https://dev.azure.com/org/project/_git/widgets");
        assert_eq!(token, "fixture-token");
        assert!(arguments.contains(&self.old));
        assert!(arguments.contains(&self.new));
        assert!(
            arguments
                .iter()
                .any(|arg| arg == "http.followRedirects=false")
        );
        assert!(
            arguments
                .iter()
                .all(|arg| !arg.contains("gh auth") && !arg.contains(token))
        );
        Ok(ProcessOutput::new(
            false,
            Vec::new(),
            format!("Refused fetch with {token}").into_bytes(),
        ))
    }
}

#[test]
fn ado_assessment_fetches_exact_approval_sources_with_oauth_and_redacts_failures() {
    let root = fixture();
    let mut provider = AzureApproval::new(2);
    provider.require_iteration_approval();
    discover_azure_approval(root.path(), &provider, true);
    let inspected: Value = serde_json::from_str(
        &run(
            &inspect_arguments(root.path(), 1),
            &AzureToken,
            &provider,
            &FixedClock,
        )
        .unwrap(),
    )
    .unwrap();
    let snapshot = inspected["items"][0]["snapshotDigest"].as_str().unwrap();
    let process = AzureObjectFailure {
        old: provider.fixture["stages"][2]["iterations"][0]["sourceRefCommit"]["commitId"]
            .as_str()
            .unwrap()
            .to_owned(),
        new: provider.fixture["stages"][2]["sourceCommit"]
            .as_str()
            .unwrap()
            .to_owned(),
    };
    let args = vec![
        "ss-ai".into(),
        "autoreapprove".into(),
        "prepare".into(),
        "--state-path".into(),
        root.path().join("run.json").into_os_string(),
        "--pull-request-id".into(),
        "1".into(),
        "--snapshot-digest".into(),
        snapshot.into(),
    ];
    let result: Value =
        serde_json::from_str(&run(&args, &process, &NoHttp, &FixedClock).unwrap()).unwrap();
    assert_eq!(result["state"], "partial");
    assert_eq!(result["counts"]["failed"], 1);
    assert_eq!(result["counts"]["indeterminate"], 0);
    let message = result["items"][0]["error"]["message"].as_str().unwrap();
    assert!(message.contains("Refused fetch with [redacted]"));
    assert!(!message.contains("fixture-token"));
}

#[test]
fn ado_inspection_reports_unknown_policy_and_persists_partial_or_scope_failures() {
    for fault in [
        "movement",
        "repeatedPage",
        "permission",
        "identity",
        "policy",
    ] {
        let root = fixture();
        let mut provider = AzureApproval::new(2);
        provider.fixture["stages"][2]["threads"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id": "deleted-discussion", "isDeleted": true
            }));
        provider.move_source = fault == "movement";
        provider.repeat_policy = fault == "repeatedPage";
        provider.deny_threads = fault == "permission";
        provider.change_identity = fault == "identity";
        if fault == "policy" {
            provider.fixture["stages"][2]["policies"][5]["settings"]["strictVoteMode"] =
                json!(true);
        }
        discover_azure_approval(root.path(), &provider, true);
        let output = run(
            &inspect_arguments(root.path(), 1),
            &AzureToken,
            &provider,
            &FixedClock,
        );
        if matches!(fault, "permission" | "identity") {
            assert_eq!(
                ss_ai::classification(&output.unwrap_err()),
                if fault == "permission" {
                    "permissionDenied"
                } else {
                    "authentication"
                }
            );
        } else {
            output.unwrap();
        }
        let saved = record(root.path());
        let inspection = &saved["inventory"][0]["inspection"];
        assert_eq!(
            saved["state"],
            match fault {
                "permission" | "identity" => "failed",
                _ => "partial",
            }
        );
        if fault == "policy" {
            assert!(inspection["decision"].is_null());
            assert_eq!(inspection["error"]["kind"], "service");
            assert!(
                inspection["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("strictVoteMode")
            );
            let result: Value = serde_json::from_str(
                &run(
                    &result_arguments(root.path()),
                    &NoProcess,
                    &NoHttp,
                    &FixedClock,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(result["counts"]["failed"], 1);
            assert_eq!(result["counts"]["indeterminate"], 0);
            assert_eq!(result["items"][0]["status"], "failed");
        } else {
            assert!(inspection["evidence"].is_null());
            assert!(!inspection["error"].is_null());
            if fault == "movement" {
                assert_eq!(inspection["error"]["kind"], "staleEvidence");
                let result: Value = serde_json::from_str(
                    &run(
                        &result_arguments(root.path()),
                        &NoProcess,
                        &NoHttp,
                        &FixedClock,
                    )
                    .unwrap(),
                )
                .unwrap();
                assert_eq!(result["items"][0]["status"], "staleDuringExecution");
                assert_eq!(result["counts"]["staleDuringExecution"], 1);
                assert_eq!(result["counts"]["failed"], 0);
            }
        }
    }
}

#[test]
fn ado_inspection_consumers_reject_corrupted_snapshots_and_changed_policy() {
    let root = fixture();
    let provider = AzureApproval::new(2);
    discover_azure_approval(root.path(), &provider, true);
    run(
        &inspect_arguments(root.path(), 1),
        &AzureToken,
        &provider,
        &FixedClock,
    )
    .unwrap();
    let original = record(root.path());
    for (path, value) in [
        (
            "/inventory/0/inspection/decision",
            json!({"status": "indeterminate", "reason": "unsupportedApprovalPolicy"}),
        ),
        (
            "/inventory/0/inspection/decision",
            json!({"status": "noPriorApproval"}),
        ),
        (
            "/inventory/0/inspection/snapshotDigest",
            json!("0".repeat(64)),
        ),
        (
            "/inventory/0/inspection/evidence/observation/reviewers/0/vote",
            json!(0),
        ),
        (
            "/inventory/1/inspection",
            original["inventory"][0]["inspection"].clone(),
        ),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(path).unwrap() = value;
        fs::write(
            root.path().join("run.json"),
            serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        let error = run(
            &result_arguments(root.path()),
            &NoProcess,
            &NoHttp,
            &FixedClock,
        )
        .unwrap_err();
        assert_eq!(ss_ai::classification(&error), "invalidState");
        assert_eq!(record(root.path()), changed);
    }
    fs::write(
        root.path().join("run.json"),
        serde_json::to_vec(&original).unwrap(),
    )
    .unwrap();
    fs::write(root.path().join("policy.md"), "Changed trusted policy").unwrap();
    run(
        &inspect_arguments(root.path(), 1),
        &NoProcess,
        &NoHttp,
        &FixedClock,
    )
    .unwrap_err();
}

/// Native GitHub history uses complete REST pages and cursor-based timeline evidence.
struct GitHubApproval {
    calls: RefCell<Vec<String>>,
    evidence: Value,
    identity_reads: Cell<usize>,
    pr_reads: Cell<usize>,
    fault: &'static str,
}

impl GitHubApproval {
    fn new() -> Self {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../ss-ai-review/tests/fixtures/github-approval.json"
        ))
        .unwrap();
        Self {
            calls: RefCell::default(),
            evidence: fixture["evidence"].clone(),
            identity_reads: Cell::new(0),
            pr_reads: Cell::new(0),
            fault: "",
        }
    }

    fn valid(&mut self) {
        self.evidence["reviews"][1]["state"] = json!("APPROVED");
        self.evidence["timeline"][1]["state"] = json!("APPROVED");
        self.evidence["timeline"].as_array_mut().unwrap().remove(2);
    }
}

impl Process for GitHubApproval {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        let path = arguments[1].as_str();
        let method = arguments[3].as_str();
        assert!(
            method == "GET" || method == "POST" && path == "graphql",
            "Inspection must not write remotely"
        );
        self.calls.borrow_mut().push(path.to_owned());
        let pages = |values: &Value| {
            Value::Array(
                values
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| json!([value]))
                    .collect(),
            )
        };
        let response = match path {
            "user" => {
                self.identity_reads.set(self.identity_reads.get() + 1);
                let id = if self.fault == "identity" && self.identity_reads.get() > 2 {
                    43
                } else {
                    42
                };
                json!({"id": id, "login": "reviewer", "type": "User"})
            }
            "repos/example/widgets" => json!({"id": 123}),
            "repos/example/widgets/pulls?state=open&per_page=100" => json!([[
                {"number": 1, "state": "open", "draft": false, "title": "recent"},
                {"number": 2, "state": "open", "draft": false, "title": "excluded old"}
            ]]),
            "repos/example/widgets/pulls/1" => {
                self.pr_reads.set(self.pr_reads.get() + 1);
                let mut pr = self.evidence["pullRequest"].clone();
                if self.fault == "movement" && self.pr_reads.get() > 1 {
                    pr["head"]["sha"] = json!("d".repeat(40));
                }
                if self.fault == "targetMovement" && self.pr_reads.get() > 1 {
                    pr["base"]["sha"] = json!("d".repeat(40));
                    pr["updated_at"] = json!("2026-09-28T00:00:00Z");
                }
                pr
            }
            "repos/example/widgets/pulls/1/reviews?per_page=100" => {
                assert!(arguments.iter().any(|argument| argument == "--paginate"));
                if self.fault == "restPage" {
                    json!([self.evidence["reviews"], null])
                } else {
                    pages(&self.evidence["reviews"])
                }
            }
            "repos/example/widgets/issues/1/comments?per_page=100" => {
                let mut comments = self.evidence["comments"].clone();
                if self.fault == "discussion" && self.pr_reads.get() > 1 {
                    comments[0]["body"] = json!("Changed discussion.");
                }
                pages(&comments)
            }
            "repos/example/widgets/pulls/1/comments?per_page=100" => {
                pages(&self.evidence["reviewComments"])
            }
            "repos/example/widgets/rules/branches/fixture-target?per_page=100" => {
                assert!(arguments.iter().any(|argument| argument == "--paginate"));
                assert!(arguments.iter().any(|argument| argument == "--slurp"));
                let required = self.evidence["policy"]["requireLastPushApproval"].clone();
                let required = if self.fault == "policyMovement" && self.pr_reads.get() > 1 {
                    json!(!required.as_bool().unwrap())
                } else {
                    required
                };
                // Both active rulesets contribute; a later disabled requirement
                // or an unrelated merge check must not override an earlier one.
                json!([[{"type": "pull_request", "parameters": {
                    "require_last_push_approval": required
                }}], [
                    {"type": "required_status_checks"},
                    {"type": "pull_request", "parameters": {"require_last_push_approval": false}}
                ]])
            }
            "graphql" => {
                let request: Value = serde_json::from_slice(input).unwrap();
                let variables = &request["variables"];
                let query = request["query"].as_str().unwrap();
                if query.contains("ApprovalPolicy") {
                    let classic = if matches!(self.fault, "classicLastPush" | "classicDisabled") {
                        json!({"requiresApprovingReviews": self.fault != "classicDisabled",
                            "requireLastPushApproval": true})
                    } else {
                        Value::Null
                    };
                    json!({"data": {"repository": {"pullRequest": {
                        "headRefOid": self.evidence["pullRequest"]["head"]["sha"],
                        "baseRefName": "fixture-target",
                        "baseRef": {"name": "fixture-target", "branchProtectionRule": classic}
                    }}}})
                } else if query.contains("committedDate") {
                    let date = if variables["num"] == 1 || self.fault == "bothRecent" {
                        "2026-09-27T00:00:00Z"
                    } else {
                        "2026-08-01T00:00:00Z"
                    };
                    json!({"data": {"repository": {"pullRequest": {"commits": {
                        "nodes": [{"commit": {"committedDate": date}}],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }}}}})
                } else {
                    assert_eq!(variables["num"], 1, "Excluded PR must not read history");
                    assert!(
                        request["query"]
                            .as_str()
                            .unwrap()
                            .contains("pullRequestCommit")
                    );
                    let index: usize = variables["cur"]
                        .as_str()
                        .map_or(0, |value| value.parse().unwrap());
                    let values = self.evidence["timeline"].as_array().unwrap();
                    let next = index + 1 < values.len();
                    let cursor = if next {
                        json!((index + 1).to_string())
                    } else {
                        Value::Null
                    };
                    let index = if self.fault == "repeatedPage" {
                        0
                    } else {
                        index
                    };
                    json!({"data": {"repository": {"pullRequest": {"timelineItems": {
                        "nodes": [values[index]], "pageInfo": {"hasNextPage": next, "endCursor": cursor}
                    }}}}})
                }
            }
            _ => panic!("Unexpected GitHub inspection endpoint: {path}"),
        };
        Ok(ProcessOutput::new(
            true,
            serde_json::to_vec(&response).unwrap(),
            Vec::new(),
        ))
    }
}

fn github_inspect_arguments(root: &Path, number: u32) -> Vec<OsString> {
    vec![
        "ss-ai".into(),
        "autoreapprove".into(),
        "inspect".into(),
        "--repository-url".into(),
        "https://github.com/example/widgets".into(),
        "--state-path".into(),
        root.join("run.json").into_os_string(),
        "--pull-request-id".into(),
        number.to_string().into(),
    ]
}

#[test]
fn github_pr_converted_to_draft_is_removed_during_inspection() {
    let root = fixture();
    let mut provider = GitHubApproval::new();
    provider.fault = "bothRecent";
    run(
        &arguments(root.path(), "https://github.com/example/widgets"),
        &provider,
        &NoHttp,
        &FixedClock,
    )
    .unwrap();
    provider.calls.borrow_mut().clear();
    provider.evidence["pullRequest"]["draft"] = json!(true);
    let result: Value = serde_json::from_str(
        &run(
            &github_inspect_arguments(root.path(), 1),
            &provider,
            &NoHttp,
            &FixedClock,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["state"], "processing");
    assert_eq!(result["counts"]["open"], 1);
    assert_eq!(result["items"], json!([]));
    assert_eq!(record(root.path())["inventory"][0]["pullRequestId"], 2);
    assert!(
        !provider
            .calls
            .borrow()
            .iter()
            .any(|path| path.contains("/reviews"))
    );
}

#[test]
fn github_inspection_joins_complete_old_approval_history_in_both_modes() {
    for dry_run in [true, false] {
        for (status, omit_comments) in [
            "alreadyValid",
            "assessmentRequired",
            "ineligible",
            "noPriorApproval",
        ]
        .into_iter()
        .flat_map(|status| [false, true].map(|omit_comments| (status, omit_comments)))
        {
            let root = fixture();
            let mut provider = GitHubApproval::new();
            if status != "assessmentRequired" {
                provider.valid();
            }
            if status == "ineligible" {
                provider.evidence["reviews"][1]["state"] = json!("CHANGES_REQUESTED");
                provider.evidence["timeline"][1]["state"] = json!("CHANGES_REQUESTED");
            } else if status == "noPriorApproval" {
                provider.evidence["reviews"][1]["user"]["id"] = json!(43);
            }
            if omit_comments {
                provider.evidence["timeline"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|item| item["state"] != "COMMENTED");
            }
            let mut args = arguments(root.path(), "https://github.com/example/widgets");
            if !dry_run {
                args.retain(|argument| argument != "--dry-run");
            }
            run(&args, &provider, &NoHttp, &FixedClock).unwrap();
            assert!(
                provider
                    .calls
                    .borrow()
                    .iter()
                    .all(|path| !path.contains("/reviews") && !path.contains("/comments"))
            );
            provider.calls.borrow_mut().clear();
            let result: Value = serde_json::from_str(
                &run(
                    &github_inspect_arguments(root.path(), 1),
                    &provider,
                    &NoHttp,
                    &FixedClock,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                result["state"],
                if status == "assessmentRequired" {
                    "processing"
                } else {
                    "complete"
                }
            );
            assert_eq!(result["counts"]["noRecentCommit"], 1);
            assert_eq!(result["counts"][status], 1);
            assert_eq!(result["items"][0]["status"], status);
            assert_eq!(result["dryRun"], dry_run);
            assert_eq!(provider.pr_reads.get(), 2);
            let captured = record(root.path());
            assert_eq!(
                captured["inventory"][0]["inspection"]["evidence"]["platform"],
                "gitHub"
            );
            assert_eq!(
                captured["inventory"][0]["inspection"]["evidence"]["observation"]["reviews"],
                provider.evidence["reviews"]
            );
            assert!(captured["inventory"][1]["inspection"].is_null());
            provider.calls.borrow_mut().clear();
            run(
                &github_inspect_arguments(root.path(), 1),
                &provider,
                &NoHttp,
                &FixedClock,
            )
            .unwrap();
            assert_eq!(
                provider.calls.borrow().as_slice(),
                ["user", "repos/example/widgets"]
            );
            assert_eq!(record(root.path()), captured);
            run(
                &github_inspect_arguments(root.path(), 2),
                &NoProcess,
                &NoHttp,
                &FixedClock,
            )
            .unwrap_err();
            run(
                &result_arguments(root.path()),
                &NoProcess,
                &NoHttp,
                &FixedClock,
            )
            .unwrap();
        }
    }
}

#[test]
fn github_retained_approval_uses_effective_rulesets_and_classic_last_push_requirements() {
    for (fault, required, current_source, status) in [
        ("", true, false, "assessmentRequired"),
        ("classicLastPush", false, false, "assessmentRequired"),
        ("classicDisabled", false, false, "alreadyValid"),
        ("", false, false, "alreadyValid"),
        ("", true, true, "alreadyValid"),
    ] {
        let root = fixture();
        let mut provider = GitHubApproval::new();
        provider.valid();
        provider.fault = fault;
        provider.evidence["policy"]["requireLastPushApproval"] = json!(required);
        if current_source {
            provider.evidence["reviews"][1]["commit_id"] =
                provider.evidence["pullRequest"]["head"]["sha"].clone();
        }
        run(
            &arguments(root.path(), "https://github.com/example/widgets"),
            &provider,
            &NoHttp,
            &FixedClock,
        )
        .unwrap();
        let output = run(
            &github_inspect_arguments(root.path(), 1),
            &provider,
            &NoHttp,
            &FixedClock,
        )
        .unwrap();
        let result: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result["items"][0]["status"], status, "{fault}");
        let saved = record(root.path());
        assert_eq!(
            saved["inventory"][0]["inspection"]["evidence"]["observation"]["policy"]["requireLastPushApproval"],
            required || fault == "classicLastPush"
        );
        assert_eq!(
            provider
                .calls
                .borrow()
                .iter()
                .filter(|path| path.contains("/rules/branches/"))
                .count(),
            2
        );
    }
}

#[test]
fn unknown_renewal_is_a_counted_failure_not_completed_indeterminate_work() {
    let root = fixture();
    let mut provider = GitHubApproval::new();
    provider.evidence["reviews"][1]["body"] = json!("<!-- ss-autoreapprove malformed -->");
    run(
        &arguments(root.path(), "https://github.com/example/widgets"),
        &provider,
        &NoHttp,
        &FixedClock,
    )
    .unwrap();
    let result: Value = serde_json::from_str(
        &run(
            &github_inspect_arguments(root.path(), 1),
            &provider,
            &NoHttp,
            &FixedClock,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["state"], "partial");
    assert_eq!(result["counts"]["failed"], 1);
    assert_eq!(result["counts"]["indeterminate"], 0);
    assert_eq!(result["items"][0]["status"], "failed");
    assert!(
        result["items"][0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unreconciledRenewalEvidence")
    );
    let saved = record(root.path());
    assert!(saved["inventory"][0]["inspection"]["evidence"].is_null());
    assert!(saved["inventory"][0]["inspection"]["decision"].is_null());
    assert!(saved["inventory"][0]["inspection"]["snapshotDigest"].is_null());
}

#[test]
fn github_capture_failures_cannot_leave_success_shaped_snapshots() {
    for fault in [
        "movement",
        "repeatedPage",
        "restPage",
        "identity",
        "discussion",
        "missingDecision",
        "conflictingComment",
        "policyMovement",
        "malformedPolicy",
    ] {
        let root = fixture();
        let mut provider = GitHubApproval::new();
        provider.fault = fault;
        match fault {
            "missingDecision" => {
                provider.evidence["timeline"]
                    .as_array_mut()
                    .unwrap()
                    .remove(1);
            }
            "conflictingComment" => {
                provider.evidence["timeline"][0]["state"] = json!("APPROVED");
            }
            "malformedPolicy" => {
                provider.evidence["policy"]["requireLastPushApproval"] = Value::Null;
            }
            _ => {}
        }
        run(
            &arguments(root.path(), "https://github.com/example/widgets"),
            &provider,
            &NoHttp,
            &FixedClock,
        )
        .unwrap();
        let output = run(
            &github_inspect_arguments(root.path(), 1),
            &provider,
            &NoHttp,
            &FixedClock,
        );
        if fault == "identity" {
            assert_eq!(
                ss_ai::classification(&output.unwrap_err()),
                "authentication"
            );
        } else {
            output.unwrap();
        }
        let saved = record(root.path());
        assert_eq!(
            saved["state"],
            if fault == "identity" {
                "failed"
            } else {
                "partial"
            }
        );
        assert!(saved["inventory"][0]["inspection"]["evidence"].is_null());
        assert!(!saved["inventory"][0]["inspection"]["error"].is_null());
        if matches!(fault, "missingDecision" | "conflictingComment") {
            let error = &saved["inventory"][0]["inspection"]["error"];
            assert_eq!(error["kind"], "service");
            let message = error["message"].as_str().unwrap();
            for expected in [
                if fault == "missingDecision" {
                    "review-2"
                } else {
                    "review-1"
                },
                if fault == "missingDecision" {
                    "DISMISSED"
                } else {
                    "COMMENTED"
                },
                "REST pulls/1/reviews",
                "GraphQL timelineItems",
                "--paginate --slurp",
                "hasNextPage/endCursor",
            ] {
                assert!(message.contains(expected), "{expected}: {message}");
            }
            assert!(saved["inventory"][0]["inspection"]["snapshotDigest"].is_null());
            assert!(saved["inventory"][0]["inspection"]["decision"].is_null());
        }
        if matches!(fault, "movement" | "discussion" | "policyMovement") {
            assert_eq!(
                saved["inventory"][0]["inspection"]["error"]["kind"],
                "staleEvidence"
            );
            let result: Value = serde_json::from_str(
                &run(
                    &result_arguments(root.path()),
                    &NoProcess,
                    &NoHttp,
                    &FixedClock,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(result["items"][0]["status"], "staleDuringExecution");
            assert_eq!(result["counts"]["staleDuringExecution"], 1);
            assert_eq!(result["counts"]["failed"], 0);
        }
    }
}

/// Signals let the test observe lock ownership without relying on elapsed time.
enum Signal {
    Enumerating,
    Contended,
    Finished,
}

/// Holds the first enumeration after its initial complete record is visible.
struct PausedMetadata {
    signal: mpsc::Sender<Signal>,
    release: mpsc::Receiver<()>,
    provider: GitHub,
}

impl Process for PausedMetadata {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if arguments[1].contains("pulls?") {
            self.signal.send(Signal::Enumerating).unwrap();
            self.release.recv().unwrap();
        }
        self.provider.execute(program, arguments, input)
    }
}

/// Replaces contention waiting with an explicit signal and continuation permit.
struct ContentionClock {
    signal: mpsc::Sender<Signal>,
    release: mpsc::Receiver<()>,
}

impl Clock for ContentionClock {
    fn now(&self) -> Timestamp {
        FixedClock.now()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        self.signal.send(Signal::Contended).unwrap();
        self.release.recv().unwrap();
        Ok(())
    }
}

#[test]
fn overlapping_discovery_preserves_initialization_and_exact_completed_evidence() {
    let root = fixture();
    let args = arguments(root.path(), "https://github.com/example/widgets");
    let (signal, events) = mpsc::channel();
    let (finish, permit) = mpsc::channel();
    let first_args = args.clone();
    let first_signal = signal.clone();
    let owner = thread::spawn(move || {
        let result = std::panic::catch_unwind(|| {
            run(
                &first_args,
                &PausedMetadata {
                    signal: first_signal.clone(),
                    release: permit,
                    provider: GitHub::new(),
                },
                &NoHttp,
                &FixedClock,
            )
        });
        first_signal.send(Signal::Finished).unwrap();
        result
    });
    let initializing = matches!(events.recv().unwrap(), Signal::Enumerating);
    if !initializing {
        drop(finish);
        owner.join().unwrap().unwrap().unwrap();
        panic!("Discovery returned without enumerating");
    }
    let before = fs::read(root.path().join("run.json")).unwrap();
    let (retry, permit) = mpsc::channel();
    let second_signal = signal;
    let contender = thread::spawn(move || {
        let provider = GitHub::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(
                &args,
                &provider,
                &NoHttp,
                &ContentionClock {
                    signal: second_signal.clone(),
                    release: permit,
                },
            )
        }));
        second_signal.send(Signal::Finished).unwrap();
        (result, provider.calls.into_inner())
    });
    let contended = matches!(events.recv().unwrap(), Signal::Contended);
    let during = fs::read(root.path().join("run.json")).unwrap();
    finish.send(()).unwrap();
    let completed = owner.join().unwrap();
    // Releasing the owner before retry also makes a missing lock fail without a hang.
    if contended {
        retry.send(()).unwrap();
    }
    let (second, calls) = contender.join().unwrap();
    assert!(contended);
    assert_eq!(before, during);
    assert_eq!(second.unwrap().unwrap(), completed.unwrap().unwrap());
    assert_eq!(calls, ["user", "repos/example/widgets"]);
    assert_eq!(record(root.path())["state"], "partial");
    assert_eq!(root.path().read_dir().unwrap().count(), 3);
}
