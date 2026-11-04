//! Exercises generation fencing and publication with real session files.
#![allow(
    clippy::indexing_slicing,
    reason = "The tests construct the JSON shapes they index"
)]
#![allow(
    clippy::create_dir,
    reason = "Reusing an existing fixture directory must fail"
)]
#![allow(
    clippy::assertions_on_result_states,
    reason = "Some tests only need to verify that an operation is rejected"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::json;
use ss_ai_review_workflow::focus_area_expansion::{Operation, update};
use ss_ai_review_workflow::focus_area_generation::{
    begin, bind_restore_locked, bind_snapshot_locked, invalidate_locked, invalidate_ready_locked,
    invalidate_snapshot_locked, is_managed_locked, publish, validate_published_locked,
    verify_preparation_locked, verify_ready_locked, verify_snapshot_locked,
};
use ss_ai_review_workflow::focus_area_map::{MapOperation, assemble, update as update_map};
use ss_ai_runtime::{Clock, LockMode, ResourceLock};

/// Unexpected contention fails immediately rather than sleeping.
struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::MIN
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Unexpected contention in an isolated fixture")
    }
}

/// Keeps the generated session local to this checkout and cleans it afterward.
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // The counter only assigns unique fixture names; it does not publish shared data.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".focus-area-generation-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("instruction-set-source")).unwrap();
        fs::write(
            root.join("instruction-set-source/guide.md"),
            "# Rule\nKeep it.\n",
        )
        .unwrap();
        Self(root)
    }

    /// Simulates a successful snapshot and restore under the shared lock.
    fn prepare_identity(&self) -> (String, String) {
        let clock = TestClock;
        let _lock = ResourceLock::open(
            &self.0.join("focus-area-cache-operation"),
            LockMode::Exclusive,
            &clock,
        )
        .unwrap();
        invalidate_locked(&self.0, &clock).unwrap();
        invalidate_snapshot_locked(&self.0, &clock).unwrap();
        let snapshot = bind_snapshot_locked(&self.0, &clock).unwrap();
        let identity =
            bind_restore_locked(&self.0, &self.0.join("instruction-set-source"), &clock).unwrap();
        (snapshot, identity)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Produces a complete generation through the production authoring APIs.
fn assemble_generation(session: &Path) {
    let clock = TestClock;
    update(session, Operation::Initialize, &clock).unwrap();
    update(session, Operation::Finalize, &clock).unwrap();
    let span = |root| json!({"root":root,"path":"guide.md","startLine":1,"endLine":2});
    let map = json!({"schemaVersion":1,"categories":[{"id":"rules","title":"Rules"}],
        "chapters":[{"id":"rule","categoryId":"rules","content":span("instruction-set-raw"),
            "sources":[span("instruction-set-source")],"applicability":[{
                "effectiveScope":"No explicit restriction; interpret the guideline in context.",
                "declarations":[],"origins":[],"uncertainties":[]}]}]});
    let path = session.join("focus-area-map.json");
    update_map(
        session,
        &path,
        MapOperation::Append(serde_json::to_vec(&map).unwrap()),
        &clock,
    )
    .unwrap();
    assemble(session, &path, &clock).unwrap();
}

#[test]
fn stale_workers_cannot_publish_after_an_overlapping_generation() {
    let fixture = Fixture::new();
    let (_, identity_a) = fixture.prepare_identity();
    let a = begin(&fixture.0, &identity_a, &TestClock).unwrap();
    assemble_generation(Path::new(&a.session_state));

    // Both versions have the same valid spans, so span validation cannot fence A.
    fs::write(
        fixture.0.join("instruction-set-source/guide.md"),
        "# Rule\nChange it.\n",
    )
    .unwrap();
    let (_, identity_b) = fixture.prepare_identity();
    let b = begin(&fixture.0, &identity_b, &TestClock).unwrap();
    assemble_generation(Path::new(&b.session_state));
    publish(&fixture.0, &b.generation_id, &TestClock).unwrap();
    let before = fs::read(fixture.0.join("focus-areas/focus-rules.md")).unwrap();
    assert!(String::from_utf8_lossy(&before).contains("Change it."));
    let error = publish(&fixture.0, &a.generation_id, &TestClock).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Preparation generation is not active")
    );
    assert_eq!(
        before,
        fs::read(fixture.0.join("focus-areas/focus-rules.md")).unwrap()
    );
    assert!(begin(&fixture.0, &identity_a, &TestClock).is_err());
    {
        let _operation = ResourceLock::open(
            &fixture.0.join("focus-area-cache-operation"),
            LockMode::Shared,
            &TestClock,
        )
        .unwrap();
        let _artifacts = ResourceLock::open(
            &fixture.0.join("focus-areas"),
            LockMode::Exclusive,
            &TestClock,
        )
        .unwrap();
        assert!(verify_ready_locked(&fixture.0, &identity_b, &TestClock).unwrap());
        assert!(!verify_ready_locked(&fixture.0, &identity_a, &TestClock).unwrap());
        fs::write(fixture.0.join("focus-areas/focus-rules.md"), "changed").unwrap();
        assert!(!verify_ready_locked(&fixture.0, &identity_b, &TestClock).unwrap());
        fs::write(fixture.0.join("focus-areas/focus-rules.md"), &before).unwrap();
        assert!(verify_ready_locked(&fixture.0, &identity_b, &TestClock).unwrap());
        assert!(invalidate_ready_locked(&fixture.0, &identity_a, &TestClock).is_err());
        invalidate_ready_locked(&fixture.0, &identity_b, &TestClock).unwrap();
        assert!(!verify_ready_locked(&fixture.0, &identity_b, &TestClock).unwrap());
        assert!(verify_preparation_locked(&fixture.0, &identity_b, &TestClock).unwrap());
    }
}

#[test]
fn publication_rejects_changed_sources_and_unverified_documents() {
    let fixture = Fixture::new();
    let (_, identity) = fixture.prepare_identity();
    let generation = begin(&fixture.0, &identity, &TestClock).unwrap();
    let preparation = Path::new(&generation.session_state);
    assemble_generation(preparation);
    fs::write(
        fixture.0.join("instruction-set-source/guide.md"),
        "# Rule\nChanged.\n",
    )
    .unwrap();
    assert!(publish(&fixture.0, &generation.generation_id, &TestClock).is_err());
    fs::write(
        fixture.0.join("instruction-set-source/guide.md"),
        "# Rule\nKeep it.\n",
    )
    .unwrap();
    fs::write(preparation.join("focus-areas/focus-rules.md"), "unexpected").unwrap();
    assert!(publish(&fixture.0, &generation.generation_id, &TestClock).is_err());
    assert!(!fixture.0.join("focus-areas").exists());
}

#[test]
fn source_and_restore_identities_are_distinct_and_not_reused() {
    let fixture = Fixture::new();
    let (snapshot, first) = fixture.prepare_identity();
    let _lock = ResourceLock::open(
        &fixture.0.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        &TestClock,
    )
    .unwrap();
    assert!(verify_snapshot_locked(&fixture.0, &snapshot, &TestClock).unwrap());
    invalidate_locked(&fixture.0, &TestClock).unwrap();
    let second = bind_restore_locked(
        &fixture.0,
        &fixture.0.join("instruction-set-source"),
        &TestClock,
    )
    .unwrap();
    assert_ne!(first, second);
    assert!(verify_snapshot_locked(&fixture.0, &snapshot, &TestClock).unwrap());
    assert!(!verify_preparation_locked(&fixture.0, &first, &TestClock).unwrap());
    assert!(verify_preparation_locked(&fixture.0, &second, &TestClock).unwrap());
    invalidate_snapshot_locked(&fixture.0, &TestClock).unwrap();
    let next_snapshot = bind_snapshot_locked(&fixture.0, &TestClock).unwrap();
    assert_ne!(snapshot, next_snapshot);
    assert!(!verify_snapshot_locked(&fixture.0, &snapshot, &TestClock).unwrap());
    assert!(
        !validate_published_locked(
            &fixture.0,
            &fixture.0.join("instruction-set-source"),
            "unbound-key",
            &TestClock,
        )
        .unwrap()
    );
}

#[test]
fn delayed_begin_cannot_adopt_an_identical_new_restore() {
    let fixture = Fixture::new();
    let (_, first) = fixture.prepare_identity();
    let (_, second) = fixture.prepare_identity();
    assert_ne!(first, second);
    assert!(begin(&fixture.0, &first, &TestClock).is_err());
    assert!(begin(&fixture.0, &second, &TestClock).is_ok());
}

#[test]
fn restored_identity_does_not_authorize_leftover_artifacts() {
    let fixture = Fixture::new();
    let (_, identity) = fixture.prepare_identity();
    fs::create_dir(fixture.0.join("instruction-set-raw")).unwrap();
    fs::write(fixture.0.join("instruction-set-raw/guide.md"), "old").unwrap();
    fs::create_dir(fixture.0.join("focus-areas")).unwrap();
    fs::write(fixture.0.join("focus-areas/focus-rules.md"), "old").unwrap();
    fs::write(fixture.0.join("instruction-set-categories.md"), "old").unwrap();
    let _operation = ResourceLock::open(
        &fixture.0.join("focus-area-cache-operation"),
        LockMode::Shared,
        &TestClock,
    )
    .unwrap();
    let _artifacts = ResourceLock::open(
        &fixture.0.join("focus-areas"),
        LockMode::Exclusive,
        &TestClock,
    )
    .unwrap();
    assert!(!verify_ready_locked(&fixture.0, &identity, &TestClock).unwrap());
}

#[test]
fn restore_binding_rechecks_the_source_snapshot() {
    let fixture = Fixture::new();
    let (snapshot, _) = fixture.prepare_identity();
    let _lock = ResourceLock::open(
        &fixture.0.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        &TestClock,
    )
    .unwrap();
    assert!(verify_snapshot_locked(&fixture.0, &snapshot, &TestClock).unwrap());
    fs::write(
        fixture.0.join("instruction-set-source/guide.md"),
        "# Rule\nChanged.\n",
    )
    .unwrap();
    assert!(
        bind_restore_locked(
            &fixture.0,
            &fixture.0.join("instruction-set-source"),
            &TestClock,
        )
        .is_err()
    );
}

#[test]
fn beginning_copies_exact_source_bytes_to_both_private_trees() {
    let fixture = Fixture::new();
    let bytes = b"\xef\xbb\xbf# Rule\r\nKeep it.\r\n";
    fs::write(fixture.0.join("instruction-set-source/guide.md"), bytes).unwrap();
    let (_, identity) = fixture.prepare_identity();
    let generation = begin(&fixture.0, &identity, &TestClock).unwrap();
    for root in ["instruction-set-source", "instruction-set-raw"] {
        assert_eq!(
            fs::read(
                Path::new(&generation.session_state)
                    .join(root)
                    .join("guide.md")
            )
            .unwrap(),
            bytes
        );
    }
}

#[test]
fn managed_state_distinguishes_absence_from_invalid_records() {
    let fixture = Fixture::new();
    let _operation = ResourceLock::open(
        &fixture.0.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        &TestClock,
    )
    .unwrap();
    assert!(!is_managed_locked(&fixture.0, &TestClock).unwrap());
    invalidate_locked(&fixture.0, &TestClock).unwrap();
    assert!(is_managed_locked(&fixture.0, &TestClock).unwrap());
    fs::write(fixture.0.join("focus-area-generation.json"), b"invalid").unwrap();
    assert!(is_managed_locked(&fixture.0, &TestClock).is_err());
}
