//! Exercises interrupted decision writes in an isolated test process.
#![cfg(target_os = "linux")]
#![allow(
    clippy::indexing_slicing,
    reason = "The assertions use fixed JSON fixture shapes"
)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::audit;
use ss_ai_runtime::{Clock, fail};

/// Avoids clock-based delays and permits no unexpected lock contention.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected lock contention"))
    }
}

/// Keeps each scenario's journal and staging files inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".decision-staging-{}-{name}", std::process::id()));
        #[expect(clippy::create_dir, reason = "A collision must not reuse test data")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn put(&self, name: &str, value: &Value) -> PathBuf {
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

/// Runs only in the size-limited child after the parent has staged inputs.
#[test]
#[cfg_attr(
    miri,
    ignore = "requires a native subprocess and kernel file-size limits"
)]
fn decision_staging_failure_child() {
    let Some(root) = std::env::var_os("SS_DECISION_FAILURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let error = audit::add_decisions(
        &root.join("decisions.json"),
        &root.join("plan.json"),
        None,
        true,
        "2024-05-06T09:10:11Z",
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
fn failed_staging_preserves_missing_or_existing_decisions_and_resumes() {
    for saved in [false, true] {
        let workspace = Workspace::new(if saved { "saved" } else { "empty" });
        let finding = |id| {
            json!({"schemaVersion":1,"id":id,"origin":"reviewer",
            "sourceFindingIds":[],"title":"Problem","description":"Details","reasoning":"Evidence",
            "consequence":"Impact","recommendedAction":"Fix","anchor":{"kind":"file","path":"a.rs"}})
        };
        let artifact = workspace.put(
            "findings.json",
            &json!({"schemaVersion":1,"kind":"consolidated",
            "findings":[finding("F1"),finding("F2")],"removed":[]}),
        );
        let journal = workspace.0.join("decisions.json");
        if saved {
            let item = workspace.put(
                "item.json",
                &json!({"findingId":"F1","decision":"auto-approved"}),
            );
            audit::add_decision(
                &journal,
                &item,
                None,
                true,
                "2024-05-06T09:10:11Z",
                &FixedClock,
            )
            .unwrap();
        }
        let before = if saved {
            Some(fs::read(&journal).unwrap())
        } else {
            None
        };
        let next = if saved { "F2" } else { "F1" };
        let plan = workspace.put(
            "plan.json",
            &json!({"schemaVersion":1,"decisions":[
            {"findingId":next,"decision":"needs-audit"}]}),
        );
        // A zero limit fails staging synchronously without terminating the child.
        let result = Command::new("/bin/sh")
            .args(["-c", "trap '' XFSZ; ulimit -f 0; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "decision_staging_failure_child", "--nocapture"])
            .env("SS_DECISION_FAILURE_ROOT", &workspace.0)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        if let Some(bytes) = before {
            assert_eq!(fs::read(&journal).unwrap(), bytes);
        } else {
            assert!(!journal.exists());
        }
        assert!(fs::read_dir(&workspace.0).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ss-ai-transaction-")
        }));
        let poll = audit::poll_classification(&artifact, &journal, &FixedClock).unwrap();
        assert_eq!(poll["finding"]["id"], next);
        assert_eq!(poll["remainingCount"], if saved { 1 } else { 2 });
        let retry = audit::add_decisions(
            &journal,
            &plan,
            None,
            true,
            "2024-05-06T09:10:11Z",
            &FixedClock,
        )
        .unwrap();
        assert_eq!(retry["recorded"], json!([next]));
        let after = audit::poll_classification(&artifact, &journal, &FixedClock).unwrap();
        if saved {
            assert_eq!(after["isComplete"], true);
            assert_eq!(after["auditFindingIds"], json!([next]));
        } else {
            assert_eq!(after["finding"]["id"], "F2");
            assert_eq!(after["remainingCount"], 1);
        }
    }
}
