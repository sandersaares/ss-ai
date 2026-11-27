//! Immutable repository inventory and audit-state initialization.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde_json::{json, to_vec_pretty, to_writer};
use ss_ai_runtime::{Clock, FileTransaction, Process, caused_by, fail};

use crate::model::{
    AUDIT_STATE_SCHEMA_VERSION, AuditState, ClassificationJournal, IndexEntry, Inventory,
    InventoryCorrection, InventoryCorrectionRequest, RUBRIC_VERSION, RevisionKind, ScanDiagnostic,
};
use crate::scan::{digest, inventory};
use crate::state::{
    output_path, output_path_text, parse_json, require_version, state_paths, write_bytes,
    write_json,
};

/// Keeps session references concise while retaining ample digest entropy.
const AUDIT_ID_HEX_CHARACTERS: usize = 24;
/// Bounds agent-authored exclusion requests at the audit input ceiling.
const MAX_CORRECTION_BYTES: u64 = 1024 * 1024;

/// Creates a fresh durable snapshot for one exact repository and scope.
pub fn initialize(
    repository_path: &Path,
    scopes: &[PathBuf],
    state_directory: &Path,
    process: &dyn Process,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    initialize_with_corrections(
        repository_path,
        scopes,
        None,
        state_directory,
        process,
        clock,
    )
}

/// Creates a snapshot with explicitly reviewed source-site exclusions.
pub fn initialize_with_corrections(
    repository_path: &Path,
    scopes: &[PathBuf],
    correction_path: Option<&Path>,
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
    if let Some(correction_path) = correction_path {
        apply_corrections(&mut inventory, correction_path, &repository_revision)?;
    }
    let index_bytes = index_lines(&index)?;
    inventory.tool_version.clear();
    inventory.tool_version.push_str(env!("CARGO_PKG_VERSION"));
    inventory.rubric_version.clear();
    inventory.rubric_version.push_str(RUBRIC_VERSION);
    inventory.content_sha256 = content_digest(&inventory, &index_bytes)?;
    let inventory_material = to_vec_pretty(&inventory)?;
    let audit_id = digest(&inventory_material);
    inventory.audit_id.clear();
    inventory
        .audit_id
        .push_str(audit_id.get(..AUDIT_ID_HEX_CHARACTERS).unwrap_or_default());
    let inventory_bytes = to_vec_pretty(&inventory)?;
    let snapshot_sha256 = digest(&inventory_bytes);
    let index_sha256 = digest(&index_bytes);
    let state = AuditState {
        schema_version: AUDIT_STATE_SCHEMA_VERSION,
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
        tool_version: inventory.tool_version.clone(),
        rubric_version: inventory.rubric_version.clone(),
        content_sha256: inventory.content_sha256.clone(),
        scanned_at: clock.now().to_string(),
        test_count: inventory.tests.len(),
        assertion_count: inventory.assertions.len(),
    };
    let journal = ClassificationJournal {
        schema_version: AUDIT_STATE_SCHEMA_VERSION,
        audit_id: state.audit_id.clone(),
        snapshot_sha256: state.snapshot_sha256.clone(),
        content_sha256: state.content_sha256.clone(),
        index_sha256: state.index_sha256.clone(),
        tool_version: state.tool_version.clone(),
        rubric_version: state.rubric_version.clone(),
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
    // The state record marks the audit ready only after its required inputs exist.
    state_transaction.write_bytes(&to_vec_pretty(&state)?)?;
    Ok(json!({
        "schemaVersion": AUDIT_STATE_SCHEMA_VERSION,
        "stateDirectory": output_path_text(&state_directory)?,
        "paths": state_paths(&state_directory)?,
        "auditId": state.audit_id,
        "snapshotSha256": state.snapshot_sha256,
        "contentSha256": state.content_sha256,
        "indexSha256": state.index_sha256,
        "toolVersion": state.tool_version,
        "rubricVersion": state.rubric_version,
        "repositoryRevision": repository_revision,
        "revisionKind": revision_kind,
        "testCount": state.test_count,
        "assertionCount": state.assertion_count,
        "fileCount": inventory.files.len(),
        "diagnosticCount": inventory.diagnostics.len(),
        "correctionCount": inventory.corrections.len(),
    })
    .to_string())
}

/// Hashes source-owned inventory content without checkout-specific absolute paths.
fn content_digest(inventory: &Inventory, index_bytes: &[u8]) -> Result<String, AppError> {
    Ok(digest(&to_vec_pretty(&json!({
        "repositoryRevision": inventory.repository_revision,
        "scopePaths": inventory.scope_paths,
        "files": inventory.files,
        "tests": inventory.tests,
        "assertions": inventory.assertions,
        "diagnostics": inventory.diagnostics,
        "corrections": inventory.corrections,
        "indexSha256": digest(index_bytes),
    }))?))
}

/// Binds exact source predicates to the corrected inventory before publication.
fn apply_corrections(
    inventory: &mut Inventory,
    path: &Path,
    repository_revision: &str,
) -> Result<(), AppError> {
    let metadata = fs::metadata(path).map_err(|error| {
        caused_by(
            "invalidInput",
            format!("Cannot inspect inventory correction '{}'", path.display()),
            error,
        )
    })?;
    if !metadata.is_file() {
        return Err(fail(
            "invalidInput",
            format!(
                "Inventory correction '{}' must be a regular file",
                path.display()
            ),
        ));
    }
    if metadata.len() > MAX_CORRECTION_BYTES {
        return Err(fail(
            "invalidInput",
            format!(
                "Inventory correction '{}' exceeds the bounded input size",
                path.display()
            ),
        ));
    }
    let request: InventoryCorrectionRequest = parse_json(&fs::read(path)?, "inventory correction")?;
    require_version(request.schema_version, "inventory correction")?;
    if request.repository_revision != repository_revision {
        return Err(fail(
            "staleState",
            format!(
                "Inventory correction targets revision '{}', not audited revision '{}'",
                request.repository_revision, repository_revision
            ),
        ));
    }
    let mut corrected = BTreeSet::new();
    for draft in request.corrections {
        let path = draft.path.replace('\\', "/");
        if path.is_empty() || draft.reason.trim().is_empty() || draft.expression.trim().is_empty() {
            return Err(fail(
                "invalidInput",
                "Inventory correction fields path, expression, and reason must be nonempty",
            ));
        }
        let matching = inventory
            .assertions
            .iter()
            .filter(|assertion| {
                assertion.path == path
                    && assertion.line == draft.line
                    && assertion.end_line == draft.end_line
                    && assertion.expression == draft.expression
            })
            .collect::<Vec<_>>();
        let [assertion] = matching.as_slice() else {
            return Err(fail(
                "invalidInput",
                format!(
                    "Inventory correction does not match exactly one assertion at '{}:{}-{}'",
                    path, draft.line, draft.end_line
                ),
            ));
        };
        if !corrected.insert(assertion.id.clone()) {
            return Err(fail(
                "invalidInput",
                format!("Inventory correction repeats assertion '{}'", assertion.id),
            ));
        }
        inventory.corrections.push(InventoryCorrection {
            assertion_id: assertion.id.clone(),
            path: assertion.path.clone(),
            line: assertion.line,
            end_line: assertion.end_line,
            column: assertion.column,
            expression: assertion.expression.clone(),
            file_sha256: assertion.file_sha256.clone(),
            reason: draft.reason,
        });
        inventory.diagnostics.push(ScanDiagnostic {
            kind: "inventoryCorrection".to_owned(),
            path: assertion.path.clone(),
            line: Some(assertion.line.get()),
            detail: format!(
                "Assertion '{}' was excluded from this audit: {}",
                assertion.id,
                inventory
                    .corrections
                    .last()
                    .map_or("", |correction| &correction.reason)
            ),
        });
    }
    if corrected.is_empty() {
        return Err(fail(
            "invalidInput",
            "Inventory correction must name at least one source assertion",
        ));
    }
    inventory
        .assertions
        .retain(|assertion| !corrected.contains(&assertion.id));
    for test in &mut inventory.tests {
        test.direct_assertion_ids
            .retain(|assertion_id| !corrected.contains(assertion_id));
        test.shared_assertion_ids
            .retain(|assertion_id| !corrected.contains(assertion_id));
    }
    Ok(())
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
fn index_lines(entries: &[IndexEntry]) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::new();
    for entry in entries {
        to_writer(&mut bytes, entry)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}
