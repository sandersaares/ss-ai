//! Exercises cache identity, publication, corruption, and diagnostics.
#![allow(
    clippy::assertions_on_result_states,
    reason = "Some tests only need to verify that an operation is rejected"
)]
#![allow(
    clippy::indexing_slicing,
    reason = "The tests construct the record shapes they index"
)]
#![allow(
    clippy::create_dir,
    reason = "Reusing an existing fixture directory must fail"
)]

use std::cell::Cell;
use std::env::consts::EXE_SUFFIX;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use ss_ai_review_workflow::focus_area_expansion::{self, Operation};
use ss_ai_review_workflow::focus_area_generation;
use ss_ai_review_workflow::focus_area_map::{self, MapOperation};
use ss_ai_review_workflow::focus_cache::{
    CacheOptions, CacheResult, check_artifacts, generator_build_fingerprint,
    restore as restore_with_snapshot, store as store_with_identity, validate_artifacts,
};
use ss_ai_review_workflow::instruction_sources::{
    InstructionSourceOptions, RepositoryInstructionSources, snapshot_instruction_sources,
};
use ss_ai_runtime::{Clock, LockMode, ResourceLock};

/// Prevents a fork from inheriting another test's lock before close-on-exec.
static FIXTURE_LOCK: RwLock<()> = RwLock::new(());

/// Keeps fixture state isolated inside the checkout.
struct Fixture {
    root: PathBuf,
    session: PathBuf,
    input: PathBuf,
    skill: PathBuf,
    recipe: PathBuf,
    cache: PathBuf,
    _shared: Option<RwLockReadGuard<'static, ()>>,
    _exclusive: Option<RwLockWriteGuard<'static, ()>>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let shared = FIXTURE_LOCK
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::with_lock(name, Some(shared), None)
    }

    fn new_exclusive(name: &str) -> Self {
        let exclusive = FIXTURE_LOCK
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::with_lock(name, None, Some(exclusive))
    }

    fn with_lock(
        name: &str,
        shared: Option<RwLockReadGuard<'static, ()>>,
        exclusive: Option<RwLockWriteGuard<'static, ()>>,
    ) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".focus-cache-test-{}-{name}", std::process::id()));
        fs::create_dir(&root).unwrap();
        copy_directory(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review"),
            &root.join("skill"),
        );
        fs::create_dir_all(root.join("input/external")).unwrap();
        fs::write(root.join("input/external/rules.md"), "rules").unwrap();
        fs::create_dir_all(root.join("session/instruction-set-raw")).unwrap();
        fs::create_dir_all(root.join("session/instruction-set-source")).unwrap();
        fs::create_dir_all(root.join("session/instruction-set-source/external")).unwrap();
        fs::copy(
            root.join("input/external/rules.md"),
            root.join("session/instruction-set-source/external/rules.md"),
        )
        .unwrap();
        fs::create_dir_all(root.join("session/focus-areas")).unwrap();
        fs::write(root.join("session/instruction-set-raw/raw.md"), "raw").unwrap();
        fs::write(
            root.join("session/focus-area-expansion-status.json"),
            r#"{"schemaVersion":1,"state":"Complete","references":[]}"#,
        )
        .unwrap();
        fs::write(
            root.join("session/instruction-set-categories.md"),
            "categories",
        )
        .unwrap();
        fs::write(root.join("session/focus-areas/all.md"), "all guidance").unwrap();
        Self {
            session: root.join("session"),
            input: root.join("input"),
            skill: root.join("skill"),
            recipe: root.join("skill/prepare-focus-areas.md"),
            cache: root.join("cache"),
            root,
            _shared: shared,
            _exclusive: exclusive,
        }
    }

    fn options(&self) -> CacheOptions<'_> {
        CacheOptions {
            session: &self.session,
            input_directory: &self.input,
            skill_root: &self.skill,
            recipe: &self.recipe,
            cache_directory: &self.cache,
            generator_profile: "test-model",
            freshness_days: 7,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Copies the installed skill so tests can alter generation inputs independently.
fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &destination.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
        }
    }
}

/// Fixes freshness and retries without using real elapsed time.
struct TestClock(i64);

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_second(self.0).unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("this test must not encounter lock contention")
    }
}

/// Captures a source identity through the same locked boundary as source publication.
fn snapshot(session: &Path, clock: &dyn Clock) -> String {
    let session = fs::canonicalize(session).unwrap();
    let _operation = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        clock,
    )
    .unwrap();
    focus_area_generation::invalidate_locked(&session, clock).unwrap();
    focus_area_generation::invalidate_snapshot_locked(&session, clock).unwrap();
    focus_area_generation::bind_snapshot_locked(&session, clock).unwrap()
}

/// Starts a restore with an explicit identity of the current source snapshot.
fn restore(
    options: &CacheOptions<'_>,
    refresh: bool,
    clock: &dyn Clock,
) -> Result<CacheResult, AppError> {
    let identity = snapshot(options.session, clock);
    restore_with_snapshot(options, refresh, &identity, clock)
}

/// Preserves the caller's restore identity when requesting cache storage.
fn store(
    options: &CacheOptions<'_>,
    result: &CacheResult,
    clock: &dyn Clock,
) -> Result<CacheResult, AppError> {
    store_with_identity(
        options,
        result.preparation_identity.as_deref().unwrap(),
        clock,
    )
}

/// Builds an actual complete generation before exercising cache publication.
fn prepare_generation(
    session: &Path,
    result: &CacheResult,
    clock: &dyn Clock,
) -> focus_area_generation::Generation {
    let generation = focus_area_generation::begin(
        session,
        result.preparation_identity.as_deref().unwrap(),
        clock,
    )
    .unwrap();
    let staged = Path::new(&generation.session_state);
    focus_area_expansion::update(staged, Operation::Initialize, clock).unwrap();
    focus_area_expansion::update(staged, Operation::Finalize, clock).unwrap();
    let span = |root| serde_json::json!({"root": root, "path": "external/rules.md", "startLine": 1, "endLine": 1});
    let map = serde_json::json!({
        "schemaVersion": 1,
        "categories": [{"id": "all", "title": "All"}],
        "chapters": [{
            "id": "rule", "categoryId": "all",
            "content": span("instruction-set-raw"),
            "sources": [span("instruction-set-source")],
            "applicability": [{
                "effectiveScope": "No explicit restriction; interpret the guideline in context.",
                "declarations": [], "origins": [], "uncertainties": []
            }]
        }]
    });
    let path = staged.join("focus-area-map.json");
    focus_area_map::update(
        staged,
        &path,
        MapOperation::Append(serde_json::to_vec(&map).unwrap()),
        clock,
    )
    .unwrap();
    focus_area_map::assemble(staged, &path, clock).unwrap();
    generation
}

/// Publishes preparation through its real receipt and artifact transaction.
fn publish_generation(session: &Path, result: &CacheResult, clock: &dyn Clock) {
    let generation = prepare_generation(session, result, clock);
    focus_area_generation::publish(session, &generation.generation_id, clock).unwrap();
}

/// Coordinates a real lock conflict without sleeping.
struct ContendedClock {
    events: Sender<bool>,
    release: Receiver<()>,
    reported: Cell<bool>,
}

impl Clock for ContendedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_second(1_800_000_000).unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        assert!(
            !self.reported.replace(true),
            "unexpected repeated contention"
        );
        self.events.send(true).unwrap();
        self.release.recv().unwrap();
        Ok(())
    }
}

#[test]
fn complete_store_restore_and_diagnostics_are_consistent() {
    let fixture = Fixture::new("roundtrip");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    assert_eq!(first.status, "Miss");
    assert_eq!(first.reason, Some("NotFound"));
    assert_eq!(first.user_instruction_file_count, Some(1));
    publish_generation(options.session, &first, &clock);
    let saved = store(&options, &first, &clock).unwrap();
    assert_eq!(saved.status, "Stored");
    fs::remove_file(options.session.join("focus-area-expansion-status.json")).unwrap();
    fs::remove_file(options.session.join("focus-area-map.json")).unwrap();
    fs::write(options.session.join("focus-areas/old.md"), "old").unwrap();
    let restored = restore(&options, false, &clock).unwrap();
    assert_eq!(restored.status, "Hit");
    assert_eq!(restored.focus_area_count, 1);
    assert!(!options.session.join("focus-areas/old.md").exists());
    assert_eq!(
        store(&options, &restored, &clock).unwrap().reason,
        Some("GenerationNotPublished")
    );
    let diagnostics: serde_json::Value = serde_json::from_slice(
        &fs::read(options.session.join("focus-area-cache-diagnostics.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(diagnostics["cache"], "Hit");
    assert_eq!(diagnostics["userInstructionFileCount"], 1);
    assert_eq!(
        check_artifacts(options.session)
            .unwrap()
            .artifact_file_count,
        3
    );
    assert_eq!(
        validate_artifacts(
            options.session,
            restored.preparation_identity.as_deref().unwrap(),
            &clock
        )
        .unwrap()
        .focus_area_count,
        1
    );
    let focus_file = options.session.join("focus-areas/focus-all.md");
    let focus_bytes = fs::read(&focus_file).unwrap();
    fs::remove_file(&focus_file).unwrap();
    assert!(
        validate_artifacts(
            options.session,
            restored.preparation_identity.as_deref().unwrap(),
            &clock
        )
        .is_err()
    );
    let diagnostics: serde_json::Value = serde_json::from_slice(
        &fs::read(options.session.join("focus-area-cache-diagnostics.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(diagnostics["cache"], "Miss");
    assert_eq!(diagnostics["reason"], "ValidationFailed");
    assert_eq!(diagnostics["userInstructionFileCount"], 1);
    fs::write(&focus_file, focus_bytes).unwrap();
    assert!(
        validate_artifacts(
            options.session,
            restored.preparation_identity.as_deref().unwrap(),
            &clock
        )
        .is_err()
    );
    assert!(
        focus_area_generation::begin(
            options.session,
            restored.preparation_identity.as_deref().unwrap(),
            &clock
        )
        .is_ok()
    );
}

#[test]
fn selected_repository_origin_is_bound_across_sessions() {
    assert_source_cache_origin(false, false);
}

#[cfg(any(unix, windows))]
#[test]
fn selected_repository_alias_retargeting_cannot_reuse_cache() {
    assert_source_cache_origin(true, false);
}

#[cfg(any(unix, windows))]
#[test]
fn configured_root_alias_retargeting_cannot_reuse_cache() {
    assert_source_cache_origin(true, true);
}

/// Exercises stored linked guidance using different source origins.
fn assert_source_cache_origin(use_alias: bool, external: bool) {
    let fixture = Fixture::new(match (use_alias, external) {
        (true, true) => "external-alias-origin",
        (true, false) => "repository-alias-origin",
        _ => "repository-origin",
    });
    let clock = TestClock(1_800_000_000);
    let paths = [PathBuf::from("AGENTS.md")];
    let repo_a = fixture.root.join("repo-a");
    let repo_b = fixture.root.join("repo-b");
    for (repository, guidance) in [(&repo_a, "Guidance A\n"), (&repo_b, "Guidance B\n")] {
        fs::create_dir_all(repository.join("docs")).unwrap();
        fs::write(repository.join("AGENTS.md"), "See docs/rules.md\n").unwrap();
        fs::write(repository.join("docs/rules.md"), guidance).unwrap();
    }
    let alias = fixture.root.join("repository-alias");
    #[cfg(any(unix, windows))]
    if use_alias {
        link_directory(&repo_a, &alias);
    }
    let first_root = if use_alias { &alias } else { &repo_a };

    let snapshot_for = |session: &Path, repository: &Path| {
        fs::create_dir_all(session).unwrap();
        snapshot_instruction_sources(
            &InstructionSourceOptions {
                session_state: session,
                skill_root: &fixture.skill,
                directory_list: external.then(|| repository.to_str().unwrap()),
                base_directory: &fixture.root,
                repository_sources: (!external).then_some(RepositoryInstructionSources {
                    root: repository,
                    paths: &paths,
                }),
            },
            &clock,
        )
        .unwrap()
    };
    let session_a = fixture.root.join("session-a");
    let first_snapshot = snapshot_for(&session_a, first_root);
    let mapping = if external {
        &first_snapshot.roots[0]
    } else {
        first_snapshot.repository_root.as_ref().unwrap()
    };
    assert_eq!(mapping.path, first_root.to_str().unwrap());
    let options_for = |session, input_directory| CacheOptions {
        session,
        input_directory,
        skill_root: &fixture.skill,
        recipe: &fixture.recipe,
        cache_directory: &fixture.cache,
        generator_profile: "test-model",
        freshness_days: 7,
    };
    let input_a = session_a.join("instruction-set-source");
    let options_a = options_for(&session_a, &input_a);
    let first = restore_with_snapshot(&options_a, false, &first_snapshot.snapshot_identity, &clock)
        .unwrap();
    assert_eq!(first.status, "Miss");
    let generation = focus_area_generation::begin(
        &session_a,
        first.preparation_identity.as_deref().unwrap(),
        &clock,
    )
    .unwrap();
    let staged = Path::new(&generation.session_state);
    let source_path = format!("{}/AGENTS.md", mapping.prefix);
    let uri = format!("file://{}", first_root.join("docs/rules.md").display());
    let span = |root: &str, path: &str| serde_json::json!({"root":root,"path":path,"startLine":1,"endLine":1});
    fs::copy(
        repo_a.join("docs/rules.md"),
        staged.join("instruction-set-raw/linked.md"),
    )
    .unwrap();
    focus_area_expansion::update(staged, Operation::Initialize, &clock).unwrap();
    let reference = serde_json::from_value(serde_json::json!({
        "id":"linked", "uri":uri,
        "referrers":[{"parentId":null,"source":span("instruction-set-source",&source_path),
            "scope":"all","declarations":[]}]
    }))
    .unwrap();
    focus_area_expansion::update(staged, Operation::Register(reference), &clock).unwrap();
    focus_area_expansion::update(
        staged,
        Operation::Complete(focus_area_expansion::ReferenceResult {
            reference_id: "linked".into(),
            outputs: vec!["linked.md".into()],
            error: None,
        }),
        &clock,
    )
    .unwrap();
    focus_area_expansion::update(staged, Operation::Finalize, &clock).unwrap();
    let skill_lines = fs::read_to_string(staged.join("instruction-set-raw/skill/focus-areas.md"))
        .unwrap()
        .lines()
        .count();
    let skill_span = |root| serde_json::json!({"root":root,"path":"skill/focus-areas.md","startLine":1,"endLine":skill_lines});
    let direct_scope = serde_json::json!({
        "effectiveScope":"No explicit restriction; interpret the guideline in context.",
        "origins":[],"declarations":[],"uncertainties":[]
    });
    let map = serde_json::json!({
        "schemaVersion":1,"categories":[{"id":"all","title":"All"}],
        "chapters":[{"id":"skill","categoryId":"all",
            "content":skill_span("instruction-set-raw"),
            "sources":[skill_span("instruction-set-source")],
            "applicability":[direct_scope]},
            {"id":"direct","categoryId":"all",
            "content":span("instruction-set-raw",&source_path),
            "sources":[span("instruction-set-source",&source_path)],
            "applicability":[direct_scope]},
            {"id":"rule","categoryId":"all",
            "content":span("instruction-set-raw","linked.md"),
            "sources":[span("instruction-set-raw","linked.md")],
            "applicability":[{"effectiveScope":"all","origins":[uri],
                "declarations":[],"uncertainties":[],"pathScope":null}]}]
    });
    let map_path = staged.join("focus-area-map.json");
    focus_area_map::update(
        staged,
        &map_path,
        MapOperation::Append(serde_json::to_vec(&map).unwrap()),
        &clock,
    )
    .unwrap();
    focus_area_map::assemble(staged, &map_path, &clock).unwrap();
    focus_area_generation::publish(&session_a, &generation.generation_id, &clock).unwrap();
    assert_eq!(store(&options_a, &first, &clock).unwrap().status, "Stored");

    let session_same = fixture.root.join("session-same");
    let same_snapshot = snapshot_for(&session_same, first_root);
    let input_same = session_same.join("instruction-set-source");
    let same = restore_with_snapshot(
        &options_for(&session_same, &input_same),
        false,
        &same_snapshot.snapshot_identity,
        &clock,
    )
    .unwrap();
    assert_eq!(same.status, "Hit");
    assert_eq!(same.key, first.key);
    assert!(
        fs::read_to_string(session_same.join("focus-areas/focus-all.md"))
            .unwrap()
            .contains("Guidance A")
    );

    let session_direct = fixture.root.join("session-direct");
    let input_direct = session_direct.join("instruction-set-source");
    if use_alias {
        let direct_snapshot = snapshot_for(&session_direct, &repo_a);
        let direct = restore_with_snapshot(
            &options_for(&session_direct, &input_direct),
            false,
            &direct_snapshot.snapshot_identity,
            &clock,
        )
        .unwrap();
        assert_eq!(direct.status, "Miss");
        assert_ne!(direct.key, first.key);
    }

    #[cfg(any(unix, windows))]
    if use_alias {
        remove_directory_link(&alias);
        link_directory(&repo_b, &alias);
    }
    let second_root = if use_alias { &alias } else { &repo_b };
    let session_b = fixture.root.join("session-b");
    let second_snapshot = snapshot_for(&session_b, second_root);
    let mapping = if external {
        &second_snapshot.roots[0]
    } else {
        second_snapshot.repository_root.as_ref().unwrap()
    };
    assert_eq!(mapping.path, second_root.to_str().unwrap());
    let input_b = session_b.join("instruction-set-source");
    let second = restore_with_snapshot(
        &options_for(&session_b, &input_b),
        false,
        &second_snapshot.snapshot_identity,
        &clock,
    )
    .unwrap();
    assert_eq!(second.status, "Miss");
    assert_ne!(second.key, first.key);
    assert!(!session_b.join("focus-areas/focus-all.md").exists());
    #[cfg(any(unix, windows))]
    if use_alias {
        remove_directory_link(&alias);
    }
}

/// Creates directory links without requiring Windows symbolic-link privileges.
#[cfg(any(unix, windows))]
fn link_directory(target: &Path, link: &Path) {
    #[cfg(unix)]
    symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        // Environment expansion keeps path characters inside quoted arguments.
        let output = Command::new("cmd")
            .args(["/D", "/S", "/V:OFF", "/C"])
            .env("SS_AI_TEST_LINK", link)
            .env("SS_AI_TEST_TARGET", target)
            .raw_arg("\"mklink /J \"%SS_AI_TEST_LINK%\" \"%SS_AI_TEST_TARGET%\"\"")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Cannot create junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Removes only the link so its target remains available to the fixture.
#[cfg(any(unix, windows))]
fn remove_directory_link(link: &Path) {
    #[cfg(unix)]
    fs::remove_file(link).unwrap();
    #[cfg(windows)]
    fs::remove_dir(link).unwrap();
}

#[test]
fn cache_miss_cannot_validate_an_older_artifact_set() {
    let fixture = Fixture::new("miss-not-ready");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let result = restore(&options, false, &clock).unwrap();
    assert_eq!(result.status, "Miss");
    assert!(
        validate_artifacts(
            options.session,
            result.preparation_identity.as_deref().unwrap(),
            &clock
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(options.session.join("focus-areas/all.md")).unwrap(),
        "all guidance"
    );
    publish_generation(options.session, &result, &clock);
    assert!(
        validate_artifacts(
            options.session,
            result.preparation_identity.as_deref().unwrap(),
            &clock
        )
        .is_ok()
    );
}

#[test]
fn stale_generation_cannot_publish_or_store_under_a_new_snapshot() {
    let fixture = Fixture::new("stale-generation");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    let generation_a = prepare_generation(options.session, &first, &clock);
    focus_area_generation::publish(options.session, &generation_a.generation_id, &clock).unwrap();
    assert_eq!(store(&options, &first, &clock).unwrap().status, "Stored");
    fs::write(options.input_directory.join("external/rules.md"), "new").unwrap();
    fs::write(
        options
            .session
            .join("instruction-set-source/external/rules.md"),
        "new",
    )
    .unwrap();
    let second = restore(&options, false, &clock).unwrap();
    let second_key = second.key.clone().unwrap();
    assert_ne!(first.key, second.key);
    let diagnostics = fs::read(options.session.join("focus-area-cache-diagnostics.json")).unwrap();
    assert!(
        validate_artifacts(
            options.session,
            first.preparation_identity.as_deref().unwrap(),
            &clock
        )
        .is_err()
    );
    assert_eq!(
        fs::read(options.session.join("focus-area-cache-diagnostics.json")).unwrap(),
        diagnostics
    );
    assert!(
        focus_area_generation::begin(
            options.session,
            first.preparation_identity.as_deref().unwrap(),
            &clock
        )
        .is_err()
    );
    assert!(
        focus_area_generation::publish(options.session, &generation_a.generation_id, &clock)
            .is_err()
    );
    assert!(store(&options, &first, &clock).is_err());
    assert_eq!(
        fs::read(options.session.join("focus-area-cache-diagnostics.json")).unwrap(),
        diagnostics
    );
    assert_eq!(
        store(&options, &second, &clock).unwrap().reason,
        Some("GenerationNotPublished")
    );
    assert!(!options.cache_directory.join(&second_key).exists());
    publish_generation(options.session, &second, &clock);
    let diagnostics = fs::read(options.session.join("focus-area-cache-diagnostics.json")).unwrap();
    assert!(store(&options, &first, &clock).is_err());
    assert_eq!(
        fs::read(options.session.join("focus-area-cache-diagnostics.json")).unwrap(),
        diagnostics
    );
    assert_eq!(store(&options, &second, &clock).unwrap().status, "Stored");
    assert_eq!(restore(&options, false, &clock).unwrap().status, "Hit");
}

#[test]
fn stale_snapshot_restore_cannot_adopt_or_leave_a_preparation_active() {
    let fixture = Fixture::new("stale-snapshot");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let old_snapshot = snapshot(options.session, &clock);
    let current_snapshot = snapshot(options.session, &clock);
    assert_ne!(old_snapshot, current_snapshot);
    let current = restore_with_snapshot(&options, false, &current_snapshot, &clock).unwrap();
    let refreshed = restore_with_snapshot(&options, true, &current_snapshot, &clock).unwrap();
    assert_eq!(current.key, refreshed.key);
    assert_ne!(current.preparation_identity, refreshed.preparation_identity);
    assert!(
        focus_area_generation::begin(
            options.session,
            current.preparation_identity.as_deref().unwrap(),
            &clock
        )
        .is_err()
    );
    assert!(restore_with_snapshot(&options, false, &old_snapshot, &clock).is_err());
    assert!(
        focus_area_generation::begin(
            options.session,
            refreshed.preparation_identity.as_deref().unwrap(),
            &clock
        )
        .is_err()
    );
    assert!(store(&options, &refreshed, &clock).is_err());
}

#[test]
fn staged_and_published_artifact_tampering_cannot_authorize_storage() {
    let fixture = Fixture::new("receipt-tampering");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    let key = first.key.clone().unwrap();
    let staged = prepare_generation(options.session, &first, &clock);
    fs::write(
        Path::new(&staged.session_state).join("focus-areas/focus-all.md"),
        "tampered",
    )
    .unwrap();
    assert!(
        focus_area_generation::publish(options.session, &staged.generation_id, &clock).is_err()
    );
    assert_eq!(
        store(&options, &first, &clock).unwrap().reason,
        Some("GenerationNotPublished")
    );
    assert!(!options.cache_directory.join(&key).exists());

    let next = restore(&options, false, &clock).unwrap();
    publish_generation(options.session, &next, &clock);
    fs::write(options.session.join("focus-areas/focus-all.md"), "tampered").unwrap();
    assert_eq!(
        store(&options, &next, &clock).unwrap().reason,
        Some("GenerationNotPublished")
    );
    assert!(!options.cache_directory.join(&key).exists());
}

#[test]
fn each_published_artifact_class_is_bound_to_the_receipt() {
    let clock = TestClock(1_800_000_000);
    for (name, path) in [
        ("raw", "instruction-set-raw/external/rules.md"),
        ("map", "focus-area-map.json"),
        ("catalog", "instruction-set-categories.md"),
        ("focus", "focus-areas/focus-all.md"),
    ] {
        let fixture = Fixture::new(&format!("receipt-{name}"));
        let options = fixture.options();
        let result = restore(&options, false, &clock).unwrap();
        let key = result.key.clone().unwrap();
        publish_generation(options.session, &result, &clock);
        fs::write(options.session.join(path), "tampered").unwrap();
        assert_eq!(
            store(&options, &result, &clock).unwrap().reason,
            Some("GenerationNotPublished"),
            "{path}"
        );
        assert!(!options.cache_directory.join(key).exists(), "{path}");
    }
}

#[test]
fn identity_changes_with_sources_generation_profile_and_freshness() {
    let fixture = Fixture::new("identity");
    let mut options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let get_key = |options: &CacheOptions<'_>| {
        let source = options.session.join("instruction-set-source");
        fs::remove_dir_all(&source).unwrap();
        copy_directory(options.input_directory, &source);
        restore(options, true, &clock).unwrap().key.unwrap()
    };
    let initial = get_key(&options);
    fs::write(options.input_directory.join("external/rules.md"), "changed").unwrap();
    let changed = get_key(&options);
    assert_ne!(initial, changed);
    fs::rename(
        options.input_directory.join("external/rules.md"),
        options.input_directory.join("external/renamed.md"),
    )
    .unwrap();
    let renamed = get_key(&options);
    assert_ne!(changed, renamed);
    fs::write(options.skill_root.join("focus-area-format.md"), "changed").unwrap();
    let changed_skill = get_key(&options);
    assert_ne!(renamed, changed_skill);
    options.generator_profile = "different-model";
    assert_ne!(changed_skill, get_key(&options));
    let changed_model = get_key(&options);
    options.freshness_days = 8;
    assert_ne!(changed_model, get_key(&options));
}

#[test]
fn counts_only_files_under_external_including_nested_files() {
    let fixture = Fixture::new("external-count");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let external = options.input_directory.join("external");
    fs::remove_dir_all(&external).unwrap();
    fs::create_dir_all(options.input_directory.join("external-other")).unwrap();
    fs::write(
        options.input_directory.join("external-other/rules.md"),
        "other",
    )
    .unwrap();
    fs::write(options.input_directory.join("external.md"), "other").unwrap();

    for (expected, populate) in [(0, false), (0, true), (2, true)] {
        if populate {
            fs::create_dir_all(external.join("nested")).unwrap();
            if expected != 0 {
                fs::write(external.join("first.md"), "first").unwrap();
                fs::write(external.join("nested/second.md"), "second").unwrap();
            }
        }
        let source = options.session.join("instruction-set-source");
        fs::remove_dir_all(&source).unwrap();
        copy_directory(options.input_directory, &source);
        let result = restore(&options, true, &clock).unwrap();
        assert_eq!(result.user_instruction_file_count, Some(expected));
        let diagnostics: serde_json::Value = serde_json::from_slice(
            &fs::read(options.session.join("focus-area-cache-diagnostics.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(diagnostics["userInstructionFileCount"], expected);
    }
}

#[test]
fn corrupted_payload_and_failed_validation_never_replace_session() {
    let fixture = Fixture::new("corruption");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    let key = first.key.clone().unwrap();
    publish_generation(options.session, &first, &clock);
    store(&options, &first, &clock).unwrap();
    let entry = options.cache_directory.join(&key);
    let original = fs::read(options.session.join("focus-areas/focus-all.md")).unwrap();
    fs::write(entry.join("payload/focus-areas/focus-all.md"), "tampered").unwrap();
    let failed = restore(&options, false, &clock).unwrap();
    assert_eq!(failed.reason, Some("InvalidEntry"));
    assert_eq!(
        fs::read(options.session.join("focus-areas/focus-all.md")).unwrap(),
        original
    );
    publish_generation(options.session, &failed, &clock);
    store(&options, &failed, &clock).unwrap();
    fs::write(entry.join("payload/focus-areas/focus-all.md"), "   ").unwrap();
    let result = restore(&options, false, &clock).unwrap();
    assert_eq!(result.reason, Some("InvalidEntry"));
    assert_eq!(
        fs::read(options.session.join("focus-areas/focus-all.md")).unwrap(),
        original
    );
}

#[test]
fn degraded_incomplete_and_changed_snapshots_are_not_stored() {
    let fixture = Fixture::new("not-stored");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    let key = first.key.clone().unwrap();
    let expansion = options.session.join("focus-area-expansion-status.json");
    fs::write(&expansion, serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 1, "state": "Degraded", "references": [{
            "id": "ref", "uri": "https://example.invalid/ref", "state": "Failed",
            "outputs": [], "error": "unavailable", "referrers": [{
                "parentId": null,
                "source": {"root": "instruction-set-source", "path": "external/rules.md", "startLine": 1, "endLine": 1},
                "scope": "all", "declarations": []
            }]
        }]
    })).unwrap()).unwrap();
    let degraded = store(&options, &first, &clock).unwrap();
    assert_eq!(degraded.reason, Some("DegradedInputs"));
    assert!(!options.cache_directory.join(&key).exists());
    fs::write(
        &expansion,
        r#"{"schemaVersion":1,"state":"Unknown","references":[]}"#,
    )
    .unwrap();
    assert!(store(&options, &first, &clock).is_err());
    fs::write(
        &expansion,
        r#"{"schemaVersion":1,"state":"Complete","references":[]}"#,
    )
    .unwrap();
    fs::write(
        options.input_directory.join("external/rules.md"),
        "different",
    )
    .unwrap();
    let changed = store(&options, &first, &clock).unwrap();
    assert_eq!(changed.reason, Some("InputsChanged"));
    assert!(!options.cache_directory.join(&key).exists());
}

#[test]
fn invalid_cached_artifacts_leave_original_artifacts_untouched() {
    let fixture = Fixture::new("rollback");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    let key = first.key.clone().unwrap();
    publish_generation(options.session, &first, &clock);
    store(&options, &first, &clock).unwrap();
    fs::write(options.session.join("focus-areas/focus-all.md"), "original").unwrap();
    fs::write(
        options.session.join("instruction-set-categories.md"),
        "original categories",
    )
    .unwrap();
    fs::remove_file(
        options
            .cache_directory
            .join(key)
            .join("payload/instruction-set-categories.md"),
    )
    .unwrap();
    let result = restore(&options, false, &clock).unwrap();
    assert_eq!(result.status, "Miss");
    assert_eq!(result.reason, Some("InvalidEntry"));
    assert_eq!(
        fs::read_to_string(options.session.join("focus-areas/focus-all.md")).unwrap(),
        "original"
    );
    assert_eq!(
        fs::read_to_string(options.session.join("instruction-set-categories.md")).unwrap(),
        "original categories"
    );
}

#[test]
fn maintenance_skips_locked_entries_and_preserves_unrelated_paths() {
    let fixture = Fixture::new("maintenance");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    publish_generation(options.session, &first, &clock);
    let other = "a".repeat(64);
    fs::create_dir(options.cache_directory.join(&other)).unwrap();
    fs::create_dir(options.cache_directory.join("unrelated")).unwrap();
    let lock = ResourceLock::open(
        &options.cache_directory.join(".locks").join(&other),
        LockMode::Exclusive,
        &clock,
    )
    .unwrap();
    store(&options, &first, &clock).unwrap();
    assert!(options.cache_directory.join(&other).exists());
    drop(lock);
    assert_eq!(store(&options, &first, &clock).unwrap().status, "Existing");
    assert!(!options.cache_directory.join(&other).exists());
    assert!(options.cache_directory.join("unrelated").exists());
    assert!(
        options
            .cache_directory
            .join(".locks")
            .join(format!("{other}.lock"))
            .exists()
    );
}

#[test]
fn maintenance_counts_the_locked_current_entry_even_when_it_is_oldest() {
    let fixture = Fixture::new("maintenance-current-limit");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    let key = first.key.clone().unwrap();
    publish_generation(options.session, &first, &clock);
    assert_eq!(store(&options, &first, &clock).unwrap().status, "Stored");
    let manifest = fs::read(options.cache_directory.join(&key).join("manifest.json")).unwrap();
    let template: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    // Populate the retention limit with newer entries so the protected entry
    // consumes a slot even when it would otherwise fall outside the limit.
    for index in 1..=64 {
        let other = format!("{index:064x}");
        assert_ne!(other, key);
        let entry = options.cache_directory.join(&other);
        fs::create_dir(&entry).unwrap();
        let mut record = template.clone();
        record["key"] = other.into();
        record["createdAtUnix"] = (clock.0 + index).into();
        fs::write(
            entry.join("manifest.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(store(&options, &first, &clock).unwrap().status, "Existing");
    assert!(options.cache_directory.join(&key).exists());
    assert!(!options.cache_directory.join(format!("{:064x}", 1)).exists());
    assert!(
        options
            .cache_directory
            .join(format!("{:064x}", 64))
            .exists()
    );
}

#[test]
fn restore_waits_for_a_busy_key_without_disturbing_its_lock_file() {
    let fixture = Fixture::new("contention");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    let key = first.key.clone().unwrap();
    publish_generation(options.session, &first, &clock);
    store(&options, &first, &clock).unwrap();
    let key_path = options.cache_directory.join(".locks").join(&key);
    let lock = ResourceLock::open(&key_path, LockMode::Exclusive, &clock).unwrap();
    thread::scope(|scope| {
        let (events_tx, events_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = scope.spawn(move || {
            let clock = ContendedClock {
                events: events_tx.clone(),
                release: release_rx,
                reported: Cell::new(false),
            };
            let result = restore(&options, false, &clock);
            events_tx.send(false).unwrap();
            result
        });
        let waited = events_rx.recv().unwrap();
        drop(lock);
        if waited {
            release_tx.send(()).unwrap();
        }
        assert!(waited, "restore completed without observing the held lock");
        assert_eq!(worker.join().unwrap().unwrap().status, "Hit");
    });
    assert!(key_path.with_extension("lock").exists());
}

#[test]
fn store_holds_operation_lock_while_waiting_for_a_busy_key() {
    let fixture = Fixture::new("store-contention");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    publish_generation(options.session, &first, &clock);
    let key_path = options
        .cache_directory
        .join(".locks")
        .join(first.key.as_ref().unwrap());
    let key_lock = ResourceLock::open(&key_path, LockMode::Exclusive, &clock).unwrap();
    let operation_path = options.session.join("focus-area-cache-operation");
    thread::scope(|scope| {
        let (events_tx, events_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = scope.spawn(move || {
            let clock = ContendedClock {
                events: events_tx.clone(),
                release: release_rx,
                reported: Cell::new(false),
            };
            let result = store(&options, &first, &clock);
            events_tx.send(false).unwrap();
            result
        });
        let waited = events_rx.recv().unwrap();
        let operation_held = ResourceLock::try_open(&operation_path, LockMode::Exclusive)
            .unwrap()
            .is_none();
        drop(key_lock);
        if waited {
            release_tx.send(()).unwrap();
        }
        let result = worker.join().unwrap();
        assert!(
            waited,
            "store completed without observing the held key lock"
        );
        assert!(
            operation_held,
            "store must retain the operation lock while waiting"
        );
        assert_eq!(result.unwrap().status, "Stored");
    });
}

#[test]
#[cfg(unix)]
fn configured_aliases_work_and_payload_links_cannot_escape_cache() {
    let fixture = Fixture::new("aliases");
    let clock = TestClock(1_800_000_000);
    fs::create_dir(&fixture.cache).unwrap();
    let aliases = fixture.root.join("aliases");
    fs::create_dir(&aliases).unwrap();
    for (name, target) in [
        ("session", &fixture.session),
        ("input", &fixture.input),
        ("skill", &fixture.skill),
        ("cache", &fixture.cache),
    ] {
        symlink(target, aliases.join(name)).unwrap();
    }
    let session = aliases.join("session");
    let input = aliases.join("input");
    let skill = aliases.join("skill");
    let recipe = skill.join("prepare-focus-areas.md");
    let cache = aliases.join("cache");
    let options = CacheOptions {
        session: &session,
        input_directory: &input,
        skill_root: &skill,
        recipe: &recipe,
        cache_directory: &cache,
        generator_profile: "test-model",
        freshness_days: 7,
    };
    let first = restore(&options, false, &clock).unwrap();
    let key = first.key.clone().unwrap();
    publish_generation(options.session, &first, &clock);
    assert_eq!(store(&options, &first, &clock).unwrap().status, "Stored");
    let original = fs::read(fixture.session.join("focus-areas/focus-all.md")).unwrap();
    let focus = fixture.cache.join(&key).join("payload/focus-areas");
    fs::remove_dir_all(&focus).unwrap();
    symlink(fixture.root.join("session/focus-areas"), &focus).unwrap();
    assert_eq!(
        restore(&options, false, &clock).unwrap().reason,
        Some("InvalidEntry")
    );
    assert_eq!(
        fs::read(fixture.session.join("focus-areas/focus-all.md")).unwrap(),
        original
    );
}

#[test]
fn unavailable_restore_records_snapshot_for_later_save() {
    let fixture = Fixture::new("unavailable-restore");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    fs::write(&fixture.cache, "not a directory").unwrap();
    let result = restore(&options, false, &clock).unwrap();
    assert_eq!(result.reason, Some("CacheUnavailable"));
    assert!(result.key.is_some());
    assert!(!result.warnings.is_empty());
    publish_generation(options.session, &result, &clock);
    let unavailable = store(&options, &result, &clock).unwrap();
    assert_eq!(unavailable.status, "Bypassed");
    assert_eq!(unavailable.reason, Some("CacheUnavailable"));
    fs::remove_file(&fixture.cache).unwrap();
    assert_eq!(store(&options, &result, &clock).unwrap().status, "Stored");
    assert_eq!(restore(&options, false, &clock).unwrap().status, "Hit");
}

#[test]
fn missing_snapshot_bypasses_and_malformed_inputs_are_errors() {
    let fixture = Fixture::new("missing-snapshot");
    let mut options = fixture.options();
    let clock = TestClock(1_800_000_000);
    assert!(store_with_identity(&options, "not-a-generation", &clock).is_err());
    let first = restore(&options, false, &clock).unwrap();
    fs::remove_file(options.session.join("focus-area-cache-identity.json")).unwrap();
    assert_eq!(
        store(&options, &first, &clock).unwrap().reason,
        Some("SnapshotUnavailable")
    );
    let second = restore(&options, false, &clock).unwrap();
    options.generator_profile = "";
    assert!(restore(&options, false, &clock).is_err());
    options.generator_profile = "test-model";
    assert!(store(&options, &second, &clock).is_err());
    fs::remove_file(options.input_directory.join("external/rules.md")).unwrap();
    assert!(restore(&options, false, &clock).is_err());
    fs::write(options.input_directory.join("external/rules.md"), "rules").unwrap();
    let current = restore(&options, false, &clock).unwrap();
    fs::write(
        options.session.join("focus-area-cache-identity.json"),
        r#"{"schemaVersion":2}"#,
    )
    .unwrap();
    assert!(store(&options, &current, &clock).is_err());
}

#[test]
fn maintenance_failure_is_recorded_without_invalidating_store() {
    let fixture = Fixture::new("maintenance-warning");
    let options = fixture.options();
    let clock = TestClock(1_800_000_000);
    let first = restore(&options, false, &clock).unwrap();
    publish_generation(options.session, &first, &clock);
    fs::create_dir(options.cache_directory.join(".locks/maintenance.lock")).unwrap();
    let result = store(&options, &first, &clock).unwrap();
    assert_eq!(result.status, "Stored");
    assert!(!result.warnings.is_empty());
    let diagnostics: serde_json::Value = serde_json::from_slice(
        &fs::read(options.session.join("focus-area-cache-diagnostics.json")).unwrap(),
    )
    .unwrap();
    assert!(!diagnostics["storeWarnings"].as_array().unwrap().is_empty());
}

/// Reconstructs the build selection independently to check the embedded identity.
fn fingerprint_from_sources(root: &Path, changed_path: Option<&str>) -> String {
    let mut paths: Vec<String> = [
        "Cargo.toml",
        "Cargo.lock",
        "packages/ss-ai/Cargo.toml",
        "packages/ss-ai-review-workflow/Cargo.toml",
        "packages/ss-ai-review-workflow/build.rs",
        "packages/ss-ai-review-context/Cargo.toml",
        "packages/ss-ai-runtime/Cargo.toml",
        "packages/ss-ai-git/Cargo.toml",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for path in [
        "packages/ss-ai/src",
        "packages/ss-ai-review-workflow/src",
        "packages/ss-ai-review-context/src",
        "packages/ss-ai-runtime/src",
        "packages/ss-ai-git/src",
    ] {
        collect_build_sources(root, Path::new(path), "rs", &mut paths);
    }
    collect_build_sources(
        root,
        Path::new("skills/ss-review/schemas"),
        "json",
        &mut paths,
    );
    paths.sort();
    let mut input = Vec::new();
    for value in [
        "ss-ai-focus-generation-build-v1",
        env!("SS_AI_FOCUS_BUILD_TARGET"),
        env!("SS_AI_FOCUS_BUILD_PROFILE"),
    ] {
        append_fingerprint_field(&mut input, value.as_bytes());
    }
    for path in paths {
        append_fingerprint_field(&mut input, path.as_bytes());
        let mut bytes = fs::read(root.join(&path)).unwrap();
        if changed_path == Some(path.as_str()) {
            bytes.push(b'\n');
        }
        append_fingerprint_field(&mut input, &bytes);
    }
    ss_ai_review_context::bytes_hash(&input).to_ascii_lowercase()
}

/// Selects only source and schema files, matching the build contract.
fn collect_build_sources(root: &Path, relative: &Path, extension: &str, paths: &mut Vec<String>) {
    for entry in fs::read_dir(root.join(relative)).unwrap() {
        let entry = entry.unwrap();
        let path = relative.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            collect_build_sources(root, &path, extension, paths);
        } else if kind.is_file() && path.extension().is_some_and(|value| value == extension) {
            paths.push(path.to_str().unwrap().replace('\\', "/"));
        }
    }
}

/// Preserves exact record boundaries in the digest input.
fn append_fingerprint_field(input: &mut Vec<u8>, value: &[u8]) {
    input.extend_from_slice(&(value.len() as u64).to_le_bytes());
    input.extend_from_slice(value);
}

#[test]
fn build_fingerprint_changes_with_source_bytes_not_checkout_path() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = package.parent().unwrap().parent().unwrap();
    let original = fingerprint_from_sources(root, None);
    assert_eq!(generator_build_fingerprint(), original);
    assert_eq!(
        fingerprint_from_sources(&root.join("packages/.."), None),
        original
    );
    let changed = fingerprint_from_sources(
        root,
        Some("packages/ss-ai-review-workflow/src/focus_cache.rs"),
    );
    assert_ne!(changed, original);
}

#[test]
fn report_build_identity_for_child_process() {
    let Some(root) = std::env::var_os("SS_AI_FOCUS_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let session = root.join("session");
    let input = root.join("input");
    let skill = root.join("skill");
    let recipe = skill.join("prepare-focus-areas.md");
    let cache = root.join("cache");
    let options = CacheOptions {
        session: &session,
        input_directory: &input,
        skill_root: &skill,
        recipe: &recipe,
        cache_directory: &cache,
        generator_profile: "test-model",
        freshness_days: 7,
    };
    let result = restore(&options, true, &TestClock(1_800_000_000)).unwrap();
    println!("\nFOCUS_TEST_KEY={}", result.key.unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "requires isolated native subprocesses")]
fn copied_executable_paths_share_cache_identity() {
    let fixture = Fixture::new_exclusive("execution-path");
    let executable = std::env::current_exe().unwrap();
    let suffix = EXE_SUFFIX;
    let mut keys = Vec::new();
    for name in ["first", "second"] {
        let path = fixture.root.join(format!("{name}{suffix}"));
        fs::copy(&executable, &path).unwrap();
        let output = Command::new(&path)
            .args([
                "--exact",
                "report_build_identity_for_child_process",
                "--nocapture",
            ])
            .env("SS_AI_FOCUS_TEST_ROOT", &fixture.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let key = stdout
            .lines()
            .find_map(|line| line.strip_prefix("FOCUS_TEST_KEY="))
            .unwrap();
        keys.push(key.to_owned());
    }
    assert_eq!(keys.first(), keys.last());
}
