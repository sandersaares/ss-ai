//! Tests failed batch staging in a process isolated from other lock-owning tests.
#![cfg(target_os = "linux")]
#![allow(
    clippy::indexing_slicing,
    reason = "The fixture uses fixed record shapes"
)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::{findings, review_work};
use ss_ai_runtime::{Clock, fail};

/// Supplies a reproducible timestamp and refuses unexpected contention.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2024-05-06T07:08:09Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected lock contention"))
    }
}

/// Runs only in the size-limited child so staging fails after the claim exists.
#[test]
#[cfg_attr(
    miri,
    ignore = "requires a native subprocess and kernel file-size limits"
)]
fn batch_staging_failure_child() {
    let Some(root) = std::env::var_os("SS_BATCH_FAILURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let batch: Value = serde_json::from_slice(&fs::read(root.join("batch.json")).unwrap()).unwrap();
    let error = review_work::complete_batch(
        &root.join("queue.json"),
        &root.join("inventory.json"),
        &root.join("groups.json"),
        &root.join("artifact.json"),
        &batch,
        &FixedClock,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("Cannot write staging file"),
        "{error}"
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "requires a native subprocess and kernel file-size limits"
)]
#[cfg_attr(mutants, ignore = "A mutation may block the child process")]
fn failed_staging_preserves_claim_and_allows_resume_and_retry() {
    // A separate test binary prevents this subprocess from inheriting locks
    // held by unrelated tests between fork and exec.
    let workspace = tempfile::Builder::new()
        .prefix(".migration-coverage-staging-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let root = workspace.path();
    let inventory = root.join("inventory.json");
    let groups = root.join("groups.json");
    let queue = root.join("queue.json");
    let artifact = root.join("artifact.json");
    let files = json!([
        {"path":root.join("old.rs"),"sizeBytes":1,"change":"deleted"},
        {"path":root.join("new.ps1"),"sizeBytes":1,"change":"new"}
    ]);
    fs::write(&inventory, serde_json::to_vec(&files).unwrap()).unwrap();
    fs::write(
        &groups,
        serde_json::to_vec(&json!([{"groupId":"rename","files":files}])).unwrap(),
    )
    .unwrap();
    let clock = FixedClock;
    findings::initialize(&artifact, "focus-001", "rules", false, &clock).unwrap();
    let state = review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    review_work::next(&queue, &clock).unwrap();
    let batch = json!({
        "schemaVersion":1,"snapshot":state["snapshot"],"groupId":"rename",
        "assessments":[
            {"groupId":"rename","path":files[0]["path"],"disposition":"reviewed","scope":"","reason":""},
            {"groupId":"rename","path":files[1]["path"],"disposition":"reviewed","scope":"","reason":""}
        ],
        "findingIds":[]
    });
    fs::write(root.join("batch.json"), serde_json::to_vec(&batch).unwrap()).unwrap();
    let before = fs::read(&queue).unwrap();
    let artifact_before = fs::read(&artifact).unwrap();
    // A zero file-size limit yields a synchronous write error rather than a signal.
    let output = Command::new("/bin/sh")
        .args(["-c", "trap '' XFSZ; ulimit -f 0; exec \"$@\"", "sh"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "batch_staging_failure_child", "--nocapture"])
        .env("SS_BATCH_FAILURE_ROOT", root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&queue).unwrap(), before);
    assert_eq!(fs::read(&artifact).unwrap(), artifact_before);
    assert!(fs::read_dir(root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".ss-ai-transaction-")
    }));
    review_work::initialize_queue(&inventory, &groups, &queue, true, &clock).unwrap();
    assert_eq!(
        review_work::next(&queue, &clock).unwrap()["item"]["groupId"],
        "rename"
    );
    assert_eq!(
        review_work::complete_batch(&queue, &inventory, &groups, &artifact, &batch, &clock)
            .unwrap()["alreadyComplete"],
        false
    );
}
