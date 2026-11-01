//! Exercises persistent finding and plan operations together.
#![allow(
    clippy::assertions_on_result_states,
    clippy::indexing_slicing,
    reason = "Tests use fixed fixture shapes and assert operation failures directly"
)]

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::{self, Write};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::{
    findings, focus_area_expansion, focus_area_generation, review_work, worker_input, workflow_plan,
};
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

/// Public workflow records must remain usable across panic-catching boundaries.
#[test]
fn public_records_are_unwind_safe() {
    fn assert_traits<T: UnwindSafe + RefUnwindSafe>() {}

    assert_traits::<findings::InitializationResult>();
    assert_traits::<findings::IndexResult>();
    assert_traits::<findings::ConsolidationResult>();
    assert_traits::<focus_area_expansion::Referrer>();
    assert_traits::<focus_area_expansion::ReferenceInput>();
    assert_traits::<focus_area_expansion::ReferenceResult>();
    assert_traits::<focus_area_expansion::Expansion>();
    assert_traits::<focus_area_expansion::Operation>();
    assert_traits::<focus_area_generation::Generation>();
    assert_traits::<worker_input::AssignmentOptions<'static>>();
    assert_traits::<workflow_plan::PlanKind>();
    assert_traits::<workflow_plan::Cluster>();
    assert_traits::<workflow_plan::Merge>();
    assert_traits::<workflow_plan::Update>();
    assert_traits::<workflow_plan::Removal>();
    assert_traits::<workflow_plan::Decision>();
    assert_traits::<workflow_plan::DecisionValue>();
    assert_traits::<workflow_plan::PlanItem>();
    assert_traits::<workflow_plan::Plan>();
}

/// Failed batches leave the artifact intact while successful lifecycle and plan
/// operations preserve identity, history, and idempotent plan authoring.
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

/// Schema validation and conflict detection protect plans before publication,
/// while explicit replacement permits an intended change to a merge group.
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

/// An empty candidate plan cannot hide damaged index evidence.
#[test]
fn empty_candidate_plans_validate_the_complete_index() {
    let directory = TestDirectory::new("candidate-index");
    let clock = FixedClock;
    let candidates = directory.path("candidates.json");
    let index = directory.path("index.json");
    fs::write(&candidates, br#"{"schemaVersion":1,"clusters":[]}"#).unwrap();
    let entry = json!({"id":"g1-1","reviewerId":"g1","focusArea":"correctness",
        "title":"Problem","description":"What fails","recommendedAction":"Fix",
        "anchorPath":"src/a.rs","impactedPaths":[]});
    let valid = json!({"schemaVersion":1,"findings":[entry]});
    fs::write(&index, serde_json::to_vec(&valid).unwrap()).unwrap();
    assert_eq!(
        findings::validate_candidates(&candidates, &index, &clock).unwrap(),
        0
    );
    fs::write(&index, br#"{"schemaVersion":1,"findings":[]}"#).unwrap();
    assert_eq!(
        findings::validate_candidates(&candidates, &index, &clock).unwrap(),
        0
    );
    let mut missing_version = valid.clone();
    missing_version
        .as_object_mut()
        .unwrap()
        .remove("schemaVersion");
    let mut incomplete = valid.clone();
    incomplete["findings"][0]
        .as_object_mut()
        .unwrap()
        .remove("reviewerId");
    let mut malformed_paths = valid.clone();
    malformed_paths["findings"][0]["impactedPaths"] = json!("src/a.rs");
    let mut unknown_field = valid;
    unknown_field["findings"][0]["priority"] = json!("high");
    for invalid in [
        json!({}),
        missing_version,
        json!({"schemaVersion":2,"findings":[]}),
        json!({"schemaVersion":1}),
        json!({"schemaVersion":1,"findings":null}),
        json!({"schemaVersion":1,"findings":entry}),
        incomplete,
        malformed_paths,
        unknown_field,
        json!({"schemaVersion":1,"findings":[entry,entry]}),
    ] {
        fs::write(&index, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            findings::validate_candidates(&candidates, &index, &clock).is_err(),
            "{invalid}"
        );
    }
}

/// Batch validation rejects incomplete or unrelated evidence before committing the claim.
#[test]
fn coverage_batch_rejects_each_inconsistent_draft_without_mutation() {
    let directory = TestDirectory::new("batch-invalid-drafts");
    let clock = FixedClock;
    let inventory = directory.path("inventory.json");
    let groups = directory.path("groups.json");
    let queue = directory.path("queue.json");
    let artifact = directory.path("artifact.json");
    let files = json!([
        {"path":directory.path("old"),"sizeBytes":0,"change":"deleted"},
        {"path":directory.path("new"),"sizeBytes":17,"change":"new"}
    ]);
    let group = json!([{"groupId":"group-1","files":files}]);
    fs::write(&inventory, serde_json::to_vec(&files).unwrap()).unwrap();
    fs::write(&groups, serde_json::to_vec(&group).unwrap()).unwrap();
    findings::initialize(&artifact, "g1", "correctness", false, &clock).unwrap();
    let state = review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    review_work::next(&queue, &clock).unwrap();
    let batch = json!({"schemaVersion":1,"snapshot":state["snapshot"],"groupId":"group-1",
        "assessments":[
            {"groupId":"group-1","path":files[0]["path"],"disposition":"reviewed","scope":"","reason":""},
            {"groupId":"group-1","path":files[1]["path"],"disposition":"reviewed","scope":"","reason":""}
        ],"findingIds":[]});
    let before = fs::read(&queue).unwrap();
    let findings_before = fs::read(&artifact).unwrap();
    let mut partial = batch.clone();
    partial["assessments"].as_array_mut().unwrap().pop();
    let mut duplicate = batch.clone();
    duplicate["assessments"][1] = duplicate["assessments"][0].clone();
    let mut foreign = batch.clone();
    foreign["assessments"][0]["path"] = json!(directory.path("foreign"));
    let mut wrong_group = batch.clone();
    wrong_group["assessments"][0]["groupId"] = json!("other");
    let mut stale = batch.clone();
    stale["snapshot"]["inventorySha256"] = json!("0".repeat(64));
    let mut missing_finding = batch.clone();
    missing_finding["findingIds"] = json!(["g1-1"]);
    let mut unclaimed = batch;
    unclaimed["groupId"] = json!("other");
    unclaimed["assessments"] = json!([]);
    for (name, invalid) in [
        ("partial", partial),
        ("duplicate", duplicate),
        ("foreign", foreign),
        ("wrong group", wrong_group),
        ("stale", stale),
        ("missing finding", missing_finding),
        ("unclaimed", unclaimed),
    ] {
        assert!(
            review_work::complete_batch(&queue, &inventory, &groups, &artifact, &invalid, &clock)
                .is_err(),
            "{name}"
        );
        assert_eq!(fs::read(&queue).unwrap(), before, "{name}");
        assert_eq!(fs::read(&artifact).unwrap(), findings_before, "{name}");
    }
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

/// Completion receipts establish the reviewer evidence used by consolidation,
/// whose merged locations and provenance must also remain usable by the index.
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

/// Completion and resume are gated by journal evidence, including empty work,
/// so a reviewer cannot advance or resume against unrelated unfinished work.
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

/// Concurrent allocation must observe the preceding writer's committed ID.
#[test]
#[cfg_attr(miri, ignore = "requires an isolated native subprocess")]
fn concurrent_appends_allocate_distinct_ids() {
    if !isolated_writer_test("concurrent_appends_allocate_distinct_ids") {
        return;
    }
    let directory = TestDirectory::new("concurrent");
    let clock = SystemClock::default();
    let artifact = directory.path("reviewer.json");
    let finding = directory.path("finding.json");
    fs::write(&finding, r#"{"title":"Problem","description":"What fails","reasoning":"Why","consequence":"Impact","recommendedAction":"Fix","anchor":{"kind":"file","path":"src/lib.rs"}}"#).unwrap();
    findings::initialize(&artifact, "reviewer", "correctness", false, &clock).unwrap();
    let results = synchronized_writers(&artifact, {
        let artifact = artifact.clone();
        move |_, clock| {
            findings::add(&artifact, &finding, "reviewer", &[], clock)
                .map_err(|error| error.to_string())
        }
    });
    let mut ids: Vec<_> = results.into_iter().map(Result::unwrap).collect();
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
    Complete(usize, Result<String, String>),
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
        panic!("Writing findings and plans must not depend on wall-clock time")
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

/// Forces both writers to encounter the retained lock, then permits each retry
/// only after the preceding writer has completed. Completion signals include
/// errors and panics, so an early failure cannot masquerade as contention.
fn synchronized_writers(
    path: &Path,
    operation: impl Fn(usize, &dyn Clock) -> Result<String, String> + Send + Sync + 'static,
) -> Vec<Result<String, String>> {
    let clock = SystemClock::default();
    let transaction = FileTransaction::open(path, &clock).unwrap();
    let (events, received) = mpsc::channel();
    let operation = std::sync::Arc::new(operation);
    let mut permits = Vec::new();
    let mut workers = Vec::new();
    for index in 0..2 {
        let (permit, receiver) = mpsc::channel();
        permits.push(permit);
        workers.push(std::thread::spawn({
            let events = events.clone();
            let operation = std::sync::Arc::clone(&operation);
            move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let clock = UpdateClock {
                        index,
                        events: events.clone(),
                        permit: RefCell::new(Some(receiver)),
                    };
                    operation(index, &clock)
                }))
                .unwrap_or_else(|_| Err("Worker panicked".into()));
                events
                    .send(UpdateEvent::Complete(index, result))
                    .expect("The coordinator receives results until every worker completes");
            }
        }));
    }
    drop(events);
    let mut waiting = Vec::new();
    let mut results = Vec::new();
    for _ in 0..2 {
        match received.recv() {
            Ok(UpdateEvent::Waiting(index)) => waiting.push(index),
            Ok(UpdateEvent::Complete(index, result)) => results.push((index, result)),
            Err(_) => break,
        }
    }
    drop(transaction);
    for index in &waiting {
        if permits[*index].send(()).is_ok() {
            match received.recv() {
                Ok(UpdateEvent::Complete(index, result)) => results.push((index, result)),
                Ok(UpdateEvent::Waiting(index)) => {
                    results.push((index, Err("Unexpected repeated contention".into())));
                }
                Err(_) => break,
            }
        }
    }
    drop(permits);
    for worker in workers {
        worker.join().unwrap();
    }
    waiting.sort_unstable();
    results.sort_by_key(|(index, _)| *index);
    assert_eq!(waiting, [0, 1], "Both workers must encounter contention");
    assert_eq!(results.len(), 2, "Both workers must report completion");
    assert_eq!(
        results.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
        [0, 1],
        "Each worker must report its own result"
    );
    results.into_iter().map(|(_, result)| result).collect()
}

/// Concurrent focus-area consumers each receive a distinct item or exhaustion.
#[test]
#[cfg_attr(miri, ignore = "requires an isolated native subprocess")]
fn concurrent_focus_area_claims_are_unique_and_exhaustive() {
    if !isolated_writer_test("concurrent_focus_area_claims_are_unique_and_exhaustive") {
        return;
    }
    let directory = TestDirectory::new("focus-claims");
    let queue = directory.path("focus.json");
    fs::write(
        &queue,
        serde_json::to_vec(&json!({"schemaVersion":1,"kind":"focusAreas",
        "items":[{"reviewerId":"a"},{"reviewerId":"b"},{"reviewerId":"c"}]}))
        .unwrap(),
    )
    .unwrap();
    let mut ids = Vec::new();
    let mut exhausted = 0;
    for _ in 0..2 {
        let results = synchronized_writers(&queue, {
            let queue = queue.clone();
            move |_, clock| {
                review_work::next(&queue, clock)
                    .map(|value| value.to_string())
                    .map_err(|error| error.to_string())
            }
        });
        for result in results {
            let page: Value = serde_json::from_str(&result.unwrap()).unwrap();
            if page["exhausted"] == true {
                exhausted += 1;
            } else {
                ids.push(page["item"]["reviewerId"].as_str().unwrap().to_owned());
            }
        }
    }
    ids.sort();
    assert_eq!(ids, ["a", "b", "c"]);
    assert_eq!(exhausted, 1);
    let final_queue: Value = serde_json::from_slice(&fs::read(queue).unwrap()).unwrap();
    assert_eq!(final_queue["items"], json!([]));
}

/// Concurrent plan writers must use the last committed state for idempotency,
/// conflicts, and replacement as well as ordinary appends.
#[test]
#[cfg_attr(miri, ignore = "requires an isolated native subprocess")]
fn synchronized_plan_appends_preserve_committed_state() {
    if !isolated_writer_test("synchronized_plan_appends_preserve_committed_state") {
        return;
    }
    let directory = TestDirectory::new("synchronized-plans");
    let clock = SystemClock::default();
    for (kind, name, first, second) in [
        (
            workflow_plan::PlanKind::MergeCandidates,
            "candidates",
            json!({"findingIds":["a","b"]}),
            json!({"findingIds":["c","d"]}),
        ),
        (
            workflow_plan::PlanKind::MergePlan,
            "merges",
            json!({"sourceFindingIds":["a","b"],"findingPath":"f"}),
            json!({"sourceFindingIds":["c","d"],"findingPath":"g"}),
        ),
        (
            workflow_plan::PlanKind::Updates,
            "updates",
            json!({"findingId":"a","findingPath":"f"}),
            json!({"findingId":"b","findingPath":"g"}),
        ),
        (
            workflow_plan::PlanKind::Removals,
            "removals",
            json!({"findingId":"a","reason":"first"}),
            json!({"findingId":"b","reason":"second"}),
        ),
        (
            workflow_plan::PlanKind::Decisions,
            "decisions",
            json!({"findingId":"a","decision":"approved"}),
            json!({"findingId":"b","decision":"rejected"}),
        ),
    ] {
        for duplicate in [false, true] {
            let plan = directory.path(&format!("{name}-{duplicate}.json"));
            let drafts = [
                directory.path(&format!("{name}-first.json")),
                directory.path(&format!("{name}-second.json")),
            ];
            fs::write(&drafts[0], serde_json::to_vec(&first).unwrap()).unwrap();
            fs::write(
                &drafts[1],
                serde_json::to_vec(if duplicate { &first } else { &second }).unwrap(),
            )
            .unwrap();
            workflow_plan::initialize(kind, &plan, &clock).unwrap();
            let results = synchronized_writers(&plan, {
                let plan = plan.clone();
                move |index, clock| {
                    workflow_plan::append(kind, &plan, &drafts[index], None, clock)
                        .map(|()| String::new())
                        .map_err(|error| error.to_string())
                }
            });
            assert!(results.iter().all(Result::is_ok), "{name}: {results:?}");
            let items = workflow_plan::read(kind, &plan, &clock).unwrap();
            assert_eq!(items.items().len(), if duplicate { 1 } else { 2 }, "{name}");
        }
    }

    let plan = directory.path("conflicting-merges.json");
    let drafts = [
        directory.path("merge-first.json"),
        directory.path("merge-second.json"),
    ];
    fs::write(
        &drafts[0],
        r#"{"sourceFindingIds":["a","b"],"findingPath":"f"}"#,
    )
    .unwrap();
    fs::write(
        &drafts[1],
        r#"{"sourceFindingIds":["b","c"],"findingPath":"g"}"#,
    )
    .unwrap();
    workflow_plan::initialize(workflow_plan::PlanKind::MergePlan, &plan, &clock).unwrap();
    let results = synchronized_writers(&plan, {
        let plan = plan.clone();
        move |index, clock| {
            workflow_plan::append(
                workflow_plan::PlanKind::MergePlan,
                &plan,
                &drafts[index],
                None,
                clock,
            )
            .map(|()| String::new())
            .map_err(|error| error.to_string())
        }
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let error = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .unwrap();
    assert!(error.contains("conflicts"), "{error}");
    assert_eq!(
        workflow_plan::read(workflow_plan::PlanKind::MergePlan, &plan, &clock)
            .unwrap()
            .items()
            .len(),
        1
    );

    let plan = directory.path("replacements.json");
    let drafts = [
        directory.path("replace-first.json"),
        directory.path("replace-second.json"),
    ];
    workflow_plan::initialize(workflow_plan::PlanKind::Updates, &plan, &clock).unwrap();
    for (index, id) in ["a", "b"].into_iter().enumerate() {
        fs::write(
            &drafts[index],
            serde_json::to_vec(&json!({"findingId":id,"findingPath":"old"})).unwrap(),
        )
        .unwrap();
        workflow_plan::append(
            workflow_plan::PlanKind::Updates,
            &plan,
            &drafts[index],
            None,
            &clock,
        )
        .unwrap();
        fs::write(
            &drafts[index],
            serde_json::to_vec(&json!({"findingId":id,"findingPath":"new"})).unwrap(),
        )
        .unwrap();
    }
    let results = synchronized_writers(&plan, {
        let plan = plan.clone();
        move |index, clock| {
            workflow_plan::append(
                workflow_plan::PlanKind::Updates,
                &plan,
                &drafts[index],
                Some(index),
                clock,
            )
            .map(|()| String::new())
            .map_err(|error| error.to_string())
        }
    });
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    let plan = workflow_plan::read(workflow_plan::PlanKind::Updates, &plan, &clock).unwrap();
    assert_eq!(plan.items().len(), 2);
    for item in plan.items() {
        let workflow_plan::PlanItem::Update(update) = item else {
            panic!("Expected an update")
        };
        assert_eq!(Path::new(&update.finding_path).file_name().unwrap(), "new");
    }
}

/// Updates to distinct findings must survive a contended read-modify-write.
#[test]
#[cfg_attr(miri, ignore = "requires an isolated native subprocess")]
fn synchronized_updates_preserve_both_rewrites() {
    if !isolated_writer_test("synchronized_updates_preserve_both_rewrites") {
        return;
    }
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

    let results = synchronized_writers(&artifact, {
        let artifact = artifact.clone();
        let updates = [("g1-1", one), ("g1-2", two)];
        move |index, clock| {
            let (id, path) = &updates[index];
            findings::update(&artifact, &[(id.to_string(), path.clone())], clock)
                .map(|()| String::new())
                .map_err(|error| error.to_string())
        }
    });
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    let artifact = findings::read_artifact(&artifact, &clock).unwrap();
    assert_eq!(artifact["findings"][0]["title"], "First rewrite");
    assert_eq!(artifact["findings"][1]["title"], "Second rewrite");
}

/// Isolates lock-sensitive tests from unrelated process creation in this test
/// binary: a forked child can retain a lock descriptor until it executes.
fn isolated_writer_test(name: &str) -> bool {
    const MARKER: &str = "SS_AI_ISOLATED_WRITER_TEST";
    if std::env::var_os(MARKER).is_some_and(|value| value == name) {
        return true;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(MARKER, name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Isolated test {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
        "Isolated test {name} did not execute exactly once:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    false
}

/// Finding records retain their content and provenance throughout the lifecycle.
#[test]
fn lifecycle_preserves_records_through_index_and_consolidation() {
    let directory = TestDirectory::new("lifecycle-roundtrip");
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
    findings::initialize(&alpha, "alpha", "correctness", false, &clock).unwrap();
    findings::add(&alpha, &content, "reviewer", &[], &clock).unwrap();
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
    findings::update(&alpha, &[("alpha-2".into(), revised)], &clock).unwrap();
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
    findings::add(&alpha, &content, "reviewer", &[], &clock).unwrap();
    let artifact = findings::read_artifact(&alpha, &clock).unwrap();
    assert_eq!(artifact["removed"][0]["finding"]["id"], "alpha-1");
    assert_eq!(artifact["removed"][0]["reason"], "Covered elsewhere");
    assert_eq!(artifact["findings"][1]["id"], "alpha-3");

    fs::write(&queue, r#"{"schemaVersion":3,"kind":"fileGroups","items":[],"inProgress":null,"completedGroupIds":[],"assessments":[]}"#).unwrap();
    findings::complete(&alpha, &queue, &clock).unwrap();
    findings::initialize(&beta, "beta", "security", false, &clock).unwrap();
    findings::complete(&beta, &queue, &clock).unwrap();
    let validation =
        findings::validate_artifacts(&root, &["alpha".into(), "beta".into()], &clock).unwrap();
    assert_eq!(validation["isValid"], true, "{validation}");
    let index_rs = directory.path("index-rs.json");
    let rust_index = findings::build_index(&root, &index_rs, &clock).unwrap();
    assert_eq!(
        rust_index,
        json!({"schemaVersion":1,"findings":[
            {"id":"alpha-2","reviewerId":"alpha","focusArea":"correctness",
             "title":"Rewrite without changing provenance",
             "description":original["description"],"recommendedAction":original["recommendedAction"],
             "anchorPath":"src/retry.rs","impactedPaths":["src/backoff.rs"],
             "followUpThreadId":"RT_kw1"},
            {"id":"alpha-3","reviewerId":"alpha","focusArea":"correctness",
             "title":"The retry loop never terminates",
             "description":original["description"],"recommendedAction":original["recommendedAction"],
             "anchorPath":"src/retry.rs","impactedPaths":["src/backoff.rs"],
             "followUpThreadId":"RT_kw1"}
        ]})
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(index_rs).unwrap()).unwrap(),
        rust_index
    );
    let rs_output = directory.path("consolidated-rs.json");
    let rust_output = findings::consolidate(&root, None, &rs_output, &clock).unwrap();
    let mut expected = original.clone();
    expected["anchor"] =
        json!({"kind":"line","path":"src/retry.rs","startLine":42,"endLine":42,"side":"right"});
    expected["impactedLocations"] =
        json!([{"path":"src/backoff.rs","startLine":8,"endLine":8,"side":"right"}]);
    expected["schemaVersion"] = json!(1);
    expected["id"] = json!("F1");
    expected["origin"] = json!("synthesis");
    expected["sourceFindingIds"] = json!(["alpha-2"]);
    expected["title"] = rewritten["title"].clone();
    let mut second = expected.clone();
    second["id"] = json!("F2");
    second["origin"] = json!("reviewer");
    second["sourceFindingIds"] = json!(["alpha-3"]);
    second["title"] = original["title"].clone();
    assert_eq!(
        rust_output,
        json!({"schemaVersion":1,"kind":"consolidated",
        "findings":[expected,second],"removed":[]})
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(rs_output).unwrap()).unwrap(),
        rust_output
    );
}

/// Authored content must meet the validation contract, including boundary values.
#[test]
fn finding_content_cases_cover_validation_boundaries() {
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
        let result = findings::add(&artifact, &content, "reviewer", &[], &clock);
        assert_eq!(result.is_ok(), valid, "Rust case {name}: {result:?}");
    }
}

/// Stored records and authored input share the prose and structured-field contract.
#[test]
fn finding_validation_covers_prose_controls_and_structured_fields() {
    let baseline = json!({
        "schemaVersion":1,"id":"g1-1","sourceFindingIds":[],"origin":"reviewer",
        "title":"Problem","description":"What fails","reasoning":"Why",
        "consequence":"Impact","recommendedAction":"Fix",
        "anchor":{"kind":"line","path":"src/retry.rs","startLine":42}
    });
    let valid = |record: &Value| {
        findings::validate_artifact(&json!({
            "schemaVersion":1,"kind":"reviewer","reviewerId":"g1",
            "focusArea":"correctness","status":"open","findings":[record]
        }))
    };
    for field in [
        "title",
        "description",
        "reasoning",
        "consequence",
        "recommendedAction",
    ] {
        let mut missing = baseline.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(valid(&missing).is_err(), "missing {field}");
        for value in [json!(""), json!("  "), json!(7)] {
            let mut invalid = baseline.clone();
            invalid[field] = value;
            assert!(valid(&invalid).is_err(), "invalid {field}");
        }
        for code in 0..32_u32 {
            let mut record = baseline.clone();
            record[field] = json!(format!("before{}after", char::from_u32(code).unwrap()));
            let allowed = field != "title" && matches!(code, 9 | 10);
            let result = valid(&record);
            assert_eq!(result.is_ok(), allowed, "{field} U+{code:04X}");
            if let Err(error) = result {
                let message = error.to_string();
                assert!(message.contains(&format!("'{field}'")), "{message}");
                assert!(message.contains(&format!("U+{code:04X}")), "{message}");
            }
        }
    }

    for (title, accepted) in [
        ("—".repeat(255), true),
        ("—".repeat(256), false),
        ("🤖".repeat(127), true),
        ("🤖".repeat(128), false),
    ] {
        let mut record = baseline.clone();
        record["title"] = json!(title);
        let result = valid(&record);
        assert_eq!(result.is_ok(), accepted, "UTF-16 title bound");
        if let Err(error) = result {
            let message = error.to_string();
            assert!(message.contains("256 characters"), "{message}");
            assert!(message.contains("at most 255"), "{message}");
        }
    }

    for (field, value) in [
        ("references", json!([{"url":"https://example.invalid"}])),
        ("references", json!([{"text":"reference","extra":true}])),
        ("impactedLocations", json!([{"startLine":3}])),
        (
            "impactedLocations",
            json!([{"path":"src/a.rs","endLine":3}]),
        ),
        (
            "impactedLocations",
            json!([{"path":"src/a.rs","side":"left"}]),
        ),
        ("impactedLocations", json!([{"path":"src\\a.rs"}])),
        ("followUp", json!({"threadId":"T"})),
        ("followUp", json!({"commentId":"C"})),
        (
            "followUp",
            json!({"threadId":"T","commentId":"C","extra":true}),
        ),
        (
            "anchor",
            json!({"kind":"line","path":"/src/a.rs","startLine":1}),
        ),
        (
            "anchor",
            json!({"kind":"line","path":"src/a.rs","startLine":"42"}),
        ),
        (
            "anchor",
            json!({"kind":"line","path":"src/a.rs","startLine":1,"extra":true}),
        ),
    ] {
        let mut record = baseline.clone();
        record[field] = value;
        assert!(valid(&record).is_err(), "{field}: {}", record[field]);
    }
    let mut record = baseline;
    record["references"] = json!([]);
    record["impactedLocations"] = json!([]);
    assert!(valid(&record).is_ok());
}

/// Merges reject lost evidence and preserve source identifiers.
#[test]
fn consolidation_merge_cases_preserve_evidence() {
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
    assert!(findings::consolidate(&reviewers, Some(&plan), &native_output, &clock).is_err());
    assert!(!native_output.exists());

    let mut covering = finding("src/retry.rs", 40);
    covering["anchor"]["endLine"] = json!(45);
    covering["impactedLocations"] = json!([{"path":"src/backoff.rs","startLine":5,"endLine":10}]);
    fs::write(&merged, serde_json::to_vec(&covering).unwrap()).unwrap();
    let native = findings::consolidate(&reviewers, Some(&plan), &native_output, &clock).unwrap();
    let mut merged_expected = covering.clone();
    merged_expected["anchor"]["side"] = json!("right");
    merged_expected["impactedLocations"][0]["side"] = json!("right");
    merged_expected["references"] = json!([]);
    merged_expected["schemaVersion"] = json!(1);
    merged_expected["id"] = json!("F1");
    merged_expected["origin"] = json!("synthesis");
    merged_expected["sourceFindingIds"] = json!(["g1-1", "g2-1"]);
    let mut untouched = finding("src/retry.rs", 42);
    untouched["anchor"]["endLine"] = json!(42);
    untouched["anchor"]["side"] = json!("right");
    untouched["references"] = json!([]);
    untouched["impactedLocations"] = json!([]);
    untouched["schemaVersion"] = json!(1);
    untouched["id"] = json!("F2");
    untouched["origin"] = json!("reviewer");
    untouched["sourceFindingIds"] = json!(["g1-2"]);
    assert_eq!(
        native,
        json!({"schemaVersion":1,"kind":"consolidated",
        "findings":[merged_expected,untouched],"removed":[]})
    );
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
    let native_before = fs::read(&native_output).unwrap();
    assert!(findings::consolidate(&reviewers, Some(&plan), &native_output, &clock).is_err());
    assert_eq!(fs::read(&native_output).unwrap(), native_before);
}

/// Anchor validation checks shape and path normalization.
#[test]
fn finding_anchor_cases_cover_validation_boundaries() {
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
            "internal location kind",
            json!({"kind":"location","path":"src/retry.rs"}),
            false,
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
        let result = findings::add(&artifact, &content, "reviewer", &[], &clock);
        assert_eq!(result.is_ok(), valid, "Rust case {name}: {result:?}");
    }

    let mut finding = baseline;
    finding["impactedLocations"] = json!([
        {"path":"src/other.rs"},
        {"path":"src/retry.rs","startLine":8,"endLine":10,"side":"left"}
    ]);
    fs::write(&content, serde_json::to_vec(&finding).unwrap()).unwrap();
    findings::add(&artifact, &content, "reviewer", &[], &clock).unwrap();
    let mut stored = findings::read_artifact(&artifact, &clock).unwrap();
    stored["findings"][0]["anchor"]["kind"] = json!("location");
    fs::write(&artifact, serde_json::to_vec(&stored).unwrap()).unwrap();
    assert!(findings::read_artifact(&artifact, &clock).is_err());
    let report = findings::validate_artifacts(&artifact, &[], &clock).unwrap();
    assert_eq!(report["isValid"], false);
}

/// UTF-8 BOMs are accepted at each file boundary without relaxing JSON checks.
#[test]
fn bom_prefixed_finding_artifact_and_plan_inputs_interoperate() {
    let directory = TestDirectory::new("bom-inputs");
    let clock = SystemClock::default();
    let artifact = directory.path("reviewer.json");
    let content = directory.path("finding.json");
    let plan = directory.path("updates.json");
    let draft = directory.path("draft.json");
    let bom = |text: &[u8]| [b"\xef\xbb\xbf".as_slice(), text].concat();
    let finding = br#"{"title":"Problem","description":"What fails","reasoning":"Why","consequence":"Impact","recommendedAction":"Fix","anchor":{"kind":"file","path":"src/lib.rs"}}"#;
    fs::write(&content, bom(finding)).unwrap();
    findings::initialize(&artifact, "g1", "correctness", false, &clock).unwrap();
    findings::add(&artifact, &content, "reviewer", &[], &clock).unwrap();
    fs::write(&artifact, bom(&fs::read(&artifact).unwrap())).unwrap();
    assert_eq!(
        findings::read_artifact(&artifact, &clock).unwrap()["findings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    findings::add(&artifact, &content, "reviewer", &[], &clock).unwrap();

    fs::write(&plan, bom(br#"{"schemaVersion":1,"updates":[]}"#)).unwrap();
    fs::write(
        &draft,
        bom(br#"{"findingId":"g1-1","findingPath":"finding.json"}"#),
    )
    .unwrap();
    assert!(workflow_plan::read(workflow_plan::PlanKind::Updates, &plan, &clock).is_ok());
    workflow_plan::append(
        workflow_plan::PlanKind::Updates,
        &plan,
        &draft,
        None,
        &clock,
    )
    .unwrap();
    assert_eq!(
        workflow_plan::read(workflow_plan::PlanKind::Updates, &plan, &clock)
            .unwrap()
            .items()
            .len(),
        1
    );

    fs::write(
        &draft,
        bom(br#"{"findingId":"g1-1","FindingId":"g1-2","findingPath":"finding.json"}"#),
    )
    .unwrap();
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
    fs::write(
        &plan,
        bom(br#"{"schemaVersion":1,"updates":[],"updates":[]}"#),
    )
    .unwrap();
    assert!(workflow_plan::read(workflow_plan::PlanKind::Updates, &plan, &clock).is_err());
    fs::write(&content, bom(br#"{"title":"Problem","title":"Other"}"#)).unwrap();
    assert!(findings::add(&artifact, &content, "reviewer", &[], &clock).is_err());
}

/// Windows does not hide dot-prefixed staging names, unlike Unix providers.
#[cfg(windows)]
#[test]
fn validation_excludes_only_recognized_staging_files() {
    let directory = TestDirectory::new("powershell-staging");
    let clock = SystemClock::default();
    let staging = ".reviewer.json.0123456789abcdef0123456789abcdef.tmp";
    fs::write(directory.path(staging), b"partial").unwrap();
    let native_staging = ".ss-ai-transaction-aB1234567890abcdefgh1234.tmp";
    fs::write(directory.path(native_staging), b"partial").unwrap();
    fs::write(directory.path("reviewer.JSON.LOCK"), b"lock").unwrap();
    let report = findings::validate_artifacts(&directory.0, &[], &clock).unwrap();
    assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
    let foreign = ".reviewer.json.0123456789abcdef0123456789abcdeg.tmp";
    fs::write(directory.path(foreign), b"foreign").unwrap();
    let generic_temporary = ".tmp123456";
    fs::write(directory.path(generic_temporary), b"foreign").unwrap();
    let malformed_native = ".ss-ai-transaction-aB1234567890abcdefgh123.tmp";
    fs::write(directory.path(malformed_native), b"foreign").unwrap();
    let stage_directory = ".other.json.0123456789abcdef0123456789abcdef.tmp";
    #[expect(clippy::create_dir, reason = "The entry must not already exist")]
    fs::create_dir(directory.path(stage_directory)).unwrap();
    #[expect(clippy::create_dir, reason = "The entry must not already exist")]
    fs::create_dir(directory.path("foreign.json.lock")).unwrap();
    let report = findings::validate_artifacts(&directory.0, &[], &clock).unwrap();
    let errors = report["errors"].to_string();
    assert!(errors.contains(foreign), "{errors}");
    assert!(errors.contains(generic_temporary), "{errors}");
    assert!(errors.contains(malformed_native), "{errors}");
    assert!(errors.contains(stage_directory), "{errors}");
    assert!(errors.contains("foreign.json.lock"), "{errors}");
    assert!(!errors.contains(staging), "{errors}");
    assert!(!errors.contains(native_staging), "{errors}");
}

/// Damaged reviewer evidence remains visible in validation reports.
#[test]
fn standalone_validation_reports_all_damaged_reviewer_evidence() {
    let directory = TestDirectory::new("validation-corpus");
    let clock = SystemClock::default();
    let artifact = directory.path("g1.json");
    let content = directory.path("finding.json");
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

    let native = findings::validate_artifacts(&artifact, &["g1".into()], &clock).unwrap();
    assert_eq!(native["isValid"], false);
    assert_eq!(native["findingCount"], 1);
    assert_eq!(native["incompleteReviewers"], json!(["g1"]));
    assert_eq!(native["missingReviewers"], json!([]));
    assert_eq!(native["unexpectedReviewers"], json!([]));
    assert_eq!(native["artifacts"][0]["reviewerId"], "g1");
    assert_eq!(native["artifacts"][0]["status"], "partial");
    assert_eq!(native["artifacts"][0]["findingCount"], 1);

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
    assert!(duplicate.contains("--force"), "{duplicate}");
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
#[cfg_attr(miri, ignore = "requires an isolated native subprocess")]
fn removal_timestamp_uses_dotnet_round_trip_precision() {
    if !isolated_writer_test("removal_timestamp_uses_dotnet_round_trip_precision") {
        return;
    }
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
