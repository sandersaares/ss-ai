//! Snapshot explicitly configured instruction roots for focus-area preparation.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf, is_separator};

use ohno::AppError;
use serde::Serialize;
use ss_ai_review_context::bytes_hash;
use ss_ai_runtime::{Clock, LockMode, ResourceLock, caused_by, fail, read_bytes};

use crate::focus_area_generation;

/// Explicit inputs for an instruction source snapshot.
#[derive(Debug)]
#[expect(
    clippy::exhaustive_structs,
    reason = "This application-internal operation is constructed directly"
)]
pub struct InstructionSourceOptions<'a> {
    /// Session directory that owns the snapshot and cache-operation lock.
    pub session_state: &'a Path,
    /// Installed skill directory containing `focus-areas.md`.
    pub skill_root: &'a Path,
    /// Value of `COPILOT_CUSTOM_INSTRUCTIONS_DIRS`, if set.
    pub directory_list: Option<&'a str>,
    /// Absolute working directory used to resolve relative configured roots.
    pub base_directory: &'a Path,
    /// Explicitly selected repository files, if any.
    pub repository_sources: Option<RepositoryInstructionSources<'a>>,
}

/// Caller-selected files to include from one repository.
#[derive(Debug)]
#[expect(
    clippy::exhaustive_structs,
    reason = "This application-internal operation is constructed directly"
)]
#[expect(
    clippy::module_name_repetitions,
    reason = "The name identifies caller-selected sources at the CLI integration boundary"
)]
pub struct RepositoryInstructionSources<'a> {
    /// Absolute repository directory containing the selected files.
    pub root: &'a Path,
    /// Repository-relative paths of the selected files.
    pub paths: &'a [PathBuf],
}

/// Snapshot provenance in the format consumed by focus-area preparation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::exhaustive_structs,
    reason = "This application-internal record has a fixed schema"
)]
pub struct InstructionSourceSnapshot {
    /// Version of the snapshot result record.
    pub schema_version: u32,
    /// Absolute path of the published source directory.
    pub source_directory: String,
    /// Configured roots in their first-occurrence order.
    pub roots: Vec<InstructionSourceRoot>,
    /// Number of copied files from configured roots.
    pub external_file_count: usize,
    /// Opaque identity binding this mapping to the published source tree.
    pub snapshot_identity: String,
}

/// Maps one configured root to its namespace in the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::exhaustive_structs,
    reason = "This application-internal record has a fixed schema"
)]
pub struct InstructionSourceRoot {
    /// Absolute configured path, retaining its spelling rather than resolving links.
    pub path: String,
    /// Snapshot-relative namespace derived from the configured path.
    pub prefix: String,
    /// Number of copied instruction files in this root.
    pub file_count: usize,
}

/// Creates and publishes the complete source snapshot under the shared cache lock.
///
/// # Errors
///
/// Returns an error if a path, source, lock, or publication operation fails.
#[expect(
    clippy::module_name_repetitions,
    reason = "The operation name identifies its output at the CLI integration boundary"
)]
pub fn snapshot_instruction_sources(
    options: &InstructionSourceOptions<'_>,
    clock: &dyn Clock,
) -> Result<InstructionSourceSnapshot, AppError> {
    if !options.session_state.is_absolute() {
        return Err(invalid("Session directory must be absolute"));
    }
    let session = normalize(options.session_state);
    let _lock = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        clock,
    )?;
    // Invalidate before checking other inputs, so a failed refresh cannot expose old preparation.
    let preparation = focus_area_generation::invalidate_locked(&session, clock);
    let snapshot = focus_area_generation::invalidate_snapshot_locked(&session, clock);
    match (preparation, snapshot) {
        (Ok(()), Ok(())) => {}
        (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
        (Err(preparation), Err(snapshot)) => {
            return Err(fail(
                "io",
                format!(
                    "Preparation invalidation failed: {preparation}; snapshot invalidation failed: {snapshot}"
                ),
            ));
        }
    }
    path_text(&session)?;
    let base = absolute_input(options.base_directory, "Base directory")?;
    let skill = absolute_input(options.skill_root, "Skill directory")?;
    let excluded = resolve_allow_missing(&session)?;
    let roots = configured_roots(options.directory_list, &base)?;
    let repository_files = repository_files(options.repository_sources.as_ref(), &excluded)?;

    // Discovery finishes before staging so a root containing the session cannot ingest it.
    let mut discovered = Vec::new();
    for root in roots {
        let files = discover(&root, &excluded)?;
        discovered.push((root, files));
    }

    let destination = session.join("instruction-set-source");
    let staging = create_staging(&session)?;
    let result = stage_and_publish(
        &discovered,
        &repository_files,
        &skill,
        &staging,
        &destination,
        clock,
    )
    .and_then(|published| complete_publication(published, &session, &destination, &staging, clock));
    let cleanup = match fs::symlink_metadata(&staging) {
        Ok(_) => fs::remove_dir_all(&staging),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    match (result, cleanup) {
        (Ok(record), Ok(())) => Ok(record),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(io_error(
            "Cannot remove instruction staging directory",
            error,
        )),
        (Err(error), Err(cleanup)) => Err(fail(
            "io",
            format!("Snapshot failed: {error}; staging cleanup failed: {cleanup}"),
        )),
    }
}

/// Builds the complete tree before moving it into the protected publication slot.
fn stage_and_publish(
    discovered: &[(PathBuf, Vec<SourceFile>)],
    repository_files: &[SourceFile],
    skill: &Path,
    staging: &Path,
    destination: &Path,
    clock: &dyn Clock,
) -> Result<PublishedSnapshot, AppError> {
    copy(
        &skill.join("focus-areas.md"),
        &staging.join("skill/focus-areas.md"),
        clock,
    )?;
    for file in repository_files {
        copy(
            &file.path,
            &staging.join("review-repository").join(&file.relative),
            clock,
        )?;
    }
    let mut roots = Vec::new();
    let mut count = 0_usize;
    for (root, files) in discovered {
        let path = path_text(root)?;
        let prefix = format!(
            "external/{}",
            bytes_hash(path.as_bytes()).to_ascii_lowercase()
        );
        for file in files {
            copy(
                &file.path,
                &staging.join(&prefix).join(&file.relative),
                clock,
            )?;
        }
        count = count
            .checked_add(files.len())
            .ok_or_else(|| invalid("Instruction file count exceeds the supported range"))?;
        roots.push(InstructionSourceRoot {
            path: path.to_owned(),
            prefix,
            file_count: files.len(),
        });
    }
    // Validate the record before replacing the previous snapshot.
    let record = InstructionSourceSnapshot {
        schema_version: 1,
        source_directory: path_text(destination)?.to_owned(),
        roots,
        external_file_count: count,
        snapshot_identity: String::new(),
    };
    let backup = staging.with_extension("previous");
    let had_previous = match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
        Ok(_) => {
            return Err(invalid(
                "Instruction snapshot must be an ordinary directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(io_error("Cannot inspect instruction snapshot", error)),
    };
    if had_previous {
        fs::rename(destination, &backup)
            .map_err(|error| io_error("Cannot retain previous instruction snapshot", error))?;
    }
    if let Err(error) = fs::rename(staging, destination) {
        if had_previous {
            fs::rename(&backup, destination).map_err(|rollback| {
                fail(
                    "io",
                    format!("Snapshot publication failed: {error}; rollback failed: {rollback}"),
                )
            })?;
        }
        return Err(io_error("Cannot publish instruction snapshot", error));
    }
    Ok(PublishedSnapshot {
        record,
        backup: had_previous.then_some(backup),
    })
}

/// Keeps the old tree available until its replacement has a source identity.
struct PublishedSnapshot {
    record: InstructionSourceSnapshot,
    backup: Option<PathBuf>,
}

/// Binds the published tree or restores the previous tree after a binding failure.
fn complete_publication(
    mut published: PublishedSnapshot,
    session: &Path,
    destination: &Path,
    staging: &Path,
    clock: &dyn Clock,
) -> Result<InstructionSourceSnapshot, AppError> {
    match focus_area_generation::bind_snapshot_locked(session, clock) {
        Ok(identity) => published.record.snapshot_identity = identity,
        Err(error) => {
            let invalidation = focus_area_generation::invalidate_snapshot_locked(session, clock);
            let rollback = rollback_publication(destination, staging, published.backup.as_deref());
            return match (invalidation, rollback) {
                (Ok(()), Ok(())) => Err(error),
                (invalidation, rollback) => Err(fail(
                    "io",
                    format!(
                        "Snapshot binding failed: {error}; invalidation: {invalidation:?}; rollback: {rollback:?}"
                    ),
                )),
            };
        }
    }
    if let Some(backup) = published.backup
        && let Err(error) = fs::remove_dir_all(&backup)
    {
        let invalidation = focus_area_generation::invalidate_snapshot_locked(session, clock);
        return match invalidation {
            Ok(()) => Err(io_error(
                "Cannot remove previous instruction snapshot",
                error,
            )),
            Err(invalidation) => Err(fail(
                "io",
                format!("Snapshot cleanup failed: {error}; invalidation failed: {invalidation}"),
            )),
        };
    }
    Ok(published.record)
}

/// Restores the old snapshot without discarding it when a move fails.
fn rollback_publication(
    destination: &Path,
    staging: &Path,
    backup: Option<&Path>,
) -> Result<(), AppError> {
    fs::rename(destination, staging)
        .map_err(|error| io_error("Cannot retain unbound instruction snapshot", error))?;
    if let Some(backup) = backup {
        fs::rename(backup, destination)
            .map_err(|error| io_error("Cannot restore previous instruction snapshot", error))?;
    }
    Ok(())
}

/// Source location and snapshot-relative spelling, including traversed link names.
#[derive(Debug)]
struct SourceFile {
    path: PathBuf,
    relative: PathBuf,
}

/// Resolves only caller-selected repository files and rejects paths outside the root.
fn repository_files(
    selection: Option<&RepositoryInstructionSources<'_>>,
    excluded: &Path,
) -> Result<Vec<SourceFile>, AppError> {
    let Some(selection) = selection else {
        return Ok(Vec::new());
    };
    if selection.paths.is_empty() {
        return Err(invalid(
            "Repository instruction selection must not be empty",
        ));
    }
    let root = absolute_input(selection.root, "Repository root")?;
    let root = fs::canonicalize(&root)
        .map_err(|error| io_error("Cannot resolve repository root", error))?;
    if !fs::metadata(&root)
        .map_err(|error| io_error("Cannot inspect repository root", error))?
        .is_dir()
    {
        return Err(invalid("Repository root must be a directory"));
    }

    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for relative in selection.paths {
        let text = path_text(relative)?;
        if text.is_empty()
            || text
                .split(is_separator)
                .any(|part| part.is_empty() || part == "." || part == "..")
            || relative.components().any(|component| {
                !matches!(component, Component::Normal(_))
                    || component.as_os_str().to_string_lossy().contains('\\')
            })
        {
            return Err(invalid(
                "Repository instruction path must be a relative file path",
            ));
        }
        if !seen.insert(relative.clone()) {
            return Err(invalid("Repository instruction path is duplicated"));
        }
        let path = fs::canonicalize(root.join(relative))
            .map_err(|error| io_error("Cannot resolve repository instruction", error))?;
        if !path.starts_with(&root) || path.starts_with(excluded) {
            return Err(invalid(
                "Repository instruction resolves outside its permitted tree",
            ));
        }
        if !fs::metadata(&path)
            .map_err(|error| io_error("Cannot inspect repository instruction", error))?
            .is_file()
        {
            return Err(invalid("Repository instruction must be an ordinary file"));
        }
        files.push(SourceFile {
            path,
            relative: relative.clone(),
        });
    }
    Ok(files)
}

/// One traversal state; ancestry is local so distinct aliases can both contribute.
struct PendingDirectory {
    path: PathBuf,
    relative: PathBuf,
    ancestors: HashSet<PathBuf>,
}

/// Finds all eligible files and detects directory-link cycles.
fn discover(root: &Path, excluded: &Path) -> Result<Vec<SourceFile>, AppError> {
    let mut pending = vec![PendingDirectory {
        path: root.to_owned(),
        relative: PathBuf::new(),
        ancestors: HashSet::new(),
    }];
    let mut files = Vec::new();
    while let Some(mut current) = pending.pop() {
        let identity = resolve_allow_missing(&current.path)?;
        if identity.starts_with(excluded) {
            continue;
        }
        let metadata = fs::metadata(&identity)
            .map_err(|error| io_error("Cannot inspect instruction directory", error))?;
        if !metadata.is_dir() {
            return Err(invalid(format!(
                "Instruction root is not a directory: {}",
                current.path.display()
            )));
        }
        if !current.ancestors.insert(identity.clone()) {
            return Err(invalid(format!(
                "Instruction directory contains a link cycle: {}",
                current.path.display()
            )));
        }
        let entries = fs::read_dir(&identity)
            .map_err(|error| io_error("Cannot enumerate instruction directory", error))?;
        let mut entries = entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| io_error("Cannot enumerate instruction directory", error))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = current.relative.join(entry.file_name());
            path_text(&relative)?;
            let link = entry
                .file_type()
                .map_err(|error| io_error("Cannot inspect instruction entry", error))?;
            if link.is_symlink() {
                let target = resolve_allow_missing(&path)?;
                if target.starts_with(excluded) {
                    continue;
                }
            }
            let metadata = fs::metadata(&path).map_err(|error| {
                io_error("Cannot inspect instruction entry or link target", error)
            })?;
            if metadata.is_dir() {
                pending.push(PendingDirectory {
                    path,
                    relative,
                    ancestors: current.ancestors.clone(),
                });
            } else if instruction_name(&entry.file_name()) {
                if !metadata.is_file() {
                    return Err(invalid(format!(
                        "Instruction source is not an ordinary file: {}",
                        path.display()
                    )));
                }
                files.push(SourceFile { path, relative });
            }
        }
    }
    Ok(files)
}

/// Selects only the instruction names used by the existing snapshot producer.
fn instruction_name(name: &OsString) -> bool {
    name.to_str()
        .is_some_and(|name| name == "AGENTS.md" || name.ends_with(".instructions.md"))
}

/// Splits configured roots without normalizing away their path identity.
fn configured_roots(list: Option<&str>, base: &Path) -> Result<Vec<PathBuf>, AppError> {
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for entry in list
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let path = Path::new(entry);
        let root = if path.is_absolute() {
            path.to_owned()
        } else {
            base.join(path)
        };
        let root = normalize(&root);
        path_text(&root)?;
        if seen.insert(root.clone()) {
            let metadata = fs::metadata(&root)
                .map_err(|error| io_error("Cannot inspect configured instruction root", error))?;
            if !metadata.is_dir() {
                return Err(invalid(format!(
                    "Configured instruction root is not a directory: {}",
                    root.display()
                )));
            }
            roots.push(root);
        }
    }
    Ok(roots)
}

/// Resolves links in every component, retaining missing suffixes for session exclusion.
fn resolve_allow_missing(path: &Path) -> Result<PathBuf, AppError> {
    resolve_components(path, &mut HashSet::new())
}

/// Resolves each link target before continuing through its remaining components.
fn resolve_components(path: &Path, links: &mut HashSet<PathBuf>) -> Result<PathBuf, AppError> {
    let mut remaining = path.components();
    let mut resolved = PathBuf::new();
    while let Some(part) = remaining.next() {
        if part == Component::ParentDir {
            resolved.pop();
            continue;
        }
        resolved.push(part.as_os_str());
        // Prefixes and roots anchor traversal; neither is a directory entry to
        // inspect. In particular, a verbatim Windows drive prefix can appear
        // absolute before its root separator has been appended.
        if matches!(part, Component::Prefix(_) | Component::RootDir) || !resolved.is_absolute() {
            continue;
        }
        match fs::symlink_metadata(&resolved) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if !links.insert(resolved.clone()) {
                    return Err(invalid(format!(
                        "Instruction link cycle: {}",
                        resolved.display()
                    )));
                }
                // Bound stack use for exceptionally deep but otherwise acyclic link chains.
                if links.len() > 256 {
                    return Err(invalid(
                        "Instruction link cycle or excessive link expansions",
                    ));
                }
                let target = fs::read_link(&resolved)
                    .map_err(|error| io_error("Cannot read instruction link", error))?;
                let target = if target.is_absolute() {
                    target
                } else {
                    resolved
                        .parent()
                        .ok_or_else(|| invalid("Link has no parent"))?
                        .join(target)
                };
                let target = resolve_components(&target, links);
                links.remove(&resolved);
                resolved = target?;
                // Root-only targets were already checked by canonicalize. Asking
                // for link metadata on a volume root is not supported everywhere.
                if resolved
                    .components()
                    .all(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
                {
                    continue;
                }
                match fs::symlink_metadata(&resolved) {
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        for part in remaining {
                            resolved.push(part.as_os_str());
                        }
                        return Ok(resolved);
                    }
                    Err(error) => {
                        return Err(io_error("Cannot inspect instruction link target", error));
                    }
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let missing = resolved
                    .file_name()
                    .ok_or_else(|| invalid("Missing path has no name"))?
                    .to_owned();
                let parent = resolved
                    .parent()
                    .ok_or_else(|| invalid("Missing path has no parent"))?;
                resolved = fs::canonicalize(parent)
                    .map_err(|error| io_error("Cannot resolve instruction path parent", error))?
                    .join(missing);
                for part in remaining {
                    resolved.push(part.as_os_str());
                }
                return Ok(resolved);
            }
            Err(error) => return Err(io_error("Cannot resolve instruction path", error)),
        }
    }
    fs::canonicalize(resolved).map_err(|error| io_error("Cannot resolve instruction path", error))
}

/// Lexically removes dot components without resolving links or changing case.
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

/// Rejects paths whose interpretation would depend on process working directory.
fn absolute_input(path: &Path, label: &str) -> Result<PathBuf, AppError> {
    if !path.is_absolute() {
        return Err(invalid(format!("{label} must be absolute")));
    }
    let path = normalize(path);
    path_text(&path)?;
    Ok(path)
}

/// Requires lossless UTF-8 for paths serialized in the snapshot record.
fn path_text(path: &Path) -> Result<&str, AppError> {
    path.to_str()
        .ok_or_else(|| invalid("Instruction path is not valid UTF-8"))
}

/// Creates a unique sibling directory without replacing another operation's staging.
fn create_staging(session: &Path) -> Result<PathBuf, AppError> {
    // A bounded scan handles stale directories without deleting another operation's data.
    for index in 0..1024_u32 {
        let path = session.join(format!(
            "instruction-set-source.{}.{}.tmp",
            std::process::id(),
            index
        ));
        if path
            .with_extension("previous")
            .try_exists()
            .map_err(|error| io_error("Cannot inspect instruction staging backup", error))?
        {
            continue;
        }
        #[expect(
            clippy::create_dir,
            reason = "Staging must be created exclusively rather than reused"
        )]
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(io_error(
                    "Cannot create instruction staging directory",
                    error,
                ));
            }
        }
    }
    Err(invalid(
        "Cannot allocate a unique instruction staging directory",
    ))
}

/// Copies exact source bytes using the runtime's shared-reader policy.
fn copy(source: &Path, destination: &Path, clock: &dyn Clock) -> Result<(), AppError> {
    let bytes = read_bytes(source, clock)?.ok_or_else(|| {
        invalid(format!(
            "Instruction source is missing: {}",
            source.display()
        ))
    })?;
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("Snapshot path has no parent"))?;
    fs::create_dir_all(parent)
        .map_err(|error| io_error("Cannot create snapshot directory", error))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options
        .open(destination)
        .map_err(|error| io_error("Cannot create snapshot file", error))?;
    file.write_all(&bytes)
        .map_err(|error| io_error("Cannot write snapshot file", error))
}

/// Reports an input or snapshot contract violation.
fn invalid(message: impl Into<String>) -> AppError {
    fail("invalidInput", message.into())
}

/// Preserves the underlying filesystem failure for callers.
fn io_error(message: &str, error: io::Error) -> AppError {
    caused_by("io", message.to_owned(), error).into()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::{instruction_name, normalize};

    #[test]
    fn selects_only_exact_instruction_names() {
        for name in ["AGENTS.md", "nested.instructions.md", ".instructions.md"] {
            assert!(instruction_name(&OsString::from(name)));
        }
        for name in [
            "agents.md",
            "AGENTS.MD",
            "x.instructions.MD",
            "instructions.md",
        ] {
            assert!(!instruction_name(&OsString::from(name)));
        }
    }

    #[test]
    fn normalization_keeps_spelling_and_eliminates_dot_components() {
        let path = Path::new("/base/One/../é/./source");
        assert_eq!(normalize(path), PathBuf::from("/base/é/source"));
    }
}
