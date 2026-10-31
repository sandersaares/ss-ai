//! Exercises bounded claim, batch, failure, and resume transitions.
#![no_main]
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "The model and generated operations have fixed small bounds"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use libfuzzer_sys::fuzz_target;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::{findings, review_work};
use ss_ai_runtime::Clock;

// Bound each generated sequence while leaving room for repeated transitions.
const MAX_OPERATIONS: usize = 32;
// Multiple groups distinguish pending work from a repeated in-progress claim.
const GROUPS: usize = 3;

/// Detects unexpected reliance on wall time or lock contention.
struct FakeClock;

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        panic!("Coverage transactions must not inspect wall time");
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Each fuzz input owns its journal without contention");
    }
}

/// Isolates every generated transaction sequence inside the checkout.
struct Files {
    root: PathBuf,
    inventory: PathBuf,
    groups: PathBuf,
    queue: PathBuf,
    artifact: PathBuf,
    entries: Vec<Value>,
}

impl Files {
    #[expect(
        clippy::create_dir,
        reason = "An existing fuzz workspace must not be reused"
    )]
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The atomic only assigns distinct names within the process.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".coverage-fuzz-{}-{id}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let files = (0..GROUPS)
            .map(|index| {
                json!({
                    "path": root.join(format!("file-{index}")).to_str().unwrap(),
                    "sizeBytes": index,
                    "change": (["deleted", "new", "modified"][index])
                })
            })
            .collect::<Vec<_>>();
        let groups = files
            .iter()
            .enumerate()
            .map(|(index, file)| json!({"groupId":group_id(index), "files":[file]}))
            .collect::<Vec<_>>();
        let result = Self {
            inventory: root.join("inventory.json"),
            groups: root.join("groups.json"),
            queue: root.join("queue.json"),
            artifact: root.join("artifact.json"),
            root,
            entries: files,
        };
        fs::write(
            &result.inventory,
            serde_json::to_vec(&result.entries).unwrap(),
        )
        .unwrap();
        fs::write(&result.groups, serde_json::to_vec(&groups).unwrap()).unwrap();
        findings::initialize(&result.artifact, "focus-001", "/focus", false, &FakeClock).unwrap();
        result
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Tracks the externally visible state independently from the persisted journal.
struct Model {
    pending: Vec<usize>,
    claimed: Option<usize>,
    completed: Vec<usize>,
    failed: bool,
    snapshot: Value,
}

/// Selects stable group IDs matching the immutable grouped inventory.
fn group_id(index: usize) -> String {
    format!("group-{index}")
}

/// Constructs the exact completion payload for an assigned singleton.
fn batch(files: &Files, model: &Model, group: usize) -> Value {
    json!({
        "schemaVersion": 1,
        "snapshot": model.snapshot,
        "groupId": group_id(group),
        "assessments": [{
            "groupId": group_id(group),
            "path": files.entries[group]["path"],
            "disposition": "reviewed",
            "scope": "",
            "reason": ""
        }],
        "findingIds": []
    })
}

/// Claims or repeats an item according to the model's current state.
fn claim(files: &Files, model: &mut Model) {
    let before = fs::read(&files.queue).unwrap();
    let result = review_work::next(&files.queue, &FakeClock);
    if model.failed {
        assert!(result.is_err());
        assert_eq!(fs::read(&files.queue).unwrap(), before);
        return;
    }
    let result = result.unwrap();
    if let Some(group) = model.claimed {
        assert_eq!(result["item"]["groupId"], group_id(group));
        assert_eq!(fs::read(&files.queue).unwrap(), before);
    } else if model.pending.is_empty() {
        assert_eq!(result["exhausted"], true);
        assert_eq!(fs::read(&files.queue).unwrap(), before);
    } else {
        let group = model.pending.remove(0);
        assert_eq!(result["item"]["groupId"], group_id(group));
        assert_eq!(result["snapshot"], model.snapshot);
        model.claimed = Some(group);
    }
}

/// Commits a full claim or verifies that failed work cannot commit.
fn complete(files: &Files, model: &mut Model) {
    let Some(group) = model.claimed else { return };
    let before = fs::read(&files.queue).unwrap();
    let result = review_work::complete_batch(
        &files.queue,
        &files.inventory,
        &files.groups,
        &files.artifact,
        &batch(files, model, group),
        &FakeClock,
    );
    if model.failed {
        assert!(result.is_err());
        assert_eq!(fs::read(&files.queue).unwrap(), before);
    } else {
        assert_eq!(result.unwrap()["alreadyComplete"], false);
        model.claimed = None;
        model.completed.push(group);
    }
}

/// Verifies invalid and conflicting batches cannot alter the journal.
fn rejected_batch(files: &Files, model: &Model, kind: u8) {
    let group = match kind {
        0 => model.claimed,
        3 => model.completed.last().copied(),
        _ => model.claimed.or_else(|| model.completed.last().copied()),
    };
    let Some(group) = group else { return };
    let mut batch = batch(files, model, group);
    match kind {
        0 => batch["assessments"] = json!([]),
        1 => batch["findingIds"] = json!(["missing"]),
        2 => {
            let replacement = if model.snapshot["inventorySha256"] == "0".repeat(64) {
                '1'
            } else {
                '0'
            };
            batch["snapshot"]["inventorySha256"] = json!(replacement.to_string().repeat(64));
        }
        _ => batch["assessments"][0]["reason"] = json!("conflict"),
    }
    let before = fs::read(&files.queue).unwrap();
    review_work::complete_batch(
        &files.queue,
        &files.inventory,
        &files.groups,
        &files.artifact,
        &batch,
        &FakeClock,
    )
    .unwrap_err();
    assert_eq!(fs::read(&files.queue).unwrap(), before);
}

/// Replays an already-completed batch without rewriting its receipt.
fn replay(files: &Files, model: &Model) {
    let Some(group) = model.completed.last().copied() else {
        return;
    };
    let before = fs::read(&files.queue).unwrap();
    let result = review_work::complete_batch(
        &files.queue,
        &files.inventory,
        &files.groups,
        &files.artifact,
        &batch(files, model, group),
        &FakeClock,
    );
    if model.failed {
        assert!(result.is_err());
    } else {
        assert_eq!(result.unwrap()["alreadyComplete"], true);
    }
    assert_eq!(fs::read(&files.queue).unwrap(), before);
}

/// Clears a failure only after the existing snapshot and coverage validate.
fn resume(files: &Files, model: &mut Model) {
    let before = fs::read(&files.queue).unwrap();
    let result = review_work::initialize_queue(
        &files.inventory,
        &files.groups,
        &files.queue,
        true,
        &FakeClock,
    )
    .unwrap();
    assert_eq!(result["snapshot"], model.snapshot);
    if !model.failed {
        assert_eq!(fs::read(&files.queue).unwrap(), before);
    }
    model.failed = false;
}

/// Ensures snapshot changes are rejected without repairing or resetting state.
fn stale_input(files: &Files) {
    let inventory = fs::read(&files.inventory).unwrap();
    let mut changed = inventory.clone();
    changed.push(b'\n');
    let before = fs::read(&files.queue).unwrap();
    fs::write(&files.inventory, changed).unwrap();
    review_work::initialize_queue(
        &files.inventory,
        &files.groups,
        &files.queue,
        true,
        &FakeClock,
    )
    .unwrap_err();
    assert_eq!(fs::read(&files.queue).unwrap(), before);
    fs::write(&files.inventory, inventory).unwrap();
}

/// Compares the complete journal with the model after every operation.
fn check(files: &Files, model: &Model) {
    let queue: Value = serde_json::from_slice(&fs::read(&files.queue).unwrap()).unwrap();
    let pending = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| group["groupId"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        pending,
        model
            .pending
            .iter()
            .map(|id| group_id(*id))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        queue["inProgress"].get("groupId").and_then(Value::as_str),
        model.claimed.map(group_id).as_deref()
    );
    assert_eq!(
        queue["completedGroupIds"],
        json!(
            model
                .completed
                .iter()
                .map(|id| group_id(*id))
                .collect::<Vec<_>>()
        )
    );
    assert_eq!(queue["failure"].is_null(), !model.failed);
    assert_eq!(queue["snapshot"], model.snapshot);
    assert_eq!(
        queue["assessments"].as_array().unwrap().len(),
        model.completed.len()
    );
    assert_eq!(
        queue["batchReceipts"].as_array().unwrap().len(),
        model.completed.len()
    );
}

fuzz_target!(|input: &[u8]| {
    let files = Files::new();
    let initialized = review_work::initialize_queue(
        &files.inventory,
        &files.groups,
        &files.queue,
        false,
        &FakeClock,
    )
    .unwrap();
    let mut model = Model {
        pending: (0..GROUPS).collect(),
        claimed: None,
        completed: Vec::new(),
        failed: false,
        snapshot: initialized["snapshot"].clone(),
    };

    // Every input reaches a completed batch, replay, and failure/resume boundary.
    claim(&files, &mut model);
    rejected_batch(&files, &model, 0);
    complete(&files, &mut model);
    replay(&files, &model);
    review_work::set_failure(&files.queue, "stopped", &FakeClock).unwrap();
    model.failed = true;
    claim(&files, &mut model);
    resume(&files, &mut model);
    check(&files, &model);

    for operation in input.iter().take(MAX_OPERATIONS) {
        match operation % 9 {
            0 => claim(&files, &mut model),
            1 => complete(&files, &mut model),
            2 => {
                review_work::set_failure(&files.queue, "stopped", &FakeClock).unwrap();
                model.failed = true;
            }
            3 => resume(&files, &mut model),
            4 => replay(&files, &model),
            5 => rejected_batch(&files, &model, 0),
            6 => rejected_batch(&files, &model, 1),
            7 => rejected_batch(&files, &model, 2),
            _ => stale_input(&files),
        }
        check(&files, &model);
    }
    // Bounded draining proves each completion advances toward exhaustion.
    if model.failed {
        resume(&files, &mut model);
    }
    for _ in 0..GROUPS {
        if model.pending.is_empty() && model.claimed.is_none() {
            break;
        }
        let completed = model.completed.len();
        claim(&files, &mut model);
        complete(&files, &mut model);
        assert_eq!(model.completed.len(), completed + 1);
        check(&files, &model);
    }
    assert!(model.pending.is_empty() && model.claimed.is_none());
    assert_eq!(model.completed.len(), GROUPS);
    claim(&files, &mut model);
    replay(&files, &model);
    rejected_batch(&files, &model, 3);
    check(&files, &model);
});
