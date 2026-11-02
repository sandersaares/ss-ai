//! Exercises review work, reviewer artifacts, and journal recovery.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixtures use fixed record shapes and assert failures"
)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::{findings, review_work};
use ss_ai_runtime::{Clock, fail};

/// Keeps the fixture deterministic and rejects unexpected contention.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected test lock contention"))
    }
}

/// Owns the inventory snapshots, journals, and artifacts for one test.
struct Fixture {
    root: PathBuf,
    inventory: PathBuf,
    groups: PathBuf,
    focus: PathBuf,
    output: PathBuf,
    artifacts: PathBuf,
    files: Value,
    grouped: Value,
    work: Value,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".migration-lifecycle-{}-{name}", process::id()));
        #[expect(clippy::create_dir, reason = "Existing test state must not be reused")]
        fs::create_dir(&root).unwrap();
        let focus = root.join("focus");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&focus).unwrap();
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(focus.join("z.md"), "Design").unwrap();
        fs::write(focus.join("a.md"), "Correctness").unwrap();
        let files = json!([
            {"path":root.join("new [file].rs"),"sizeBytes":1,"change":"new"},
            {"path":root.join("modified.rs"),"sizeBytes":2,"change":"modified"},
            {"path":root.join("deleted.rs"),"sizeBytes":3,"change":"deleted"}
        ]);
        let grouped = json!([
            {"groupId":"group-001","files":[files[0],files[1]]},
            {"groupId":"group-002","files":[files[2]]}
        ]);
        let inventory = root.join("inventory.json");
        let groups = root.join("groups.json");
        fs::write(&inventory, serde_json::to_vec(&files).unwrap()).unwrap();
        fs::write(&groups, serde_json::to_vec(&grouped).unwrap()).unwrap();
        let output = root.join("work");
        let work = review_work::create(
            &inventory,
            &groups,
            &focus,
            &output,
            None,
            None,
            &FixedClock,
        )
        .unwrap();
        Self {
            root,
            inventory,
            groups,
            focus,
            output,
            artifacts,
            files,
            grouped,
            work,
        }
    }

    fn path(&self, field: &str) -> PathBuf {
        PathBuf::from(self.work[field].as_str().unwrap())
    }

    fn manifest(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("manifestPath")).unwrap()).unwrap()
    }

    fn claim(&self) -> Value {
        review_work::next(&self.path("focusAreaQueuePath"), &FixedClock).unwrap()["item"].clone()
    }

    fn initialize(&self, reviewer: &Value) -> (PathBuf, PathBuf) {
        let queue = PathBuf::from(reviewer["fileGroupQueuePath"].as_str().unwrap());
        review_work::initialize_queue(
            &self.path("inventoryPath"),
            &self.path("fileGroupsPath"),
            &queue,
            false,
            &FixedClock,
        )
        .unwrap();
        let artifact = self
            .artifacts
            .join(format!("{}.json", reviewer["reviewerId"].as_str().unwrap()));
        findings::initialize(
            &artifact,
            reviewer["reviewerId"].as_str().unwrap(),
            reviewer["focusArea"].as_str().unwrap(),
            false,
            &FixedClock,
        )
        .unwrap();
        (queue, artifact)
    }

    fn complete_group(&self, queue: &Path, group: &Value) {
        for file in group["files"].as_array().unwrap() {
            let assessment = json!({"groupId":group["groupId"],"path":file["path"],
                "disposition":"reviewed","scope":"","reason":""});
            review_work::set_assessment(
                queue,
                &self.path("inventoryPath"),
                &self.path("fileGroupsPath"),
                &assessment,
                &FixedClock,
            )
            .unwrap();
        }
        review_work::complete_group(queue, group["groupId"].as_str().unwrap(), &FixedClock)
            .unwrap();
    }

    fn complete_all(&self) -> Vec<(PathBuf, PathBuf)> {
        let mut reviewers = Vec::new();
        for _ in 0..2 {
            let reviewer = self.claim();
            let (queue, artifact) = self.initialize(&reviewer);
            for group in self.grouped.as_array().unwrap() {
                assert_eq!(
                    review_work::next(&queue, &FixedClock).unwrap()["item"],
                    *group
                );
                self.complete_group(&queue, group);
            }
            findings::complete(&artifact, &queue, &FixedClock).unwrap();
            reviewers.push((queue, artifact));
        }
        reviewers
    }

    fn validate(&self) -> Value {
        review_work::validate(&self.path("manifestPath"), &self.artifacts, &FixedClock).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn migration_snapshots_dispatch_and_independent_resume() {
    let fixture = Fixture::new("snapshot");
    assert_eq!(fixture.work["fileCount"], 3);
    assert_eq!(fixture.work["groupCount"], 2);
    assert_eq!(fixture.work["reviewerCount"], 2);
    let manifest = fixture.manifest();
    let reviewers = manifest["reviewers"].as_array().unwrap();
    assert_eq!(reviewers[0]["reviewerId"], "focus-001");
    assert_eq!(reviewers[1]["reviewerId"], "focus-002");
    assert_ne!(
        reviewers[0]["fileGroupQueuePath"],
        reviewers[1]["fileGroupQueuePath"]
    );
    for (index, original) in ["a.md", "z.md"].iter().enumerate() {
        let snapshot = PathBuf::from(reviewers[index]["focusArea"].as_str().unwrap());
        assert_eq!(snapshot, fixture.focus.join(original));
        assert_eq!(
            fs::read(snapshot).unwrap(),
            fs::read(fixture.focus.join(original)).unwrap()
        );
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(fixture.path("inventoryPath")).unwrap()).unwrap(),
        fixture.files
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(fixture.path("fileGroupsPath")).unwrap())
            .unwrap(),
        fixture.grouped
    );
    fs::write(&fixture.inventory, "[]").unwrap();
    fs::write(&fixture.groups, "[]").unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(fixture.path("inventoryPath")).unwrap()).unwrap(),
        fixture.files
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(fixture.path("fileGroupsPath")).unwrap())
            .unwrap(),
        fixture.grouped
    );

    let first = fixture.claim();
    fs::write(
        &fixture.inventory,
        serde_json::to_vec(&fixture.files).unwrap(),
    )
    .unwrap();
    fs::write(
        &fixture.groups,
        serde_json::to_vec(&fixture.grouped).unwrap(),
    )
    .unwrap();
    assert!(
        review_work::create(
            &fixture.inventory,
            &fixture.groups,
            &fixture.focus,
            &fixture.output,
            None,
            None,
            &FixedClock
        )
        .is_err()
    );
    let second = fixture.claim();
    assert_eq!(second["reviewerId"], "focus-002");
    assert_eq!(
        review_work::next(&fixture.path("focusAreaQueuePath"), &FixedClock).unwrap()["exhausted"],
        true
    );
    let (first_queue, _) = fixture.initialize(&first);
    let (second_queue, _) = fixture.initialize(&second);
    assert_eq!(
        review_work::next(&first_queue, &FixedClock).unwrap()["item"],
        fixture.grouped[0]
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&second_queue).unwrap()).unwrap()["items"],
        fixture.grouped
    );
    assert!(
        review_work::initialize_queue(
            &fixture.path("inventoryPath"),
            &fixture.path("fileGroupsPath"),
            &first_queue,
            false,
            &FixedClock
        )
        .is_err()
    );
    review_work::initialize_queue(
        &fixture.path("inventoryPath"),
        &fixture.path("fileGroupsPath"),
        &first_queue,
        true,
        &FixedClock,
    )
    .unwrap();
    assert_eq!(
        review_work::next(&first_queue, &FixedClock).unwrap()["item"],
        fixture.grouped[0]
    );
    fixture.complete_group(&first_queue, &fixture.grouped[0]);
    let state = review_work::initialize_queue(
        &fixture.path("inventoryPath"),
        &fixture.path("fileGroupsPath"),
        &first_queue,
        true,
        &FixedClock,
    )
    .unwrap();
    assert_eq!(state["completedGroupCount"], 1);
    assert_eq!(
        review_work::next(&first_queue, &FixedClock).unwrap()["item"],
        fixture.grouped[1]
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(second_queue).unwrap()).unwrap()["items"],
        fixture.grouped
    );
}

#[test]
fn migration_manifest_validation_rejects_missing_wrong_and_duplicate_artifacts() {
    let fixture = Fixture::new("manifest");
    let reviewers = fixture.complete_all();
    let result = fixture.validate();
    assert_eq!(result["isValid"], true);
    assert_eq!(result["artifacts"].as_array().unwrap().len(), 2);
    assert_eq!(result["findingCount"], 0);

    let original = fs::read(&reviewers[1].1).unwrap();
    fs::remove_file(&reviewers[1].1).unwrap();
    let result = fixture.validate();
    assert_eq!(result["isValid"], false);
    assert_eq!(result["missingReviewers"], json!(["focus-002"]));
    fs::write(&reviewers[1].1, original).unwrap();

    let bytes = fs::read(&reviewers[0].1).unwrap();
    let mut artifact: Value = serde_json::from_slice(&bytes).unwrap();
    artifact["focusArea"] = json!("unassigned");
    fs::write(&reviewers[0].1, serde_json::to_vec(&artifact).unwrap()).unwrap();
    let result = fixture.validate();
    assert_eq!(result["isValid"], false);
    assert!(
        result["errors"]
            .to_string()
            .contains("different focus area")
    );
    fs::write(&reviewers[0].1, bytes).unwrap();

    let duplicate = fixture.artifacts.join("duplicate.json");
    fs::copy(&reviewers[0].1, &duplicate).unwrap();
    let result = fixture.validate();
    assert_eq!(result["isValid"], false);
    assert!(result["errors"].to_string().contains("Duplicate reviewer"));
    fs::remove_file(duplicate).unwrap();

    let manifest_path = fixture.path("manifestPath");
    let original = fs::read(&manifest_path).unwrap();
    let mut manifest = fixture.manifest();
    manifest["reviewers"] = json!([]);
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(review_work::validate(&manifest_path, &fixture.artifacts, &FixedClock).is_err());
    for field in ["reviewerId", "focusArea", "fileGroupQueuePath"] {
        let mut manifest: Value = serde_json::from_slice(&original).unwrap();
        manifest["reviewers"][1][field] = manifest["reviewers"][0][field].clone();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(
            review_work::validate(&manifest_path, &fixture.artifacts, &FixedClock).is_err(),
            "{field}"
        );
    }
}

#[test]
fn migration_manifest_and_resume_reject_incomplete_or_altered_journals() {
    let fixture = Fixture::new("journal-coverage");
    let reviewers = fixture.complete_all();
    let queue = &reviewers[0].0;
    let original = fs::read(queue).unwrap();
    let mut journal: Value = serde_json::from_slice(&original).unwrap();
    journal["completedGroupIds"] = json!(["group-001"]);
    fs::write(queue, serde_json::to_vec(&journal).unwrap()).unwrap();
    let before = fs::read(queue).unwrap();
    assert!(
        review_work::initialize_queue(
            &fixture.path("inventoryPath"),
            &fixture.path("fileGroupsPath"),
            queue,
            true,
            &FixedClock,
        )
        .is_err()
    );
    assert_eq!(fs::read(queue).unwrap(), before);
    assert_eq!(fixture.validate()["isValid"], false);

    fs::write(queue, &original).unwrap();
    let dispatch = fixture.path("focusAreaQueuePath");
    let saved_dispatch = fs::read(&dispatch).unwrap();
    let mut dispatch_value: Value = serde_json::from_slice(&saved_dispatch).unwrap();
    dispatch_value["items"] = json!([fixture.manifest()["reviewers"][0]]);
    fs::write(&dispatch, serde_json::to_vec(&dispatch_value).unwrap()).unwrap();
    let result = fixture.validate();
    assert_eq!(result["isValid"], false);
    assert!(result["errors"].to_string().contains("dispatch queue"));
    fs::write(dispatch, saved_dispatch).unwrap();

    journal = serde_json::from_slice(&original).unwrap();
    journal["completedGroupIds"] = json!([]);
    journal["items"] = fixture.grouped.clone();
    journal["assessments"] = json!([]);
    fs::write(queue, serde_json::to_vec(&journal).unwrap()).unwrap();
    let result = fixture.validate();
    assert_eq!(result["isValid"], false);
    assert!(result["errors"].to_string().contains("not complete"));
    journal["items"][0]["files"][0]["path"] = json!(fixture.root.join("changed.rs"));
    fs::write(queue, serde_json::to_vec(&journal).unwrap()).unwrap();
    let before = fs::read(queue).unwrap();
    assert!(
        review_work::initialize_queue(
            &fixture.path("inventoryPath"),
            &fixture.path("fileGroupsPath"),
            queue,
            true,
            &FixedClock,
        )
        .is_err()
    );
    assert_eq!(fs::read(queue).unwrap(), before);
}

#[test]
fn migration_interrupted_worker_retains_findings_and_replays_its_claim() {
    let fixture = Fixture::new("interrupted");
    let reviewer = fixture.claim();
    let (queue, artifact) = fixture.initialize(&reviewer);
    let finding = fixture.root.join("finding.json");
    fs::write(
        &finding,
        serde_json::to_vec(&json!({
            "title":"Preserve review progress","description":"Progress must survive replacement.",
            "reasoning":"An interrupted worker may have written findings.",
            "consequence":"Resetting loses findings.","recommendedAction":"Retain the artifact.",
            "anchor":{"kind":"file","path":"review.ps1"}
        }))
        .unwrap(),
    )
    .unwrap();
    for (index, group) in fixture.grouped.as_array().unwrap().iter().enumerate() {
        assert_eq!(
            review_work::next(&queue, &FixedClock).unwrap()["item"],
            *group
        );
        assert_eq!(
            findings::add(&artifact, &finding, "reviewer", &[], &FixedClock).unwrap(),
            format!("focus-001-{}", index + 1)
        );
        if index == 0 {
            fixture.complete_group(&queue, group);
        }
    }
    let journal = fs::read(&queue).unwrap();
    let state = review_work::initialize_queue(
        &fixture.path("inventoryPath"),
        &fixture.path("fileGroupsPath"),
        &queue,
        true,
        &FixedClock,
    )
    .unwrap();
    assert_eq!(state["completedGroupCount"], 1);
    assert_eq!(state["inProgressGroupId"], "group-002");
    assert_eq!(state["remainingCount"], 0);
    assert_eq!(fs::read(&queue).unwrap(), journal);
    let resumed = findings::initialize_with_queue(
        &artifact,
        "focus-001",
        reviewer["focusArea"].as_str().unwrap(),
        false,
        true,
        &queue,
        &FixedClock,
    )
    .unwrap();
    assert_eq!(resumed["findings"][0]["id"], "focus-001-1");
    assert_eq!(resumed["findings"][1]["id"], "focus-001-2");
    assert!(findings::complete(&artifact, &queue, &FixedClock).is_err());
    assert_eq!(
        review_work::next(&queue, &FixedClock).unwrap()["item"],
        fixture.grouped[1]
    );
    assert_eq!(
        findings::add(&artifact, &finding, "reviewer", &[], &FixedClock).unwrap(),
        "focus-001-3"
    );
    fixture.complete_group(&queue, &fixture.grouped[1]);
    assert_eq!(
        review_work::next(&queue, &FixedClock).unwrap()["exhausted"],
        true
    );
    assert_eq!(
        findings::read_artifact(&artifact, &FixedClock).unwrap()["findings"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    findings::complete(&artifact, &queue, &FixedClock).unwrap();
}

#[test]
fn migration_claim_order_requires_current_group_and_durable_completion() {
    let fixture = Fixture::new("claim-order");
    let reviewer = fixture.claim();
    let (queue, artifact) = fixture.initialize(&reviewer);
    let first = review_work::next(&queue, &FixedClock).unwrap();
    assert_eq!(first["item"], fixture.grouped[0]);
    assert_eq!(first["remainingCount"], 1);
    let before = fs::read(&queue).unwrap();
    assert_eq!(
        review_work::next(&queue, &FixedClock).unwrap()["item"],
        first["item"]
    );
    assert_eq!(fs::read(&queue).unwrap(), before);
    assert!(review_work::complete_group(&queue, "group-002", &FixedClock).is_err());
    assert_eq!(fs::read(&queue).unwrap(), before);
    fixture.complete_group(&queue, &fixture.grouped[0]);
    let last = review_work::next(&queue, &FixedClock).unwrap();
    assert_eq!(last["item"], fixture.grouped[1]);
    assert_eq!(last["remainingCount"], 0);
    assert!(findings::complete(&artifact, &queue, &FixedClock).is_err());
    let before = fs::read(&queue).unwrap();
    assert_eq!(
        review_work::complete_group(&queue, "group-001", &FixedClock).unwrap()["alreadyComplete"],
        true
    );
    assert_eq!(fs::read(&queue).unwrap(), before);
    assert_eq!(
        review_work::next(&queue, &FixedClock).unwrap()["item"],
        last["item"]
    );
    fixture.complete_group(&queue, &fixture.grouped[1]);
    for _ in 0..2 {
        let exhausted = review_work::next(&queue, &FixedClock).unwrap();
        assert_eq!(exhausted["exhausted"], true);
        assert!(exhausted["item"].is_null());
        assert_eq!(exhausted["remainingCount"], 0);
    }
    findings::complete(&artifact, &queue, &FixedClock).unwrap();
}

#[test]
fn migration_malformed_missing_and_wrong_kind_queues_never_report_exhaustion() {
    let fixture = Fixture::new("corrupt-queues");
    let reviewer = fixture.claim();
    let (queue, artifact) = fixture.initialize(&reviewer);
    let valid = fs::read(&queue).unwrap();
    let mut mutations = Vec::new();
    for text in ["{broken", "[]", "null"] {
        mutations.push(text.to_owned());
    }
    let mut journal: Value = serde_json::from_slice(&valid).unwrap();
    for (field, value) in [
        ("items", Value::Null),
        ("items", json!({})),
        ("items", json!([null])),
        ("schemaVersion", json!(2)),
        ("completedGroupIds", json!(["one", "one"])),
        ("completedGroupIds", json!({})),
        ("kind", json!("unknown")),
    ] {
        let mut invalid = journal.clone();
        invalid[field] = value;
        mutations.push(serde_json::to_string(&invalid).unwrap());
    }
    journal["items"] = json!([]);
    journal["completedGroupIds"] = json!(["group-001"]);
    journal["inProgress"] = fixture.grouped[0].clone();
    mutations.push(serde_json::to_string(&journal).unwrap());
    for text in mutations {
        fs::write(&queue, &text).unwrap();
        assert!(review_work::next(&queue, &FixedClock).is_err(), "{text}");
        assert_eq!(fs::read_to_string(&queue).unwrap(), text);
    }
    fs::remove_file(&queue).unwrap();
    assert!(review_work::next(&queue, &FixedClock).is_err());
    let dispatch = fixture.path("focusAreaQueuePath");
    assert!(findings::complete(&artifact, &dispatch, &FixedClock).is_err());
}
