//! Real isolated Git objects with injected service effects exercise the full command lifecycle.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Tests use bounded fixture arrays and counters"
)]

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_skills_util::{Clock, Http, HttpResponse, NativeProcess, Process, ProcessOutput, run_in};
use tempfile::TempDir;

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2026-09-28T10:00:00Z".parse().unwrap()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("No test response requires a timed retry")
    }
}

struct NoHttp;
impl Http for NoHttp {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub uses the injected CLI")
    }
}

/// Keeps all service writes in memory while real Git fetches from a test-owned repository.
struct Provider {
    repository: PathBuf,
    evidence: RefCell<Value>,
    writes: Cell<usize>,
    fetches: Cell<usize>,
    fault: Cell<&'static str>,
    target_reads: Cell<usize>,
    host_directory: RefCell<Option<PathBuf>>,
}

impl Provider {
    fn new(repository: &Path, old: &str, new: &str) -> Self {
        let fixture: Value =
            serde_json::from_str(include_str!("fixtures/github-approval.json")).unwrap();
        let mut evidence = fixture["evidence"].clone();
        evidence["pullRequest"]["head"]["sha"] = json!(new);
        for review in evidence["reviews"].as_array_mut().unwrap() {
            review["commit_id"] = json!(old);
        }
        evidence["timeline"][2]["pullRequestCommit"]["commit"]["oid"] = json!(new);
        // UI approvals require no saved target tip or full-review instrumentation.
        evidence["pullRequest"]["base"]
            .as_object_mut()
            .unwrap()
            .remove("sha");
        evidence["pullRequest"]["base"]
            .as_object_mut()
            .unwrap()
            .remove("ref");
        Self {
            repository: repository.to_owned(),
            evidence: RefCell::new(evidence),
            writes: Cell::new(0),
            fetches: Cell::new(0),
            fault: Cell::new(""),
            target_reads: Cell::new(0),
            host_directory: RefCell::new(None),
        }
    }

    fn later(&self, state: &str) {
        let mut evidence = self.evidence.borrow_mut();
        let source = evidence["pullRequest"]["head"]["sha"].clone();
        evidence["reviews"].as_array_mut().unwrap().push(json!({
            "id": 5, "node_id": "later-review", "user": {"id": 42, "node_id": "user-42"},
            "state": state, "submitted_at": "2026-09-28T08:00:00Z", "commit_id": source,
            "body": "Independent human decision."
        }));
        if state != "PENDING" {
            evidence["timeline"].as_array_mut().unwrap().push(
                json!({"__typename": "PullRequestReview", "id": "later-review", "state": state}),
            );
        }
    }
}

impl Process for Provider {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        let path = arguments[1].as_str();
        let method = arguments[3].as_str();
        if method == "POST" && path.ends_with("/reviews") {
            self.writes.set(self.writes.get() + 1);
            if self.fault.get() == "rateLimit" {
                return Ok(ProcessOutput::new(
                    false,
                    b"HTTP/2.0 429 Too Many Requests\r\nRetry-After: 1\r\n\r\n{}".to_vec(),
                    Vec::new(),
                ));
            }
            if self.fault.get() == "permission" {
                return Ok(ProcessOutput::new(
                    false,
                    b"HTTP/2.0 403 Forbidden\r\n\r\n{}".to_vec(),
                    Vec::new(),
                ));
            }
            let request: Value = serde_json::from_slice(input).unwrap();
            assert_eq!(request["event"], "APPROVE");
            let mut evidence = self.evidence.borrow_mut();
            assert_eq!(request["commit_id"], evidence["pullRequest"]["head"]["sha"]);
            let review = json!({
                "id": 4, "node_id": "renewal-review", "user": {"id": 42, "node_id": "user-42"},
                "state": "APPROVED", "submitted_at": "2026-09-28T09:00:00Z",
                "commit_id": request["commit_id"], "body": request["body"]
            });
            if self.fault.get() != "unknown" {
                evidence["reviews"]
                    .as_array_mut()
                    .unwrap()
                    .push(review.clone());
                evidence["timeline"].as_array_mut().unwrap().push(json!({"__typename": "PullRequestReview", "id": "renewal-review", "state": "APPROVED"}));
            }
            if self.fault.get() == "postPush" {
                evidence["pullRequest"]["head"]["sha"] = json!("f".repeat(40));
            }
            if self.fault.get() == "duplicate" {
                let mut duplicate = review.clone();
                duplicate["id"] = json!(6);
                duplicate["node_id"] = json!("duplicate-renewal");
                evidence["reviews"].as_array_mut().unwrap().push(duplicate);
                evidence["timeline"].as_array_mut().unwrap().push(json!({
                    "__typename": "PullRequestReview", "id": "duplicate-renewal", "state": "APPROVED"
                }));
            }
            drop(evidence);
            if self.fault.get() == "postWithdrawal" {
                self.later("CHANGES_REQUESTED");
            }
            if self.fault.get() == "journalFailure" {
                // A directory at the journal name deterministically refuses atomic file replacement.
                let path = host_journal(self.host_directory.borrow().as_ref().unwrap());
                fs::rename(&path, path.with_extension("saved")).unwrap();
                fs::create_dir_all(path).unwrap();
            }
            return Ok(if matches!(self.fault.get(), "lostReply" | "unknown") {
                ProcessOutput::new(false, Vec::new(), b"connection lost".to_vec())
            } else {
                ProcessOutput::new(true, serde_json::to_vec(&review).unwrap(), Vec::new())
            });
        }
        assert!(
            method == "GET" || path == "graphql",
            "Unexpected service mutation"
        );
        if self.fault.get() == "postReadFailure" && self.writes.get() > 0 {
            return Ok(ProcessOutput::new(
                false,
                b"HTTP/2.0 403 Forbidden\r\n\r\n{}".to_vec(),
                Vec::new(),
            ));
        }
        let evidence = self.evidence.borrow();
        let value = match path {
            "user" => {
                json!({"id": if self.fault.get() == "auth" { 99 } else { 42 }, "login": "reviewer", "type": "User"})
            }
            "repos/example/widgets" => json!({"id": 123}),
            "repos/example/widgets/pulls?state=open&per_page=100" => json!([[
                {"number": 1, "state": "open", "draft": false, "title": "Recent source"},
                {"number": 2, "state": "open", "draft": false, "title": "Excluded old"}
            ]]),
            "repos/example/widgets/pulls/1" => {
                self.target_reads.set(self.target_reads.get() + 1);
                let mut value = evidence["pullRequest"].clone();
                // Base-tip advancement on every read must not fence otherwise identical source evidence.
                value["base"]["sha"] = json!(format!("{:040x}", self.target_reads.get()));
                value["updated_at"] = json!(self.target_reads.get());
                value
            }
            "repos/example/widgets/pulls/1/reviews?per_page=100" => json!([evidence["reviews"]]),
            "repos/example/widgets/issues/1/comments?per_page=100" => json!([evidence["comments"]]),
            "repos/example/widgets/pulls/1/comments?per_page=100" => {
                json!([evidence["reviewComments"]])
            }
            "graphql" => {
                let request: Value = serde_json::from_slice(input).unwrap();
                if request["query"].as_str().unwrap().contains("committedDate") {
                    let at = if request["variables"]["num"] == 1 {
                        "2026-09-27T00:00:00Z"
                    } else {
                        "2020-01-01T00:00:00Z"
                    };
                    json!({"data": {"repository": {"pullRequest": {"commits": {
                        "nodes": [{"commit": {"committedDate": at}}],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }}}}})
                } else {
                    assert_eq!(request["variables"]["num"], 1);
                    json!({"data": {"repository": {"pullRequest": {"timelineItems": {
                        "nodes": evidence["timeline"], "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }}}}})
                }
            }
            _ => panic!("Unexpected provider path {path}"),
        };
        Ok(ProcessOutput::new(
            true,
            serde_json::to_vec(&value).unwrap(),
            Vec::new(),
        ))
    }

    fn isolated_git(&self, directory: &Path, arguments: &[String]) -> io::Result<ProcessOutput> {
        let mut arguments = arguments.to_vec();
        if arguments.iter().any(|argument| argument == "fetch") {
            self.fetches.set(self.fetches.get() + 1);
            let remote = arguments
                .iter_mut()
                .find(|argument| argument.as_str() == "https://github.com/example/widgets")
                .unwrap();
            self.repository.to_str().unwrap().clone_into(remote);
        }
        NativeProcess::new(directory).isolated_git(directory, &arguments)
    }
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .env("GIT_AUTHOR_DATE", "2026-09-27T12:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-09-27T12:00:00Z")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn sources(root: &Path) -> (String, String) {
    git(root, &["init", "--quiet"]);
    fs::write(root.join("notes.txt"), "An ordinary sentnce.\n").unwrap();
    git(root, &["add", "--", "notes.txt"]);
    git(root, &["commit", "--quiet", "-m", "Original source"]);
    let old = git(root, &["rev-parse", "HEAD"]);
    fs::write(root.join("notes.txt"), "An ordinary sentence.\n").unwrap();
    git(root, &["add", "--", "notes.txt"]);
    git(root, &["commit", "--quiet", "-m", "Editorial correction"]);
    (old, git(root, &["rev-parse", "HEAD"]))
}

fn invoke(
    root: &Path,
    provider: &Provider,
    operation: &str,
    args: &[String],
) -> Result<Value, AppError> {
    let mut arguments = vec![
        OsString::from("ss-skills-util"),
        "autoreapprove".into(),
        operation.into(),
        "--state-path".into(),
        root.join("run.json").into_os_string(),
    ];
    arguments.extend(args.iter().map(OsString::from));
    Ok(serde_json::from_str(&run_in(
        &arguments,
        provider,
        &NoHttp,
        &FixedClock,
        &root.parent().unwrap().join("host-store"),
    )?)?)
}

fn prepare(root: &Path, provider: &Provider, dry_run: bool) -> (String, Value) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("policy.md"),
        include_str!("../../../skills/ss-autoreapprove/significance.md"),
    )
    .unwrap();
    let mut args = vec![
        "--repository-url".to_owned(),
        "https://github.com/example/widgets".to_owned(),
        "--policy-path".to_owned(),
        root.join("policy.md").to_str().unwrap().to_owned(),
    ];
    if dry_run {
        args.push("--dry-run".to_owned());
    }
    let discovered = invoke(root, provider, "discover", &args).unwrap();
    assert_eq!(discovered["counts"]["noRecentCommit"], 1);
    let inspected = invoke(
        root,
        provider,
        "inspect",
        &[
            "--repository-url".into(),
            "https://github.com/example/widgets".into(),
            "--pull-request-id".into(),
            "1".into(),
        ],
    )
    .unwrap();
    assert_eq!(inspected["items"][0]["status"], "assessmentRequired");
    let snapshot = inspected["items"][0]["snapshotDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    let prepared = work(root, provider, "prepare", &snapshot, &[]).unwrap();
    if prepared.get("requiredDocuments").is_some() {
        assert_eq!(
            prepared["requiredDocuments"],
            json!([
                "significance-policy",
                "assessment-contract",
                "approval-discussion"
            ])
        );
    } else {
        assert_eq!(prepared["items"][0]["status"], "failed");
    }
    (snapshot, prepared)
}

fn work(
    root: &Path,
    provider: &Provider,
    operation: &str,
    snapshot: &str,
    args: &[String],
) -> Result<Value, AppError> {
    let mut all = vec![
        "--pull-request-id".into(),
        "1".into(),
        "--snapshot-digest".into(),
        snapshot.to_owned(),
    ];
    all.extend_from_slice(args);
    invoke(root, provider, operation, &all)
}

fn assess(root: &Path, provider: &Provider, snapshot: &str, prepared: &Value) {
    let mut references = vec!["policy".to_owned(), "discussion".to_owned()];
    for change in prepared["changes"].as_array().unwrap() {
        let id = change["id"].as_str().unwrap();
        for side in ["old", "new"] {
            if !change[side].is_null() {
                references.push(format!("{id}:{side}"));
            }
        }
    }
    for reference in references {
        let mut offset = 0;
        loop {
            let read = work(
                root,
                provider,
                "read",
                snapshot,
                &[
                    "--reference".into(),
                    reference.clone(),
                    "--offset".into(),
                    offset.to_string(),
                    "--limit".into(),
                    "113".into(),
                ],
            )
            .unwrap();
            if read["complete"] == true {
                break;
            }
            let next = read["nextOffset"].as_u64().unwrap();
            assert!(next > offset);
            offset = next;
        }
    }
    for change in prepared["changes"].as_array().unwrap() {
        let id = change["id"].as_str().unwrap();
        let mut arguments = vec![
            "--change-id".into(), id.into(), "--disposition".into(), "insignificant".into(),
            "--rationale".into(), "The complete fixture change has no significant design change or likely performance/correctness regression.".into(),
        ];
        for side in ["old", "new"] {
            if !change[side].is_null() {
                arguments.extend(["--evidence".into(), format!("{id}:{side}")]);
            }
        }
        work(root, provider, "judge", snapshot, &arguments).unwrap();
    }
    let current = work(root, provider, "assessment", snapshot, &[]).unwrap();
    let mut cumulative = vec![
        "--change-id".into(),
        "cumulative".into(),
        "--disposition".into(),
        "insignificant".into(),
        "--rationale".into(),
        "Combined fixture changes have no significant design change or likely performance/correctness regression; discussion has no new objection.".into(),
        "--evidence".into(),
        "policy".into(),
        "--evidence".into(),
        "discussion".into(),
    ];
    if let Some(previous) = current["cumulativeDigest"].as_str() {
        // Missing/stale expected versions cannot silently replace a concurrent judgment.
        work(root, provider, "judge", snapshot, &cumulative).unwrap_err();
        cumulative.extend(["--previous-digest".into(), previous.to_owned()]);
    }
    work(root, provider, "judge", snapshot, &cumulative).unwrap();
    work(root, provider, "complete", snapshot, &[]).unwrap();
}

fn publish(root: &Path, provider: &Provider, snapshot: &str) -> Result<Value, AppError> {
    work(
        root,
        provider,
        "publish",
        snapshot,
        &[
            "--repository-url".into(),
            "https://github.com/example/widgets".into(),
            "--attribution".into(),
            "[Copilot speaking]".into(),
        ],
    )
}

#[test]
fn native_git_assessment_and_guarded_publication_have_exact_effect_boundaries() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    let baseline = git(repository.path(), &["status", "--porcelain=v1"]);
    for fault in [
        "",
        "dryRun",
        "lostReply",
        "unknown",
        "prePush",
        "postPush",
        "withdrawal",
        "pending",
        "discussion",
        "auth",
        "postWithdrawal",
        "postReadFailure",
        "duplicate",
        "rateLimit",
        "permission",
        "journalFailure",
        "otherUnknown",
    ] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("first");
        let provider = Provider::new(repository.path(), &old, &new);
        let (snapshot, prepared) = prepare(&root, &provider, fault == "dryRun");
        assert_eq!(prepared["oldCommit"], old);
        assert_eq!(prepared["newCommit"], new);
        assert_eq!(prepared["changeCount"], 1);
        work(&root, &provider, "complete", &snapshot, &[]).unwrap_err();
        work(
            &root,
            &provider,
            "read",
            &snapshot,
            &[
                "--reference".into(),
                "policy".into(),
                "--offset".into(),
                "10".into(),
            ],
        )
        .unwrap_err();
        assess(&root, &provider, &snapshot, &prepared);
        assert_eq!(provider.writes.get(), 0);
        provider.fault.set(fault);
        match fault {
            "prePush" => {
                provider.evidence.borrow_mut()["pullRequest"]["head"]["sha"] =
                    json!("e".repeat(40));
            }
            "withdrawal" => provider.later("CHANGES_REQUESTED"),
            "pending" => provider.later("PENDING"),
            "discussion" => {
                provider.evidence.borrow_mut()["pullRequest"]["body"] =
                    json!("New unresolved objection.");
            }
            _ => {}
        }
        if fault == "dryRun" {
            publish(&root, &provider, &snapshot).unwrap_err();
            let result = invoke(&root, &provider, "results", &[]).unwrap();
            assert_eq!(result["state"], "complete");
            assert_eq!(result["items"][0]["status"], "wouldReapprove");
            assert_eq!(provider.writes.get(), 0);
            continue;
        }
        if fault == "auth" {
            let error = publish(&root, &provider, &snapshot).unwrap_err();
            assert_eq!(ss_skills_util::classification(&error), "authentication");
            assert_eq!(provider.writes.get(), 0);
            assert_eq!(
                invoke(&root, &provider, "results", &[]).unwrap()["state"],
                "failed"
            );
            continue;
        }
        if fault == "otherUnknown" {
            let path = root.join("run.json");
            let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            record["state"] = json!("failed");
            record["error"] =
                json!({"kind": "ambiguousWrite", "message": "Another PR has unresolved work."});
            fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
            publish(&root, &provider, &snapshot).unwrap_err();
            assert_eq!(provider.writes.get(), 0);
            continue;
        }
        check_publication(&root, &provider, &snapshot, fault);
    }
    assert_eq!(
        git(repository.path(), &["status", "--porcelain=v1"]),
        baseline
    );
    assert_eq!(git(repository.path(), &["rev-parse", "HEAD"]), new);
    // Keep process launches in one test: fork must not inherit another test's retained file lock.
    force_push_inventory_preserves_every_path_kind_and_rejects_missing_objects();
    empty_source_diff_and_corrupted_assessments_never_bypass_discussion_or_coverage();
    risk_judgments_and_uncertainty_prevent_publication();
    prepared_run_cannot_bypass_another_runs_unknown_effect();
}

/// A saved preparation does not reserve permission to write after another run becomes uncertain.
fn prepared_run_cannot_bypass_another_runs_unknown_effect() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    let scope = TempDir::new().unwrap();
    let first = scope.path().join("first");
    let second = scope.path().join("second");
    let provider = Provider::new(repository.path(), &old, &new);
    let (snapshot, prepared) = prepare(&first, &provider, false);
    assess(&first, &provider, &snapshot, &prepared);
    let run_path = first.join("run.json");
    let before = fs::read(&run_path).unwrap();

    // Refuse preflight after preparation, then restore the run as if the process
    // stopped before saving its error. The host's prepared record remains intact.
    provider.fault.set("auth");
    assert_eq!(
        ss_skills_util::classification(&publish(&first, &provider, &snapshot).unwrap_err()),
        "authentication"
    );
    fs::write(&run_path, before).unwrap();
    provider.fault.set("");
    let (second_snapshot, prepared) = prepare(&second, &provider, false);
    assess(&second, &provider, &second_snapshot, &prepared);
    provider.fault.set("unknown");
    let result = publish(&second, &provider, &second_snapshot).unwrap();
    assert_eq!(result["state"], "failed");
    assert_eq!(provider.writes.get(), 1);

    provider.fault.set("");
    let error = publish(&first, &provider, &snapshot).unwrap_err();
    assert_eq!(ss_skills_util::classification(&error), "ambiguousWrite");
    assert_eq!(
        provider.writes.get(),
        1,
        "Prepared work must not replay an unknown effect"
    );
}

/// Each policy concern and insufficient evidence independently prevents a remote effect.
fn risk_judgments_and_uncertainty_prevent_publication() {
    let repository = TempDir::new().unwrap();
    let (old, new) = sources(repository.path());
    for (disposition, rationale, status) in [
        (
            "significant",
            "Significant design change replaces the approved ownership model.",
            "needsReview",
        ),
        (
            "significant",
            "Likely performance regression adds a scan per item.",
            "needsReview",
        ),
        (
            "significant",
            "Likely correctness regression drops the empty-input case.",
            "needsReview",
        ),
        (
            "uncertain",
            "Missing contract context prevents assessing correctness.",
            "failed",
        ),
    ] {
        let scope = TempDir::new().unwrap();
        let root = scope.path().join("risk");
        let provider = Provider::new(repository.path(), &old, &new);
        let (snapshot, prepared) = prepare(&root, &provider, false);
        let id = prepared["changes"][0]["id"].as_str().unwrap();
        work(
            &root,
            &provider,
            "judge",
            &snapshot,
            &[
                "--change-id".into(),
                id.into(),
                "--disposition".into(),
                disposition.into(),
                "--rationale".into(),
                rationale.into(),
                "--evidence".into(),
                format!("{id}:new"),
            ],
        )
        .unwrap();
        // A favorable aggregate must not hide either a significant or uncertain member.
        work(
            &root,
            &provider,
            "judge",
            &snapshot,
            &[
                "--change-id".into(),
                "cumulative".into(),
                "--disposition".into(),
                "insignificant".into(),
                "--rationale".into(),
                "Claimed acceptable aggregate.".into(),
                "--evidence".into(),
                "policy".into(),
                "--evidence".into(),
                "discussion".into(),
            ],
        )
        .unwrap();
        let completed = work(&root, &provider, "complete", &snapshot, &[]).unwrap();
        assert_eq!(completed["status"], status);
        if status == "failed" {
            assert_eq!(completed["error"]["kind"], "service");
            let saved: Value =
                serde_json::from_slice(&fs::read(root.join("run.json")).unwrap()).unwrap();
            assert_eq!(saved["state"], "partial");
            assert!(!saved["inventory"][0]["executionError"].is_null());
        }
        publish(&root, &provider, &snapshot).unwrap_err();
        assert_eq!(provider.writes.get(), 0);
    }
}

/// Checks terminal effects and recovery separately from scenario preparation.
fn check_publication(root: &Path, provider: &Provider, snapshot: &str, fault: &str) {
    let result = if fault == "journalFailure" {
        recover_failed_journal(root, provider, snapshot)
    } else {
        publish(root, provider, snapshot).unwrap()
    };
    let expected = match fault {
        "unknown" | "postReadFailure" | "duplicate" | "rateLimit" | "permission" => "failed",
        "prePush" | "postPush" | "discussion" | "postWithdrawal" => "staleDuringExecution",
        "withdrawal" | "pending" => "ineligible",
        _ => "reapproved",
    };
    assert_eq!(result["items"][0]["status"], expected, "{fault}: {result}");
    assert_eq!(
        provider.writes.get(),
        usize::from(!matches!(
            fault,
            "prePush" | "withdrawal" | "pending" | "discussion"
        ))
    );
    if fault == "permission" {
        assert_eq!(result["state"], "failed");
        publish(root, provider, snapshot).unwrap_err();
    } else {
        publish(root, provider, snapshot).unwrap();
    }
    assert_eq!(
        provider.writes.get(),
        usize::from(!matches!(
            fault,
            "prePush" | "withdrawal" | "pending" | "discussion"
        )),
        "Recovery must never duplicate a write"
    );
    let receipt = &result["items"][0]["publication"];
    let host_path = Path::new(receipt["journalPath"].as_str().unwrap());
    assert!(host_path.is_absolute() && host_path.is_file());
    if fault.is_empty() {
        reject_publication_corruption(host_path, root, provider, snapshot);
    }
    if fault == "postReadFailure" {
        fs::write(root.join("policy.md"), "Updated installed policy.").unwrap();
        provider.fault.set("");
        let run_path = root.join("run.json");
        let pending = fs::read(&run_path).unwrap();
        let recovered = publish(root, provider, snapshot).unwrap();
        assert_eq!(recovered["items"][0]["status"], "reapproved");
        assert_eq!(provider.writes.get(), 1);

        // Retain the terminal host record but replay the last durable run bytes,
        // as when execution stops between the host save and the run save.
        fs::write(&run_path, pending).unwrap();
        let reads = provider.target_reads.get();
        let repaired = publish(root, provider, snapshot).unwrap();
        assert_eq!(repaired["items"][0]["status"], "reapproved");
        assert_eq!(
            repaired["items"][0]["publication"],
            recovered["items"][0]["publication"]
        );
        assert_eq!(
            provider.target_reads.get(),
            reads,
            "Terminal recovery is local only"
        );
        assert_eq!(
            provider.writes.get(),
            1,
            "Receipt recovery must never replay a review"
        );
    }
    if fault == "unknown" {
        let second = root.parent().unwrap().join("second");
        let (snapshot, prepared) = prepare(&second, provider, false);
        assess(&second, provider, &snapshot, &prepared);
        let error = publish(&second, provider, &snapshot).unwrap_err();
        assert_eq!(ss_skills_util::classification(&error), "ambiguousWrite");
        assert_eq!(provider.writes.get(), 1);
    }
}

fn host_journal(directory: &Path) -> PathBuf {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap()
}

/// A failed acknowledgement save must leave a recoverable intent, not a blocked assessment error.
fn recover_failed_journal(root: &Path, provider: &Provider, snapshot: &str) -> Value {
    let directory = root
        .parent()
        .unwrap()
        .join("host-store")
        .join("publications");
    provider.host_directory.replace(Some(directory.clone()));
    let error = publish(root, provider, snapshot).unwrap_err();
    assert_eq!(ss_skills_util::classification(&error), "ambiguousWrite");
    assert_eq!(
        invoke(root, provider, "results", &[]).unwrap()["state"],
        "failed"
    );
    let path = host_journal(&directory);
    fs::remove_dir(&path).unwrap();
    fs::rename(path.with_extension("saved"), &path).unwrap();
    provider.fault.set("");
    let result = publish(root, provider, snapshot).unwrap();
    assert_eq!(provider.writes.get(), 1);
    result
}

/// Recovery validates the effect and readback instead of trusting a terminal status string.
fn reject_publication_corruption(
    host_path: &Path,
    root: &Path,
    provider: &Provider,
    snapshot: &str,
) {
    let bytes = fs::read(host_path).unwrap();
    let record: Value = serde_json::from_slice(&bytes).unwrap();
    for (path, replacement) in [
        ("/operations/0/state", json!("write-intent")),
        ("/operations/0/effect/state", json!("unknown")),
        (
            "/operations/0/effect/acknowledgement/commit_id",
            json!("a".repeat(40)),
        ),
        ("/operations/0/result/reviewId", json!("different-review")),
        (
            "/operations/0/verification/evidence/observation/pullRequest/head/sha",
            json!("a".repeat(40)),
        ),
    ] {
        let mut corrupt = record.clone();
        *corrupt.pointer_mut(path).unwrap() = replacement;
        fs::write(host_path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        publish(root, provider, snapshot).unwrap_err();
        assert_eq!(provider.writes.get(), 1);
    }
    fs::write(host_path, bytes).unwrap();
}

fn force_push_inventory_preserves_every_path_kind_and_rejects_missing_objects() {
    let repository = TempDir::new().unwrap();
    git(repository.path(), &["init", "--quiet"]);
    for (path, text) in [
        ("removed.txt", "Removed on the rewritten source.\n"),
        (
            "moved.txt",
            "An unchanged uniquely identifiable renamed document.\n",
        ),
        ("mode.txt", "An executable mode change.\n"),
        ("link", "A regular file becomes a symlink.\n"),
        ("binary", "An ordinary text becomes binary.\n"),
        ("large", "A small file becomes too large.\n"),
        ("mixed.txt", "An ordinary sentnce.\n"),
    ] {
        fs::write(repository.path().join(path), text).unwrap();
    }
    git(repository.path(), &["add", "."]);
    git(
        repository.path(),
        &["commit", "--quiet", "-m", "Original approved tree"],
    );
    let old = git(repository.path(), &["rev-parse", "HEAD"]);
    // An unrelated root commit models a force push; three-dot comparison cannot represent it.
    git(
        repository.path(),
        &["checkout", "--quiet", "--orphan", "rewritten"],
    );
    fs::remove_file(repository.path().join("removed.txt")).unwrap();
    git(repository.path(), &["mv", "--", "moved.txt", "renamed.txt"]);
    fs::write(repository.path().join("binary"), [0, 0xff]).unwrap();
    fs::write(repository.path().join("large"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    fs::write(repository.path().join("added.txt"), "Added content.\n").unwrap();
    fs::write(
        repository.path().join("mixed.txt"),
        "An ordinary sentence.\n",
    )
    .unwrap();
    git(repository.path(), &["add", "."]);
    git(
        repository.path(),
        &["update-index", "--chmod=+x", "--", "mode.txt"],
    );
    // Index-only entries work on filesystems without symlink privileges and never initialize a submodule.
    let blob = git(repository.path(), &["rev-parse", &format!("{old}:link")]);
    git(
        repository.path(),
        &[
            "update-index",
            "--cacheinfo",
            &format!("120000,{blob},link"),
        ],
    );
    git(
        repository.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{old},submodule"),
        ],
    );
    git(
        repository.path(),
        &["commit", "--quiet", "-m", "Unrelated replacement tree"],
    );
    let new = git(repository.path(), &["rev-parse", "HEAD"]);
    let original_status = git(repository.path(), &["status", "--porcelain=v1"]);
    let scope = TempDir::new().unwrap();
    let root = scope.path().join("force");
    let provider = Provider::new(repository.path(), &old, &new);
    let (snapshot, prepared) = prepare(&root, &provider, true);
    assert_eq!(prepared["changeCount"], 9);
    let page = work(
        &root,
        &provider,
        "assessment",
        &snapshot,
        &["--after".into(), "4".into(), "--limit".into(), "2".into()],
    )
    .unwrap();
    assert_eq!(page["nextOffset"], 6);
    assert_eq!(page["changes"][0], prepared["changes"][4]);
    assert_eq!(page["changes"].as_array().unwrap().len(), 2);
    let state: Value = serde_json::from_slice(&fs::read(root.join("run.json")).unwrap()).unwrap();
    let changes = state["inventory"][0]["assessment"]["inventory"]["changes"]
        .as_array()
        .unwrap();
    for (path, kind, content) in [
        ("removed.txt", "D", "text"),
        ("renamed.txt", "R", "text"),
        ("mode.txt", "M", "text"),
        ("link", "T", "text"),
        ("binary", "M", "binary"),
        ("large", "M", "tooLarge"),
        ("added.txt", "A", "text"),
        ("submodule", "A", "submodule"),
        ("mixed.txt", "M", "text"),
    ] {
        let change = changes
            .iter()
            .find(|change| change["new"]["path"] == path || change["old"]["path"] == path)
            .unwrap();
        assert_eq!(change["kind"], kind);
        let side = if change["new"].is_null() {
            "old"
        } else {
            "new"
        };
        assert_eq!(change[side]["content"]["kind"], content);
        let id = change["id"].as_str().unwrap();
        if matches!(content, "binary" | "tooLarge" | "submodule") {
            work(
                &root,
                &provider,
                "judge",
                &snapshot,
                &[
                    "--change-id".into(),
                    id.into(),
                    "--disposition".into(),
                    "insignificant".into(),
                    "--rationale".into(),
                    "Not permitted.".into(),
                    "--evidence".into(),
                    format!("{id}:{side}"),
                ],
            )
            .unwrap_err();
        }
        work(
            &root,
            &provider,
            "judge",
            &snapshot,
            &[
                "--change-id".into(),
                id.into(),
                "--disposition".into(),
                "significant".into(),
                "--rationale".into(),
                "Unsupported content or structural change requires review.".into(),
                "--evidence".into(),
                format!("{id}:{side}"),
            ],
        )
        .unwrap();
    }
    // A cumulative insignificant judgment cannot erase significant member decisions.
    work(
        &root,
        &provider,
        "judge",
        &snapshot,
        &[
            "--change-id".into(),
            "cumulative".into(),
            "--disposition".into(),
            "insignificant".into(),
            "--rationale".into(),
            "Insufficient cumulative claim.".into(),
            "--evidence".into(),
            "policy".into(),
            "--evidence".into(),
            "discussion".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        work(&root, &provider, "complete", &snapshot, &[]).unwrap()["status"],
        "needsReview"
    );
    assert_eq!(provider.writes.get(), 0);
    assert_eq!(
        git(repository.path(), &["status", "--porcelain=v1"]),
        original_status
    );
    let missing = Provider::new(repository.path(), &"f".repeat(40), &new);
    let (_, result) = prepare(&scope.path().join("missing"), &missing, true);
    assert_eq!(result["items"][0]["status"], "failed");
    assert!(!result["items"][0]["error"].is_null());
    assert_eq!(missing.writes.get(), 0);

    // Structural edits and a behavioral correction are eligible when all present content is readable.
    // Restore only opaque sides; the path, mode, type, addition and removal cases remain.
    for path in ["binary", "large"] {
        fs::write(
            repository.path().join(path),
            git(repository.path(), &["show", &format!("{old}:{path}")]),
        )
        .unwrap();
    }
    git(
        repository.path(),
        &["update-index", "--force-remove", "--", "submodule"],
    );
    fs::write(
        repository.path().join("added.txt"),
        "fn clamp(value: i32) -> i32 { value.max(0) }\n",
    )
    .unwrap();
    git(
        repository.path(),
        &["add", "--", "binary", "large", "added.txt"],
    );
    git(
        repository.path(),
        &["commit", "--quiet", "-m", "Complete readable changes"],
    );
    let readable = git(repository.path(), &["rev-parse", "HEAD"]);
    let root = scope.path().join("readable");
    let provider = Provider::new(repository.path(), &old, &readable);
    let (snapshot, prepared) = prepare(&root, &provider, false);
    assert!(
        prepared["changes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|change| change["contentAvailable"] == true)
    );
    assess(&root, &provider, &snapshot, &prepared);
    assert_eq!(
        publish(&root, &provider, &snapshot).unwrap()["items"][0]["status"],
        "reapproved"
    );
    assert_eq!(provider.writes.get(), 1);
}

fn empty_source_diff_and_corrupted_assessments_never_bypass_discussion_or_coverage() {
    let repository = TempDir::new().unwrap();
    let (old, _) = sources(repository.path());
    git(
        repository.path(),
        &["checkout", "--quiet", "--detach", &old],
    );
    git(
        repository.path(),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Same source tree",
        ],
    );
    let new = git(repository.path(), &["rev-parse", "HEAD"]);
    let scope = TempDir::new().unwrap();
    let root = scope.path().join("empty");
    let provider = Provider::new(repository.path(), &old, &new);
    let (snapshot, prepared) = prepare(&root, &provider, true);
    assert_eq!(prepared["changeCount"], 0);
    work(
        &root,
        &provider,
        "judge",
        &snapshot,
        &[
            "--change-id".into(),
            "cumulative".into(),
            "--disposition".into(),
            "insignificant".into(),
            "--rationale".into(),
            "Exact trees are equal.".into(),
            "--evidence".into(),
            "policy".into(),
            "--evidence".into(),
            "discussion".into(),
        ],
    )
    .unwrap();
    work(&root, &provider, "complete", &snapshot, &[]).unwrap_err();
    assess(&root, &provider, &snapshot, &prepared);
    let path = root.join("run.json");
    let original = fs::read(&path).unwrap();
    let record: Value = serde_json::from_slice(&original).unwrap();
    for (pointer, replacement) in [
        ("/schemaVersion", json!(4)),
        (
            "/inventory/0/assessment/documents",
            json!(["significance-policy", "ss-review-audit"]),
        ),
        ("/inventory/0/assessment/reads", json!({})),
        ("/inventory/0/assessment/cumulative", Value::Null),
        ("/inventory/0/assessment/inventory/complete", json!(false)),
        (
            "/inventory/0/assessment/snapshotDigest",
            json!("f".repeat(64)),
        ),
    ] {
        let mut corrupt = record.clone();
        *corrupt.pointer_mut(pointer).unwrap() = replacement;
        fs::write(&path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        invoke(&root, &provider, "results", &[]).unwrap_err();
    }
    fs::write(path, original).unwrap();
    assert_eq!(
        invoke(&root, &provider, "results", &[]).unwrap()["items"][0]["status"],
        "wouldReapprove"
    );
    assert_eq!(provider.writes.get(), 0);
    let output = Command::new(env!("CARGO_BIN_EXE_ss-skills-util"))
        .args(["autoreapprove", "results", "--state-path"])
        .arg(root.join("run.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["items"][0]["status"], "wouldReapprove");
}
