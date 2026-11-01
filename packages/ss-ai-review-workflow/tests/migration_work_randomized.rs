//! Exercises bounded claim, completion, and resume sequences through native APIs.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixture uses fixed-size records"
)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::review_work;
use ss_ai_runtime::{Clock, fail};

/// Makes unexpected lock contention fail without waiting.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected test lock contention"))
    }
}

/// Isolates journals from concurrent test processes in the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(seed: u64) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".migration-work-{}-{seed}", process::id()));
        #[expect(clippy::create_dir, reason = "Existing test state must not be reused")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "The generated operations and their shared invariants are checked together"
)]
fn migration_bounded_claim_resume_sequences_preserve_exact_coverage() {
    // Fixed seeds make every failure reproducible while exercising different operation orders.
    for seed in [1_u64, 17, 1009, 65537] {
        let workspace = Workspace::new(seed);
        let clock = FixedClock;
        let files: Vec<_> = (0..3)
            .map(|index| {
                json!({"path":workspace.0.join(format!("{index}.rs")), "sizeBytes":index,
                    "change":"modified"})
            })
            .collect();
        let groups: Vec<_> = files
            .iter()
            .enumerate()
            .map(|(index, file)| json!({"groupId":format!("group-{index}"),"files":[file]}))
            .collect();
        let inventory = workspace.write("inventory.json", &json!(files));
        let grouped = workspace.write("groups.json", &json!(groups));
        let queue = workspace.0.join("queue.json");
        review_work::initialize_queue(&inventory, &grouped, &queue, false, &clock).unwrap();

        let mut random = seed;
        let mut completed = 0;
        let mut current = false;
        // A fixed operation budget and a deterministic completion tail prevent hangs.
        for _ in 0..64 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            match (random >> 32) % 4 {
                0 => {
                    let claim = review_work::next(&queue, &clock).unwrap();
                    if completed == groups.len() {
                        assert_eq!(claim["exhausted"], true, "seed {seed}");
                        assert!(claim["item"].is_null());
                    } else {
                        assert_eq!(claim["item"], groups[completed], "seed {seed}");
                        current = true;
                    }
                }
                1 if current => {
                    let group = &groups[completed];
                    let assessment = json!({"groupId":group["groupId"],"path":group["files"][0]["path"],
                        "disposition":"reviewed","scope":"","reason":""});
                    review_work::set_assessment(&queue, &inventory, &grouped, &assessment, &clock)
                        .unwrap();
                    let id = group["groupId"].as_str().unwrap();
                    assert_eq!(
                        review_work::complete_group(&queue, id, &clock).unwrap()["alreadyComplete"],
                        false
                    );
                    completed += 1;
                    current = false;
                }
                2 => {
                    let before = fs::read(&queue).unwrap();
                    let resumed =
                        review_work::initialize_queue(&inventory, &grouped, &queue, true, &clock)
                            .unwrap();
                    assert_eq!(resumed["completedGroupCount"], completed);
                    assert_eq!(fs::read(&queue).unwrap(), before);
                }
                3 if completed > 0 => {
                    let before = fs::read(&queue).unwrap();
                    let id = groups[0]["groupId"].as_str().unwrap();
                    assert_eq!(
                        review_work::complete_group(&queue, id, &clock).unwrap()["alreadyComplete"],
                        true
                    );
                    assert_eq!(fs::read(&queue).unwrap(), before);
                }
                3 => {
                    let before = fs::read(&queue).unwrap();
                    assert!(review_work::complete_group(&queue, "unknown", &clock).is_err());
                    assert_eq!(fs::read(&queue).unwrap(), before);
                }
                _ => {}
            }
            let journal: Value = serde_json::from_slice(&fs::read(&queue).unwrap()).unwrap();
            let done = journal["completedGroupIds"].as_array().unwrap();
            assert_eq!(done.len(), completed, "seed {seed}");
            for (id, group) in done.iter().zip(&groups) {
                assert_eq!(*id, group["groupId"], "seed {seed}");
            }
            if current {
                assert_eq!(journal["inProgress"], groups[completed], "seed {seed}");
            } else {
                assert!(journal["inProgress"].is_null(), "seed {seed}");
            }
            let pending = journal["items"].as_array().unwrap();
            let first_pending = completed + usize::from(current);
            assert_eq!(pending, &groups[first_pending..], "seed {seed}");
        }
        for group in &groups[completed..] {
            assert_eq!(review_work::next(&queue, &clock).unwrap()["item"], *group);
            let assessment = json!({"groupId":group["groupId"],"path":group["files"][0]["path"],
                "disposition":"reviewed","scope":"","reason":""});
            review_work::set_assessment(&queue, &inventory, &grouped, &assessment, &clock).unwrap();
            review_work::complete_group(&queue, group["groupId"].as_str().unwrap(), &clock)
                .unwrap();
        }
        let exhausted = review_work::next(&queue, &clock).unwrap();
        assert_eq!(exhausted["exhausted"], true);
        assert!(exhausted["item"].is_null());
        let resumed =
            review_work::initialize_queue(&inventory, &grouped, &queue, true, &clock).unwrap();
        assert_eq!(resumed["completedGroupCount"], groups.len());
        assert!(resumed["inProgressGroupId"].is_null());
        assert_eq!(resumed["remainingCount"], 0);
    }
}
