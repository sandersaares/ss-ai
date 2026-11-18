//! Preserves diagnostic updates committed while a cache store waits.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::findings;
use ss_ai_review_workflow::focus_area_generation;
use ss_ai_review_workflow::focus_cache::{CacheOptions, restore, store};
use ss_ai_runtime::{Clock, FileTransaction, LockMode, ResourceLock};
use tempfile::Builder;

/// Makes unrelated contention fail rather than waiting on a real clock.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected contention")
    }
}

/// Commits a competing update after the store observes the diagnostic lock.
struct CompetingWriter<'a> {
    path: PathBuf,
    held: RefCell<Option<FileTransaction<'a>>>,
    waited: Cell<bool>,
}

impl Clock for CompetingWriter<'_> {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        assert!(!self.waited.replace(true), "unexpected repeated contention");
        drop(self.held.borrow_mut().take().unwrap());
        let transaction = FileTransaction::open(&self.path, &NoWait)?;
        transaction.write_bytes(br#"{"cache":"Hit","reason":null}"#)?;
        Ok(())
    }
}

/// A store must merge against the record published by the preceding owner.
#[test]
fn cache_store_merges_diagnostics_after_lock_contention() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = Builder::new()
        .prefix(".diagnostic-migration-")
        .tempdir_in(&root)
        .unwrap();
    let session = workspace.path().join("session");
    let input = workspace.path().join("input");
    let skill = root.join("skills/ss-review");
    let cache = workspace.path().join("cache");
    fs::create_dir_all(session.join("instruction-set-source/external")).unwrap();
    fs::create_dir_all(session.join("instruction-set-raw")).unwrap();
    fs::create_dir_all(session.join("focus-areas")).unwrap();
    fs::create_dir_all(input.join("external")).unwrap();
    fs::write(input.join("external/rules.md"), "rules").unwrap();
    fs::write(
        session.join("instruction-set-source/external/rules.md"),
        "rules",
    )
    .unwrap();
    fs::write(session.join("instruction-set-raw/raw.md"), "raw").unwrap();
    fs::write(session.join("instruction-set-categories.md"), "categories").unwrap();
    fs::write(session.join("focus-areas/all.md"), "all guidance").unwrap();
    fs::write(
        session.join("focus-area-expansion-status.json"),
        r#"{"schemaVersion":1,"state":"Complete","references":[]}"#,
    )
    .unwrap();
    let session = fs::canonicalize(session).unwrap();
    let options = CacheOptions {
        session: &session,
        input_directory: &input,
        skill_root: &skill,
        recipe: &skill.join("prepare-focus-areas.md"),
        cache_directory: &cache,
        generator_profile: "test-model",
        freshness_days: 7,
    };
    let operation = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        &NoWait,
    )
    .unwrap();
    let identity = focus_area_generation::bind_snapshot_locked(&session, &NoWait).unwrap();
    drop(operation);
    let restored = restore(&options, false, &identity, &NoWait).unwrap();
    // Missing cache identity takes the normal bypass path without requiring generation.
    fs::remove_file(session.join("focus-area-cache-identity.json")).unwrap();
    let path = session.join("focus-area-cache-diagnostics.json");
    let held = FileTransaction::open(&path, &NoWait).unwrap();
    let clock = CompetingWriter {
        path: path.clone(),
        held: RefCell::new(Some(held)),
        waited: Cell::new(false),
    };
    store(
        &options,
        restored.preparation_identity.as_deref().unwrap(),
        &clock,
    )
    .unwrap();
    assert!(clock.waited.get());
    let record: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(record.get("cache").unwrap(), &json!("Hit"));
    assert_eq!(record.get("store").unwrap(), &json!("Bypassed"));
    assert_eq!(
        record.get("storeReason").unwrap(),
        &json!("SnapshotUnavailable")
    );
}

/// Validation ignores an active sibling lock and an incomplete publication.
#[test]
fn finding_validation_ignores_active_lock_and_staging_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = Builder::new()
        .prefix(".finding-validation-")
        .tempdir_in(root)
        .unwrap();
    let path = workspace.path().join("reviewer.json");
    fs::write(&path, br#"{"schemaVersion":1,"kind":"reviewer","reviewerId":"shared","focusArea":"correctness","status":"complete","findings":[]}"#).unwrap();
    let staging = workspace
        .path()
        .join(".ss-ai-transaction-aB1234567890abcdefgh1234.tmp");
    fs::write(&staging, b"{partial").unwrap();
    let lock = FileTransaction::open(&path, &NoWait).unwrap();
    let report =
        findings::validate_artifacts(workspace.path(), &["shared".into()], &NoWait).unwrap();
    assert_eq!(report.get("isValid").unwrap(), true);
    assert_eq!(
        report.get("artifacts").unwrap().as_array().unwrap().len(),
        1
    );
    assert!(staging.exists());
    assert!(path.with_extension("json.lock").exists());
    drop(lock);
}
