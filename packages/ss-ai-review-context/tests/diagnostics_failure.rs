//! Exercises diagnostic updates when locking or atomic publication fails.

use std::cell::Cell;
use std::fs;
use std::num::NonZeroU32;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::Value;
use ss_ai_review_context::{MalformedDiagnostics, increment_diagnostic, read_diagnostics};
use ss_ai_runtime::{Clock, FileTransaction, LockMode, ResourceLock, classification};

/// Exercises bounded retries without consulting or waiting for the real clock.
#[derive(Default)]
struct ImmediateClock(Cell<usize>);

impl Clock for ImmediateClock {
    fn now(&self) -> Timestamp {
        panic!("Diagnostic updates do not need wall-clock time")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        self.0.set(self.0.get().saturating_add(1));
        Ok(())
    }
}

/// Isolates retained locks and staging files inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".diagnostic-failure-{}-{name}", std::process::id()));
        #[expect(clippy::create_dir, reason = "The fixture must not reuse prior state")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn diagnostic(&self) -> PathBuf {
        self.0.join("input-diagnostics.json")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
#[cfg_attr(
    mutants,
    ignore = "Removing the retry limit can prevent the held-lock scenario from terminating"
)]
fn exhausted_contention_preserves_record_and_retained_owner() {
    let workspace = Workspace::new("contention");
    let path = workspace.diagnostic();
    let clock = ImmediateClock::default();
    increment_diagnostic(&path, "setupCalls", NonZeroU32::MIN, &clock).unwrap();
    let before = fs::read(&path).unwrap();
    let owner = FileTransaction::open(&path, &clock).unwrap();
    let error = increment_diagnostic(&path, "setupCalls", NonZeroU32::MIN, &clock).unwrap_err();
    assert_eq!(classification(&error), "contention");
    assert!(error.find_source::<MalformedDiagnostics>().is_none());
    assert!(clock.0.get() > 0);
    assert_eq!(fs::read(&path).unwrap(), before);
    let lock_path = workspace.0.join("input-diagnostics.json.lock");
    assert!(lock_path.is_file());
    assert!(
        ResourceLock::try_open(&path, LockMode::Exclusive)
            .unwrap()
            .is_none()
    );
    drop(owner);
    assert!(lock_path.is_file());
    increment_diagnostic(&path, "setupCalls", NonZeroU32::MIN, &clock).unwrap();
    assert_eq!(
        read_diagnostics(&path, &clock)
            .unwrap()
            .pointer("/counts/setupCalls")
            .and_then(Value::as_u64),
        Some(2)
    );
}

/// The parent limits this process's file size so staging fails after lock acquisition.
#[test]
#[cfg(target_os = "linux")]
#[cfg_attr(
    miri,
    ignore = "requires a native subprocess and kernel file-size limits"
)]
fn diagnostic_write_failure_child() {
    let Some(path) = std::env::var_os("SS_DIAGNOSTIC_FAILURE_PATH") else {
        return;
    };
    let error = increment_diagnostic(
        &PathBuf::from(path),
        "setupCalls",
        NonZeroU32::MIN,
        &ImmediateClock::default(),
    )
    .unwrap_err();
    assert_eq!(classification(&error), "io");
    assert!(
        error.to_string().contains("Cannot write staging file"),
        "{error}"
    );
}

#[test]
#[cfg(target_os = "linux")]
#[cfg_attr(
    miri,
    ignore = "requires a native subprocess and kernel file-size limits"
)]
#[cfg_attr(
    mutants,
    ignore = "A mutation may block the child process rather than completing the test"
)]
fn failed_staging_write_preserves_counts_and_releases_lock() {
    let workspace = Workspace::new("write");
    let path = workspace.diagnostic();
    let clock = ImmediateClock::default();
    increment_diagnostic(&path, "setupCalls", NonZeroU32::MIN, &clock).unwrap();
    let before = fs::read(&path).unwrap();
    // Ignoring SIGXFSZ makes a zero file-size limit produce a synchronous
    // staging write error while leaving the already-written record readable.
    let output = Command::new("/bin/sh")
        .args(["-c", "trap '' XFSZ; ulimit -f 0; exec \"$@\"", "sh"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "diagnostic_write_failure_child", "--nocapture"])
        .env("SS_DIAGNOSTIC_FAILURE_PATH", &path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(fs::read_dir(&workspace.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".ss-ai-transaction-")
    }));
    let record: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(
        record.pointer("/counts/setupCalls").and_then(Value::as_u64),
        Some(1)
    );
    assert!(workspace.0.join("input-diagnostics.json.lock").is_file());
    let lock = ResourceLock::try_open(&path, LockMode::Exclusive).unwrap();
    assert!(lock.is_some(), "The failed update must release its lock");
    drop(lock);
    increment_diagnostic(&path, "setupCalls", NonZeroU32::MIN, &clock).unwrap();
    assert_eq!(
        read_diagnostics(&path, &clock)
            .unwrap()
            .pointer("/counts/setupCalls")
            .and_then(Value::as_u64),
        Some(2)
    );
}
