//! Shared state loading, freshness checks, and bounded evidence search.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::num::NonZeroUsize;
use std::path::{Component, Path, PathBuf, absolute};

use ohno::AppError;
use serde::de::DeserializeOwned;
use serde_json::{Value, from_slice, json, to_vec_pretty};
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail, read_bytes};

use crate::batch::ValidatedJournal;
use crate::model::{
    AUDIT_STATE_SCHEMA_VERSION, AssertionSite, AuditState, ClassificationJournal, IndexEntry,
    IndexedFile, Inventory, RUBRIC_VERSION, TestCase,
};
use crate::scan::digest;

/// Bounds each evidence page to a human-reviewable targeted search result.
const MAX_SEARCH_RESULTS: usize = 200;
/// Keeps one multi-query evidence call bounded even when every query returns a full page.
const MAX_SEARCH_QUERIES: usize = 10;
/// Bounds one complete owner-context response while retaining ordinary test bodies.
const MAX_ASSERTION_CONTEXT_BYTES: usize = 128 * 1024;
/// Keeps evidence handles compact while retaining snapshot-bound identity.
const EVIDENCE_SNAPSHOT_PREFIX_CHARACTERS: usize = 12;

/// Searches the immutable line index without loading all evidence into the agent.
pub fn search(
    state_directory: &Path,
    query: &str,
    kind: Option<&str>,
    limit: usize,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    search_many(state_directory, &[query.to_owned()], kind, limit, clock)
}

/// Searches several independently bounded queries from one immutable state load.
pub fn search_many(
    state_directory: &Path,
    queries: &[String],
    kind: Option<&str>,
    limit: usize,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    if queries.is_empty() || queries.len() > MAX_SEARCH_QUERIES {
        return Err(fail(
            "invalidInput",
            format!("Search requires between 1 and {MAX_SEARCH_QUERIES} queries"),
        ));
    }
    let limit = NonZeroUsize::new(limit)
        .filter(|limit| limit.get() <= MAX_SEARCH_RESULTS)
        .ok_or_else(|| fail("invalidInput", "Search limit must be between 1 and 200"))?;
    if let Some(kind) = kind
        && !matches!(
            kind,
            "documentation" | "publicDeclaration" | "traitImplementation" | "test" | "source"
        )
    {
        return Err(fail("invalidInput", "Unknown evidence kind"));
    }
    let loaded = load(state_directory, clock)?;
    let mut results = Vec::new();
    for query in queries {
        results.push(search_query(
            &loaded.index,
            &loaded.state.snapshot_sha256,
            query,
            kind,
            limit,
        )?);
    }
    Ok(json!({
        "schemaVersion": 1,
        "auditId": loaded.state.audit_id,
        "snapshotSha256": loaded.state.snapshot_sha256,
        "contentSha256": loaded.state.content_sha256,
        "indexSha256": loaded.state.index_sha256,
        "toolVersion": loaded.state.tool_version,
        "rubricVersion": loaded.state.rubric_version,
        "results": results,
        "queryCount": queries.len()
    })
    .to_string())
}

/// Returns one assertion card with its complete bounded enclosing owner source.
pub fn inspect_assertion(
    state_directory: &Path,
    assertion_id: &str,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let loaded = load_fresh(state_directory, clock)?;
    let assertion = loaded
        .inventory
        .assertions
        .iter()
        .find(|assertion| assertion.id == assertion_id)
        .ok_or_else(|| {
            fail(
                "invalidInput",
                format!("Unknown assertion '{assertion_id}'"),
            )
        })?;
    let containing_test = assertion
        .containing_test_id
        .as_deref()
        .and_then(|id| loaded.inventory.tests.iter().find(|test| test.id == id));
    let source =
        fs::read_to_string(Path::new(&loaded.inventory.repository_root).join(&assertion.path))
            .map_err(|error| {
                caused_by(
                    "staleState",
                    format!("Cannot read assertion owner '{}'", assertion.path),
                    error,
                )
            })?;
    let lines = source.lines().collect::<Vec<_>>();
    let start = assertion.owner_line.get();
    let end = assertion.owner_end_line.min(lines.len());
    let owner_text = lines
        .get(start.saturating_sub(1)..end)
        .ok_or_else(|| {
            fail(
                "invalidState",
                format!("Assertion '{assertion_id}' has an invalid owner range"),
            )
        })?
        .join("\n");
    if owner_text.len() > MAX_ASSERTION_CONTEXT_BYTES {
        return Err(fail(
            "invalidInput",
            format!("Assertion '{assertion_id}' owner context exceeds the bounded response size"),
        ));
    }
    let nested_assertions = loaded
        .inventory
        .assertions
        .iter()
        .filter(|candidate| {
            candidate.path == assertion.path
                && candidate.owner_line == assertion.owner_line
                && candidate.owner_end_line == assertion.owner_end_line
        })
        .map(|candidate| candidate.id.as_str())
        .collect::<Vec<_>>();
    let binding_candidates = lines
        .iter()
        .enumerate()
        .skip(start.saturating_sub(1))
        .take(end.saturating_sub(start).saturating_add(1))
        .filter(|(_, line)| {
            let line = line.trim_start();
            line.starts_with("let ")
                || line.starts_with("const ")
                || line.contains(".get_or_insert")
                || line.contains(" = ")
        })
        .map(|(offset, line)| {
            json!({
                "line": offset.saturating_add(1),
                "source": line,
            })
        })
        .collect::<Vec<_>>();
    let terms = identifier_terms(&assertion.expression);
    let related_evidence_candidates = loaded
        .index
        .iter()
        .filter(|entry| {
            entry.path != assertion.path || entry.line.get() < start || entry.line.get() > end
        })
        .filter(|entry| {
            let excerpt = entry.excerpt.to_ascii_lowercase();
            terms.iter().any(|term| excerpt.contains(term))
        })
        .take(24)
        .map(|entry| {
            json!({
                "path": entry.path,
                "line": entry.line,
                "endLine": entry.end_line,
                "owner": entry.owner,
                "sourceRole": entry.source_role,
                "excerpt": entry.excerpt,
                "status": "candidate"
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schemaVersion": 1,
        "auditId": loaded.state.audit_id,
        "snapshotSha256": loaded.state.snapshot_sha256,
        "contentSha256": loaded.state.content_sha256,
        "indexSha256": loaded.state.index_sha256,
        "toolVersion": loaded.state.tool_version,
        "rubricVersion": loaded.state.rubric_version,
        "assertion": assertion,
        "containingTest": containing_test,
        "owner": {
            "path": assertion.path,
            "line": assertion.owner_line,
            "endLine": assertion.owner_end_line,
            "source": owner_text
        },
        "nestedAssertionIds": nested_assertions,
        "bindingCandidates": binding_candidates,
        "relatedEvidenceCandidates": related_evidence_candidates
    })
    .to_string())
}

/// Extracts conservative lexical hints without claiming symbol resolution.
fn identifier_terms(expression: &str) -> Vec<String> {
    let mut current = String::new();
    let mut terms = Vec::new();
    for character in expression.chars().chain(std::iter::once(' ')) {
        if character.is_ascii_alphanumeric() || character == '_' {
            current.push(character);
        } else if !current.is_empty() {
            let lower = current.to_ascii_lowercase();
            if lower.len() >= 4
                && !matches!(
                    lower.as_str(),
                    "assert"
                        | "expect"
                        | "unwrap"
                        | "matches"
                        | "result"
                        | "value"
                        | "true"
                        | "false"
                )
            {
                terms.push(lower);
            }
            current.clear();
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

/// Executes one bounded AND-term query against the already validated index.
fn search_query(
    index: &[IndexEntry],
    snapshot_sha256: &str,
    query: &str,
    kind: Option<&str>,
    limit: NonZeroUsize,
) -> Result<Value, AppError> {
    if query.trim().is_empty() {
        return Err(fail("invalidInput", "Search query must not be empty"));
    }
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let mut matches = Vec::new();
    let mut truncated = false;
    let mut haystack = String::new();
    for (offset, entry) in index.iter().enumerate() {
        if kind.is_some_and(|kind| entry.kind != kind) {
            continue;
        }
        haystack.clear();
        haystack.push_str(&entry.path);
        haystack.push(' ');
        haystack.push_str(&entry.text);
        haystack.make_ascii_lowercase();
        if terms.iter().all(|term| haystack.contains(term)) {
            if matches.len() == limit.get() {
                truncated = true;
                break;
            }
            matches.push(json!({
                "evidenceHandle": evidence_handle(snapshot_sha256, offset),
                "path": entry.path,
                "line": entry.line,
                "endLine": entry.end_line,
                "language": entry.language,
                "kind": entry.kind,
                "owner": entry.owner,
                "sourceRole": entry.source_role,
                "excerpt": entry.excerpt,
                "previewTruncated": entry.preview_truncated,
                "repositoryRevision": entry.repository_revision,
                "fileSha256": entry.file_sha256
            }));
        }
    }
    Ok(json!({
        "query": query,
        "kind": kind,
        "matches": matches,
        "truncated": truncated
    }))
}

/// Returns exact snapshot evidence without exposing an arbitrary repository read.
pub fn inspect_evidence(
    state_directory: &Path,
    evidence_handle: &str,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let loaded = load(state_directory, clock)?;
    let offset = evidence_index(&loaded.state.snapshot_sha256, &loaded.index)
        .get(evidence_handle)
        .copied()
        .ok_or_else(|| {
            fail(
                "invalidInput",
                format!("Evidence handle '{evidence_handle}' is not bound to this audit snapshot"),
            )
        })?;
    let entry = loaded.index.get(offset).ok_or_else(|| {
        fail(
            "invalidState",
            format!("Evidence handle '{evidence_handle}' has no indexed range"),
        )
    })?;
    Ok(json!({
        "schemaVersion": 1,
        "auditId": loaded.state.audit_id,
        "snapshotSha256": loaded.state.snapshot_sha256,
        "evidenceHandle": evidence_handle,
        "path": entry.path,
        "line": entry.line,
        "endLine": entry.end_line,
        "language": entry.language,
        "kind": entry.kind,
        "owner": entry.owner,
        "sourceRole": entry.source_role,
        "text": entry.text,
        "preview": entry.excerpt,
        "previewTruncated": entry.preview_truncated,
        "repositoryRevision": entry.repository_revision,
        "fileSha256": entry.file_sha256
    })
    .to_string())
}

/// Builds the same compact, snapshot-bound evidence reference for search and submit.
pub(crate) fn evidence_handle(snapshot_sha256: &str, offset: usize) -> String {
    let prefix = snapshot_sha256
        .get(..EVIDENCE_SNAPSHOT_PREFIX_CHARACTERS)
        .unwrap_or(snapshot_sha256);
    format!("E{prefix}-{}", offset.saturating_add(1))
}

/// Resolves handles to exact index positions without trusting caller-provided paths.
pub(crate) fn evidence_index(
    snapshot_sha256: &str,
    index: &[IndexEntry],
) -> BTreeMap<String, usize> {
    index
        .iter()
        .enumerate()
        .map(|(offset, _)| (evidence_handle(snapshot_sha256, offset), offset))
        .collect()
}

/// Validated, consistently loaded state shared by batch and report operations.
pub(crate) struct LoadedState {
    pub(crate) state: AuditState,
    pub(crate) inventory: Inventory,
    pub(crate) index: Vec<IndexEntry>,
    pub(crate) journal: ValidatedJournal,
}

/// Loads current-schema state without rehashing audited source files.
pub(crate) fn load(state_directory: &Path, clock: &dyn Clock) -> Result<LoadedState, AppError> {
    let state: AuditState = read_json(&state_directory.join("audit-state.json"), clock)?;
    require_audit_version(state.schema_version, "audit state")?;
    if state.tool_version != env!("CARGO_PKG_VERSION") {
        return Err(fail(
            "invalidState",
            format!(
                "Audit state tool version '{}' does not match the running utility '{}'",
                state.tool_version,
                env!("CARGO_PKG_VERSION")
            ),
        ));
    }
    if state.rubric_version != RUBRIC_VERSION {
        return Err(fail(
            "invalidState",
            format!(
                "Audit state rubric version '{}' does not match the running rubric '{}'",
                state.rubric_version, RUBRIC_VERSION
            ),
        ));
    }
    let inventory_bytes = require_bytes(&state_directory.join("inventory.json"), clock)?;
    if digest(&inventory_bytes) != state.snapshot_sha256 {
        return Err(fail(
            "invalidState",
            "Inventory does not match the audit snapshot",
        ));
    }
    let inventory: Inventory = parse_json(&inventory_bytes, "inventory")?;
    require_audit_version(inventory.schema_version, "inventory")?;
    if inventory.audit_id != state.audit_id
        || inventory.repository_root != state.repository_root
        || inventory.repository_revision != state.repository_revision
        || inventory.revision_kind != state.revision_kind
        || inventory.scope_paths != state.scope_paths
        || inventory.tool_version != state.tool_version
        || inventory.rubric_version != state.rubric_version
        || inventory.content_sha256 != state.content_sha256
    {
        return Err(fail(
            "invalidState",
            "Inventory identity does not match the audit state",
        ));
    }
    if inventory.tests.len() != state.test_count
        || inventory.assertions.len() != state.assertion_count
    {
        return Err(fail(
            "invalidState",
            "Inventory counts do not match the audit state",
        ));
    }
    validate_inventory(&inventory)?;

    let index_bytes = require_bytes(&state_directory.join("search-index.jsonl"), clock)?;
    if digest(&index_bytes) != state.index_sha256 {
        return Err(fail(
            "invalidState",
            "Search index does not match the audit snapshot",
        ));
    }
    let index = parse_index(&index_bytes)?;
    validate_index(&inventory, &index)?;

    let journal: ClassificationJournal =
        match read_bytes(&state_directory.join("classifications.json"), clock)? {
            Some(bytes) => parse_json(&bytes, "classification journal")?,
            None => return Err(fail("invalidState", "Classification journal is missing")),
        };
    let journal = ValidatedJournal::from_persisted(journal, &state, &inventory, &index)?;

    Ok(LoadedState {
        state,
        inventory,
        index,
        journal,
    })
}

/// Loads state and verifies that every source still matches the immutable snapshot.
pub(crate) fn load_fresh(
    state_directory: &Path,
    clock: &dyn Clock,
) -> Result<LoadedState, AppError> {
    let loaded = load(state_directory, clock)?;
    validate_source_snapshot(&loaded.inventory, &loaded.state.repository_root)?;
    Ok(loaded)
}

/// Reads and validates a complete JSON record at an exact state path.
pub(crate) fn read_json<T: DeserializeOwned>(
    path: &Path,
    clock: &dyn Clock,
) -> Result<T, AppError> {
    let bytes = require_bytes(path, clock)?;
    parse_json(&bytes, path.to_string_lossy().as_ref())
}

/// Reads a required complete file without accepting a missing state as empty.
pub(crate) fn require_bytes(path: &Path, clock: &dyn Clock) -> Result<Vec<u8>, AppError> {
    read_bytes(path, clock)?.ok_or_else(|| {
        fail(
            "invalidState",
            format!("Required audit file is missing: {}", path.display()),
        )
    })
}

/// Parses one strict record and reports its exact logical role on failure.
pub(crate) fn parse_json<T: DeserializeOwned>(bytes: &[u8], role: &str) -> Result<T, AppError> {
    from_slice(bytes)
        .map_err(|error| caused_by("invalidState", format!("Cannot parse {role}"), error).into())
}

/// Rejects records from a utility version this implementation cannot interpret.
pub(crate) fn require_version(version: u32, role: &str) -> Result<(), AppError> {
    if version != 1 {
        return Err(fail(
            "invalidState",
            format!("Unsupported {role} schema version {version}"),
        ));
    }
    Ok(())
}

/// Rejects audit-state records from another inventory-correction contract.
pub(crate) fn require_audit_version(version: u32, role: &str) -> Result<(), AppError> {
    if version != AUDIT_STATE_SCHEMA_VERSION {
        return Err(fail(
            "invalidState",
            format!("Unsupported {role} schema version {version}"),
        ));
    }
    Ok(())
}

/// Atomically publishes one complete JSON record while retaining its sibling lock.
pub(crate) fn write_json<T: serde::Serialize>(
    path: &Path,
    value: &T,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let bytes = to_vec_pretty(value)?;
    let transaction = FileTransaction::open(path, clock)?;
    transaction.write_bytes(&bytes)?;
    Ok(())
}

/// Atomically publishes exact bytes after the caller has completed validation.
pub(crate) fn write_bytes(path: &Path, bytes: &[u8], clock: &dyn Clock) -> Result<(), AppError> {
    let transaction = FileTransaction::open(path, clock)?;
    transaction.write_bytes(bytes)?;
    Ok(())
}

/// Resolves an output path without creating anything before destination checks.
pub(crate) fn resolve_output_path(path: &Path) -> Result<PathBuf, AppError> {
    let absolute = absolute(path)?;
    output_path_text(&absolute)?;
    let name = absolute
        .file_name()
        .ok_or_else(|| fail("invalidInput", "Output path requires a file name"))?;
    let parent = absolute
        .parent()
        .ok_or_else(|| fail("invalidInput", "Output path requires a parent directory"))?;
    let resolved = resolve_directory(parent)?.join(name);
    output_path_text(&resolved)?;
    Ok(resolved)
}

/// Resolves existing directories through aliases and projects missing children.
fn resolve_directory(path: &Path) -> Result<PathBuf, AppError> {
    match fs::canonicalize(path) {
        Ok(path) => return Ok(path),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = path
        .parent()
        .ok_or_else(|| fail("invalidInput", "Cannot resolve output directory"))?;
    let mut resolved = resolve_directory(parent)?;
    match path.components().next_back() {
        Some(Component::ParentDir) => {
            _ = resolved.pop();
        }
        Some(Component::Normal(name)) => resolved.push(name),
        _ => {}
    }
    Ok(resolved)
}

/// Prepares a destination only after its effective path is representable.
pub(crate) fn output_path(path: &Path) -> Result<PathBuf, AppError> {
    let output = resolve_output_path(path)?;
    create_output_parent(&output)?;
    Ok(output)
}

/// Creates the parent of an already checked artifact path.
pub(crate) fn create_output_parent(path: &Path) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| fail("invalidInput", "Output path requires a parent directory"))?;
    fs::create_dir_all(parent)?;
    Ok(())
}

/// Returns the exact spelling used by receipts; JSON paths cannot be lossy.
pub(crate) fn output_path_text(path: &Path) -> Result<&str, AppError> {
    path.to_str()
        .ok_or_else(|| fail("invalidInput", "Audit output path must be UTF-8"))
}

/// Keeps existing generation paths untouched, even if a link there is dangling.
pub(crate) fn path_is_vacant(path: &Path) -> Result<bool, AppError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

/// Rejects destinations that would replace or mutate an audited source through an alias.
pub(crate) fn reject_source_alias(path: &Path, inventory: &Inventory) -> Result<(), AppError> {
    if !path.try_exists()? {
        return Ok(());
    }
    for file in &inventory.files {
        if same_file::is_same_file(path, Path::new(&inventory.repository_root).join(&file.path))? {
            return Err(fail(
                "invalidInput",
                format!("Output path aliases audited source '{}'", file.path),
            ));
        }
    }
    Ok(())
}

/// Prevents an assertion export from replacing canonical audit state.
pub(crate) fn reject_state_destination(
    requested: &Path,
    path: &Path,
    state_directory: &Path,
) -> Result<(), AppError> {
    let requested = absolute(requested)?;
    let requested_state = absolute(state_directory)?;
    let state_directory = fs::canonicalize(state_directory)?;
    let resolved_reports = resolve_directory(&state_directory.join("reports"))?;
    // Reserve all ASCII case variants across platforms, including before the
    // first publication when filesystem identity cannot be checked.
    let in_reports = |path: &Path, state: &Path| {
        path.strip_prefix(state).is_ok_and(|tail| {
            tail.components()
                .next()
                .is_some_and(|component| component.as_os_str().eq_ignore_ascii_case("reports"))
        })
    };
    if in_reports(&requested, &requested_state)
        || in_reports(path, &state_directory)
        || path.starts_with(&resolved_reports)
    {
        return Err(fail(
            "invalidInput",
            "Batch output cannot target the report publication area",
        ));
    }
    for name in [
        "audit-state.json",
        "inventory.json",
        "search-index.jsonl",
        "classifications.json",
    ] {
        let destination = state_directory.join(name);
        if paths_alias(path, &destination)? {
            return Err(fail(
                "invalidInput",
                format!("Batch output cannot replace audit state file '{name}'"),
            ));
        }
    }
    Ok(())
}

/// Protects an operation's input files from its prospective output paths.
pub(crate) fn reject_input_alias(output: &Path, inputs: &[&Path]) -> Result<(), AppError> {
    for input in inputs {
        if paths_alias(output, input)? {
            return Err(fail(
                "invalidInput",
                format!("Batch output aliases input file '{}'", input.display()),
            ));
        }
    }
    Ok(())
}

/// Handles both path spelling and filesystem identity, including hard links.
fn paths_alias(left: &Path, right: &Path) -> Result<bool, AppError> {
    Ok(left == right
        || (left.try_exists()? && right.try_exists()? && same_file::is_same_file(left, right)?))
}

/// Parses the newline-delimited searchable evidence index.
fn parse_index(bytes: &[u8]) -> Result<Vec<IndexEntry>, AppError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| caused_by("invalidState", "Search index is not UTF-8", error))?;
    text.lines()
        .enumerate()
        .map(|(offset, line)| {
            parse_json(
                line.as_bytes(),
                &format!("search index line {}", offset.saturating_add(1)),
            )
        })
        .collect()
}

/// Ensures citations cannot refer to a line or hash absent from the snapshot.
fn validate_index(inventory: &Inventory, index: &[IndexEntry]) -> Result<(), AppError> {
    let files: BTreeMap<&str, &IndexedFile> = inventory
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    for entry in index {
        let Some(file) = files.get(entry.path.as_str()) else {
            return Err(fail(
                "invalidState",
                format!("Search index references unknown file '{}'", entry.path),
            ));
        };
        let hash_matches = file.sha256 == entry.file_sha256;
        let revision_matches = entry.repository_revision == inventory.repository_revision;
        let range_valid = entry.line <= entry.end_line && entry.end_line.get() <= file.line_count;
        if !hash_matches || !revision_matches || !range_valid || entry.source_role.trim().is_empty()
        {
            return Err(fail(
                "invalidState",
                format!("Search index provenance does not match '{}'", entry.path),
            ));
        }
    }
    Ok(())
}

/// Reconciles all persisted IDs, paths, hashes, and source ranges.
fn validate_inventory(inventory: &Inventory) -> Result<(), AppError> {
    let files: BTreeMap<&str, &IndexedFile> = inventory
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    if files.len() != inventory.files.len() {
        return Err(fail("invalidState", "Inventory repeats a source file"));
    }
    let tests: BTreeMap<&str, &TestCase> = inventory
        .tests
        .iter()
        .map(|test| (test.id.as_str(), test))
        .collect();
    if tests.len() != inventory.tests.len() {
        return Err(fail("invalidState", "Inventory repeats a test ID"));
    }
    for test in &inventory.tests {
        let Some(file) = files.get(test.path.as_str()) else {
            return Err(fail(
                "invalidState",
                format!("Test '{}' references an unknown file", test.id),
            ));
        };
        if test.end_line < test.line.get() || test.end_line > file.line_count {
            return Err(fail(
                "invalidState",
                format!("Test '{}' has an invalid source range", test.id),
            ));
        }
    }

    let assertions: BTreeMap<&str, &AssertionSite> = inventory
        .assertions
        .iter()
        .map(|assertion| (assertion.id.as_str(), assertion))
        .collect();
    if assertions.len() != inventory.assertions.len() {
        return Err(fail("invalidState", "Inventory repeats an assertion ID"));
    }
    for assertion in &inventory.assertions {
        let Some(file) = files.get(assertion.path.as_str()) else {
            return Err(fail(
                "invalidState",
                format!("Assertion '{}' references an unknown file", assertion.id),
            ));
        };
        let assertion_range_valid =
            assertion.line.get() <= assertion.end_line && assertion.end_line <= file.line_count;
        let owner_start_valid = assertion.owner_line <= assertion.line;
        let owner_contains_assertion = assertion.owner_end_line >= assertion.end_line;
        let owner_within_file = assertion.owner_end_line <= file.line_count;
        let owner_range_valid = owner_start_valid && owner_contains_assertion && owner_within_file;
        if !assertion_range_valid
            || !owner_range_valid
            || assertion.expression.trim().is_empty()
            || assertion.file_sha256 != file.sha256
        {
            return Err(fail(
                "invalidState",
                format!("Assertion '{}' has invalid source provenance", assertion.id),
            ));
        }
        if let Some(test_id) = assertion.containing_test_id.as_deref() {
            let Some(test) = tests.get(test_id) else {
                return Err(fail(
                    "invalidState",
                    format!("Assertion '{}' references an unknown test", assertion.id),
                ));
            };
            if test.path != assertion.path
                || assertion.line < test.line
                || assertion.end_line > test.end_line
            {
                return Err(fail(
                    "invalidState",
                    format!(
                        "Assertion '{}' is outside its containing test",
                        assertion.id
                    ),
                ));
            }
        }
        for test_id in &assertion.calling_test_ids {
            if !tests.contains_key(test_id.as_str()) {
                return Err(fail(
                    "invalidState",
                    format!("Assertion '{}' references an unknown caller", assertion.id),
                ));
            }
        }
        for evidence in assertion
            .caller_evidence
            .iter()
            .chain(&assertion.unresolved_caller_candidates)
        {
            let Some(caller) = tests.get(evidence.test_id.as_str()) else {
                return Err(fail(
                    "invalidState",
                    format!(
                        "Assertion '{}' has caller evidence for an unknown test",
                        assertion.id
                    ),
                ));
            };
            let Some(call_file) = files.get(evidence.path.as_str()) else {
                return Err(fail(
                    "invalidState",
                    format!(
                        "Assertion '{}' has caller evidence in an unknown file",
                        assertion.id
                    ),
                ));
            };
            let caller_range_valid = evidence.line.get() <= evidence.end_line
                && evidence.line >= caller.line
                && evidence.end_line <= caller.end_line
                && evidence.end_line <= call_file.line_count;
            if caller.path != evidence.path
                || !caller_range_valid
                || evidence.kind.trim().is_empty()
                || evidence.expression.trim().is_empty()
            {
                return Err(fail(
                    "invalidState",
                    format!("Assertion '{}' has invalid caller provenance", assertion.id),
                ));
            }
        }
    }
    for test in &inventory.tests {
        for assertion_id in test
            .direct_assertion_ids
            .iter()
            .chain(test.shared_assertion_ids.iter())
        {
            if !assertions.contains_key(assertion_id.as_str()) {
                return Err(fail(
                    "invalidState",
                    format!("Test '{}' references an unknown assertion", test.id),
                ));
            }
        }
    }
    let mut corrections = BTreeSet::new();
    for correction in &inventory.corrections {
        let Some(file) = files.get(correction.path.as_str()) else {
            return Err(fail(
                "invalidState",
                format!(
                    "Correction for assertion '{}' references an unknown file",
                    correction.assertion_id
                ),
            ));
        };
        if !corrections.insert(correction.assertion_id.as_str()) {
            return Err(fail(
                "invalidState",
                format!(
                    "Inventory repeats correction for assertion '{}'",
                    correction.assertion_id
                ),
            ));
        }
        if assertions.contains_key(correction.assertion_id.as_str())
            || correction.end_line < correction.line.get()
            || correction.end_line > file.line_count
            || correction.expression.trim().is_empty()
            || correction.reason.trim().is_empty()
            || correction.file_sha256 != file.sha256
        {
            return Err(fail(
                "invalidState",
                format!(
                    "Correction for assertion '{}' has invalid source provenance",
                    correction.assertion_id
                ),
            ));
        }
    }
    Ok(())
}

/// Rejects source movement so resumed classifications remain bound to exact lines.
pub(crate) fn validate_source_snapshot(inventory: &Inventory, root: &str) -> Result<(), AppError> {
    let root = Path::new(root);
    for file in &inventory.files {
        let path = root.join(Path::new(&file.path));
        let bytes = fs::read(&path).map_err(|error| {
            caused_by(
                "staleState",
                format!("Cannot reread audited file '{}'", file.path),
                error,
            )
        })?;
        if digest(&bytes) != file.sha256 {
            return Err(fail(
                "staleState",
                format!("Audited file changed after initialization: {}", file.path),
            ));
        }
    }
    Ok(())
}

/// Rejects duplicate or unknown classifications before later commands use them.
pub(crate) fn validate_journal(
    inventory: &Inventory,
    journal: &ClassificationJournal,
    index: &[IndexEntry],
) -> Result<(), AppError> {
    let assertions: BTreeMap<&str, &AssertionSite> = inventory
        .assertions
        .iter()
        .map(|assertion| (assertion.id.as_str(), assertion))
        .collect();
    let tests: BTreeSet<&str> = inventory
        .tests
        .iter()
        .map(|test| test.id.as_str())
        .collect();
    let mut seen = BTreeMap::new();
    for classification in &journal.classifications {
        let Some(assertion) = assertions.get(classification.assertion_id.as_str()) else {
            return Err(fail(
                "invalidState",
                format!(
                    "Classification references unknown assertion '{}'",
                    classification.assertion_id
                ),
            ));
        };
        if seen
            .insert(classification.assertion_id.clone(), ())
            .is_some()
        {
            return Err(fail(
                "invalidState",
                format!(
                    "Classification journal repeats assertion '{}'",
                    classification.assertion_id
                ),
            ));
        }
        crate::batch::validate_classification(classification, assertion, &tests, index)?;
    }
    Ok(())
}

/// Supplies a diagnostic map of state file paths to skill recipes.
pub(crate) fn state_paths(state_directory: &Path) -> Result<Value, AppError> {
    Ok(json!({
        "state": output_path_text(&state_directory.join("audit-state.json"))?,
        "inventory": output_path_text(&state_directory.join("inventory.json"))?,
        "index": output_path_text(&state_directory.join("search-index.jsonl"))?,
        "classifications": output_path_text(&state_directory.join("classifications.json"))?,
    }))
}
