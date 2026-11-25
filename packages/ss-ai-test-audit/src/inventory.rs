//! Immutable repository inventory and audit-state initialization.

use std::fs;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde_json::json;
use ss_ai_runtime::{Clock, FileTransaction, Process, caused_by, fail};

use crate::model::{AuditState, ClassificationJournal, RevisionKind};
use crate::scan::{digest, inventory};
use crate::state::{output_path, state_paths, write_bytes, write_json};

/// Keeps session references concise while retaining ample digest entropy.
const AUDIT_ID_HEX_CHARACTERS: usize = 24;

/// Creates a fresh durable snapshot for one exact repository and scope.
pub fn initialize(
    repository_path: &Path,
    scopes: &[PathBuf],
    state_directory: &Path,
    process: &dyn Process,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let repository_path = fs::canonicalize(repository_path)
        .map_err(|error| caused_by("invalidInput", "Cannot access repository path", error))?;
    let state_directory = output_path(&state_directory.join(".initialize-marker"))?
        .parent()
        .ok_or_else(|| fail("invalidInput", "State directory requires a path"))?
        .to_owned();
    let (repository_root, repository_revision, revision_kind) =
        repository_identity(&repository_path, process)?;
    if state_directory.starts_with(&repository_root) {
        return Err(fail(
            "invalidInput",
            "Audit state directory must be outside the repository",
        ));
    }
    prepare_state_directory(&state_directory)?;
    let state_transaction =
        FileTransaction::open(&state_directory.join("audit-state.json"), clock)?;
    if state_transaction.read_bytes()?.is_some() {
        return Err(fail(
            "invalidInput",
            "Audit state directory already has an audit state",
        ));
    }
    let scopes = normalize_scopes(&repository_root, scopes)?;
    let (mut inventory, index) = inventory(
        &repository_root,
        &repository_revision,
        revision_kind,
        &scopes,
    )?;
    let inventory_material = serde_json::to_vec_pretty(&inventory)?;
    let audit_id = digest(&inventory_material);
    inventory.audit_id.clear();
    inventory
        .audit_id
        .push_str(audit_id.get(..AUDIT_ID_HEX_CHARACTERS).unwrap_or_default());
    let inventory_bytes = serde_json::to_vec_pretty(&inventory)?;
    let snapshot_sha256 = digest(&inventory_bytes);
    let index_bytes = index_lines(&index)?;
    let index_sha256 = digest(&index_bytes);
    let state = AuditState {
        schema_version: 1,
        audit_id: inventory.audit_id.clone(),
        snapshot_sha256,
        index_sha256,
        repository_root: repository_root
            .to_str()
            .ok_or_else(|| fail("invalidInput", "Repository path must be UTF-8"))?
            .to_owned(),
        repository_revision: repository_revision.clone(),
        revision_kind,
        scope_paths: scopes,
        scanned_at: clock.now().to_string(),
        test_count: inventory.tests.len(),
        assertion_count: inventory.assertions.len(),
    };
    let journal = ClassificationJournal {
        schema_version: 1,
        audit_id: state.audit_id.clone(),
        snapshot_sha256: state.snapshot_sha256.clone(),
        classifications: Vec::new(),
    };
    write_bytes(
        &state_directory.join("inventory.json"),
        &inventory_bytes,
        clock,
    )?;
    write_bytes(
        &state_directory.join("search-index.jsonl"),
        &index_bytes,
        clock,
    )?;
    write_json(
        &state_directory.join("classifications.json"),
        &journal,
        clock,
    )?;
    state_transaction.write_bytes(&serde_json::to_vec_pretty(&state)?)?;
    Ok(json!({
        "schemaVersion": 1,
        "stateDirectory": state_directory,
        "paths": state_paths(&state_directory),
        "auditId": state.audit_id,
        "snapshotSha256": state.snapshot_sha256,
        "repositoryRevision": repository_revision,
        "revisionKind": revision_kind,
        "testCount": state.test_count,
        "assertionCount": state.assertion_count,
        "fileCount": inventory.files.len(),
        "diagnosticCount": inventory.diagnostics.len(),
    })
    .to_string())
}

/// Requires a new state directory so initialization cannot mix source snapshots.
fn prepare_state_directory(path: &Path) -> Result<(), AppError> {
    fs::create_dir_all(path)?;
    let has_entries = fs::read_dir(path)?.next().transpose()?.is_some();
    if has_entries {
        return Err(fail(
            "invalidInput",
            "Audit state directory must be empty or absent",
        ));
    }
    Ok(())
}

/// Keeps stored scopes deterministic and repository-relative.
fn normalize_scopes(repository: &Path, scopes: &[PathBuf]) -> Result<Vec<String>, AppError> {
    if scopes.is_empty() {
        return Ok(vec![".".to_owned()]);
    }
    let mut normalized = scopes
        .iter()
        .map(|scope| {
            let requested = if scope.is_absolute() {
                scope.to_owned()
            } else {
                repository.join(scope)
            };
            if fs::symlink_metadata(&requested)?.file_type().is_symlink() {
                return Err(fail(
                    "invalidInput",
                    "Audit scopes cannot be symbolic links",
                ));
            }
            let absolute = fs::canonicalize(requested)?;
            let relative = absolute.strip_prefix(repository).map_err(|_error| {
                fail(
                    "invalidInput",
                    "Requested audit scope is outside the repository",
                )
            })?;
            Ok(relative.to_string_lossy().replace('\\', "/"))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

/// Resolves a Git identity when the caller selected a repository root.
fn repository_identity(
    path: &Path,
    process: &dyn Process,
) -> Result<(PathBuf, String, RevisionKind), AppError> {
    let mut has_git_marker = false;
    for ancestor in path.ancestors() {
        if ancestor.join(".git").try_exists()? {
            has_git_marker = true;
            break;
        }
    }
    if !has_git_marker {
        return Ok((
            path.to_owned(),
            "unversioned".to_owned(),
            RevisionKind::Unversioned,
        ));
    }
    let root = git_output(process, path, &["rev-parse", "--show-toplevel"])?;
    let revision = git_output(process, path, &["rev-parse", "HEAD"])?;
    let root = fs::canonicalize(PathBuf::from(root))?;
    if !path.starts_with(&root) {
        return Err(fail(
            "invalidInput",
            "Repository path is not inside Git's selected working tree",
        ));
    }
    if !matches!(revision.len(), 40 | 64) || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(fail(
            "invalidState",
            "Git returned an invalid repository revision",
        ));
    }
    Ok((root, revision, RevisionKind::Git))
}

/// Runs one local Git query without provider authentication or shell parsing.
fn git_output(
    process: &dyn Process,
    repository: &Path,
    arguments: &[&str],
) -> Result<String, AppError> {
    let arguments = arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect::<Vec<_>>();
    let output = process.isolated_git(repository, &arguments)?;
    if !output.success {
        return Err(fail(
            "invalidInput",
            format!(
                "Git repository identity failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// Serializes one compact JSON document per searchable line.
fn index_lines(entries: &[crate::model::IndexEntry]) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut bytes, entry)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}
