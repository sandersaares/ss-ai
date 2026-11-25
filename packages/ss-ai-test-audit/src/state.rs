//! Shared state loading, freshness checks, and bounded evidence search.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail, read_bytes};

use crate::model::{AuditState, ClassificationJournal, IndexEntry, IndexedFile, Inventory};
use crate::scan::digest;

/// Bounds each evidence page to a human-reviewable targeted search result.
const MAX_SEARCH_RESULTS: usize = 200;
/// Keeps one multi-query evidence call bounded even when every query returns a full page.
const MAX_SEARCH_QUERIES: usize = 10;

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
    if limit == 0 || limit > MAX_SEARCH_RESULTS {
        return Err(fail(
            "invalidInput",
            "Search limit must be between 1 and 200",
        ));
    }
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
        results.push(search_query(&loaded.index, query, kind, limit)?);
    }
    Ok(json!({
        "schemaVersion": 1,
        "auditId": loaded.state.audit_id,
        "snapshotSha256": loaded.state.snapshot_sha256,
        "results": results,
        "queryCount": queries.len()
    })
    .to_string())
}

/// Executes one bounded AND-term query against the already validated index.
fn search_query(
    index: &[IndexEntry],
    query: &str,
    kind: Option<&str>,
    limit: usize,
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
    for entry in index {
        if kind.is_some_and(|kind| entry.kind != kind) {
            continue;
        }
        let haystack = format!("{} {}", entry.path, entry.excerpt).to_ascii_lowercase();
        if terms.iter().all(|term| haystack.contains(term)) {
            if matches.len() == limit {
                truncated = true;
                break;
            }
            matches.push(entry.clone());
        }
    }
    Ok(json!({
        "query": query,
        "kind": kind,
        "matches": matches,
        "truncated": truncated
    }))
}

/// Validated, consistently loaded state shared by batch and report operations.
pub(crate) struct LoadedState {
    pub(crate) state: AuditState,
    pub(crate) inventory: Inventory,
    pub(crate) index: Vec<IndexEntry>,
    pub(crate) journal: ClassificationJournal,
}

/// Loads schema-version-one state without rehashing audited source files.
pub(crate) fn load(state_directory: &Path, clock: &dyn Clock) -> Result<LoadedState, AppError> {
    let state: AuditState = read_json(&state_directory.join("audit-state.json"), clock)?;
    require_version(state.schema_version, "audit state")?;
    let inventory_bytes = require_bytes(&state_directory.join("inventory.json"), clock)?;
    if digest(&inventory_bytes) != state.snapshot_sha256 {
        return Err(fail(
            "invalidState",
            "Inventory does not match the audit snapshot",
        ));
    }
    let inventory: Inventory = parse_json(&inventory_bytes, "inventory")?;
    require_version(inventory.schema_version, "inventory")?;
    if inventory.audit_id != state.audit_id
        || inventory.repository_root != state.repository_root
        || inventory.repository_revision != state.repository_revision
        || inventory.revision_kind != state.revision_kind
        || inventory.scope_paths != state.scope_paths
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
    require_version(journal.schema_version, "classification journal")?;
    if journal.audit_id != state.audit_id || journal.snapshot_sha256 != state.snapshot_sha256 {
        return Err(fail(
            "invalidState",
            "Classification journal belongs to a different audit",
        ));
    }
    validate_journal(&inventory, &journal, &index)?;

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
    serde_json::from_slice(bytes)
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

/// Atomically publishes one complete JSON record while retaining its sibling lock.
pub(crate) fn write_json<T: serde::Serialize>(
    path: &Path,
    value: &T,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(value)?;
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

/// Resolves an output path and creates only its containing directory.
pub(crate) fn output_path(path: &Path) -> Result<PathBuf, AppError> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::path::absolute(path)?
    };
    let name = absolute
        .file_name()
        .ok_or_else(|| fail("invalidInput", "Output path requires a file name"))?;
    let parent = absolute
        .parent()
        .ok_or_else(|| fail("invalidInput", "Output path requires a parent directory"))?;
    fs::create_dir_all(parent)?;
    Ok(fs::canonicalize(parent)?.join(name))
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
    path: &Path,
    state_directory: &Path,
) -> Result<(), AppError> {
    for name in [
        "audit-state.json",
        "inventory.json",
        "search-index.jsonl",
        "classifications.json",
    ] {
        let destination = state_directory.join(name);
        let same_path = destination == path
            || (destination.try_exists()?
                && path.try_exists()?
                && same_file::is_same_file(path, destination)?);
        if same_path {
            return Err(fail(
                "invalidInput",
                format!("Batch output cannot replace audit state file '{name}'"),
            ));
        }
    }
    Ok(())
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
    let files: BTreeMap<&str, &str> = inventory
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.sha256.as_str()))
        .collect();
    for entry in index {
        let Some(expected_hash) = files.get(entry.path.as_str()) else {
            return Err(fail(
                "invalidState",
                format!("Search index references unknown file '{}'", entry.path),
            ));
        };
        if expected_hash != &entry.file_sha256
            || entry.repository_revision != inventory.repository_revision
        {
            return Err(fail(
                "invalidState",
                format!("Search index provenance does not match '{}'", entry.path),
            ));
        }
        if entry.line == 0 {
            return Err(fail("invalidState", "Search index line must be positive"));
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
    let tests: BTreeMap<&str, &crate::model::TestCase> = inventory
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
        if test.line == 0 || test.end_line < test.line || test.end_line > file.line_count {
            return Err(fail(
                "invalidState",
                format!("Test '{}' has an invalid source range", test.id),
            ));
        }
    }

    let assertions: BTreeMap<&str, &crate::model::AssertionSite> = inventory
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
        if assertion.line == 0
            || assertion.column == 0
            || assertion.end_line < assertion.line
            || assertion.end_line > file.line_count
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
    let assertions: BTreeMap<&str, &crate::model::AssertionSite> = inventory
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
pub(crate) fn state_paths(state_directory: &Path) -> Value {
    json!({
        "state": state_directory.join("audit-state.json"),
        "inventory": state_directory.join("inventory.json"),
        "index": state_directory.join("search-index.jsonl"),
        "classifications": state_directory.join("classifications.json"),
    })
}
