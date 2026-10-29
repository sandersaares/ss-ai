//! Validated finding records and atomic artifact mutations.

use std::collections::{HashMap, HashSet};
use std::fs;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde_json::{Map, Value, json};
use ss_ai_runtime::{Clock, FileTransaction, fail, read_bytes};

use crate::workflow_plan::{PlanItem, PlanKind, parse_json};

/// Describes the artifact created or resumed under its lock.
#[derive(Debug)]
#[non_exhaustive]
pub struct InitializationResult {
    /// Validated artifact after initialization.
    pub artifact: Value,
    /// Whether resume found an existing artifact.
    pub resumed_existing: bool,
}

/// Describes an index using the reviewer artifacts read for its creation.
#[derive(Debug)]
#[non_exhaustive]
pub struct IndexResult {
    /// Published index document.
    pub artifact: Value,
    /// Number of reviewer artifacts, including empty ones.
    pub artifact_count: usize,
    /// Number of indexed findings.
    pub finding_count: usize,
    /// Destination supplied by the caller.
    pub output_path: PathBuf,
}

/// Describes the published consolidation without rereading its inputs.
#[derive(Debug)]
#[non_exhaustive]
pub struct ConsolidationResult {
    /// Published consolidated artifact.
    pub artifact: Value,
    /// Number of consolidated findings.
    pub finding_count: usize,
    /// Number of findings that merge multiple sources.
    pub merged_count: usize,
    /// Number of findings carried over from one source.
    pub carried_over_count: usize,
    /// Destination supplied by the caller.
    pub output_path: PathBuf,
}

/// Keeps authored content distinct from unchecked JSON and stored provenance.
struct ValidatedContent(Value);

/// Reads a strictly validated finding artifact.
///
/// # Errors
///
/// Returns an error when the artifact is missing or malformed.
pub fn read_artifact(path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let bytes = read_bytes(path, clock)?
        .ok_or_else(|| fail("invalidInput", "Finding artifact is missing"))?;
    parse_artifact(&bytes)
}

/// Reports completeness and shape problems for a file or reviewer directory.
///
/// # Errors
///
/// Returns an error when the requested filesystem location cannot be inspected.
pub fn validate_artifacts(
    path: &Path,
    expected_reviewers: &[String],
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let path = std::path::absolute(path)
        .map_err(|error| ss_ai_runtime::caused_by("io", "Cannot resolve artifact path", error))?;
    let mut files = Vec::new();
    let mut errors = Vec::new();
    if path.is_dir() {
        let entries = fs::read_dir(&path)
            .map_err(|error| ss_ai_runtime::caused_by("io", "Cannot inspect artifacts", error))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                ss_ai_runtime::caused_by("io", "Cannot inspect artifact entry", error)
            })?;
            let entry_path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if hidden_entry(&entry)? || name.ends_with(".json.lock") {
                continue;
            }
            if !entry_path.is_file()
                || !entry_path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            {
                errors.push(format!(
                    "Unexpected artifact entry '{name}' in '{}'",
                    path.display()
                ));
            } else {
                files.push(entry_path);
            }
        }
        files.sort();
    } else if path.is_file() {
        files.push(path);
    } else {
        errors.push(format!("Artifact path does not exist: {}", path.display()));
    }
    let mut artifacts = Vec::new();
    let mut present = HashSet::new();
    let mut incomplete = Vec::new();
    let mut unexpected = Vec::new();
    let mut count = 0_usize;
    for file in files {
        let artifact = match read_bytes(&file, clock)
            .and_then(|bytes| bytes.ok_or_else(|| invalid("Finding artifact is missing")))
            .and_then(|bytes| parse_json(&bytes))
        {
            Ok(artifact)
                if artifact.get("schemaVersion") == Some(&json!(1))
                    && matches!(
                        artifact.get("kind").and_then(Value::as_str),
                        Some("reviewer" | "consolidated")
                    ) =>
            {
                artifact
            }
            Ok(_) => {
                errors.push(format!("{}: invalid finding artifact", file.display()));
                continue;
            }
            Err(error) => {
                errors.push(format!("{}: {error}", file.display()));
                continue;
            }
        };
        // A report retains readable artifacts even when individual records are
        // malformed, so an invalid reviewer is not mistaken for a missing one.
        if let Err(error) = validate_artifact(&artifact) {
            errors.push(format!("{}: {error}", file.display()));
        }
        let kind = artifact
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let findings = artifact
            .get("findings")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        count = count
            .checked_add(findings.len())
            .ok_or_else(|| invalid("Finding count overflow"))?;
        if kind == "reviewer" {
            let reviewer = artifact
                .get("reviewerId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !present.insert(reviewer.to_owned()) {
                errors.push(format!("Duplicate reviewer: {reviewer}"));
            }
            if artifact.get("status") != Some(&json!("complete")) {
                incomplete.push(reviewer.to_owned());
            }
            if !matches!(
                artifact.get("status").and_then(Value::as_str),
                Some("open" | "complete")
            ) {
                errors.push(format!(
                    "{}: invalid reviewer status '{}'",
                    file.display(),
                    artifact
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                ));
            }
            if !expected_reviewers.is_empty() && !expected_reviewers.iter().any(|id| id == reviewer)
            {
                unexpected.push(reviewer.to_owned());
            }
        }
        for finding in findings {
            if let Err(error) = validate_record(finding) {
                errors.push(format!("{}: {error}", file.display()));
            }
        }
        if let Some(removed) = artifact.get("removed").and_then(Value::as_array) {
            for entry in removed {
                let result =
                    object(entry, &["finding", "reason", "removedUtc"]).and_then(|entry| {
                        validate_record(
                            entry
                                .get("finding")
                                .ok_or_else(|| invalid("Missing removed finding"))?,
                        )?;
                        text(entry.get("reason"), "reason")?;
                        text(entry.get("removedUtc"), "removedUtc")?;
                        Ok(())
                    });
                if let Err(error) = result {
                    errors.push(format!("{}: {error}", file.display()));
                }
            }
        }
        artifacts.push(json!({
            "path": file.to_string_lossy(), "kind": kind,
            "reviewerId": artifact.get("reviewerId"),
            "focusArea": artifact.get("focusArea"), "status": artifact.get("status"),
            "findingCount": findings.len()
        }));
    }
    let missing: Vec<_> = expected_reviewers
        .iter()
        .filter(|id| !present.contains(*id))
        .cloned()
        .collect();
    Ok(json!({
        "isValid": errors.is_empty() && incomplete.is_empty() && unexpected.is_empty() && missing.is_empty(),
        "artifacts": artifacts, "findingCount": count, "errors": errors,
        "missingReviewers": missing, "incompleteReviewers": incomplete, "unexpectedReviewers": unexpected
    }))
}

/// Validates a finding artifact at a consumer boundary.
///
/// # Errors
///
/// Returns an error for an invalid artifact or finding.
pub fn validate_artifact(value: &Value) -> Result<(), AppError> {
    let fields = object(
        value,
        &[
            "schemaVersion",
            "kind",
            "reviewerId",
            "focusArea",
            "status",
            "findings",
            "removed",
        ],
    )?;
    if fields.get("schemaVersion") != Some(&json!(1)) {
        return Err(invalid("Invalid finding artifact version"));
    }
    let kind = text(fields.get("kind"), "kind")?;
    match kind {
        "reviewer" => {
            let reviewer = text(fields.get("reviewerId"), "reviewerId")?;
            reviewer_id(reviewer)?;
            text(fields.get("focusArea"), "focusArea")?;
            if !matches!(text(fields.get("status"), "status")?, "open" | "complete") {
                return Err(invalid("Invalid reviewer status"));
            }
        }
        "consolidated" => {
            if ["reviewerId", "focusArea", "status"]
                .iter()
                .any(|name| fields.contains_key(*name))
            {
                return Err(invalid(
                    "Consolidated artifact cannot carry reviewer fields",
                ));
            }
        }
        _ => return Err(invalid("Invalid artifact kind")),
    }
    let findings = array(fields.get("findings"), "findings")?;
    let mut ids = HashSet::new();
    for finding in findings {
        validate_record(finding)?;
        let id = text(finding.get("id"), "id")?;
        if !ids.insert(id.to_owned()) {
            return Err(invalid("Duplicate finding identifier"));
        }
    }
    if let Some(removed) = fields.get("removed") {
        for (index, entry) in array(Some(removed), "removed")?.iter().enumerate() {
            let entry = object(entry, &["finding", "reason", "removedUtc"])?;
            let finding = entry
                .get("finding")
                .ok_or_else(|| invalid("Missing removed finding"))?;
            validate_record(finding).map_err(|error| {
                fail(
                    "invalidInput",
                    format!("Finding in removal record at position {index} has missing or invalid fields: {error}"),
                )
            })?;
            if !ids.insert(text(finding.get("id"), "id")?.to_owned()) {
                return Err(invalid("Duplicate finding identifier in history"));
            }
            text(entry.get("reason"), "reason")?;
            text(entry.get("removedUtc"), "removedUtc")?;
        }
    }
    Ok(())
}

/// Parses and validates a complete persisted artifact.
fn parse_artifact(bytes: &[u8]) -> Result<Value, AppError> {
    let value = parse_json(bytes)?;
    validate_artifact(&value)?;
    Ok(value)
}

/// Creates a reviewer artifact under its retained sibling lock.
///
/// `force` replaces an existing artifact.
///
/// # Errors
///
/// Returns an error for incompatible existing work or failed publication.
pub fn initialize(
    path: &Path,
    reviewer: &str,
    focus_area: &str,
    force: bool,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    reviewer_id(reviewer)?;
    if focus_area.trim().is_empty() {
        return Err(invalid("Invalid reviewer initialization"));
    }
    let transaction = FileTransaction::open(path, clock)?;
    let existing = transaction.read_bytes()?;
    if existing.is_some() && !force {
        return Err(invalid(
            "Finding artifact already exists; use -Resume to preserve it or -Force for a new review",
        ));
    }
    let value = json!({"schemaVersion":1,"kind":"reviewer","reviewerId":reviewer,"focusArea":focus_area,"status":"open","findings":[]});
    publish(&transaction, &value)?;
    Ok(value)
}

/// Creates or resumes a reviewer after checking its file-group journal.
///
/// # Errors
///
/// Returns an error when the journal and artifact disagree or publication fails.
pub fn initialize_with_queue(
    path: &Path,
    reviewer: &str,
    focus_area: &str,
    force: bool,
    resume: bool,
    queue_path: &Path,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    Ok(
        initialize_with_queue_result(path, reviewer, focus_area, force, resume, queue_path, clock)?
            .artifact,
    )
}

/// Creates or resumes a reviewer and reports whether an artifact was resumed.
///
/// # Errors
///
/// Returns an error when the journal and artifact disagree or publication fails.
pub fn initialize_with_queue_result(
    path: &Path,
    reviewer: &str,
    focus_area: &str,
    force: bool,
    resume: bool,
    queue_path: &Path,
    clock: &dyn Clock,
) -> Result<InitializationResult, AppError> {
    reviewer_id(reviewer)?;
    if focus_area.trim().is_empty() || (force && resume) || same_lock(path, queue_path) {
        return Err(invalid("Invalid reviewer initialization"));
    }
    let artifact_transaction = FileTransaction::open(path, clock)?;
    let existing = artifact_transaction.read_bytes()?;
    let resumed_existing = resume && existing.is_some();
    let queue_transaction = FileTransaction::open(queue_path, clock)?;
    let queue = queue_transaction
        .read_bytes()?
        .map(|bytes| parse_json(&bytes))
        .transpose()?;
    if let Some(queue) = &queue {
        validate_queue(queue, false, clock)?;
        if existing.is_none()
            && (queue.get("inProgress") != Some(&Value::Null)
                || !array(queue.get("completedGroupIds"), "completedGroupIds")?.is_empty())
        {
            return Err(invalid("Artifact is missing despite journaled work"));
        }
    } else if let Some(bytes) = &existing {
        let artifact = parse_artifact(bytes)?;
        if !array(artifact.get("findings"), "findings")?.is_empty() {
            return Err(invalid("Journal is missing despite recorded findings"));
        }
    }
    let value = if resume {
        if let Some(bytes) = existing {
            let mut artifact = parse_artifact(&bytes)?;
            if artifact.get("kind") != Some(&json!("reviewer"))
                || artifact.get("reviewerId") != Some(&json!(reviewer))
                || artifact.get("focusArea") != Some(&json!(focus_area))
            {
                return Err(invalid("Reviewer identity does not match artifact"));
            }
            artifact
                .as_object_mut()
                .ok_or_else(|| invalid("Invalid artifact"))?
                .insert("status".to_owned(), json!("open"));
            artifact
        } else {
            json!({"schemaVersion":1,"kind":"reviewer","reviewerId":reviewer,"focusArea":focus_area,"status":"open","findings":[]})
        }
    } else {
        if existing.is_some() && !force {
            return Err(invalid(
                "Finding artifact already exists; use -Resume to preserve it or -Force for a new review",
            ));
        }
        json!({"schemaVersion":1,"kind":"reviewer","reviewerId":reviewer,"focusArea":focus_area,"status":"open","findings":[]})
    };
    publish(&artifact_transaction, &value)?;
    Ok(InitializationResult {
        artifact: value,
        resumed_existing,
    })
}

/// Appends authored content and assigns an identifier without reusing history.
///
/// # Errors
///
/// Returns an error for invalid content, completed work, or file failures.
pub fn add(
    path: &Path,
    finding_path: &Path,
    origin: &str,
    source_ids: &[String],
    clock: &dyn Clock,
) -> Result<String, AppError> {
    add_with_context(path, finding_path, None, origin, source_ids, clock)
}

/// Appends a finding and validates its follow-up against a captured discussion.
///
/// # Errors
///
/// Returns an error when the finding or capture is invalid, or publication fails.
pub fn add_with_context(
    path: &Path,
    finding_path: &Path,
    context_path: Option<&Path>,
    origin: &str,
    source_ids: &[String],
    clock: &dyn Clock,
) -> Result<String, AppError> {
    if !matches!(origin, "reviewer" | "synthesis") || source_ids.iter().any(|s| s.trim().is_empty())
    {
        return Err(invalid("Invalid finding provenance"));
    }
    let content = read_content(finding_path, clock)?;
    if let Some(context_path) = context_path {
        let context = ss_ai_review_context::read_capture(context_path, clock)?;
        validate_follow_up(&content.0, &context)?;
    }
    let transaction = FileTransaction::open(path, clock)?;
    let bytes = transaction
        .read_bytes()?
        .ok_or_else(|| invalid("Finding artifact is missing"))?;
    let mut artifact = parse_artifact(&bytes)?;
    if artifact.get("status") == Some(&json!("complete")) {
        return Err(invalid(
            "Completed reviewer artifact cannot accept findings",
        ));
    }
    let id = next_id(&artifact)?;
    let record = make_record(&content, &id, origin, source_ids)?;
    artifact
        .get_mut("findings")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("Missing findings"))?
        .push(record);
    publish(&transaction, &artifact)?;
    Ok(id)
}

/// Confirms a follow-up names a replyable captured thread and its opening comment.
fn validate_follow_up(finding: &Value, context: &Value) -> Result<(), AppError> {
    let Some(follow_up) = finding.get("followUp") else {
        return Ok(());
    };
    let thread_id = text(follow_up.get("threadId"), "threadId")?;
    let comment_id = text(follow_up.get("commentId"), "commentId")?;
    let thread = array(context.get("threads"), "threads")?
        .iter()
        .find(|thread| thread.get("id").and_then(Value::as_str) == Some(thread_id))
        .ok_or_else(|| {
            fail(
                "invalidInput",
                format!("Follow-up thread '{thread_id}' is absent from capture"),
            )
        })?;
    if thread.get("openingCommentId").and_then(Value::as_str) != Some(comment_id)
        || thread.get("kind").and_then(Value::as_str) != Some("text")
    {
        return Err(invalid(
            "Follow-up must target the opening comment of a discussion thread",
        ));
    }
    match context.get("platform").and_then(Value::as_str) {
        Some("gitHub") if !thread.get("context").is_some_and(|value| !value.is_null()) => {
            Err(invalid("GitHub follow-up must target a review thread"))
        }
        Some("gitHub" | "azureDevOps") => Ok(()),
        _ => Err(invalid("Unsupported capture platform")),
    }
}

/// Builds an index from the reviewer artifacts in a directory.
///
/// # Errors
///
/// Returns an error for missing, incompatible, or ambiguous reviewer artifacts.
pub fn build_index(directory: &Path, output: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    Ok(build_index_result(directory, output, clock)?.artifact)
}

/// Builds an index and reports counts from the same input snapshot.
///
/// # Errors
///
/// Returns an error for missing, incompatible, or ambiguous reviewer artifacts.
pub fn build_index_result(
    directory: &Path,
    output: &Path,
    clock: &dyn Clock,
) -> Result<IndexResult, AppError> {
    let (sources, artifact_count) = reviewer_sources(directory, clock)?;
    let mut records = Vec::new();
    for (metadata, finding) in &sources {
        let anchor = finding
            .get("anchor")
            .ok_or_else(|| invalid("Missing anchor"))?;
        let mut paths = Vec::new();
        if let Some(locations) = finding.get("impactedLocations") {
            for location in array(Some(locations), "impactedLocations")? {
                let path = text(location.get("path"), "path")?;
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        let mut record = json!({
            "id": finding["id"], "reviewerId": metadata["reviewerId"],
            "focusArea": metadata["focusArea"], "title": finding["title"],
            "description": finding["description"], "recommendedAction": finding["recommendedAction"],
            "anchorPath": anchor["path"], "impactedPaths": paths,
        });
        if let Some(follow_up) = finding.get("followUp") {
            record
                .as_object_mut()
                .ok_or_else(|| invalid("Invalid index entry"))?
                .insert(
                    "followUpThreadId".to_owned(),
                    follow_up
                        .get("threadId")
                        .ok_or_else(|| invalid("Missing threadId"))?
                        .clone(),
                );
        }
        records.push(record);
    }
    let document = json!({"schemaVersion": 1, "findings": records});
    validate_index(&document)?;
    let transaction = FileTransaction::open(output, clock)?;
    transaction.write_bytes(ss_ai_review_context::powershell_json(&document)?.as_bytes())?;
    Ok(IndexResult {
        artifact: document,
        artifact_count,
        finding_count: sources.len(),
        output_path: output.to_path_buf(),
    })
}

/// Validates candidate identifiers against a strictly shaped finding index.
///
/// # Errors
///
/// Returns an error when a plan or index is malformed or names unknown findings.
pub fn validate_candidates(
    candidates: &Path,
    index: &Path,
    clock: &dyn Clock,
) -> Result<usize, AppError> {
    let plan = crate::workflow_plan::read(PlanKind::MergeCandidates, candidates, clock)?;
    let bytes = read_bytes(index, clock)?.ok_or_else(|| invalid("Finding index is missing"))?;
    let index = parse_json(&bytes)?;
    let ids = validate_index(&index)?;
    for item in plan.items() {
        let PlanItem::Cluster(cluster) = item else {
            return Err(invalid("Invalid candidate plan"));
        };
        if cluster.finding_ids.iter().any(|id| !ids.contains(id)) {
            return Err(invalid("Candidate names unknown finding"));
        }
    }
    Ok(plan.items().len())
}

/// Checks the index envelope and entries before reading or publishing them.
fn validate_index(index: &Value) -> Result<HashSet<String>, AppError> {
    let fields = object(index, &["schemaVersion", "findings"])?;
    if fields.get("schemaVersion") != Some(&json!(1)) {
        return Err(invalid("Invalid index version"));
    }
    let mut ids = HashSet::new();
    for entry in array(fields.get("findings"), "findings")? {
        let fields = object(
            entry,
            &[
                "id",
                "reviewerId",
                "focusArea",
                "title",
                "description",
                "recommendedAction",
                "anchorPath",
                "impactedPaths",
                "followUpThreadId",
            ],
        )?;
        for field in [
            "id",
            "reviewerId",
            "focusArea",
            "title",
            "description",
            "recommendedAction",
            "anchorPath",
        ] {
            text(fields.get(field), field)?;
        }
        let reviewer = text(fields.get("reviewerId"), "reviewerId")?;
        if !reviewer
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            return Err(invalid("Invalid index reviewer identifier"));
        }
        if let Some(thread) = fields.get("followUpThreadId") {
            text(Some(thread), "followUpThreadId")?;
        }
        for path in array(fields.get("impactedPaths"), "impactedPaths")? {
            text(Some(path), "impactedPaths")?;
        }
        if !ids.insert(text(fields.get("id"), "id")?.to_owned()) {
            return Err(invalid("Duplicate finding identifier in index"));
        }
    }
    Ok(ids)
}

/// Matches the filesystem provider's hidden-file filtering for directory reads.
#[cfg(windows)]
fn hidden_entry(entry: &fs::DirEntry) -> Result<bool, AppError> {
    // Windows marks hidden entries with FILE_ATTRIBUTE_HIDDEN.
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    let metadata = entry
        .metadata()
        .map_err(|error| ss_ai_runtime::caused_by("io", "Cannot inspect directory entry", error))?;
    Ok(metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
}

/// Matches the filesystem provider's hidden-file filtering for directory reads.
#[cfg(not(windows))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "The Windows implementation can fail when reading file attributes"
)]
fn hidden_entry(entry: &fs::DirEntry) -> Result<bool, AppError> {
    Ok(entry.file_name().to_string_lossy().starts_with('.'))
}

/// Reads reviewer records in filename order without allowing ambiguous identities.
fn reviewer_sources(
    directory: &Path,
    clock: &dyn Clock,
) -> Result<(Vec<(Value, Value)>, usize), AppError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| ss_ai_runtime::caused_by("io", "Cannot read reviewer directory", error))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            ss_ai_runtime::caused_by("io", "Cannot read reviewer directory entry", error)
        })?;
        if hidden_entry(&entry)? {
            continue;
        }
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            && path.is_file()
        {
            paths.push(path);
        }
    }
    paths.sort();
    if paths.is_empty() {
        return Err(invalid("No reviewer artifacts"));
    }
    let artifact_count = paths.len();
    let mut result = Vec::new();
    let mut ids = HashSet::new();
    for path in paths {
        let artifact = read_artifact(&path, clock)?;
        if artifact.get("kind") != Some(&json!("reviewer")) {
            return Err(invalid("Expected reviewer artifact"));
        }
        // The index needs only these fields; copying the full artifact per finding
        // would also repeatedly copy every finding and the removal history.
        let metadata = json!({
            "reviewerId": artifact.get("reviewerId"),
            "focusArea": artifact.get("focusArea"),
        });
        for finding in array(artifact.get("findings"), "findings")? {
            if !ids.insert(text(finding.get("id"), "id")?.to_owned()) {
                return Err(invalid("Duplicate finding identifier across artifacts"));
            }
            result.push((metadata.clone(), finding.clone()));
        }
    }
    Ok((result, artifact_count))
}

/// Consolidates reviewer artifacts and a validated optional merge plan.
///
/// Unmentioned findings are carried forward. Merged findings must cover all
/// source locations and preserve any follow-up destination.
///
/// # Errors
///
/// Returns an error when input is invalid, ambiguous, or cannot be published.
pub fn consolidate(
    directory: &Path,
    plan_path: Option<&Path>,
    output: &Path,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    Ok(consolidate_result(directory, plan_path, output, clock)?.artifact)
}

/// Consolidates findings and reports the result from the published document.
///
/// # Errors
///
/// Returns an error when input is invalid, ambiguous, or cannot be published.
pub fn consolidate_result(
    directory: &Path,
    plan_path: Option<&Path>,
    output: &Path,
    clock: &dyn Clock,
) -> Result<ConsolidationResult, AppError> {
    let transaction = FileTransaction::open(output, clock)?;
    let (sources, _) = reviewer_sources(directory, clock)?;
    let mut positions = HashMap::new();
    for (index, (_, finding)) in sources.iter().enumerate() {
        positions.insert(text(finding.get("id"), "id")?.to_owned(), index);
    }
    let mut groups = HashMap::new();
    let mut claimed = HashSet::new();
    if let Some(plan_path) = plan_path {
        let plan = crate::workflow_plan::read(PlanKind::MergePlan, plan_path, clock)?;
        for item in plan.items() {
            let PlanItem::Merge(group) = item else {
                return Err(invalid("Invalid merge plan"));
            };
            let mut first = usize::MAX;
            let mut replacements = Vec::new();
            for id in &group.source_finding_ids {
                let position = *positions
                    .get(id)
                    .ok_or_else(|| invalid("Merge references unknown finding"))?;
                if !claimed.insert(id.clone()) {
                    return Err(fail(
                        "invalidInput",
                        format!("Finding '{id}' appears in multiple merges"),
                    ));
                }
                first = first.min(position);
                replacements.push(
                    &sources
                        .get(position)
                        .ok_or_else(|| invalid("Missing merge source"))?
                        .1,
                );
            }
            let path = Path::new(&group.finding_path);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                plan_path
                    .parent()
                    .ok_or_else(|| invalid("Merge plan requires a parent"))?
                    .join(path)
            };
            let mut content = read_content(&path, clock)?;
            validate_merge(&mut content, &replacements)?;
            let origin = if replacements
                .iter()
                .any(|finding| finding.get("origin") == Some(&json!("synthesis")))
            {
                "synthesis"
            } else {
                "reviewer"
            };
            groups.insert(first, (content, origin, group.source_finding_ids.clone()));
        }
    }
    let mut artifact = json!({"schemaVersion":1,"kind":"consolidated","findings":[],"removed":[]});
    for (position, (_, finding)) in sources.iter().enumerate() {
        let source_id = text(finding.get("id"), "id")?;
        let record = if let Some((content, origin, ids)) = groups.remove(&position) {
            Some((content, origin.to_owned(), ids))
        } else if !claimed.contains(source_id) {
            let mut content = finding.clone();
            let fields = content
                .as_object_mut()
                .ok_or_else(|| invalid("Invalid finding"))?;
            for name in ["schemaVersion", "id", "origin", "sourceFindingIds"] {
                fields.remove(name);
            }
            Some((
                validated_content(content)?,
                text(finding.get("origin"), "origin")?.to_owned(),
                vec![source_id.to_owned()],
            ))
        } else {
            None
        };
        if let Some((mut content, origin, ids)) = record {
            normalize_content(&mut content.0)?;
            let id = next_id(&artifact)?;
            let record = make_record(&content, &id, &origin, &ids)?;
            artifact
                .get_mut("findings")
                .ok_or_else(|| invalid("Missing findings"))?
                .as_array_mut()
                .ok_or_else(|| invalid("Missing findings"))?
                .push(record);
        }
    }
    let findings = array(artifact.get("findings"), "findings")?;
    let finding_count = findings.len();
    let merged_count = findings
        .iter()
        .filter(|finding| {
            finding
                .get("sourceFindingIds")
                .and_then(Value::as_array)
                .is_some_and(|ids| ids.len() > 1)
        })
        .count();
    publish(&transaction, &artifact)?;
    Ok(ConsolidationResult {
        artifact,
        finding_count,
        merged_count,
        carried_over_count: finding_count
            .checked_sub(merged_count)
            .ok_or_else(|| invalid("Invalid consolidation count"))?,
        output_path: output.to_path_buf(),
    })
}

/// Prevents a merge from discarding positions or changing a reply destination.
fn validate_merge(content: &mut ValidatedContent, sources: &[&Value]) -> Result<(), AppError> {
    let content = &mut content.0;
    let covering = coverage(content)?;
    let mut follow_up: Option<Value> = None;
    for source in sources {
        for required in coverage(source)? {
            if !covering
                .iter()
                .any(|candidate| location_covers(candidate, &required))
            {
                return Err(invalid(
                    "Merged finding does not cover every source location",
                ));
            }
        }
        if let Some(target) = source.get("followUp") {
            if follow_up
                .as_ref()
                .is_some_and(|existing| existing != target)
            {
                return Err(invalid("Cannot merge different follow-up destinations"));
            }
            follow_up = Some(target.clone());
        }
    }
    if let Some(target) = follow_up {
        if content
            .get("followUp")
            .is_some_and(|existing| existing != &target)
        {
            return Err(invalid("Merged finding changes follow-up destination"));
        }
        content
            .as_object_mut()
            .ok_or_else(|| invalid("Invalid merged finding"))?
            .insert("followUp".to_owned(), target);
    }
    Ok(())
}

/// Produces comparable canonical positions from a finding.
fn coverage(finding: &Value) -> Result<Vec<Value>, AppError> {
    let mut result = Vec::new();
    let mut anchor = finding
        .get("anchor")
        .ok_or_else(|| invalid("Missing anchor"))?
        .clone();
    normalize_location(&mut anchor)?;
    result.push(anchor);
    if let Some(locations) = finding.get("impactedLocations") {
        for location in array(Some(locations), "impactedLocations")? {
            let mut location = location.clone();
            normalize_location(&mut location)?;
            result.push(location);
        }
    }
    Ok(result)
}

/// Whole-file locations cover the file; line ranges must contain the same side.
fn location_covers(candidate: &Value, required: &Value) -> bool {
    if candidate.get("path") != required.get("path") {
        return false;
    }
    match (
        candidate.get("startLine").and_then(Value::as_i64),
        required.get("startLine").and_then(Value::as_i64),
    ) {
        (None, _) => true,
        (Some(start), Some(required_start)) => {
            candidate.get("side") == required.get("side")
                && start <= required_start
                && candidate.get("endLine").and_then(Value::as_i64)
                    >= required.get("endLine").and_then(Value::as_i64)
        }
        _ => false,
    }
}

/// Replaces authored content without changing identity or provenance.
///
/// # Errors
///
/// Returns an error if any request is invalid or a finding is missing.
pub fn update(
    path: &Path,
    updates: &[(String, PathBuf)],
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let mut seen = HashSet::new();
    let mut contents = Vec::new();
    for (id, source) in updates {
        if !seen.insert(id) {
            return Err(invalid("Duplicate update identifier"));
        }
        contents.push(read_content(source, clock)?);
    }
    let transaction = FileTransaction::open(path, clock)?;
    let bytes = transaction
        .read_bytes()?
        .ok_or_else(|| invalid("Finding artifact is missing"))?;
    let mut artifact = parse_artifact(&bytes)?;
    let findings = artifact
        .get_mut("findings")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("Missing findings"))?;
    for ((id, _), content) in updates.iter().zip(contents) {
        let index = findings
            .iter()
            .position(|finding| finding.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| invalid("Finding to update does not exist"))?;
        let old = findings
            .get(index)
            .ok_or_else(|| invalid("Finding to update does not exist"))?;
        let origin = text(old.get("origin"), "origin")?;
        let sources = array(old.get("sourceFindingIds"), "sourceFindingIds")?
            .iter()
            .map(|value| text(Some(value), "sourceFindingIds").map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        let replacement = make_record(&content, id, origin, &sources)?;
        *findings
            .get_mut(index)
            .ok_or_else(|| invalid("Finding to update does not exist"))? = replacement;
    }
    publish(&transaction, &artifact)
}

/// Moves findings into immutable removal history in one transaction.
///
/// # Errors
///
/// Returns an error if any request is invalid or a finding is missing.
pub fn remove(
    path: &Path,
    removals: &[(String, String)],
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let mut seen = HashSet::new();
    if removals
        .iter()
        .any(|(id, reason)| id.trim().is_empty() || reason.trim().is_empty() || !seen.insert(id))
    {
        return Err(invalid("Invalid or repeated removal"));
    }
    let transaction = FileTransaction::open(path, clock)?;
    // Match .NET's round-trip UTC timestamp representation in removal history.
    let removed_utc = format!("{:.7}", clock.now());
    let bytes = transaction
        .read_bytes()?
        .ok_or_else(|| invalid("Finding artifact is missing"))?;
    let mut artifact = parse_artifact(&bytes)?;
    for (id, reason) in removals {
        let findings = artifact
            .get_mut("findings")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| invalid("Missing findings"))?;
        let index = findings
            .iter()
            .position(|finding| finding.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| invalid("Finding to remove does not exist"))?;
        let finding = findings.remove(index);
        let object = artifact
            .as_object_mut()
            .ok_or_else(|| invalid("Invalid artifact"))?;
        let removed = object.entry("removed").or_insert_with(|| json!([]));
        removed
            .as_array_mut()
            .ok_or_else(|| invalid("Invalid removal history"))?
            .push(json!({"finding": finding, "reason":reason, "removedUtc":removed_utc}));
    }
    publish(&transaction, &artifact)
}

/// Completes a reviewer only after its file-group journal is complete.
///
/// The artifact and journal locks remain held while validating receipts and
/// publishing the completion marker.
///
/// # Errors
///
/// Returns an error for incomplete, malformed, or unverified journal work.
pub fn complete(path: &Path, queue_path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    if same_lock(path, queue_path) {
        return Err(invalid("Artifact and journal paths must differ"));
    }
    let artifact_transaction = FileTransaction::open(path, clock)?;
    let bytes = artifact_transaction
        .read_bytes()?
        .ok_or_else(|| invalid("Finding artifact is missing"))?;
    let mut artifact = parse_artifact(&bytes)?;
    if artifact.get("kind") != Some(&json!("reviewer")) {
        return Err(invalid("Only reviewer artifacts can be completed"));
    }
    let queue_transaction = FileTransaction::open(queue_path, clock)?;
    let bytes = queue_transaction
        .read_bytes()?
        .ok_or_else(|| invalid("File-group journal is missing"))?;
    let queue = parse_json(&bytes)?;
    validate_queue(&queue, true, clock)?;
    artifact
        .as_object_mut()
        .ok_or_else(|| invalid("Invalid artifact"))?
        .insert("status".to_owned(), json!("complete"));
    publish(&artifact_transaction, &artifact)?;
    Ok(artifact)
}

/// Avoids recursively acquiring a sibling lock through equivalent paths.
fn same_lock(path: &Path, queue_path: &Path) -> bool {
    let lock_identity = |path: &Path| {
        Some((
            fs::canonicalize(path.parent()?).ok()?,
            path.file_name()?.to_owned(),
        ))
    };
    path == queue_path
        || lock_identity(path)
            .zip(lock_identity(queue_path))
            .is_some_and(|(artifact, queue)| artifact == queue)
}

/// Validates the journal schema and every persisted batch receipt.
fn validate_queue(
    queue: &Value,
    require_complete: bool,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let fields = object(
        queue,
        &[
            "schemaVersion",
            "kind",
            "items",
            "inProgress",
            "completedGroupIds",
            "assessments",
            "snapshot",
            "failure",
            "batchReceipts",
        ],
    )?;
    let items = array(fields.get("items"), "items")?;
    let in_progress = fields
        .get("inProgress")
        .ok_or_else(|| invalid("Missing in-progress group"))?;
    if fields.get("schemaVersion") != Some(&json!(3))
        || fields.get("kind") != Some(&json!("fileGroups"))
    {
        return Err(invalid("Invalid file-group journal"));
    }
    if let Some(failure) = fields.get("failure").filter(|value| !value.is_null()) {
        text(Some(failure), "failure")?;
    }
    if require_complete
        && (!items.is_empty()
            || !in_progress.is_null()
            || fields.get("failure").is_some_and(|value| !value.is_null()))
    {
        return Err(invalid("File-group journal is not complete"));
    }
    if let Some(snapshot) = fields.get("snapshot").filter(|value| !value.is_null()) {
        let snapshot = object(snapshot, &["inventorySha256", "fileGroupsSha256"])?;
        for field in ["inventorySha256", "fileGroupsSha256"] {
            uppercase_hash(text(snapshot.get(field), field)?)?;
        }
    }
    let mut completed = HashSet::new();
    for group in array(fields.get("completedGroupIds"), "completedGroupIds")? {
        let id = text(Some(group), "groupId")?;
        schema_id(id)?;
        if !completed.insert(id.to_owned()) {
            return Err(invalid("Duplicate completed group identifier"));
        }
    }
    let mut all_groups = completed.clone();
    for group in items
        .iter()
        .chain((!in_progress.is_null()).then_some(in_progress))
    {
        let group = object(group, &["groupId", "files"])?;
        let id = text(group.get("groupId"), "groupId")?;
        schema_id(id)?;
        if !all_groups.insert(id.to_owned()) {
            return Err(invalid("Duplicate journal group"));
        }
        let files = array(group.get("files"), "files")?;
        if files.is_empty() {
            return Err(invalid("Journal group has no files"));
        }
        for file in files {
            let file = object(file, &["path", "sizeBytes", "change"])?;
            text(file.get("path"), "path")?;
            if file.get("sizeBytes").and_then(Value::as_u64).is_none()
                || !matches!(
                    file.get("change").and_then(Value::as_str),
                    Some("new" | "modified" | "deleted")
                )
            {
                return Err(invalid("Invalid journal file"));
            }
        }
    }
    let mut assessed = HashSet::new();
    for assessment in array(fields.get("assessments"), "assessments")? {
        let assessment = object(
            assessment,
            &["groupId", "path", "disposition", "scope", "reason"],
        )?;
        schema_id(text(assessment.get("groupId"), "groupId")?)?;
        let path = text(assessment.get("path"), "path")?;
        if !Path::new(path).is_absolute() || !assessed.insert(path.to_owned()) {
            return Err(invalid("Invalid or duplicate assessment path"));
        }
        let disposition = text(assessment.get("disposition"), "disposition")?;
        let scope = assessment
            .get("scope")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Invalid assessment scope"))?;
        let reason = assessment
            .get("reason")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Invalid assessment reason"))?;
        if !matches!(disposition, "reviewed" | "excluded")
            || (disposition == "excluded" && (scope.trim().is_empty() || reason.trim().is_empty()))
        {
            return Err(invalid("Invalid assessment disposition"));
        }
    }
    let mut receipt_groups = HashSet::new();
    if let Some(receipts) = fields.get("batchReceipts") {
        for receipt in array(Some(receipts), "batchReceipts")? {
            let receipt = object(
                receipt,
                &["groupId", "sha256", "artifactPath", "findingIds"],
            )?;
            let group = text(receipt.get("groupId"), "groupId")?;
            schema_id(group)?;
            uppercase_hash(text(receipt.get("sha256"), "sha256")?)?;
            if !completed.contains(group) || !receipt_groups.insert(group.to_owned()) {
                return Err(invalid("Invalid batch receipt group"));
            }
            let artifact_path = text(receipt.get("artifactPath"), "artifactPath")?;
            if !Path::new(artifact_path).is_absolute() {
                return Err(invalid("Batch receipt artifact path must be absolute"));
            }
            let artifact = read_artifact(Path::new(artifact_path), clock)?;
            if artifact.get("kind") != Some(&json!("reviewer")) {
                return Err(invalid("Batch receipt requires a reviewer artifact"));
            }
            let mut persisted = HashSet::new();
            for finding in array(artifact.get("findings"), "findings")? {
                persisted.insert(text(finding.get("id"), "id")?.to_owned());
            }
            if let Some(removed) = artifact.get("removed") {
                for entry in array(Some(removed), "removed")? {
                    persisted.insert(
                        text(
                            entry.get("finding").and_then(|finding| finding.get("id")),
                            "id",
                        )?
                        .to_owned(),
                    );
                }
            }
            let mut receipt_ids = HashSet::new();
            for finding in array(receipt.get("findingIds"), "findingIds")? {
                let id = text(Some(finding), "findingId")?;
                if !receipt_ids.insert(id) || !persisted.contains(id) {
                    return Err(invalid("Batch receipt names missing or duplicate finding"));
                }
            }
        }
    }
    Ok(())
}

/// Validates a file-group schema identifier.
fn schema_id(id: &str) -> Result<(), AppError> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(invalid("Invalid journal identifier"));
    }
    Ok(())
}

/// Validates a journal's uppercase SHA-256 representation.
fn uppercase_hash(hash: &str) -> Result<(), AppError> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
    {
        return Err(invalid("Invalid journal hash"));
    }
    Ok(())
}

/// Ensures every published artifact is valid before atomic replacement.
fn publish(transaction: &FileTransaction<'_>, value: &Value) -> Result<(), AppError> {
    validate_artifact(value)?;
    let text = ss_ai_review_context::powershell_json(value)?;
    transaction.write_bytes(text.as_bytes())
}

/// Reads authored finding content and applies its canonical stored form.
fn read_content(path: &Path, clock: &dyn Clock) -> Result<ValidatedContent, AppError> {
    let bytes = read_bytes(path, clock)?.ok_or_else(|| {
        fail(
            "invalidInput",
            format!("Finding content '{}' is missing", path.display()),
        )
    })?;
    let context = |error| {
        fail(
            "invalidInput",
            format!("Finding content '{}': {error}", path.display()),
        )
    };
    let value = parse_json(&bytes).map_err(context)?;
    let mut value = validated_content(value).map_err(context)?;
    normalize_content(&mut value.0).map_err(context)?;
    Ok(value)
}

/// Establishes the authored-content boundary before adding tool-owned fields.
fn validated_content(value: Value) -> Result<ValidatedContent, AppError> {
    validate_content(&value, false)?;
    Ok(ValidatedContent(value))
}

/// Constructs the tool-owned portion without trusting author-supplied identity.
fn make_record(
    content: &ValidatedContent,
    id: &str,
    origin: &str,
    source_ids: &[String],
) -> Result<Value, AppError> {
    let mut record = Map::new();
    record.insert("schemaVersion".into(), json!(1));
    record.insert("id".into(), json!(id));
    record.insert("sourceFindingIds".into(), json!(source_ids));
    record.insert("origin".into(), json!(origin));
    record.extend(
        content
            .0
            .as_object()
            .ok_or_else(|| invalid("Invalid finding content"))?
            .clone(),
    );
    let record = Value::Object(record);
    validate_record(&record)?;
    Ok(record)
}

/// Keeps discarded identifiers reserved when assigning the next ordinal.
fn next_id(artifact: &Value) -> Result<String, AppError> {
    let prefix = match text(artifact.get("kind"), "kind")? {
        "reviewer" => format!("{}-", text(artifact.get("reviewerId"), "reviewerId")?),
        "consolidated" => "F".to_owned(),
        _ => return Err(invalid("Invalid artifact kind")),
    };
    let active = array(artifact.get("findings"), "findings")?.iter();
    let removed = artifact
        .get("removed")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("finding"));
    let highest = active
        .chain(removed)
        .filter_map(|finding| finding.get("id").and_then(Value::as_str))
        .filter_map(|id| id.strip_prefix(&prefix))
        .filter_map(|suffix| suffix.parse::<i32>().ok())
        .max()
        .unwrap_or(0);
    let next = highest
        .checked_add(1)
        .ok_or_else(|| invalid("Finding identifier space exhausted"))?;
    Ok(format!("{prefix}{next}"))
}

/// Validates the tool-owned fields and authored content of a stored finding.
fn validate_record(value: &Value) -> Result<(), AppError> {
    validate_content(value, true)?;
    if value.get("schemaVersion") != Some(&json!(1)) {
        return Err(invalid("Invalid finding version"));
    }
    text(value.get("id"), "id")?;
    if !matches!(
        text(value.get("origin"), "origin")?,
        "reviewer" | "synthesis"
    ) {
        return Err(invalid("Invalid finding origin"));
    }
    for id in array(value.get("sourceFindingIds"), "sourceFindingIds")? {
        text(Some(id), "sourceFindingIds")?;
    }
    Ok(())
}

/// Checks known fields, prose, locations and follow-up identifiers.
fn validate_content(value: &Value, record: bool) -> Result<(), AppError> {
    let allowed = [
        "schemaVersion",
        "id",
        "sourceFindingIds",
        "origin",
        "title",
        "description",
        "reasoning",
        "consequence",
        "recommendedAction",
        "references",
        "impactedLocations",
        "anchor",
        "followUp",
    ];
    let fields = object(value, &allowed)?;
    if !record
        && ["schemaVersion", "id", "sourceFindingIds", "origin"]
            .iter()
            .any(|name| fields.contains_key(*name))
    {
        return Err(invalid("Authored content cannot assign provenance"));
    }
    for name in [
        "title",
        "description",
        "reasoning",
        "consequence",
        "recommendedAction",
    ] {
        let prose = text(fields.get(name), name)?;
        // Tracker title limits motivate the bound; UTF-16 units match the
        // retained .NET string-length check in FindingModel.ps1.
        if (name == "title" && prose.encode_utf16().count() > 255)
            || prose
                .chars()
                .any(|c| (c as u32) < 32 && (name == "title" || !matches!(c, '\t' | '\n')))
        {
            return Err(invalid("Invalid finding prose"));
        }
    }
    location(
        fields
            .get("anchor")
            .ok_or_else(|| invalid("Missing anchor"))?,
        true,
    )?;
    if let Some(references) = fields.get("references") {
        for reference in array(Some(references), "references")? {
            let reference = object(reference, &["text", "url"])?;
            text(reference.get("text"), "reference text")?;
            if let Some(url) = reference.get("url") {
                text(Some(url), "reference url")?;
            }
        }
    }
    if let Some(locations) = fields.get("impactedLocations") {
        for entry in array(Some(locations), "impactedLocations")? {
            location(entry, false)?;
        }
    }
    if let Some(follow_up) = fields.get("followUp") {
        let follow_up = object(follow_up, &["threadId", "commentId"])?;
        text(follow_up.get("threadId"), "threadId")?;
        text(follow_up.get("commentId"), "commentId")?;
    }
    Ok(())
}

/// Validates a repository-relative anchor or impacted location.
fn location(value: &Value, anchor: bool) -> Result<(), AppError> {
    let fields = object(
        value,
        if anchor {
            &["kind", "path", "startLine", "endLine", "side"]
        } else {
            &["path", "startLine", "endLine", "side"]
        },
    )?;
    canonical_path(text(fields.get("path"), "path")?)?;
    let kind = if anchor {
        text(fields.get("kind"), "kind")?
    } else {
        "location"
    };
    if !matches!(kind, "line" | "file" | "location") {
        return Err(invalid("Invalid anchor kind"));
    }
    let start = fields
        .get("startLine")
        .map(|v| {
            v.as_i64()
                .filter(|n| *n > 0 && *n <= i64::from(i32::MAX))
                .ok_or_else(|| invalid("Invalid start line"))
        })
        .transpose()?;
    let end = fields
        .get("endLine")
        .map(|v| {
            v.as_i64()
                .filter(|n| *n > 0 && *n <= i64::from(i32::MAX))
                .ok_or_else(|| invalid("Invalid end line"))
        })
        .transpose()?;
    if (kind == "line" && start.is_none())
        || (kind == "file" && (start.is_some() || end.is_some() || fields.contains_key("side")))
        || (start.is_none() && (end.is_some() || fields.contains_key("side")))
        || matches!((start, end), (Some(a), Some(b)) if b < a)
    {
        return Err(invalid("Invalid finding line range"));
    }
    if let Some(side) = fields.get("side")
        && !matches!(side.as_str(), Some("left" | "right"))
    {
        return Err(invalid("Invalid diff side"));
    }
    Ok(())
}

/// Normalizes repository paths and supplies the stored line defaults.
fn normalize_content(value: &mut Value) -> Result<(), AppError> {
    let value = value
        .as_object_mut()
        .ok_or_else(|| invalid("Invalid finding"))?;
    for name in ["references", "impactedLocations"] {
        value.entry(name).or_insert_with(|| json!([]));
    }
    normalize_location(
        value
            .get_mut("anchor")
            .ok_or_else(|| invalid("Missing anchor"))?,
    )?;
    for location in value
        .get_mut("impactedLocations")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("Invalid locations"))?
    {
        normalize_location(location)?;
    }
    Ok(())
}

/// Normalizes a single location after validation.
fn normalize_location(value: &mut Value) -> Result<(), AppError> {
    let value = value
        .as_object_mut()
        .ok_or_else(|| invalid("Invalid location"))?;
    let path = canonical_path(text(value.get("path"), "path")?)?;
    value.insert("path".into(), json!(path));
    if let Some(start) = value.get("startLine").cloned() {
        value.entry("endLine").or_insert(start);
        value.entry("side").or_insert_with(|| json!("right"));
    }
    Ok(())
}

/// Produces the path spelling shared by all finding consumers.
fn canonical_path(path: &str) -> Result<String, AppError> {
    if path.starts_with('/')
        || path.contains('\\')
        || path.as_bytes().get(1) == Some(&b':')
            && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
    {
        return Err(invalid("Finding path must be repository-relative"));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            ".." => return Err(invalid("Finding path escapes repository")),
            "" | "." => {}
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(invalid("Finding path is empty"));
    }
    Ok(parts.join("/"))
}

/// Checks a reviewer's identifier before it can become a filename.
fn reviewer_id(id: &str) -> Result<(), AppError> {
    if id.trim().is_empty() || id.contains("..") || id.chars().any(|c| "\\/:*?\"<>|".contains(c)) {
        return Err(invalid("Invalid reviewer identifier"));
    }
    Ok(())
}

/// Checks object shape without silently ignoring unrecognized fields.
fn object<'a>(value: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>, AppError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("Expected object"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(fail("invalidInput", format!("Unknown field '{key}'")));
    }
    Ok(object)
}

/// Requires an explicit JSON array.
fn array<'a>(value: Option<&'a Value>, name: &str) -> Result<&'a Vec<Value>, AppError> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| fail("invalidInput", format!("Expected array '{name}'")))
}

/// Requires non-whitespace text without coercion.
fn text<'a>(value: Option<&'a Value>, name: &str) -> Result<&'a str, AppError> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            fail(
                "invalidInput",
                format!("Expected non-empty string '{name}'"),
            )
        })
}

/// Classifies invalid input consistently with the runtime.
fn invalid(message: &str) -> AppError {
    fail("invalidInput", message)
}
