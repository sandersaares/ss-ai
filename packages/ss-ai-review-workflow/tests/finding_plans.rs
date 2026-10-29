//! Exercises persistent finding and plan operations together.
#![allow(
    clippy::assertions_on_result_states,
    clippy::indexing_slicing,
    reason = "Tests use fixed fixture shapes and assert operation failures directly"
)]

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::{findings, workflow_plan};
use ss_ai_runtime::{Clock, FileTransaction, SystemClock, fail};

/// Keeps test files inside the checkout, including atomic staging files.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".finding-plans-test-{}-{name}", std::process::id()));
        #[expect(
            clippy::create_dir,
            reason = "A collision must fail rather than reuse test files"
        )]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn artifacts_and_plans_preserve_work_across_failed_batches() {
    let directory = TestDirectory::new("batch");
    let clock = SystemClock::default();
    let artifact = directory.path("reviewer.json");
    let finding = directory.path("finding.json");
    let content = json!({
        "title":"Problem", "description":"What fails", "reasoning":"Why",
        "consequence":"Impact", "recommendedAction":"Fix",
        "anchor":{"kind":"line","path":"src/./lib.rs","startLine":3}
    });
    fs::write(&finding, serde_json::to_vec(&content).unwrap()).unwrap();
    findings::initialize(&artifact, "reviewer", "correctness", false, &clock).unwrap();
    let first = findings::add(&artifact, &finding, "reviewer", &[], &clock).unwrap();
    assert_eq!(first, "reviewer-1");
    let record = findings::read_artifact(&artifact, &clock).unwrap();
    assert_eq!(
        record["findings"][0]["anchor"],
        json!({
            "kind":"line","path":"src/lib.rs","startLine":3,"endLine":3,"side":"right"
        })
    );

    let before = fs::read(&artifact).unwrap();
    findings::update(&artifact, &[], &clock).unwrap();
    findings::remove(&artifact, &[], &clock).unwrap();
    assert_eq!(
        findings::read_artifact(&artifact, &clock).unwrap(),
        serde_json::from_slice::<Value>(&before).unwrap()
    );
    let updates = [
        (first.clone(), finding.clone()),
        ("missing".into(), finding.clone()),
    ];
    assert!(findings::update(&artifact, &updates, &clock).is_err());
    assert_eq!(fs::read(&artifact).unwrap(), before);
    let mut rewritten = content;
    rewritten["title"] = json!("Rewritten");
    fs::write(&finding, serde_json::to_vec(&rewritten).unwrap()).unwrap();
    findings::update(&artifact, &[(first.clone(), finding.clone())], &clock).unwrap();
    let rewritten = findings::read_artifact(&artifact, &clock).unwrap();
    assert_eq!(rewritten["findings"][0]["id"], first);
    assert_eq!(rewritten["findings"][0]["origin"], "reviewer");
    assert_eq!(rewritten["findings"][0]["title"], "Rewritten");
    let before = fs::read(&artifact).unwrap();
    assert!(
        findings::remove(
            &artifact,
            &[
                (first.clone(), "Not applicable".into()),
                ("missing".into(), "Missing".into())
            ],
            &clock,
        )
        .is_err()
    );
    assert_eq!(fs::read(&artifact).unwrap(), before);
    findings::remove(
        &artifact,
        &[(first.clone(), "Not applicable".into())],
        &clock,
    )
    .unwrap();
    assert_eq!(
        findings::add(&artifact, &finding, "reviewer", &[], &clock).unwrap(),
        "reviewer-2"
    );
    let record = findings::read_artifact(&artifact, &clock).unwrap();
    assert_eq!(record["removed"][0]["finding"]["id"], first);

    let plan = directory.path("updates.json");
    let draft = directory.path("draft.json");
    fs::write(
        &draft,
        r#"{"findingId":"reviewer-2","findingPath":"finding.json"}"#,
    )
    .unwrap();
    workflow_plan::initialize(workflow_plan::PlanKind::Updates, &plan, &clock).unwrap();
    workflow_plan::append(
        workflow_plan::PlanKind::Updates,
        &plan,
        &draft,
        None,
        &clock,
    )
    .unwrap();
    workflow_plan::append(
        workflow_plan::PlanKind::Updates,
        &plan,
        &draft,
        None,
        &clock,
    )
    .unwrap();
    let parsed = workflow_plan::read(workflow_plan::PlanKind::Updates, &plan, &clock).unwrap();
    assert_eq!(parsed.items().len(), 1);
    let serialized: Value = serde_json::from_slice(&fs::read(&plan).unwrap()).unwrap();
    assert_eq!(
        serialized["updates"][0]["findingPath"],
        finding.to_str().unwrap()
    );
    fs::write(
        &draft,
        r#"{"findingId":"reviewer-2","findingPath":"other.json"}"#,
    )
    .unwrap();
    let before = fs::read(&plan).unwrap();
    assert!(
        workflow_plan::append(
            workflow_plan::PlanKind::Updates,
            &plan,
            &draft,
            None,
            &clock
        )
        .is_err()
    );
    assert_eq!(fs::read(&plan).unwrap(), before);
    workflow_plan::append(
        workflow_plan::PlanKind::Updates,
        &plan,
        &draft,
        Some(0),
        &clock,
    )
    .unwrap();
}

#[test]
fn plan_schemas_reject_malformed_records_and_conflicts() {
    let directory = TestDirectory::new("schemas");
    let clock = SystemClock::default();
    let path = directory.path("plan.json");
    let draft = directory.path("draft.json");
    for (kind, field, invalid) in [
        (
            workflow_plan::PlanKind::MergeCandidates,
            "clusters",
            r#"{"findingIds":["a","a"]}"#,
        ),
        (
            workflow_plan::PlanKind::MergePlan,
            "groups",
            r#"{"sourceFindingIds":["a"],"findingPath":"f"}"#,
        ),
        (
            workflow_plan::PlanKind::Updates,
            "updates",
            r#"{"findingId":"a","findingPath":null}"#,
        ),
        (
            workflow_plan::PlanKind::Removals,
            "removals",
            r#"{"findingId":"a","reason":" "}"#,
        ),
        (
            workflow_plan::PlanKind::Decisions,
            "decisions",
            r#"{"findingId":"a","decision":"approved","reason":null}"#,
        ),
    ] {
        fs::write(&path, format!(r#"{{"schemaVersion":1,"{field}":[]}}"#)).unwrap();
        fs::write(&draft, invalid).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(workflow_plan::append(kind, &path, &draft, None, &clock).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    fs::write(&path, r#"{"schemaVersion":1,"groups":[]}"#).unwrap();
    fs::write(
        &draft,
        r#"{"sourceFindingIds":["a","b"],"findingPath":"f"}"#,
    )
    .unwrap();
    workflow_plan::append(
        workflow_plan::PlanKind::MergePlan,
        &path,
        &draft,
        None,
        &clock,
    )
    .unwrap();
    fs::write(
        &draft,
        r#"{"sourceFindingIds":["b","c"],"findingPath":"f"}"#,
    )
    .unwrap();
    assert!(
        workflow_plan::append(
            workflow_plan::PlanKind::MergePlan,
            &path,
            &draft,
            None,
            &clock
        )
        .is_err()
    );
    assert!(
        workflow_plan::append(
            workflow_plan::PlanKind::MergePlan,
            &path,
            &draft,
            Some(0),
            &clock
        )
        .is_ok()
    );
}

#[test]
fn artifact_rejects_unknown_fields() {
    let invalid = json!({
        "schemaVersion":1, "kind":"reviewer", "reviewerId":"R",
        "focusArea":"correctness", "status":"complete", "findings":[], "unexpected":true
    });
    assert!(findings::validate_artifact(&invalid).is_err());
    let mut invalid = invalid;
    invalid.as_object_mut().unwrap().remove("unexpected");
    assert!(findings::validate_artifact(&invalid).is_ok());
}

#[test]
fn merge_preserves_locations_and_untouched_findings() {
    let directory = TestDirectory::new("merge");
    let clock = SystemClock::default();
    let reviewers = directory.path("reviewers");
    #[expect(
        clippy::create_dir,
        reason = "A collision must fail rather than reuse test files"
    )]
    fs::create_dir(&reviewers).unwrap();
    let a = reviewers.join("a.json");
    let b = reviewers.join("b.json");
    let content = |path| {
        json!({
            "title":"Problem", "description":"What fails", "reasoning":"Why",
            "consequence":"Impact", "recommendedAction":"Fix",
            "anchor":{"kind":"file","path":path}
        })
    };
    let authored = directory.path("finding.json");
    findings::initialize(&a, "a", "correctness", false, &clock).unwrap();
    findings::initialize(&b, "b", "safety", false, &clock).unwrap();
    let report = findings::validate_artifacts(
        &reviewers,
        &["a".into(), "b".into(), "missing".into()],
        &clock,
    )
    .unwrap();
    assert_eq!(report["isValid"], false);
    assert_eq!(report["incompleteReviewers"], json!(["a", "b"]));
    assert_eq!(report["missingReviewers"], json!(["missing"]));
    let mut a_content = content("a.rs");
    a_content["followUp"] = json!({"threadId":"thread","commentId":"comment"});
    let mut b_content = content("b.rs");
    b_content["followUp"] = json!({"threadId":"thread","commentId":"comment"});
    fs::write(&authored, serde_json::to_vec(&a_content).unwrap()).unwrap();
    findings::add(&a, &authored, "reviewer", &[], &clock).unwrap();
    fs::write(&authored, serde_json::to_vec(&b_content).unwrap()).unwrap();
    findings::add(&b, &authored, "reviewer", &[], &clock).unwrap();
    let output = directory.path("consolidated.json");
    let open = findings::consolidate(&reviewers, None, &output, &clock).unwrap();
    assert_eq!(open["findings"].as_array().unwrap().len(), 2);
    fs::remove_file(&output).unwrap();
    for (path, id) in [(&a, "a-1"), (&b, "b-1")] {
        let queue = directory.path(&format!("queue-{id}.json"));
        let journal = json!({
            "schemaVersion": 3, "kind": "fileGroups", "items": [], "inProgress": null,
            "completedGroupIds": ["group"], "assessments": [],
            "batchReceipts": [{"groupId":"group", "sha256":"A".repeat(64),
                "artifactPath":path.to_str().unwrap(), "findingIds":[id]}]
        });
        fs::write(&queue, serde_json::to_vec(&journal).unwrap()).unwrap();
        findings::complete(path, &queue, &clock).unwrap();
    }
    let report =
        findings::validate_artifacts(&reviewers, &["a".into(), "b".into()], &clock).unwrap();
    assert_eq!(report["isValid"], true);
    let merged = directory.path("merged.json");
    fs::write(&merged, serde_json::to_vec(&content("a.rs")).unwrap()).unwrap();
    let plan = directory.path("plan.json");
    fs::write(&plan, serde_json::to_vec(&json!({
        "schemaVersion":1,"groups":[{"sourceFindingIds":["a-1","b-1"],"findingPath":"merged.json"}]
    })).unwrap()).unwrap();
    assert!(findings::consolidate(&reviewers, Some(&plan), &output, &clock).is_err());
    assert!(!output.exists());
    let mut covering = content("a.rs");
    covering["impactedLocations"] = json!([{"path":"b.rs"}]);
    covering["followUp"] = json!({"threadId":"other","commentId":"comment"});
    fs::write(&merged, serde_json::to_vec(&covering).unwrap()).unwrap();
    assert!(findings::consolidate(&reviewers, Some(&plan), &output, &clock).is_err());
    covering.as_object_mut().unwrap().remove("followUp");
    fs::write(&merged, serde_json::to_vec(&covering).unwrap()).unwrap();
    let result = findings::consolidate_result(&reviewers, Some(&plan), &output, &clock).unwrap();
    assert_eq!(
        (
            result.finding_count,
            result.merged_count,
            result.carried_over_count
        ),
        (1, 1, 0)
    );
    assert_eq!(result.output_path, output);
    let result = result.artifact;
    assert_eq!(result["findings"][0]["id"], "F1");
    assert_eq!(
        result["findings"][0]["sourceFindingIds"],
        json!(["a-1", "b-1"])
    );
    assert_eq!(
        result["findings"][0]["followUp"],
        json!({"threadId":"thread","commentId":"comment"})
    );
    let result = findings::consolidate_result(&reviewers, None, &output, &clock).unwrap();
    assert_eq!(
        (
            result.finding_count,
            result.merged_count,
            result.carried_over_count
        ),
        (2, 0, 2)
    );

    let empty = reviewers.join("empty.json");
    findings::initialize(&empty, "empty", "correctness", false, &clock).unwrap();
    #[cfg(unix)]
    fs::write(reviewers.join(".hidden.json"), "not an artifact").unwrap();
    let index = directory.path("index.json");
    let result = findings::build_index_result(&reviewers, &index, &clock).unwrap();
    assert_eq!((result.artifact_count, result.finding_count), (3, 2));
    assert_eq!(result.output_path, index);
    assert_eq!(result.artifact["findings"].as_array().unwrap().len(), 2);
    let candidates = directory.path("candidates.json");
    fs::write(
        &candidates,
        r#"{"schemaVersion":1,"clusters":[{"findingIds":["a-1","b-1"]}]}"#,
    )
    .unwrap();
    assert_eq!(
        findings::validate_candidates(&candidates, &index, &clock).unwrap(),
        1
    );
    fs::write(
        &candidates,
        r#"{"schemaVersion":1,"clusters":[{"findingIds":["a-1","missing"]}]}"#,
    )
    .unwrap();
    assert!(findings::validate_candidates(&candidates, &index, &clock).is_err());
    fs::write(
        &candidates,
        r#"{"schemaVersion":1,"clusters":[],"clusters":[]}"#,
    )
    .unwrap();
    assert!(findings::validate_candidates(&candidates, &index, &clock).is_err());
}

#[test]
fn completion_rejects_unfinished_or_unverified_work() {
    let directory = TestDirectory::new("completion");
    let clock = SystemClock::default();
    let artifact = directory.path("reviewer.json");
    let queue = directory.path("queue.json");
    findings::initialize(&artifact, "reviewer", "correctness", false, &clock).unwrap();
    let before = fs::read(&artifact).unwrap();
    assert!(findings::complete(&artifact, &queue, &clock).is_err());
    let mut journal = json!({
        "schemaVersion":3, "kind":"fileGroups", "items":[], "inProgress":null,
        "completedGroupIds":["group"], "assessments":[],
        "batchReceipts":[{"groupId":"group","sha256":"A".repeat(64),
            "artifactPath":artifact.to_str().unwrap(),"findingIds":["missing"]}]
    });
    fs::write(&queue, serde_json::to_vec(&journal).unwrap()).unwrap();
    assert!(findings::complete(&artifact, &queue, &clock).is_err());
    assert_eq!(fs::read(&artifact).unwrap(), before);
    journal["batchReceipts"][0]["findingIds"] = json!([]);
    journal["failure"] = json!("worker failed");
    fs::write(&queue, serde_json::to_vec(&journal).unwrap()).unwrap();
    assert!(findings::complete(&artifact, &queue, &clock).is_err());
    journal["failure"] = Value::Null;
    fs::write(&queue, serde_json::to_vec(&journal).unwrap()).unwrap();
    let complete = findings::complete(&artifact, &queue, &clock).unwrap();
    assert_eq!(complete["status"], "complete");
    assert_eq!(complete["findings"], json!([]));
    let resumed = findings::initialize_with_queue_result(
        &artifact,
        "reviewer",
        "correctness",
        false,
        true,
        &queue,
        &clock,
    )
    .unwrap();
    assert!(resumed.resumed_existing);
    assert_eq!(resumed.artifact["status"], "open");
    let new_artifact = directory.path("new.json");
    let new_queue = directory.path("new-queue.json");
    let created = findings::initialize_with_queue_result(
        &new_artifact,
        "new",
        "correctness",
        false,
        true,
        &new_queue,
        &clock,
    )
    .unwrap();
    assert!(!created.resumed_existing);
    assert!(
        findings::initialize_with_queue(
            &artifact,
            "other",
            "correctness",
            false,
            true,
            &queue,
            &clock,
        )
        .is_err()
    );
    let missing_artifact = directory.path("missing-reviewer.json");
    assert!(
        findings::initialize_with_queue(
            &missing_artifact,
            "reviewer",
            "correctness",
            false,
            true,
            &queue,
            &clock,
        )
        .is_err()
    );
}

#[test]
fn concurrent_appends_allocate_distinct_ids() {
    let directory = TestDirectory::new("concurrent");
    let clock = SystemClock::default();
    let artifact = directory.path("reviewer.json");
    let finding = directory.path("finding.json");
    fs::write(&finding, r#"{"title":"Problem","description":"What fails","reasoning":"Why","consequence":"Impact","recommendedAction":"Fix","anchor":{"kind":"file","path":"src/lib.rs"}}"#).unwrap();
    findings::initialize(&artifact, "reviewer", "correctness", false, &clock).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    // Both workers must start before either join, because they share a barrier.
    #[expect(
        clippy::needless_collect,
        reason = "Joining lazily would block at the barrier"
    )]
    let threads: Vec<_> = std::iter::repeat_with(|| {
        std::thread::spawn({
            let barrier = std::sync::Arc::clone(&barrier);
            let artifact = artifact.clone();
            let finding = finding.clone();
            move || {
                barrier.wait();
                findings::add(
                    &artifact,
                    &finding,
                    "reviewer",
                    &[],
                    &SystemClock::default(),
                )
                .unwrap()
            }
        })
    })
    .take(2)
    .collect();
    let mut ids: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    ids.sort();
    assert_eq!(ids, ["reviewer-1", "reviewer-2"]);
    assert_eq!(
        findings::read_artifact(&artifact, &clock).unwrap()["findings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

/// Reports contention and completion without elapsed-time coordination.
enum UpdateEvent {
    Waiting(usize),
    Complete(usize, bool),
}

/// Holds a worker on its first lock conflict until the test permits it.
struct UpdateClock {
    index: usize,
    events: Sender<UpdateEvent>,
    permit: RefCell<Option<Receiver<()>>>,
}

/// Signals a parent PowerShell process when the native lock actually contends.
struct ProcessInteropClock {
    waited: Cell<bool>,
}

impl Clock for ProcessInteropClock {
    fn now(&self) -> Timestamp {
        panic!("Appending findings must not depend on wall-clock time")
    }

    fn wait(&self, _duration: Duration) -> Result<(), AppError> {
        if self.waited.replace(true) {
            return Err(fail("contention", "Unexpected repeated lock contention"));
        }
        writeln!(io::stderr(), "lock-contended")
            .map_err(|error| ss_ai_runtime::caused_by("io", "Cannot signal contention", error))?;
        let mut reply = String::new();
        io::stdin()
            .read_line(&mut reply)
            .map_err(|error| ss_ai_runtime::caused_by("io", "Cannot receive retry", error))?;
        if reply.trim_end() != "retry" {
            return Err(fail("invalidInput", "Expected retry permission"));
        }
        Ok(())
    }
}

/// Runs only when the Pester parent supplies the paths for a cross-process lock test.
#[test]
fn native_process_lock_interop_helper() {
    let Some(artifact) = std::env::var_os("SS_AI_TEST_INTEROP_ARTIFACT") else {
        return;
    };
    let finding = std::env::var_os("SS_AI_TEST_INTEROP_FINDING")
        .expect("The parent supplies both finding paths");
    let clock = ProcessInteropClock {
        waited: Cell::new(false),
    };
    findings::add(
        Path::new(&artifact),
        Path::new(&finding),
        "reviewer",
        &[],
        &clock,
    )
    .unwrap();
}

impl Clock for UpdateClock {
    fn now(&self) -> Timestamp {
        panic!("Updating findings must not depend on wall-clock time")
    }

    fn wait(&self, _duration: Duration) -> Result<(), AppError> {
        let permit = self
            .permit
            .borrow_mut()
            .take()
            .ok_or_else(|| fail("contention", "Unexpected repeated lock contention"))?;
        self.events
            .send(UpdateEvent::Waiting(self.index))
            .map_err(|error| ss_ai_runtime::caused_by("io", "Cannot signal contention", error))?;
        permit
            .recv()
            .map_err(|error| ss_ai_runtime::caused_by("io", "Cannot receive permit", error))?;
        Ok(())
    }
}

#[test]
fn synchronized_updates_preserve_both_rewrites() {
    let directory = TestDirectory::new("synchronized-updates");
    let clock = SystemClock::default();
    let artifact = directory.path("reviewer.json");
    let original = directory.path("original.json");
    let one = directory.path("one.json");
    let two = directory.path("two.json");
    let mut content = json!({
        "title":"The retry loop never terminates", "description":"What fails",
        "reasoning":"Why", "consequence":"Impact", "recommendedAction":"Fix",
        "anchor":{"kind":"line","path":"src/retry.rs","startLine":42}
    });
    fs::write(&original, serde_json::to_vec(&content).unwrap()).unwrap();
    findings::initialize(&artifact, "g1", "correctness", false, &clock).unwrap();
    findings::add(&artifact, &original, "reviewer", &[], &clock).unwrap();
    findings::add(&artifact, &original, "reviewer", &[], &clock).unwrap();
    content["title"] = json!("First rewrite");
    fs::write(&one, serde_json::to_vec(&content).unwrap()).unwrap();
    content["title"] = json!("Second rewrite");
    fs::write(&two, serde_json::to_vec(&content).unwrap()).unwrap();

    // Both updates must encounter the held lock before either can read the artifact.
    let transaction = FileTransaction::open(&artifact, &clock).unwrap();
    let (events, received) = mpsc::channel();
    let mut permits = Vec::new();
    let mut workers = Vec::new();
    for (index, (id, path)) in [("g1-1", one), ("g1-2", two)].into_iter().enumerate() {
        let (permit, receiver) = mpsc::channel();
        permits.push(permit);
        workers.push(std::thread::spawn({
            let artifact = artifact.clone();
            let events = events.clone();
            move || {
                let clock = UpdateClock {
                    index,
                    events: events.clone(),
                    permit: RefCell::new(Some(receiver)),
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    findings::update(&artifact, &[(id.to_owned(), path)], &clock)
                }));
                events
                    .send(UpdateEvent::Complete(
                        index,
                        result.is_ok_and(|result| result.is_ok()),
                    ))
                    .unwrap();
            }
        }));
    }
    let mut waiting = Vec::new();
    let mut results = Vec::new();
    for _ in 0..2 {
        match received.recv().unwrap() {
            UpdateEvent::Waiting(index) => waiting.push(index),
            UpdateEvent::Complete(index, success) => results.push((index, success)),
        }
    }
    drop(transaction);
    // Release one worker at a time so retries do not depend on scheduling or time.
    for index in &waiting {
        permits[*index].send(()).unwrap();
        match received.recv().unwrap() {
            UpdateEvent::Complete(completed, success) => results.push((completed, success)),
            UpdateEvent::Waiting(_) => panic!("A permitted worker unexpectedly retried"),
        }
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(waiting.len(), 2);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|(_, success)| *success));
    let artifact = findings::read_artifact(&artifact, &clock).unwrap();
    assert_eq!(artifact["findings"][0]["title"], "First rewrite");
    assert_eq!(artifact["findings"][1]["title"], "Second rewrite");
}

/// Runs the predecessor model so both implementations consume actual records.
fn powershell(script: &str, arguments: &[&str], work_dir: &Path) {
    let output = powershell_output(script, arguments, work_dir);
    assert!(
        output.status.success(),
        "{script}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Returns predecessor-model diagnostics for acceptance and rejection comparisons.
fn powershell_output(script: &str, arguments: &[&str], work_dir: &Path) -> Output {
    let script_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/finding_predecessor.ps1");
    Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(script_path)
        .arg("-CommandName")
        .arg(script)
        .args(arguments)
        .env("TMPDIR", work_dir)
        .output()
        .unwrap()
}

#[test]
#[cfg_attr(miri, ignore = "The predecessor model runs in a separate process")]
fn powershell_and_rust_share_lifecycle_index_and_consolidation_records() {
    let directory = TestDirectory::new("powershell-roundtrip");
    let clock = SystemClock::default();
    let root = directory.path("reviewers");
    #[expect(
        clippy::create_dir,
        reason = "A collision must fail rather than reuse test files"
    )]
    fs::create_dir(&root).unwrap();
    let alpha = root.join("alpha.json");
    let beta = root.join("beta.json");
    let content = directory.path("authored.json");
    let revised = directory.path("revised.json");
    let queue = directory.path("queue.json");
    let original = json!({
        "title":"The retry loop never terminates",
        "description":"A \"quoted\" phrase, a `backtick`, an escape \\n, and a path C:\\src\\lib.rs.\nA second line with 🚀.",
        "reasoning":"The counter is incremented in a branch that cannot be reached.",
        "consequence":"A transient failure hangs the caller indefinitely.",
        "recommendedAction":"Increment the counter outside the branch.",
        "anchor":{"kind":"line","path":"src/./retry.rs","startLine":42},
        "references":[{"text":"Reference","url":"https://example.invalid"}],
        "impactedLocations":[{"path":"src/backoff.rs","startLine":8}],
        "followUp":{"threadId":"RT_kw1","commentId":"PRRC_1"}
    });
    fs::write(&content, serde_json::to_vec(&original).unwrap()).unwrap();
    let mut rewritten = original.clone();
    rewritten["title"] = json!("Rewrite without changing provenance");
    fs::write(&revised, serde_json::to_vec(&rewritten).unwrap()).unwrap();
    let root_text = root.to_str().unwrap();
    let alpha_text = alpha.to_str().unwrap();
    let content_text = content.to_str().unwrap();
    let revised_text = revised.to_str().unwrap();
    powershell(
        "New-FindingArtifact.ps1",
        &[
            "-OutputDirectory",
            root_text,
            "-ReviewerId",
            "alpha",
            "-FocusArea",
            "correctness",
        ],
        &directory.0,
    );
    powershell(
        "Add-Finding.ps1",
        &["-ArtifactPath", alpha_text, "-FindingPath", content_text],
        &directory.0,
    );
    let artifact = findings::read_artifact(&alpha, &clock).unwrap();
    assert_eq!(artifact["findings"][0]["id"], "alpha-1");
    assert_eq!(
        artifact["findings"][0]["description"],
        original["description"]
    );
    assert_eq!(artifact["findings"][0]["anchor"]["path"], "src/retry.rs");
    assert_eq!(
        findings::add(&alpha, &content, "synthesis", &["source-1".into()], &clock).unwrap(),
        "alpha-2"
    );
    powershell(
        "Update-Finding.ps1",
        &[
            "-ArtifactPath",
            alpha_text,
            "-FindingId",
            "alpha-2",
            "-FindingPath",
            revised_text,
        ],
        &directory.0,
    );
    let artifact = findings::read_artifact(&alpha, &clock).unwrap();
    assert_eq!(artifact["findings"][1]["origin"], "synthesis");
    assert_eq!(
        artifact["findings"][1]["sourceFindingIds"],
        json!(["source-1"])
    );
    assert_eq!(artifact["findings"][1]["title"], rewritten["title"]);
    findings::remove(
        &alpha,
        &[("alpha-1".into(), "Covered elsewhere".into())],
        &clock,
    )
    .unwrap();
    powershell(
        "Add-Finding.ps1",
        &["-ArtifactPath", alpha_text, "-FindingPath", content_text],
        &directory.0,
    );
    let artifact = findings::read_artifact(&alpha, &clock).unwrap();
    assert_eq!(artifact["removed"][0]["finding"]["id"], "alpha-1");
    assert_eq!(artifact["removed"][0]["reason"], "Covered elsewhere");
    assert_eq!(artifact["findings"][1]["id"], "alpha-3");

    fs::write(&queue, r#"{"schemaVersion":3,"kind":"fileGroups","items":[],"inProgress":null,"completedGroupIds":[],"assessments":[]}"#).unwrap();
    powershell(
        "Complete-FindingArtifact.ps1",
        &[
            "-ArtifactPath",
            alpha_text,
            "-FileGroupQueuePath",
            queue.to_str().unwrap(),
        ],
        &directory.0,
    );
    findings::initialize(&beta, "beta", "security", false, &clock).unwrap();
    findings::complete(&beta, &queue, &clock).unwrap();
    powershell(
        "Test-FindingArtifact.ps1",
        &["-Path", root_text, "-ExpectedReviewerId", "alpha,beta"],
        &directory.0,
    );
    let index_ps = directory.path("index-ps.json");
    let index_rs = directory.path("index-rs.json");
    powershell(
        "Get-FindingIndex.ps1",
        &[
            "-ReviewerArtifactDirectory",
            root_text,
            "-OutputPath",
            index_ps.to_str().unwrap(),
        ],
        &directory.0,
    );
    let rust_index = findings::build_index(&root, &index_rs, &clock).unwrap();
    let ps_index: Value = serde_json::from_slice(&fs::read(index_ps).unwrap()).unwrap();
    assert_eq!(rust_index, ps_index);
    let ps_output = directory.path("consolidated-ps.json");
    let rs_output = directory.path("consolidated-rs.json");
    powershell(
        "New-ConsolidatedFindings.ps1",
        &[
            "-ReviewerArtifactDirectory",
            root_text,
            "-OutputPath",
            ps_output.to_str().unwrap(),
        ],
        &directory.0,
    );
    let rust_output = findings::consolidate(&root, None, &rs_output, &clock).unwrap();
    let ps_output: Value = serde_json::from_slice(&fs::read(ps_output).unwrap()).unwrap();
    assert_eq!(rust_output, ps_output);
}

#[test]
#[cfg_attr(miri, ignore = "The predecessor model runs in a separate process")]
fn finding_content_cases_match_the_predecessor_corpus() {
    let directory = TestDirectory::new("content-corpus");
    let clock = SystemClock::default();
    let artifact = directory.path("reviewer.json");
    let content = directory.path("finding.json");
    findings::initialize(&artifact, "g1", "correctness", false, &clock).unwrap();
    let baseline = json!({
        "title":"The retry loop never terminates",
        "description":"The loop condition is never updated.",
        "reasoning":"The counter is incremented in a branch that cannot be reached.",
        "consequence":"A transient failure hangs the caller indefinitely.",
        "recommendedAction":"Increment the counter outside the branch.",
        "anchor":{"kind":"line","path":"src/retry.rs","startLine":42}
    });
    let mut cases = vec![("valid", baseline.clone(), true)];
    for (name, field, value, valid) in [
        ("blank description", "description", json!("  "), false),
        ("non-string reasoning", "reasoning", json!(5), false),
        (
            "control in prose",
            "description",
            json!("one\u{0001}two"),
            false,
        ),
        (
            "formatting whitespace",
            "description",
            json!("one\ntwo\tthree"),
            true,
        ),
        ("control in title", "title", json!("one\ntwo"), false),
        ("title at bound", "title", json!("a".repeat(255)), true),
        ("title over bound", "title", json!("a".repeat(256)), false),
        ("unknown priority", "priority", json!("high"), false),
        ("tool-owned id", "id", json!("manual"), false),
    ] {
        let mut finding = baseline.clone();
        finding[field] = value;
        cases.push((name, finding, valid));
    }
    for (name, finding, valid) in cases {
        fs::write(&content, serde_json::to_vec(&finding).unwrap()).unwrap();
        let result = powershell_output(
            "ValidateFinding",
            &["-FindingPath", content.to_str().unwrap()],
            &directory.0,
        );
        assert_eq!(result.status.success(), valid, "PowerShell case {name}");
        let result = findings::add(&artifact, &content, "reviewer", &[], &clock);
        assert_eq!(result.is_ok(), valid, "Rust case {name}: {result:?}");
    }
}

#[test]
#[cfg_attr(miri, ignore = "The predecessor model runs in a separate process")]
fn consolidation_merge_cases_match_the_predecessor_corpus() {
    let directory = TestDirectory::new("merge-corpus");
    let clock = SystemClock::default();
    let reviewers = directory.path("reviewers");
    #[expect(
        clippy::create_dir,
        reason = "A collision must fail rather than reuse test files"
    )]
    fs::create_dir(&reviewers).unwrap();
    let first = reviewers.join("g1.json");
    let second = reviewers.join("g2.json");
    let content = directory.path("content.json");
    let merged = directory.path("merged.json");
    let plan = directory.path("plan.json");
    let legacy_output = directory.path("legacy.json");
    let native_output = directory.path("native.json");
    let finding = |path, line| {
        json!({
            "title":"Terminate the retry loop",
            "description":"The loop condition is never updated.",
            "reasoning":"The counter is incremented in a branch that cannot be reached.",
            "consequence":"A transient failure hangs the caller indefinitely.",
            "recommendedAction":"Increment the counter outside the branch.",
            "anchor":{"kind":"line","path":path,"startLine":line},
            "followUp":{"threadId":"RT_kw1","commentId":"PRRC_1"}
        })
    };
    findings::initialize(&first, "g1", "correctness", false, &clock).unwrap();
    findings::initialize(&second, "g2", "security", false, &clock).unwrap();
    fs::write(
        &content,
        serde_json::to_vec(&finding("src/retry.rs", 42)).unwrap(),
    )
    .unwrap();
    findings::add(&first, &content, "reviewer", &[], &clock).unwrap();
    findings::add(&first, &content, "reviewer", &[], &clock).unwrap();
    fs::write(
        &content,
        serde_json::to_vec(&finding("src/backoff.rs", 8)).unwrap(),
    )
    .unwrap();
    findings::add(&second, &content, "synthesis", &[], &clock).unwrap();
    fs::write(
        &merged,
        serde_json::to_vec(&finding("src/retry.rs", 42)).unwrap(),
    )
    .unwrap();
    fs::write(&plan, serde_json::to_vec(&json!({
            "schemaVersion":1, "groups":[{"sourceFindingIds":["g1-1","g2-1"],"findingPath":"merged.json"}]
        })).unwrap()).unwrap();
    let args = [
        "-ReviewerArtifactDirectory",
        reviewers.to_str().unwrap(),
        "-MergePlanPath",
        plan.to_str().unwrap(),
        "-OutputPath",
        legacy_output.to_str().unwrap(),
    ];
    let legacy = powershell_output("New-ConsolidatedFindings.ps1", &args, &directory.0);
    assert!(
        !legacy.status.success(),
        "Predecessor must reject dropped locations"
    );
    assert!(findings::consolidate(&reviewers, Some(&plan), &native_output, &clock).is_err());
    assert!(!legacy_output.exists() && !native_output.exists());

    let mut covering = finding("src/retry.rs", 40);
    covering["anchor"]["endLine"] = json!(45);
    covering["impactedLocations"] = json!([{"path":"src/backoff.rs","startLine":5,"endLine":10}]);
    fs::write(&merged, serde_json::to_vec(&covering).unwrap()).unwrap();
    powershell("New-ConsolidatedFindings.ps1", &args, &directory.0);
    let native = findings::consolidate(&reviewers, Some(&plan), &native_output, &clock).unwrap();
    let legacy: Value = serde_json::from_slice(&fs::read(&legacy_output).unwrap()).unwrap();
    assert_eq!(native, legacy);
    assert_eq!(native["findings"][0]["origin"], "synthesis");
    assert_eq!(
        native["findings"][0]["sourceFindingIds"],
        json!(["g1-1", "g2-1"])
    );
    assert_eq!(native["findings"][1]["sourceFindingIds"], json!(["g1-2"]));

    fs::write(
        &plan,
        serde_json::to_vec(&json!({
            "schemaVersion":1, "groups":[
                {"sourceFindingIds":["g1-1","g2-1"],"findingPath":"merged.json"},
                {"sourceFindingIds":["g1-1","g1-2"],"findingPath":"merged.json"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let duplicate = findings::consolidate(&reviewers, Some(&plan), &native_output, &clock)
        .unwrap_err()
        .to_string();
    assert!(duplicate.contains("'g1-1'"), "{duplicate}");
    fs::write(&plan, serde_json::to_vec(&json!({
        "schemaVersion":1, "groups":[{"sourceFindingIds":["g1-1","g2-1"],"findingPath":"merged.json"}]
    })).unwrap()).unwrap();

    let mut different = finding("src/backoff.rs", 8);
    different["followUp"]["threadId"] = json!("another-thread");
    fs::write(&content, serde_json::to_vec(&different).unwrap()).unwrap();
    findings::update(&second, &[("g2-1".into(), content)], &clock).unwrap();
    let legacy_before = fs::read(&legacy_output).unwrap();
    let native_before = fs::read(&native_output).unwrap();
    assert!(
        !powershell_output("New-ConsolidatedFindings.ps1", &args, &directory.0)
            .status
            .success()
    );
    assert!(findings::consolidate(&reviewers, Some(&plan), &native_output, &clock).is_err());
    assert_eq!(fs::read(&legacy_output).unwrap(), legacy_before);
    assert_eq!(fs::read(&native_output).unwrap(), native_before);
}

#[test]
#[cfg_attr(miri, ignore = "The predecessor model runs in a separate process")]
fn finding_anchor_cases_match_the_predecessor_corpus() {
    let directory = TestDirectory::new("anchor-corpus");
    let clock = SystemClock::default();
    let artifact = directory.path("reviewer.json");
    let content = directory.path("finding.json");
    findings::initialize(&artifact, "g1", "correctness", false, &clock).unwrap();
    let baseline = json!({
        "title":"The retry loop never terminates",
        "description":"The loop condition is never updated.",
        "reasoning":"The counter is incremented in a branch that cannot be reached.",
        "consequence":"A transient failure hangs the caller indefinitely.",
        "recommendedAction":"Increment the counter outside the branch.",
        "anchor":{"kind":"line","path":"src/retry.rs","startLine":42}
    });
    let mut cases = Vec::new();
    for (name, anchor, valid) in [
        (
            "file anchor",
            json!({"kind":"file","path":"src/retry.rs"}),
            true,
        ),
        (
            "file with position",
            json!({"kind":"file","path":"src/retry.rs","startLine":1}),
            false,
        ),
        (
            "missing line",
            json!({"kind":"line","path":"src/retry.rs"}),
            false,
        ),
        (
            "zero line",
            json!({"kind":"line","path":"src/retry.rs","startLine":0}),
            false,
        ),
        (
            "reversed lines",
            json!({"kind":"line","path":"src/retry.rs","startLine":42,"endLine":41}),
            false,
        ),
        (
            "left side",
            json!({"kind":"line","path":"src/retry.rs","startLine":42,"side":"left"}),
            true,
        ),
        (
            "unknown side",
            json!({"kind":"line","path":"src/retry.rs","startLine":42,"side":"middle"}),
            false,
        ),
        (
            "canonicalizable",
            json!({"kind":"line","path":"src/./retry.rs","startLine":42}),
            true,
        ),
        (
            "escaping path",
            json!({"kind":"line","path":"../retry.rs","startLine":42}),
            false,
        ),
        (
            "backslash path",
            json!({"kind":"line","path":"src\\retry.rs","startLine":42}),
            false,
        ),
    ] {
        let mut finding = baseline.clone();
        finding["anchor"] = anchor;
        cases.push((name, finding, valid));
    }
    for (name, finding, valid) in cases {
        fs::write(&content, serde_json::to_vec(&finding).unwrap()).unwrap();
        let result = powershell_output(
            "ValidateFinding",
            &["-FindingPath", content.to_str().unwrap()],
            &directory.0,
        );
        assert_eq!(result.status.success(), valid, "PowerShell case {name}");
        let result = findings::add(&artifact, &content, "reviewer", &[], &clock);
        assert_eq!(result.is_ok(), valid, "Rust case {name}: {result:?}");
    }
}

#[test]
#[cfg_attr(miri, ignore = "The predecessor model runs in a separate process")]
fn standalone_validation_reports_all_damaged_reviewer_evidence() {
    let directory = TestDirectory::new("validation-corpus");
    let clock = SystemClock::default();
    let artifact = directory.path("g1.json");
    let content = directory.path("finding.json");
    let legacy = directory.path("legacy-validation.json");
    fs::write(&content, r#"{"title":"The retry loop never terminates","description":"The loop condition is never updated.","reasoning":"Why","consequence":"Impact","recommendedAction":"Fix","anchor":{"kind":"line","path":"src/retry.rs","startLine":42}}"#).unwrap();
    findings::initialize(&artifact, "g1", "correctness", false, &clock).unwrap();
    findings::add(&artifact, &content, "reviewer", &[], &clock).unwrap();
    let mut damaged = findings::read_artifact(&artifact, &clock).unwrap();
    damaged["status"] = json!("partial");
    damaged["findings"][0]
        .as_object_mut()
        .unwrap()
        .remove("title");
    damaged["findings"][0]["severity"] = json!("high");
    fs::write(&artifact, serde_json::to_vec(&damaged).unwrap()).unwrap();

    powershell(
        "GetArtifactValidation",
        &[
            "-Path",
            artifact.to_str().unwrap(),
            "-ExpectedReviewerId",
            "g1",
            "-OutputPath",
            legacy.to_str().unwrap(),
        ],
        &directory.0,
    );
    let legacy: Value = serde_json::from_slice(&fs::read(&legacy).unwrap()).unwrap();
    let native = findings::validate_artifacts(&artifact, &["g1".into()], &clock).unwrap();
    for field in [
        "isValid",
        "findingCount",
        "artifacts",
        "missingReviewers",
        "incompleteReviewers",
        "unexpectedReviewers",
    ] {
        assert_eq!(
            native[field], legacy[field],
            "Mismatched report field {field}"
        );
    }

    let errors = native["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(errors.contains("partial"));
    assert!(errors.contains("severity"));
    assert!(native["errors"].as_array().unwrap().len() > 1);
}

#[test]
fn errors_identify_duplicate_and_invalid_finding_inputs() {
    let directory = TestDirectory::new("diagnostics");
    let clock = SystemClock::default();
    let artifact = directory.path("reviewer.json");
    let content = directory.path("invalid.json");
    findings::initialize(&artifact, "reviewer", "correctness", false, &clock).unwrap();
    let duplicate = findings::initialize(&artifact, "reviewer", "correctness", false, &clock)
        .unwrap_err()
        .to_string();
    assert!(duplicate.contains("-Force"), "{duplicate}");
    fs::write(&content, r#"{"priority":"high"}"#).unwrap();
    let invalid = findings::add(&artifact, &content, "reviewer", &[], &clock)
        .unwrap_err()
        .to_string();
    assert!(invalid.contains("'priority'"), "{invalid}");
    assert!(invalid.contains("invalid.json"), "{invalid}");
    fs::write(&content, "{").unwrap();
    let malformed = findings::update(&artifact, &[("reviewer-1".into(), content)], &clock)
        .unwrap_err()
        .to_string();
    assert!(malformed.contains("invalid.json"), "{malformed}");
}

/// Supplies a fixed removal moment without using wall-clock time.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2024-05-06T07:08:09Z".parse().unwrap()
    }

    fn wait(&self, _duration: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected lock contention"))
    }
}

#[test]
fn removal_timestamp_uses_dotnet_round_trip_precision() {
    let directory = TestDirectory::new("removal-time");
    let artifact = directory.path("reviewer.json");
    let finding = directory.path("finding.json");
    let clock = FixedClock;
    findings::initialize(&artifact, "g1", "correctness", false, &clock).unwrap();
    let content = json!({
        "title":"Problem", "description":"What fails", "reasoning":"Why",
        "consequence":"Impact", "recommendedAction":"Fix",
        "anchor":{"kind":"file","path":"src/lib.rs"},
        "followUp":{"threadId":"RT_kw1","commentId":"PRRC_1"}
    });
    fs::write(&finding, serde_json::to_vec(&content).unwrap()).unwrap();
    let id = findings::add(&artifact, &finding, "reviewer", &[], &clock).unwrap();
    findings::remove(&artifact, &[(id, "Not applicable".into())], &clock).unwrap();
    let result = findings::read_artifact(&artifact, &clock).unwrap();
    assert_eq!(
        result["removed"][0]["removedUtc"],
        "2024-05-06T07:08:09.0000000Z"
    );
}

#[test]
fn foreign_entry_report_quotes_its_basename() {
    let directory = TestDirectory::new("foreign-entry");
    #[expect(clippy::create_dir, reason = "The entry must not already exist")]
    fs::create_dir(directory.path("staging")).unwrap();
    let report = findings::validate_artifacts(&directory.0, &[], &SystemClock::default()).unwrap();
    assert!(report["errors"].to_string().contains("'staging'"));
}
