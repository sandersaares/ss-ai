//! Exercises workflow-record contracts.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The tests construct the fixed record shapes they inspect"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::focus_area_expansion::{
    Operation, ReferenceInput, ReferenceResult, update,
};
use ss_ai_review_workflow::workflow_plan::PlanKind;
use ss_ai_review_workflow::{audit, findings, review_work, workflow_plan};
use ss_ai_runtime::Clock;

/// Keeps each transaction's inputs and retained locks in a private checkout directory.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // The counter only distinguishes directories within this process.
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".workflow-data-migration-{}-{sequence}",
            std::process::id()
        ));
        #[expect(
            clippy::create_dir,
            reason = "The fixture must not reuse existing state"
        )]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn put(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Isolated tests must not wait on wall-clock time or unexpected contention.
struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::MIN
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Unexpected contention in an isolated workflow fixture")
    }
}

#[test]
fn plan_envelopes_drafts_and_replacements_preserve_prior_work() {
    let workspace = Workspace::new();
    let clock = TestClock;
    let plan = workspace.path("plan.json");
    workflow_plan::initialize(PlanKind::MergePlan, &plan, &clock).unwrap();
    assert!(
        workflow_plan::read(PlanKind::MergePlan, &plan, &clock)
            .unwrap()
            .items()
            .is_empty()
    );
    assert!(workflow_plan::initialize(PlanKind::MergePlan, &plan, &clock).is_err());
    for malformed in [
        "{}",
        r#"{"groups":[]}"#,
        r#"{"schemaVersion":1,"group":[]}"#,
        r#"{"schemaVersion":1,"groups":{}}"#,
    ] {
        fs::write(&plan, malformed).unwrap();
        assert!(workflow_plan::read(PlanKind::MergePlan, &plan, &clock).is_err());
    }
    assert!(workflow_plan::read(PlanKind::MergePlan, &workspace.path("missing"), &clock).is_err());
    workflow_plan::initialize(PlanKind::MergePlan, &workspace.path("merges.json"), &clock).unwrap();
    let merges = workspace.path("merges.json");
    let draft = workspace.put(
        "item.json",
        &json!({"sourceFindingIds":["a","b"],"findingPath":"merged.json"}),
    );
    workflow_plan::append(PlanKind::MergePlan, &merges, &draft, None, &clock).unwrap();
    workflow_plan::append(PlanKind::MergePlan, &merges, &draft, None, &clock).unwrap();
    assert_eq!(
        workflow_plan::read(PlanKind::MergePlan, &merges, &clock)
            .unwrap()
            .items()
            .len(),
        1
    );
    let before = fs::read(&merges).unwrap();
    let overlapping = workspace.put(
        "item.json",
        &json!({"sourceFindingIds":["a","c"],"findingPath":"other.json"}),
    );
    assert!(
        workflow_plan::append(PlanKind::MergePlan, &merges, &overlapping, None, &clock).is_err()
    );
    assert_eq!(fs::read(&merges).unwrap(), before);

    for (index, (kind, draft)) in [
        (PlanKind::MergeCandidates, json!({"findingIds":"a"})),
        (PlanKind::MergeCandidates, json!({"findingIds":["a"]})),
        (PlanKind::MergeCandidates, json!({"findingIds":["a","a"]})),
        (
            PlanKind::Updates,
            json!({"findingId":"a","findingPath":"x","text":"unexpected"}),
        ),
        (PlanKind::Removals, json!({"findingId":"a"})),
        (
            PlanKind::Decisions,
            json!({"findingId":"a","decision":"maybe"}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let path = workspace.path(&format!("draft-{index}.json"));
        let item = workspace.put("item.json", &draft);
        workflow_plan::initialize(kind, &path, &clock).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(workflow_plan::append(kind, &path, &item, None, &clock).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }

    let removals = workspace.path("removals.json");
    workflow_plan::initialize(PlanKind::Removals, &removals, &clock).unwrap();
    for (id, reason) in [("a", "Superseded"), ("b", "Duplicate")] {
        let item = workspace.put("item.json", &json!({"findingId":id,"reason":reason}));
        workflow_plan::append(PlanKind::Removals, &removals, &item, None, &clock).unwrap();
    }
    let item = workspace.put("item.json", &json!({"findingId":"a","reason":"Addressed"}));
    workflow_plan::append(PlanKind::Removals, &removals, &item, Some(0), &clock).unwrap();
    let document: Value = serde_json::from_slice(&fs::read(&removals).unwrap()).unwrap();
    assert_eq!(document["removals"][0]["reason"], "Addressed");
    assert_eq!(document["removals"][1]["reason"], "Duplicate");
    let before = fs::read(&removals).unwrap();
    let conflict = workspace.put("item.json", &json!({"findingId":"b","reason":"Conflict"}));
    assert!(
        workflow_plan::append(PlanKind::Removals, &removals, &conflict, Some(0), &clock).is_err()
    );
    assert_eq!(fs::read(removals).unwrap(), before);
}

#[test]
fn assessments_reject_invalid_evidence_and_support_completed_group_repair() {
    let workspace = Workspace::new();
    let clock = TestClock;
    let files = json!([
        {"path":workspace.path("a.rs"),"sizeBytes":1,"change":"modified"},
        {"path":workspace.path("deleted.rs"),"sizeBytes":1,"change":"deleted"}
    ]);
    let inventory = workspace.put("inventory.json", &files);
    let groups = workspace.put(
        "groups.json",
        &json!([{"groupId":"group-001","files":files}]),
    );
    let queue = workspace.path("queue.json");
    review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    review_work::next(&queue, &clock).unwrap();
    let initial = fs::read(&queue).unwrap();
    assert!(review_work::complete_group(&queue, "group-001", &clock).is_err());
    for invalid in [
        json!({"groupId":"group-001","path":files[0]["path"],"disposition":"excluded","scope":"","reason":""}),
        json!({"groupId":"group-001","path":workspace.path("outside.rs"),"disposition":"reviewed","scope":"","reason":""}),
    ] {
        assert!(
            review_work::set_assessment(&queue, &inventory, &groups, &invalid, &clock).is_err()
        );
        assert_eq!(fs::read(&queue).unwrap(), initial);
    }
    let reviewed = json!({"groupId":"group-001","path":files[0]["path"],"disposition":"reviewed","scope":"","reason":""});
    let excluded = json!({"groupId":"group-001","path":files[1]["path"],"disposition":"excluded","scope":"Published scripts","reason":"Deleted Rust file"});
    review_work::set_assessment(&queue, &inventory, &groups, &reviewed, &clock).unwrap();
    assert!(review_work::complete_group(&queue, "group-001", &clock).is_err());
    review_work::set_assessment(&queue, &inventory, &groups, &excluded, &clock).unwrap();
    review_work::complete_group(&queue, "group-001", &clock).unwrap();
    review_work::initialize_queue(&inventory, &groups, &queue, true, &clock).unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(&queue).unwrap()).unwrap();
    assert!(saved["assessments"].as_array().unwrap().contains(&excluded));
    let repaired = json!({"groupId":"group-001","path":files[1]["path"],"disposition":"reviewed","scope":"","reason":""});
    review_work::set_assessment(&queue, &inventory, &groups, &repaired, &clock).unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(&queue).unwrap()).unwrap();
    assert_eq!(saved["completedGroupIds"], json!(["group-001"]));
    assert!(saved["assessments"].as_array().unwrap().contains(&repaired));
}

#[test]
fn final_gate_detects_missing_assessments_and_accepts_incremental_repair() {
    let workspace = Workspace::new();
    let clock = TestClock;
    let files = json!([
        {"path":workspace.path("a.rs"),"sizeBytes":1,"change":"modified"},
        {"path":workspace.path("b.rs"),"sizeBytes":1,"change":"deleted"}
    ]);
    let inventory = workspace.put("inventory.json", &files);
    let groups = workspace.put(
        "groups.json",
        &json!([{"groupId":"group-001","files":files}]),
    );
    let focus = workspace.path("focus");
    fs::create_dir_all(&focus).unwrap();
    fs::write(focus.join("area.md"), "Focus").unwrap();
    let result = review_work::create(
        &inventory,
        &groups,
        &focus,
        &workspace.path("work"),
        None,
        None,
        &clock,
    )
    .unwrap();
    let manifest = PathBuf::from(result["manifestPath"].as_str().unwrap());
    let inventory = PathBuf::from(result["inventoryPath"].as_str().unwrap());
    let groups = PathBuf::from(result["fileGroupsPath"].as_str().unwrap());
    let dispatch = PathBuf::from(result["focusAreaQueuePath"].as_str().unwrap());
    let assignment = review_work::next(&dispatch, &clock).unwrap()["item"].clone();
    let queue = PathBuf::from(assignment["fileGroupQueuePath"].as_str().unwrap());
    let artifacts = workspace.path("artifacts");
    fs::create_dir_all(&artifacts).unwrap();
    let artifact = artifacts.join("focus-001.json");
    findings::initialize(
        &artifact,
        "focus-001",
        assignment["focusArea"].as_str().unwrap(),
        false,
        &clock,
    )
    .unwrap();
    review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    review_work::next(&queue, &clock).unwrap();
    let assessment = |path| json!({"groupId":"group-001","path":path,"disposition":"reviewed","scope":"","reason":""});
    for file in files.as_array().unwrap() {
        review_work::set_assessment(
            &queue,
            &inventory,
            &groups,
            &assessment(&file["path"]),
            &clock,
        )
        .unwrap();
    }
    review_work::complete_group(&queue, "group-001", &clock).unwrap();
    findings::complete(&artifact, &queue, &clock).unwrap();
    assert_eq!(
        review_work::validate(&manifest, &artifacts, &clock).unwrap()["isValid"],
        true
    );
    let mut journal: Value = serde_json::from_slice(&fs::read(&queue).unwrap()).unwrap();
    journal["assessments"] = json!([]);
    fs::write(&queue, serde_json::to_vec(&journal).unwrap()).unwrap();
    assert_eq!(
        review_work::validate(&manifest, &artifacts, &clock).unwrap()["isValid"],
        false
    );
    for file in files.as_array().unwrap() {
        review_work::set_assessment(
            &queue,
            &inventory,
            &groups,
            &assessment(&file["path"]),
            &clock,
        )
        .unwrap();
    }
    assert_eq!(
        review_work::validate(&manifest, &artifacts, &clock).unwrap()["isValid"],
        true
    );
}

/// Creates exact direct and nested source provenance for expansion transactions.
fn reference(id: &str, uri: &str, parent: Option<&str>, root: &str, path: &str) -> ReferenceInput {
    serde_json::from_value(json!({"id":id,"uri":uri,"referrers":[{
        "parentId":parent,"source":{"root":root,"path":path,"startLine":1,"endLine":1},
        "scope":"Published APIs","declarations":[]
    }]}))
    .unwrap()
}

#[test]
fn expansion_rejects_bad_provenance_and_conflicts_without_partial_publication() {
    let workspace = Workspace::new();
    let clock = TestClock;
    for root in ["instruction-set-source", "instruction-set-raw"] {
        fs::create_dir_all(workspace.path(root)).unwrap();
        for file in ["source.md", "parent.md", "child.md"] {
            fs::write(workspace.path(root).join(file), "Reference\n").unwrap();
        }
    }
    let session = &workspace.0;
    let manifest = workspace.path("focus-area-expansion-status.json");
    update(session, Operation::Initialize, &clock).unwrap();
    let before = fs::read(&manifest).unwrap();
    let raw = reference(
        "bad",
        "https://example.invalid/bad",
        None,
        "instruction-set-raw",
        "source.md",
    );
    assert!(update(session, Operation::Register(raw), &clock).is_err());
    assert_eq!(fs::read(&manifest).unwrap(), before);
    let parent = reference(
        "parent",
        "https://example.invalid/parent",
        None,
        "instruction-set-source",
        "source.md",
    );
    update(session, Operation::Register(parent), &clock).unwrap();
    let before = fs::read(&manifest).unwrap();
    for invalid in [
        reference(
            "duplicate",
            "https://example.invalid/parent",
            None,
            "instruction-set-source",
            "source.md",
        ),
        reference(
            "child",
            "https://example.invalid/child",
            Some("missing"),
            "instruction-set-raw",
            "parent.md",
        ),
    ] {
        assert!(update(session, Operation::Register(invalid), &clock).is_err());
        assert_eq!(fs::read(&manifest).unwrap(), before);
    }
    update(
        session,
        Operation::Complete(ReferenceResult {
            reference_id: "parent".into(),
            outputs: vec!["parent.md".into()],
            error: None,
        }),
        &clock,
    )
    .unwrap();
    let cycle = reference(
        "parent",
        "https://example.invalid/parent",
        Some("parent"),
        "instruction-set-raw",
        "parent.md",
    );
    let before = fs::read(&manifest).unwrap();
    assert!(update(session, Operation::Register(cycle), &clock).is_err());
    assert_eq!(fs::read(&manifest).unwrap(), before);
    let child = reference(
        "child",
        "https://example.invalid/child",
        Some("parent"),
        "instruction-set-raw",
        "parent.md",
    );
    update(session, Operation::Register(child), &clock).unwrap();
    let before = fs::read(&manifest).unwrap();
    assert!(
        update(
            session,
            Operation::Complete(ReferenceResult {
                reference_id: "child".into(),
                outputs: vec!["parent.md".into()],
                error: None,
            }),
            &clock
        )
        .is_err()
    );
    assert_eq!(fs::read(&manifest).unwrap(), before);
}

#[test]
fn expansion_read_rejects_corrupted_stored_provenance_without_mutation() {
    let workspace = Workspace::new();
    for root in ["instruction-set-source", "instruction-set-raw"] {
        fs::create_dir_all(workspace.path(root)).unwrap();
        fs::write(workspace.path(root).join("source.md"), "Source").unwrap();
        fs::write(workspace.path(root).join("parent.md"), "Output").unwrap();
    }
    let clock = TestClock;
    update(&workspace.0, Operation::Initialize, &clock).unwrap();
    update(
        &workspace.0,
        Operation::Register(reference(
            "parent",
            "https://example.invalid/parent",
            None,
            "instruction-set-source",
            "source.md",
        )),
        &clock,
    )
    .unwrap();
    update(
        &workspace.0,
        Operation::Complete(ReferenceResult {
            reference_id: "parent".into(),
            outputs: vec!["parent.md".into()],
            error: None,
        }),
        &clock,
    )
    .unwrap();
    update(&workspace.0, Operation::Finalize, &clock).unwrap();
    let path = workspace.path("focus-area-expansion-status.json");
    let mut document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    document["references"][0]["referrers"][0]["source"]["root"] = json!("instruction-set-raw");
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(update(&workspace.0, Operation::Read(Some("parent".into())), &clock).is_err());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn generated_expansion_chains_reject_cycle_without_mutation() {
    let workspace = Workspace::new();
    let clock = TestClock;
    for root in ["instruction-set-source", "instruction-set-raw"] {
        fs::create_dir_all(workspace.path(root)).unwrap();
        fs::write(workspace.path(root).join("source.md"), "Source").unwrap();
    }
    update(&workspace.0, Operation::Initialize, &clock).unwrap();
    // A chain makes the final node a descendant of every earlier node.
    for index in 0_usize..6 {
        let name = format!("node-{index}");
        let uri = format!("https://example.invalid/{index}");
        let output = format!("{name}.md");
        fs::write(
            workspace.path("instruction-set-raw").join(&output),
            "Guidance",
        )
        .unwrap();
        let parent = index
            .checked_sub(1)
            .map(|previous| format!("node-{previous}"));
        let (root, path) = match &parent {
            Some(parent) => ("instruction-set-raw", format!("{parent}.md")),
            None => ("instruction-set-source", "source.md".into()),
        };
        update(
            &workspace.0,
            Operation::Register(reference(&name, &uri, parent.as_deref(), root, &path)),
            &clock,
        )
        .unwrap();
        update(
            &workspace.0,
            Operation::Complete(ReferenceResult {
                reference_id: name,
                outputs: vec![output],
                error: None,
            }),
            &clock,
        )
        .unwrap();
    }
    let manifest = workspace.path("focus-area-expansion-status.json");
    let before = fs::read(&manifest).unwrap();
    let cycle = reference(
        "node-0",
        "https://example.invalid/0",
        Some("node-5"),
        "instruction-set-raw",
        "node-5.md",
    );
    assert!(update(&workspace.0, Operation::Register(cycle), &clock).is_err());
    assert_eq!(fs::read(manifest).unwrap(), before);
    assert_eq!(
        update(&workspace.0, Operation::Finalize, &clock).unwrap()["state"],
        "Complete"
    );
}

#[test]
fn timestamp_feedback_is_verbatim_ordered_and_attached_only_by_decision() {
    let workspace = Workspace::new();
    let clock = TestClock;
    let finding = json!({"schemaVersion":1,"id":"F-1","sourceFindingIds":[],"origin":"reviewer",
        "title":"Title","description":"Description","reasoning":"Reasoning",
        "consequence":"Consequence","recommendedAction":"Action","references":[],
        "impactedLocations":[],"anchor":{"kind":"file","path":"src/lib.rs"}});
    let findings = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":[finding],"removed":[]}),
    );
    let feedback = workspace.path("feedback.json");
    let decisions = workspace.path("decisions.json");
    let text = workspace.path("text.txt");
    fs::write(&text, "2026-10-07T00:00:00Z").unwrap();
    audit::add_feedback(&feedback, &findings, "one", "F-1", &text, &clock).unwrap();
    audit::add_feedback(&feedback, &findings, "one", "F-1", &text, &clock).unwrap();
    assert_eq!(
        audit::get_feedback(&feedback, "F-1", &clock).unwrap(),
        "2026-10-07T00:00:00Z"
    );
    fs::write(&text, "Please clarify.").unwrap();
    audit::add_feedback(&feedback, &findings, "two", "F-1", &text, &clock).unwrap();
    let expected = "2026-10-07T00:00:00Z\n\nPlease clarify.";
    assert_eq!(
        audit::get_feedback(&feedback, "F-1", &clock).unwrap(),
        expected
    );
    assert!(!decisions.exists());
    let before = fs::read(&feedback).unwrap();
    assert!(audit::add_feedback(&feedback, &findings, "one", "F-1", &text, &clock).is_err());
    assert_eq!(fs::read(&feedback).unwrap(), before);
    let plan = workspace.put(
        "decision.json",
        &json!({"schemaVersion":1,"decisions":[{"findingId":"F-1","decision":"approved"}]}),
    );
    audit::add_decisions(
        &decisions,
        &plan,
        Some(&feedback),
        false,
        "2026-01-01T00:00:00Z",
        &clock,
    )
    .unwrap();
    assert_eq!(
        audit::read_decisions(&decisions, &clock).unwrap()["decisions"][0]["feedback"],
        expected
    );
}

#[test]
fn merge_approval_requires_matching_pull_request_commit_and_true_baseline() {
    let workspace = Workspace::new();
    let clock = TestClock;
    let first = "a".repeat(40);
    let second = "b".repeat(40);
    let context = |commit: &str| {
        json!({"pullRequestUrl":"https://example.test/pr/1",
        "pullRequest":{"reviewedSourceCommit":commit,"isAuthoredBySelf":false}})
    };
    let old = workspace.put("old.json", &context(&first));
    let new = workspace.put("new.json", &context(&second));
    let approval = workspace.path("approval.json");
    let set = |approve, previous: Option<&std::path::Path>, carry| {
        audit::set_merge_approval_from_files(&approval, &new, approve, previous, carry, &clock)
    };
    assert_eq!(
        set(false, Some(&old), true).unwrap()["status"],
        "NoApproval"
    );
    audit::set_merge_approval_from_files(&approval, &old, false, None, false, &clock).unwrap();
    assert_eq!(
        set(false, Some(&old), true).unwrap()["status"],
        "NoApproval"
    );
    assert!(
        !audit::test_merge_approval(&approval, "https://example.test/pr/1", &second, &clock)
            .unwrap()
    );
    audit::set_merge_approval_from_files(&approval, &old, true, None, false, &clock).unwrap();
    assert!(
        audit::test_merge_approval(&approval, "https://example.test/pr/1", &first, &clock).unwrap()
    );
    assert!(
        audit::test_merge_approval(&approval, "https://example.test/pr/2", &first, &clock).is_err()
    );
    assert!(
        audit::test_merge_approval(&approval, "https://example.test/pr/1", &second, &clock)
            .is_err()
    );
    assert_eq!(set(false, Some(&old), true).unwrap()["status"], "Recorded");
    assert_eq!(set(false, Some(&old), true).unwrap()["status"], "Recorded");
    let record: Value = serde_json::from_slice(&fs::read(&approval).unwrap()).unwrap();
    assert_eq!(record["approvedSourceCommit"], second);
    workspace.put("new.json", &context(&"c".repeat(40)));
    assert!(set(false, Some(&old), true).is_err());
}
