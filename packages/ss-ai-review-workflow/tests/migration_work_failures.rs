//! Isolates subprocess write failures from concurrent lock-owning tests.
#![cfg(target_os = "linux")]
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixture uses fixed record shapes and asserts failures"
)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{self, Command};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::json;
use ss_ai_review_workflow::review_work;
use ss_ai_runtime::{Clock, fail};

/// Stops unexpected contention without a wall-clock delay.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected test lock contention"))
    }
}

/// Owns the fixture files used by the failure subprocess.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".migration-write-failures-{}", process::id()));
        #[expect(clippy::create_dir, reason = "Existing test state must not be reused")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires a subprocess and a kernel file-size limit")]
fn migration_failed_journal_write_child() {
    let Some(queue) = env::var_os("SS_MIGRATION_FAILURE_QUEUE") else {
        return;
    };
    let queue = PathBuf::from(queue);
    match env::var("SS_MIGRATION_FAILURE_OPERATION").unwrap().as_str() {
        "claim" => assert!(review_work::next(&queue, &FixedClock).is_err()),
        "complete" => {
            assert!(review_work::complete_group(&queue, "group-001", &FixedClock).is_err());
        }
        operation => panic!("Unexpected operation {operation}"),
    }
}

#[test]
#[cfg_attr(miri, ignore = "requires a subprocess and a kernel file-size limit")]
#[cfg_attr(mutants, ignore = "A mutation may block a subprocess")]
fn migration_failed_claim_and_completion_preserve_journal_and_release_lock() {
    let workspace = Workspace::new();
    let file = json!({"path":workspace.0.join("file.rs"),"sizeBytes":1,"change":"modified"});
    let group = json!({"groupId":"group-001","files":[file]});
    let inventory = workspace.0.join("inventory.json");
    let groups = workspace.0.join("groups.json");
    let queue = workspace.0.join("queue.json");
    fs::write(&inventory, serde_json::to_vec(&json!([file])).unwrap()).unwrap();
    fs::write(&groups, serde_json::to_vec(&json!([group])).unwrap()).unwrap();
    review_work::initialize_queue(&inventory, &groups, &queue, false, &FixedClock).unwrap();
    for operation in ["claim", "complete"] {
        if operation == "complete" {
            let assessment = json!({"groupId":"group-001","path":file["path"],
                "disposition":"reviewed","scope":"","reason":""});
            review_work::set_assessment(&queue, &inventory, &groups, &assessment, &FixedClock)
                .unwrap();
        }
        let before = fs::read(&queue).unwrap();
        // The child receives a synchronous staging-write failure instead of SIGXFSZ.
        let result = Command::new("/bin/sh")
            .args(["-c", "trap '' XFSZ; ulimit -f 0; exec \"$@\"", "sh"])
            .arg(env::current_exe().unwrap())
            .args([
                "--exact",
                "migration_failed_journal_write_child",
                "--nocapture",
            ])
            .env("SS_MIGRATION_FAILURE_QUEUE", &queue)
            .env("SS_MIGRATION_FAILURE_OPERATION", operation)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read(&queue).unwrap(), before);
        assert_eq!(
            review_work::next(&queue, &FixedClock).unwrap()["item"],
            group
        );
    }
    review_work::complete_group(&queue, "group-001", &FixedClock).unwrap();
    assert_eq!(
        review_work::next(&queue, &FixedClock).unwrap()["exhausted"],
        true
    );
}
