//! Exercises instruction snapshot publication and filesystem traversal.
#![allow(
    clippy::assertions_on_result_states,
    clippy::indexing_slicing,
    reason = "The fixtures assert fixed record shapes and expected failures"
)]
#![allow(
    clippy::create_dir,
    reason = "Each fixture directory must be new rather than reused"
)]

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use ss_ai_review_context::bytes_hash;
use ss_ai_review_workflow::focus_area_generation;
use ss_ai_review_workflow::instruction_sources::{
    InstructionSourceOptions, InstructionSourceSnapshot, RepositoryInstructionSources,
    snapshot_instruction_sources,
};
use ss_ai_runtime::{Clock, LockMode, ResourceLock, classification};

/// Isolates each fixture inside the checkout without using a temporary directory.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".instruction-sources-test-{}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("skill")).unwrap();
        fs::write(path.join("skill/focus-areas.md"), b"\xef\xbb\xbfskill\r\n").unwrap();
        Self(path)
    }

    fn put(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn snapshot(&self, list: Option<&str>) -> Result<InstructionSourceSnapshot, AppError> {
        snapshot_instruction_sources(
            &InstructionSourceOptions {
                session_state: &self.0.join("session"),
                skill_root: &self.0.join("skill"),
                directory_list: list,
                base_directory: &self.0,
                repository_sources: None,
            },
            &NoWait,
        )
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Fails immediately if a fixture unexpectedly encounters lock contention.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        panic!("Snapshot discovery must not read the clock")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Unexpected contention in instruction fixture")
    }
}

/// Coordinates a real lock overlap without sleeps or wall-clock deadlines.
struct PausedClock {
    observed: Sender<()>,
    release: Receiver<()>,
    paused: Cell<bool>,
}

impl Clock for PausedClock {
    fn now(&self) -> Timestamp {
        panic!("Snapshot discovery must not read the clock")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        if !self.paused.replace(true) {
            self.observed.send(()).unwrap();
            self.release.recv().unwrap();
        }
        Ok(())
    }
}

/// Starts a preparation using a restore identity bound under the session lock.
fn begin_preparation(session: &Path) -> focus_area_generation::Generation {
    let identity = {
        let _lock = ResourceLock::open(
            &session.join("focus-area-cache-operation"),
            LockMode::Exclusive,
            &NoWait,
        )
        .unwrap();
        focus_area_generation::bind_restore_locked(
            session,
            &session.join("instruction-set-source"),
            &NoWait,
        )
        .unwrap()
    };
    focus_area_generation::begin(session, &identity, &NoWait).unwrap()
}

/// Checks a snapshot identity while holding the same operation lock as readers.
fn snapshot_is_current(session: &Path, identity: &str) -> bool {
    let _lock = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        &NoWait,
    )
    .unwrap();
    focus_area_generation::verify_snapshot_locked(session, identity, &NoWait).unwrap()
}

#[test]
fn records_roots_and_exact_bytes_and_replaces_removed_sources() {
    let workspace = Workspace::new("bytes");
    let content = b"\xef\xbb\xbf# \xc3\xa4\r\n";
    workspace.put("one/.github/instructions/custom.instructions.md", content);
    workspace.put("one/AGENTS.md", b"agents");
    workspace.put("one/ignored.md", b"ignored");
    workspace.put("two/AGENTS.md", b"agents");
    let result = workspace.snapshot(Some(" one, ,two,one ")).unwrap();
    let record = serde_json::to_value(&result).unwrap();
    assert_eq!(record["schemaVersion"], 1);
    assert_eq!(record["externalFileCount"], 3);
    assert_eq!(record["roots"].as_array().unwrap().len(), 2);
    assert_eq!(
        result.roots[0].path,
        workspace.0.join("one").to_str().unwrap()
    );
    assert_eq!(
        result.roots[0].prefix,
        format!(
            "external/{}",
            bytes_hash(result.roots[0].path.as_bytes()).to_lowercase()
        )
    );
    assert_eq!(result.roots[0].file_count, 2);
    let source = Path::new(&result.source_directory);
    assert_eq!(
        fs::read(source.join("skill/focus-areas.md")).unwrap(),
        b"\xef\xbb\xbfskill\r\n"
    );
    assert_eq!(
        fs::read(
            source
                .join(&result.roots[0].prefix)
                .join(".github/instructions/custom.instructions.md")
        )
        .unwrap(),
        content
    );
    let empty = workspace.snapshot(Some(" , ")).unwrap();
    assert!(empty.roots.is_empty());
    assert_eq!(empty.external_file_count, 0);
    assert!(!source.join(&result.roots[0].prefix).exists());
}

#[test]
fn failures_preserve_published_snapshot() {
    let workspace = Workspace::new("failure");
    workspace.put("instructions/AGENTS.md", b"original");
    let before = workspace.snapshot(Some("instructions")).unwrap();
    let published = Path::new(&before.source_directory)
        .join(&before.roots[0].prefix)
        .join("AGENTS.md");
    assert!(workspace.snapshot(Some("missing")).is_err());
    assert!(workspace.snapshot(Some("skill/focus-areas.md")).is_err());
    fs::remove_file(workspace.0.join("skill/focus-areas.md")).unwrap();
    assert!(workspace.snapshot(Some("instructions")).is_err());
    assert_eq!(fs::read(published).unwrap(), b"original");
}

#[test]
fn invalid_base_and_skill_paths_cannot_replace_the_previous_snapshot() {
    let workspace = Workspace::new("invalid-inputs");
    workspace.put("instructions/AGENTS.md", b"original");
    let before = workspace.snapshot(Some("instructions")).unwrap();
    let session = workspace.0.join("session");
    let skill = workspace.0.join("skill");
    let relative = Path::new("relative");
    for (base, skill_root) in [
        (relative, skill.as_path()),
        (workspace.0.as_path(), relative),
    ] {
        let error = snapshot_instruction_sources(
            &InstructionSourceOptions {
                session_state: &session,
                skill_root,
                directory_list: Some("instructions"),
                base_directory: base,
                repository_sources: None,
            },
            &NoWait,
        );
        assert!(error.is_err());
    }
    let published = Path::new(&before.source_directory)
        .join(&before.roots[0].prefix)
        .join("AGENTS.md");
    assert_eq!(fs::read(published).unwrap(), b"original");
}

#[test]
fn snapshot_identity_changes_and_failures_invalidate_previous_identities() {
    let workspace = Workspace::new("snapshot-identity");
    workspace.put("instructions/AGENTS.md", b"unchanged");
    let session = workspace.0.join("session");
    let first = workspace.snapshot(Some("instructions")).unwrap();
    assert!(snapshot_is_current(&session, &first.snapshot_identity));
    let second = workspace.snapshot(Some("instructions")).unwrap();
    assert_ne!(first.snapshot_identity, second.snapshot_identity);
    assert!(!snapshot_is_current(&session, &first.snapshot_identity));
    assert!(snapshot_is_current(&session, &second.snapshot_identity));
    workspace.snapshot(Some("missing")).unwrap_err();
    assert!(!snapshot_is_current(&session, &second.snapshot_identity));
}

#[cfg(unix)]
#[test]
fn identity_binding_failure_restores_the_previous_snapshot() {
    let workspace = Workspace::new("identity-bind-failure");
    workspace.put("safe/AGENTS.md", b"original");
    workspace.put("unsupported/bad\\name.instructions.md", b"replacement");
    let session = workspace.0.join("session");
    let previous = workspace.snapshot(Some("safe")).unwrap();
    workspace.snapshot(Some("unsupported")).unwrap_err();
    let original = Path::new(&previous.source_directory)
        .join(&previous.roots[0].prefix)
        .join("AGENTS.md");
    assert_eq!(fs::read(original).unwrap(), b"original");
    assert!(!snapshot_is_current(&session, &previous.snapshot_identity));
}

#[test]
fn stages_explicit_repository_files_without_discovering_other_sources() {
    let workspace = Workspace::new("repository-files");
    workspace.put(
        "repository/.github/instructions/custom.instructions.md",
        b"\xef\xbb\xbfexact\r\n",
    );
    workspace.put("repository/AGENTS.md", b"not selected");
    let paths = [PathBuf::from(".github/instructions/custom.instructions.md")];
    let result = snapshot_instruction_sources(
        &InstructionSourceOptions {
            session_state: &workspace.0.join("session"),
            skill_root: &workspace.0.join("skill"),
            directory_list: None,
            base_directory: &workspace.0,
            repository_sources: Some(RepositoryInstructionSources {
                root: &workspace.0.join("repository"),
                paths: &paths,
            }),
        },
        &NoWait,
    )
    .unwrap();
    let snapshot = Path::new(&result.source_directory);
    assert_eq!(
        fs::read(snapshot.join("review-repository").join(&paths[0])).unwrap(),
        b"\xef\xbb\xbfexact\r\n"
    );
    assert!(!snapshot.join("review-repository/AGENTS.md").exists());
    assert_eq!(result.external_file_count, 0);
    assert!(snapshot_is_current(
        &workspace.0.join("session"),
        &result.snapshot_identity
    ));
}

#[test]
fn rejects_ambiguous_and_escaping_repository_paths() {
    let workspace = Workspace::new("repository-invalid");
    workspace.put("repository/AGENTS.md", b"selected");
    let root = workspace.0.join("repository");
    let duplicate = [PathBuf::from("AGENTS.md"), PathBuf::from("AGENTS.md")];
    let parent = [PathBuf::from("../skill/focus-areas.md")];
    let empty = [PathBuf::new()];
    let ambiguous = [PathBuf::from("./AGENTS.md")];
    let absolute = [workspace.0.join("skill/focus-areas.md")];
    let session = workspace.0.join("session");
    for paths in [
        &[][..],
        &duplicate[..],
        &parent[..],
        &empty[..],
        &ambiguous[..],
        &absolute[..],
    ] {
        let previous = workspace.snapshot(None).unwrap();
        snapshot_instruction_sources(
            &InstructionSourceOptions {
                session_state: &session,
                skill_root: &workspace.0.join("skill"),
                directory_list: None,
                base_directory: &workspace.0,
                repository_sources: Some(RepositoryInstructionSources { root: &root, paths }),
            },
            &NoWait,
        )
        .unwrap_err();
        assert!(!snapshot_is_current(&session, &previous.snapshot_identity));
    }
}

#[cfg(unix)]
#[test]
fn rejects_repository_symlinks_outside_the_root() {
    use std::os::unix::fs::symlink;

    let workspace = Workspace::new("repository-link-escape");
    workspace.put("repository/AGENTS.md", b"selected");
    symlink(
        workspace.0.join("skill/focus-areas.md"),
        workspace.0.join("repository/escape.md"),
    )
    .unwrap();
    let paths = [PathBuf::from("escape.md")];
    snapshot_instruction_sources(
        &InstructionSourceOptions {
            session_state: &workspace.0.join("session"),
            skill_root: &workspace.0.join("skill"),
            directory_list: None,
            base_directory: &workspace.0,
            repository_sources: Some(RepositoryInstructionSources {
                root: &workspace.0.join("repository"),
                paths: &paths,
            }),
        },
        &NoWait,
    )
    .unwrap_err();
}

#[test]
fn each_snapshot_attempt_invalidates_the_active_generation() {
    let workspace = Workspace::new("generation-invalidation");
    workspace.put("instructions/AGENTS.md", b"original");
    let session = workspace.0.join("session");
    workspace.snapshot(Some("instructions")).unwrap();

    let generation = begin_preparation(&session);
    workspace.snapshot(Some("instructions")).unwrap();
    let error =
        focus_area_generation::publish(&session, &generation.generation_id, &NoWait).unwrap_err();
    assert_eq!(classification(&error), "staleGeneration");

    let generation = begin_preparation(&session);
    workspace.snapshot(Some("missing")).unwrap_err();
    let error =
        focus_area_generation::publish(&session, &generation.generation_id, &NoWait).unwrap_err();
    assert_eq!(classification(&error), "staleGeneration");

    workspace.snapshot(Some("instructions")).unwrap();
    let generation = begin_preparation(&session);
    snapshot_instruction_sources(
        &InstructionSourceOptions {
            session_state: &session,
            skill_root: &workspace.0.join("skill"),
            directory_list: Some("instructions"),
            base_directory: Path::new("relative"),
            repository_sources: None,
        },
        &NoWait,
    )
    .unwrap_err();
    let error =
        focus_area_generation::publish(&session, &generation.generation_id, &NoWait).unwrap_err();
    assert_eq!(classification(&error), "staleGeneration");

    workspace.snapshot(Some("instructions")).unwrap();
    let generation = begin_preparation(&session);
    snapshot_instruction_sources(
        &InstructionSourceOptions {
            session_state: &session,
            skill_root: Path::new("relative"),
            directory_list: Some("instructions"),
            base_directory: &workspace.0,
            repository_sources: None,
        },
        &NoWait,
    )
    .unwrap_err();
    let error =
        focus_area_generation::publish(&session, &generation.generation_id, &NoWait).unwrap_err();
    assert_eq!(classification(&error), "staleGeneration");

    workspace.snapshot(Some("instructions")).unwrap();
    let generation = begin_preparation(&session);
    fs::remove_file(workspace.0.join("skill/focus-areas.md")).unwrap();
    workspace.snapshot(Some("instructions")).unwrap_err();
    let error =
        focus_area_generation::publish(&session, &generation.generation_id, &NoWait).unwrap_err();
    assert_eq!(classification(&error), "staleGeneration");
}

#[test]
fn nested_session_is_excluded_across_repeated_snapshots() {
    let workspace = Workspace::new("nested-session");
    let source = workspace.put("guidance.instructions.md", b"guidance");
    let first = workspace.snapshot(Some(".")).unwrap();
    assert_eq!(first.external_file_count, 1);
    let second = workspace.snapshot(Some(".")).unwrap();
    assert_eq!(second.external_file_count, 1);
    fs::remove_file(source).unwrap();
    assert_eq!(
        workspace.snapshot(Some(".")).unwrap().external_file_count,
        0
    );
    assert_eq!(
        workspace.snapshot(Some(".")).unwrap().external_file_count,
        0
    );
}

#[cfg(unix)]
#[test]
fn follows_links_excludes_session_aliases_and_detects_cycles() {
    use std::os::unix::fs::symlink;

    let workspace = Workspace::new("links");
    workspace.put("root/instructions/AGENTS.md", b"actual");
    fs::create_dir(workspace.0.join("external")).unwrap();
    workspace.put("external/custom.instructions.md", b"linked");
    symlink(
        workspace.0.join("external"),
        workspace.0.join("root/linked"),
    )
    .unwrap();
    let first = workspace.snapshot(Some("root")).unwrap();
    assert_eq!(first.external_file_count, 2);
    assert_eq!(
        fs::read(
            Path::new(&first.source_directory)
                .join(&first.roots[0].prefix)
                .join("linked/custom.instructions.md")
        )
        .unwrap(),
        b"linked"
    );
    symlink(
        workspace.0.join("session/instruction-set-source/missing"),
        workspace.0.join("root/old-session"),
    )
    .unwrap();
    assert_eq!(
        workspace
            .snapshot(Some("root"))
            .unwrap()
            .external_file_count,
        2
    );
    symlink(workspace.0.join("root"), workspace.0.join("root/cycle")).unwrap();
    assert!(
        workspace
            .snapshot(Some("root"))
            .unwrap_err()
            .to_string()
            .contains("cycle")
    );
    fs::remove_file(workspace.0.join("root/cycle")).unwrap();
    symlink(
        workspace.0.join("absent"),
        workspace.0.join("root/dangling"),
    )
    .unwrap();
    assert!(workspace.snapshot(Some("root")).is_err());
    assert_eq!(
        fs::read(
            Path::new(&first.source_directory)
                .join(&first.roots[0].prefix)
                .join("linked/custom.instructions.md")
        )
        .unwrap(),
        b"linked"
    );
}

#[cfg(unix)]
#[test]
fn follows_links_before_resolving_parent_components() {
    use std::os::unix::fs::symlink;

    let workspace = Workspace::new("link-parent");
    workspace.put("outside/nested/placeholder", b"");
    workspace.put("outside/AGENTS.md", b"outside");
    fs::create_dir(workspace.0.join("root")).unwrap();
    symlink(
        workspace.0.join("outside/nested"),
        workspace.0.join("root/nested-alias"),
    )
    .unwrap();
    symlink("nested-alias/..", workspace.0.join("root/parent-alias")).unwrap();
    workspace.put("root/child/AGENTS.md", b"child");
    symlink("child", workspace.0.join("root/repeated-alias")).unwrap();
    symlink(
        "repeated-alias/../repeated-alias",
        workspace.0.join("root/repeated-path"),
    )
    .unwrap();
    let result = workspace.snapshot(Some("root")).unwrap();
    let copied = Path::new(&result.source_directory)
        .join(&result.roots[0].prefix)
        .join("parent-alias/AGENTS.md");
    assert_eq!(fs::read(copied).unwrap(), b"outside");
    let repeated = Path::new(&result.source_directory)
        .join(&result.roots[0].prefix)
        .join("repeated-path/AGENTS.md");
    assert_eq!(fs::read(repeated).unwrap(), b"child");
}

#[cfg(unix)]
#[test]
fn reports_a_direct_link_cycle_without_waiting() {
    use std::os::unix::fs::symlink;

    let workspace = Workspace::new("direct-cycle");
    fs::create_dir(workspace.0.join("root")).unwrap();
    symlink("cycle", workspace.0.join("root/cycle")).unwrap();
    assert!(
        workspace
            .snapshot(Some("root"))
            .unwrap_err()
            .to_string()
            .contains("cycle")
    );
}

#[test]
fn holds_exclusive_cache_lock_during_snapshot() {
    let workspace = Workspace::new("lock");
    let session = workspace.0.join("session");
    let lock_path = session.join("focus-area-cache-operation");
    let reader = ResourceLock::open(&lock_path, LockMode::Shared, &NoWait).unwrap();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let base = workspace.0.clone();
    let worker = thread::spawn(move || {
        let clock = PausedClock {
            observed: observed_tx,
            release: release_rx,
            paused: Cell::new(false),
        };
        snapshot_instruction_sources(
            &InstructionSourceOptions {
                session_state: &base.join("session"),
                skill_root: &base.join("skill"),
                directory_list: None,
                base_directory: &base,
                repository_sources: None,
            },
            &clock,
        )
    });
    observed_rx.recv().unwrap();
    assert!(!session.join("instruction-set-source").exists());
    drop(reader);
    release_tx.send(()).unwrap();
    assert_eq!(worker.join().unwrap().unwrap().external_file_count, 0);
    assert!(lock_path.with_extension("lock").exists());
}
