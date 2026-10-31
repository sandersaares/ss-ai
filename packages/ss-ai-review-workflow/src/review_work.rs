//! Immutable review-work preparation and durable coverage journals.
#![allow(
    clippy::indexing_slicing,
    reason = "JSON mutations follow schema validation; dynamic-programming indexes stay within their allocated tables"
)]
#![allow(
    clippy::arithmetic_side_effects,
    reason = "Counters are bounded by allocated collection lengths and group sizes by validated integer inputs"
)]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::mem;
use std::path::{Component, Path, PathBuf};

use ohno::AppError;
use serde_json::{Map, Value, json};
use ss_ai_review_context::{bytes_hash, powershell_json};
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail, read_bytes};

use crate::findings;
use crate::workflow_plan::parse_json;

/// Keeps manifest validation attached to data crossing workflow module boundaries.
pub(crate) struct ValidatedManifest(Value);

impl ValidatedManifest {
    /// Exposes the immutable record after its schema and assignments are checked.
    pub(crate) fn as_value(&self) -> &Value {
        &self.0
    }
}

/// Publishes a validated inventory without replacing existing input by default.
///
/// # Errors
///
/// Returns an error when input is invalid, publication conflicts, or I/O fails.
pub fn set_input_inventory(
    inventory: &Path,
    output: &Path,
    replace: bool,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let output = full_path(output)?;
    let transaction = FileTransaction::open(&output, clock)?;
    let records = read_inventory(inventory, clock)?;
    publish_input(&transaction, &output, &records, replace)
}

/// Publishes groups only after checking their complete inventory coverage.
///
/// # Errors
///
/// Returns an error when input is invalid, publication conflicts, or I/O fails.
pub fn set_input_groups(
    inventory: &Path,
    groups: &Path,
    output: &Path,
    replace: bool,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let output = full_path(output)?;
    let transaction = FileTransaction::open(&output, clock)?;
    let files = read_inventory(inventory, clock)?;
    let groups = read_groups(groups, &files, clock)?;
    publish_input(&transaction, &output, &groups, replace)
}

/// Creates the immutable authority and its private reviewer journals.
///
/// # Errors
///
/// Returns an error when preparation is invalid or any destination already exists.
pub fn create(
    inventory: &Path,
    groups: &Path,
    focus_areas: &Path,
    output: &Path,
    repository_root: Option<&Path>,
    artifact_directory: Option<&Path>,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    if repository_root.is_some() != artifact_directory.is_some() {
        return Err(invalid(
            "Applicability preparation requires repository and artifact directories",
        ));
    }
    let focus_path = full_path(focus_areas)?;
    let _focus_lock = FileTransaction::open(&focus_path, clock)?;
    let files = read_inventory(inventory, clock)?;
    let groups = read_groups(groups, &files, clock)?;
    let mut areas = Vec::new();
    for entry in fs::read_dir(&focus_path)
        .map_err(|error| caused_by("io", "Cannot read focus areas", error))?
    {
        let entry =
            entry.map_err(|error| caused_by("io", "Cannot read focus area entry", error))?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("md"))
        {
            let text = read_text(&path, clock)?;
            if text.trim().is_empty() {
                return Err(invalid("Focus area is empty"));
            }
            areas.push((path, text));
        }
    }
    areas.sort_by(|left, right| left.0.file_name().cmp(&right.0.file_name()));
    if areas.is_empty() {
        return Err(invalid("No focus areas found"));
    }
    let repository = repository_root.map(full_path).transpose()?;
    if let Some(repository) = &repository
        && !repository.is_dir()
    {
        return Err(invalid("Repository root must be a directory"));
    }
    let artifact_directory = artifact_directory.map(full_path).transpose()?;
    let maps = if repository.is_some() {
        areas
            .iter()
            .map(|(_, text)| read_applicability(text))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let root = full_path(output)?;
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| caused_by("io", "Cannot create output parent", error))?;
    }
    #[expect(
        clippy::create_dir,
        reason = "Preparation must reject an existing output directory"
    )]
    fs::create_dir(&root)
        .map_err(|error| caused_by("io", "Cannot create new review-work directory", error))?;
    let inventory_path = root.join("file-inventory.json");
    let groups_path = root.join("file-inventory-grouped.json");
    let queue_path = root.join("focus-areas.json");
    let manifest_path = root.join("manifest.json");
    write_new(&inventory_path, &files, clock)?;
    write_new(&groups_path, &groups, clock)?;
    let inputs = read_inputs(&inventory_path, &groups_path, clock)?;
    let mut reviewers = Vec::new();
    let mut excluded_count = 0_usize;
    for (index, (area, text)) in areas.iter().enumerate() {
        let id = format!("focus-{:03}", index + 1);
        let journal_path = root.join("file-groups").join(format!("{id}.json"));
        let mut reviewer = json!({"reviewerId":id,"focusArea":path_text(area)?,"fileGroupQueuePath":path_text(&journal_path)?});
        if let (Some(repository), Some(artifact_directory)) = (&repository, &artifact_directory) {
            let excluded = scope_exclusions(
                maps.get(index).and_then(Option::as_ref),
                repository,
                &groups,
            )?;
            let all_excluded = excluded.len() == array(Some(&files), "files")?.len();
            let artifact_path = artifact_directory.join(format!("{id}.json"));
            reviewer["dispatch"] = json!(if all_excluded { "excluded" } else { "review" });
            reviewer["focusAreaSha256"] = json!(bytes_hash(text.as_bytes()));
            reviewer["artifactPath"] = json!(path_text(&artifact_path)?);
            findings::initialize(&artifact_path, &id, path_text(area)?, false, clock)?;
            let excluded_paths: HashSet<_> = excluded
                .iter()
                .filter_map(|entry| entry["path"].as_str())
                .collect();
            let mut completed = Vec::new();
            let mut pending = Vec::new();
            for group in array(Some(&groups), "groups")? {
                if array(group.get("files"), "files")?.iter().all(|file| {
                    file["path"]
                        .as_str()
                        .is_some_and(|path| excluded_paths.contains(path))
                }) {
                    completed.push(group["groupId"].clone());
                } else {
                    pending.push(group.clone());
                }
            }
            let journal = json!({"schemaVersion":3,"kind":"fileGroups","items":pending,"inProgress":null,"completedGroupIds":completed,"assessments":excluded,"snapshot":inputs.snapshot,"failure":null,"batchReceipts":[]});
            findings::validate_queue(&journal, false, clock)?;
            validate_coverage(&journal, &groups, false)?;
            write_new(&journal_path, &journal, clock)?;
            if all_excluded {
                findings::complete(&artifact_path, &journal_path, clock)?;
                excluded_count += 1;
            }
        }
        reviewers.push(reviewer);
    }
    let dispatch: Vec<_> = reviewers
        .iter()
        .filter(|reviewer| reviewer["dispatch"] != "excluded")
        .cloned()
        .collect();
    write_new(
        &queue_path,
        &json!({"schemaVersion":1,"kind":"focusAreas","items":dispatch}),
        clock,
    )?;
    let mut manifest = json!({"schemaVersion":1,"inventoryPath":path_text(&inventory_path)?,"fileGroupsPath":path_text(&groups_path)?,"focusAreaQueuePath":path_text(&queue_path)?,"reviewers":reviewers});
    if let Some(repository) = &repository {
        manifest["repositoryRoot"] = json!(path_text(repository)?);
        manifest["snapshot"] = inputs.snapshot;
    }
    validate_manifest(&manifest)?;
    // Publishing the authority last prevents workers from using partial preparation.
    write_new(&manifest_path, &manifest, clock)?;
    Ok(
        json!({"manifestPath":path_text(&manifest_path)?,"inventoryPath":path_text(&inventory_path)?,"fileGroupsPath":path_text(&groups_path)?,"focusAreaQueuePath":path_text(&queue_path)?,"reviewerCount":areas.len(),"dispatchCount":areas.len()-excluded_count,"excludedReviewerCount":excluded_count,"fileCount":array(Some(&files), "files")?.len(),"groupCount":array(Some(&groups), "groups")?.len()}),
    )
}

/// Validates the manifest's finding and file-coverage completion gate.
///
/// # Errors
///
/// Returns an error when the authority or its immutable inputs cannot be read.
pub fn validate(
    manifest: &Path,
    artifact_directory: &Path,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let manifest = read_manifest(manifest, clock)?;
    let manifest = manifest.as_value();
    let inventory_path = Path::new(nonblank(manifest.get("inventoryPath"), "inventoryPath")?);
    let groups_path = Path::new(nonblank(manifest.get("fileGroupsPath"), "fileGroupsPath")?);
    let inputs = read_inputs(inventory_path, groups_path, clock)?;
    if manifest.get("snapshot").is_some() {
        assert_snapshot(manifest.get("snapshot"), &inputs.snapshot)?;
    }
    let reviewers = array(manifest.get("reviewers"), "reviewers")?;
    let reviewer_ids: Vec<_> = reviewers
        .iter()
        .map(|reviewer| {
            reviewer["reviewerId"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    let mut result = findings::validate_artifacts(artifact_directory, &reviewer_ids, clock)?;
    let mut errors: Vec<Value> = array(result.get("errors"), "errors")?.clone();
    let mut failed = Vec::new();
    let dispatch_path = Path::new(nonblank(
        manifest.get("focusAreaQueuePath"),
        "focusAreaQueuePath",
    )?);
    let dispatch_transaction = FileTransaction::open(dispatch_path, clock)?;
    let dispatch = read_queue(&dispatch_transaction, clock)?;
    if dispatch["kind"] != "focusAreas" || !array(dispatch.get("items"), "items")?.is_empty() {
        errors.push(json!(
            "The focus-area dispatch queue has not been exhausted."
        ));
    }
    drop(dispatch_transaction);
    let artifacts = array(result.get("artifacts"), "artifacts")?.clone();
    for reviewer in reviewers {
        let id = nonblank(reviewer.get("reviewerId"), "reviewerId")?;
        let found = artifacts
            .iter()
            .find(|artifact| artifact["reviewerId"] == id);
        let issue = validate_reviewer(reviewer, found, manifest, &inputs, clock);
        match issue {
            Ok(Some(reason)) => {
                failed.push(json!(id));
                errors.push(json!(format!("Reviewer '{id}' failed: {reason}")));
            }
            Ok(None) => {}
            Err(error) => errors.push(json!(error.to_string())),
        }
    }
    result["isValid"] = json!(result["isValid"] == true && errors.is_empty());
    result["errors"] = Value::Array(errors);
    result["failedReviewers"] = Value::Array(failed);
    Ok(result)
}

/// Checks one manifest assignment while retaining its artifact-before-journal locks.
fn validate_reviewer(
    reviewer: &Value,
    artifact_summary: Option<&Value>,
    manifest: &Value,
    inputs: &Inputs,
    clock: &dyn Clock,
) -> Result<Option<String>, AppError> {
    let focus_path = nonblank(reviewer.get("focusArea"), "focusArea")?;
    let focus_text = if reviewer.get("focusAreaSha256").is_some() {
        let text = read_text(Path::new(focus_path), clock)?;
        if reviewer["focusAreaSha256"] != bytes_hash(text.as_bytes()) {
            return Err(invalid("Focus-area snapshot changed"));
        }
        Some(text)
    } else {
        None
    };
    let artifact_path = artifact_summary
        .and_then(|artifact| artifact.get("path"))
        .and_then(Value::as_str);
    let artifact_transaction = artifact_path
        .map(|path| FileTransaction::open(Path::new(path), clock))
        .transpose()?;
    let journal_path = Path::new(nonblank(
        reviewer.get("fileGroupQueuePath"),
        "fileGroupQueuePath",
    )?);
    if artifact_path.is_some_and(|path| same_lock(Path::new(path), journal_path)) {
        return Err(invalid("Artifact and journal paths must differ"));
    }
    let journal_transaction = FileTransaction::open(journal_path, clock)?;
    let journal = read_queue(&journal_transaction, clock)?;
    let failure = journal
        .get("failure")
        .filter(|failure| !failure.is_null())
        .and_then(Value::as_str)
        .map(str::to_owned);
    if failure.is_some() {
        return Ok(failure);
    }
    let Some(artifact_summary) = artifact_summary else {
        return Ok(failure);
    };
    if artifact_summary.get("focusArea").and_then(Value::as_str) != Some(focus_path) {
        return Err(invalid("Finding artifact has a different focus area"));
    }
    if reviewer.get("artifactPath").is_some()
        && reviewer.get("artifactPath") != artifact_summary.get("path")
    {
        return Err(invalid("Finding artifact has a different assigned path"));
    }
    let bytes = artifact_transaction
        .as_ref()
        .ok_or_else(|| invalid("Finding artifact missing"))?
        .read_bytes()?
        .ok_or_else(|| invalid("Finding artifact missing"))?;
    let artifact = parse_json(decode_text(&bytes)?.as_bytes())?;
    findings::validate_artifact(&artifact)?;
    if artifact["reviewerId"] != reviewer["reviewerId"]
        || artifact["focusArea"] != reviewer["focusArea"]
    {
        return Err(invalid("Finding artifact changed during validation"));
    }
    if artifact["status"] != "complete" {
        return Err(invalid("Finding artifact is not complete"));
    }
    findings::validate_queue(&journal, true, clock)?;
    validate_coverage(&journal, &inputs.groups, false)?;
    if manifest.get("snapshot").is_some() || !journal.get("snapshot").is_none_or(Value::is_null) {
        assert_snapshot(journal.get("snapshot"), &inputs.snapshot)?;
    }
    if let Some(receipts) = journal.get("batchReceipts") {
        for receipt in array(Some(receipts), "batchReceipts")? {
            if receipt["artifactPath"].as_str() != artifact_path {
                return Err(invalid("Batch receipt names another artifact"));
            }
        }
    }
    if reviewer["dispatch"] == "excluded" {
        let map = read_applicability(
            focus_text
                .as_deref()
                .ok_or_else(|| invalid("Missing focus snapshot"))?,
        )?;
        let root = Path::new(nonblank(manifest.get("repositoryRoot"), "repositoryRoot")?);
        let expected = scope_exclusions(map.as_ref(), root, &inputs.groups)?;
        if expected.len() != array(Some(&inputs.files), "inventory")?.len()
            || !array(artifact.get("findings"), "findings")?.is_empty()
            || array(journal.get("assessments"), "assessments")?
                .iter()
                .any(|entry| entry["disposition"] != "excluded")
        {
            return Err(invalid("Invalid completed-empty exclusion"));
        }
        for entry in expected {
            let actual = array(journal.get("assessments"), "assessments")?
                .iter()
                .find(|item| item["path"] == entry["path"])
                .ok_or_else(|| invalid("Missing exclusion evidence"))?;
            if actual["scope"] != entry["scope"] || actual["reason"] != entry["reason"] {
                return Err(invalid("Exclusion evidence differs from scope metadata"));
            }
        }
    }
    Ok(failure)
}

/// Publishes a new record without allowing an existing one to be reset.
fn write_new(path: &Path, value: &Value, clock: &dyn Clock) -> Result<(), AppError> {
    let transaction = FileTransaction::open(path, clock)?;
    if transaction.read_bytes()?.is_some() {
        return Err(invalid("Review-work destination already exists"));
    }
    publish(&transaction, value)
}

/// Reads validated generated applicability metadata or retains unmarked work.
fn read_applicability(text: &str) -> Result<Option<Value>, AppError> {
    const PREFIX: &str = "<!-- ss-review applicability ";
    let text = text.replace("\r\n", "\n");
    if !text.starts_with("<!-- ss-review applicability") {
        return Ok(None);
    }
    let rest = text
        .strip_prefix(PREFIX)
        .ok_or_else(|| invalid("Malformed applicability metadata"))?;
    let (encoded, content) = rest
        .split_once(" -->\n")
        .ok_or_else(|| invalid("Malformed applicability metadata"))?;
    let bytes = decode_base64(encoded)?;
    let metadata = parse_json(&bytes)?;
    let metadata = object(&metadata, &["schemaVersion", "contentSha256", "map"])?;
    if metadata.len() != 3
        || metadata.get("schemaVersion") != Some(&json!(1))
        || metadata.get("contentSha256").and_then(Value::as_str)
            != Some(bytes_hash(content.as_bytes()).as_str())
    {
        return Err(invalid("Stale applicability metadata"));
    }
    let map = metadata
        .get("map")
        .ok_or_else(|| invalid("Missing applicability map"))?;
    validate_applicability_map(map)?;
    Ok(Some(map.clone()))
}

/// Checks the complete focus-area schema before treating its scope as proof.
fn validate_applicability_map(map: &Value) -> Result<(), AppError> {
    let map = object(map, &["schemaVersion", "categories", "chapters"])?;
    if map.get("schemaVersion") != Some(&json!(1)) {
        return Err(invalid("Invalid applicability schema"));
    }
    let categories = array(map.get("categories"), "categories")?;
    if categories.len() != 1 {
        return Err(invalid("Applicability requires one category"));
    }
    let category = object(&categories[0], &["id", "title"])?;
    let category_id = map_id(category.get("id"))?;
    let title = category
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty() && !title.contains(['\r', '\n']))
        .ok_or_else(|| invalid("Invalid category title"))?;
    let _ = title;
    let chapters = array(map.get("chapters"), "chapters")?;
    if chapters.is_empty() {
        return Err(invalid("Applicability requires chapters"));
    }
    let mut ids = HashSet::new();
    for chapter in chapters {
        let chapter = object(
            chapter,
            &["id", "categoryId", "content", "sources", "applicability"],
        )?;
        let id = map_id(chapter.get("id"))?;
        if !ids.insert(id) || map_id(chapter.get("categoryId"))? != category_id {
            return Err(invalid("Invalid applicability chapter"));
        }
        validate_span(chapter.get("content"))?;
        let sources = array(chapter.get("sources"), "sources")?;
        if sources.is_empty() {
            return Err(invalid("Chapter requires sources"));
        }
        for source in sources {
            validate_span(Some(source))?;
        }
        let contexts = array(chapter.get("applicability"), "applicability")?;
        if contexts.is_empty() {
            return Err(invalid("Chapter requires applicability"));
        }
        for context in contexts {
            let context = object(
                context,
                &[
                    "effectiveScope",
                    "pathScope",
                    "declarations",
                    "origins",
                    "uncertainties",
                ],
            )?;
            nonblank(context.get("effectiveScope"), "effectiveScope")?;
            if let Some(scope) = context.get("pathScope").filter(|scope| !scope.is_null()) {
                validate_path_scope(scope)?;
            }
            for declaration in array(context.get("declarations"), "declarations")? {
                validate_span(Some(declaration))?;
            }
            for origin in array(context.get("origins"), "origins")? {
                nonblank(Some(origin), "origin")?;
            }
            for uncertainty in array(context.get("uncertainties"), "uncertainties")? {
                let uncertainty = object(uncertainty, &["reason", "evidence"])?;
                nonblank(uncertainty.get("reason"), "reason")?;
                let evidence = array(uncertainty.get("evidence"), "evidence")?;
                if evidence.is_empty() {
                    return Err(invalid("Uncertainty requires evidence"));
                }
                for span in evidence {
                    validate_span(Some(span))?;
                }
            }
        }
    }
    Ok(())
}

/// Validates the path language used as applicability evidence.
fn validate_path_scope(scope: &Value) -> Result<(), AppError> {
    let scope = object(scope, &["base", "include"])?;
    let base = nonblank(scope.get("base"), "base")?;
    if base != "."
        && (base.split('/').any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b' ' | b'-')
                })
        }))
    {
        return Err(invalid("Invalid path-scope base"));
    }
    let include = array(scope.get("include"), "include")?;
    let mut unique = HashSet::new();
    if include.is_empty() {
        return Err(invalid("Missing path alternatives"));
    }
    for pattern in include {
        let pattern = pattern
            .as_str()
            .filter(|pattern| !pattern.is_empty())
            .ok_or_else(|| invalid("Invalid path alternative"))?;
        if !pattern.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'.' | b'*' | b'?' | b' ' | b'/' | b'-')
        }) || !unique.insert(pattern)
        {
            return Err(invalid("Invalid path alternative"));
        }
    }
    Ok(())
}

/// Validates the focus-area map's lower-case identifier vocabulary.
fn map_id(value: Option<&Value>) -> Result<&str, AppError> {
    let id = nonblank(value, "map identifier")?;
    if id.split('-').any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    }) {
        return Err(invalid("Invalid map identifier"));
    }
    Ok(id)
}

/// Checks provenance span shape before trusting its scope declaration.
fn validate_span(value: Option<&Value>) -> Result<(), AppError> {
    let span = object(
        value.ok_or_else(|| invalid("Missing span"))?,
        &["root", "path", "startLine", "endLine"],
    )?;
    if !matches!(
        span.get("root").and_then(Value::as_str),
        Some("instruction-set-source" | "instruction-set-raw")
    ) || span
        .get("path")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        || span
            .get("startLine")
            .and_then(Value::as_u64)
            .is_none_or(|line| line == 0)
        || span
            .get("endLine")
            .and_then(Value::as_u64)
            .is_none_or(|line| line == 0)
    {
        return Err(invalid("Invalid applicability span"));
    }
    Ok(())
}

/// Decodes the standard padded base64 used by the focus-area assembler.
fn decode_base64(text: &str) -> Result<Vec<u8>, AppError> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let (chunks, remainder) = text.as_bytes().as_chunks::<4>();
    if chunks.is_empty() || !remainder.is_empty() {
        return Err(invalid("Invalid applicability encoding"));
    }
    let mut output = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let [a, b, c, d] = *chunk;
        let a = digit(a).ok_or_else(|| invalid("Invalid applicability encoding"))?;
        let b = digit(b).ok_or_else(|| invalid("Invalid applicability encoding"))?;
        let c_value = if c == b'=' {
            0
        } else {
            digit(c).ok_or_else(|| invalid("Invalid applicability encoding"))?
        };
        let d_value = if d == b'=' {
            0
        } else {
            digit(d).ok_or_else(|| invalid("Invalid applicability encoding"))?
        };
        if (c == b'=' && d != b'=') || ((c == b'=' || d == b'=') && index + 1 != chunks.len()) {
            return Err(invalid("Invalid applicability encoding"));
        }
        output.push((a << 2) | (b >> 4));
        if c != b'=' {
            output.push((b << 4) | (c_value >> 2));
        }
        if d != b'=' {
            output.push((c_value << 6) | d_value);
        }
    }
    Ok(output)
}

/// Expands a complete scope proof into ordinary per-file exclusions.
fn scope_exclusions(
    map: Option<&Value>,
    root: &Path,
    groups: &Value,
) -> Result<Vec<Value>, AppError> {
    let Some(map) = map else {
        return Ok(Vec::new());
    };
    let mut contexts = Vec::new();
    for chapter in array(map.get("chapters"), "chapters")? {
        contexts.extend(array(chapter.get("applicability"), "applicability")?.iter());
    }
    if contexts.iter().any(|context| {
        context.get("pathScope").is_none_or(Value::is_null)
            || context["uncertainties"]
                .as_array()
                .is_none_or(|entries| !entries.is_empty())
            || context["declarations"].as_array().is_none_or(Vec::is_empty)
    }) {
        return Ok(Vec::new());
    }
    let scope = contexts
        .iter()
        .map(|context| context["effectiveScope"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let root = normalize_path(root);
    let mut excluded = Vec::new();
    for group in array(Some(groups), "groups")? {
        for file in array(group.get("files"), "files")? {
            let path = nonblank(file.get("path"), "path")?;
            let full = normalize_path(Path::new(path));
            let Ok(relative) = full.strip_prefix(&root) else {
                continue;
            };
            if relative.as_os_str().is_empty() {
                continue;
            }
            let relative = path_text(relative)?.replace('\\', "/");
            // Case equivalence outside ASCII is not inferred from the filesystem.
            if !relative.is_ascii() {
                continue;
            }
            let mut included = false;
            for context in &contexts {
                if scope_matches(&context["pathScope"], &relative)? {
                    included = true;
                    break;
                }
            }
            if !included {
                excluded.push(json!({"groupId":group["groupId"],"path":path,"disposition":"excluded","scope":scope,"reason":format!("Repository-relative path '{relative}' matches no path alternative in any section of the validated focus-area metadata.")}));
            }
        }
    }
    Ok(excluded)
}

/// Matches the intentionally small path-scope glob language.
fn scope_matches(scope: &Value, path: &str) -> Result<bool, AppError> {
    let base = nonblank(scope.get("base"), "base")?;
    for pattern in array(scope.get("include"), "include")? {
        let pattern = nonblank(Some(pattern), "pattern")?;
        let full = if base == "." {
            pattern.to_owned()
        } else {
            format!("{base}/{pattern}")
        };
        let segments: Vec<_> = full.split('/').collect();
        if segments.iter().any(|segment| {
            segment.is_empty()
                || *segment == "."
                || *segment == ".."
                || (segment.contains("**") && *segment != "**")
        }) {
            return Err(invalid("Unsupported path-scope pattern"));
        }
        let path: Vec<_> = path.split('/').collect();
        if match_segments(&segments, &path) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Matches segments with a dynamic table so globstars always make progress.
fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    let mut previous = vec![false; path.len() + 1];
    let mut next = vec![false; path.len() + 1];
    previous[0] = true;
    for segment in pattern {
        next.fill(false);
        if *segment == "**" {
            next[0] = previous[0];
            for index in 1..=path.len() {
                next[index] = previous[index] || next[index - 1];
            }
        } else {
            for index in 1..=path.len() {
                next[index] = previous[index - 1] && match_segment(segment, path[index - 1]);
            }
        }
        mem::swap(&mut previous, &mut next);
    }
    previous[path.len()]
}

/// Matches a single case-insensitive segment without crossing path separators.
fn match_segment(pattern: &str, path: &str) -> bool {
    let chars: Vec<_> = path.chars().flat_map(char::to_lowercase).collect();
    let mut previous = vec![false; chars.len() + 1];
    let mut next = vec![false; chars.len() + 1];
    previous[0] = true;
    for token in pattern.chars().flat_map(char::to_lowercase) {
        next.fill(false);
        if token == '*' {
            next[0] = previous[0];
            for index in 1..=chars.len() {
                next[index] = previous[index] || next[index - 1];
            }
        } else {
            for index in 1..=chars.len() {
                next[index] = previous[index - 1] && (token == '?' || token == chars[index - 1]);
            }
        }
        mem::swap(&mut previous, &mut next);
    }
    previous[chars.len()]
}

/// Serializes a preparation input while holding its retained sibling lock.
fn publish_input(
    transaction: &FileTransaction<'_>,
    output: &Path,
    records: &Value,
    replace: bool,
) -> Result<Value, AppError> {
    if transaction.read_bytes()?.is_some() && !replace {
        return Err(invalid(
            "Input already exists; replacement requires explicit consent",
        ));
    }
    publish(transaction, records)?;
    Ok(
        json!({"status":"Recorded", "path":path_text(output)?, "count":records.as_array().map_or(0, Vec::len)}),
    )
}

/// Reads inventory and group inputs once, binding the journal to those texts.
struct Inputs {
    files: Value,
    groups: Value,
    snapshot: Value,
}

/// Loads and validates immutable input snapshots before a transaction.
fn read_inputs(inventory: &Path, groups: &Path, clock: &dyn Clock) -> Result<Inputs, AppError> {
    let inventory_text = read_text(inventory, clock)?;
    let groups_text = read_text(groups, clock)?;
    let files = parse_json(inventory_text.as_bytes())?;
    validate_inventory(&files)?;
    let groups = parse_json(groups_text.as_bytes())?;
    validate_groups(&groups, &files)?;
    Ok(Inputs {
        files,
        groups,
        snapshot: json!({"inventorySha256":bytes_hash(inventory_text.as_bytes()), "fileGroupsSha256":bytes_hash(groups_text.as_bytes())}),
    })
}

/// Reads one inventory with the same text decoding used for snapshot identity.
fn read_inventory(path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let value = parse_json(read_text(path, clock)?.as_bytes())?;
    validate_inventory(&value)?;
    Ok(value)
}

/// Reads one grouped inventory against its validated inventory.
fn read_groups(path: &Path, files: &Value, clock: &dyn Clock) -> Result<Value, AppError> {
    let value = parse_json(read_text(path, clock)?.as_bytes())?;
    validate_groups(&value, files)?;
    Ok(value)
}

/// Enforces unique normalized paths while retaining their original spelling.
fn validate_inventory(files: &Value) -> Result<(), AppError> {
    let files = array(Some(files), "inventory")?;
    if files.is_empty() {
        return Err(invalid("Inventory must not be empty"));
    }
    let mut seen = HashSet::new();
    for file in files {
        let file = object(file, &["path", "sizeBytes", "change"])?;
        let path = absolute_path(file.get("path"), "path")?;
        if !seen.insert(normalize_path(Path::new(path))) {
            return Err(invalid("Inventory repeats a path"));
        }
        let size = file.get("sizeBytes").and_then(Value::as_i64);
        if !size.is_some_and(|size| size >= 0)
            || !matches!(
                file.get("change").and_then(Value::as_str),
                Some("new" | "modified" | "deleted")
            )
        {
            return Err(invalid("Invalid inventory metadata"));
        }
    }
    Ok(())
}

/// Enforces exact coverage and the documented review-context group limits.
fn validate_groups(groups: &Value, files: &Value) -> Result<(), AppError> {
    let groups = array(Some(groups), "groups")?;
    let files = array(Some(files), "inventory")?;
    if groups.is_empty() {
        return Err(invalid("Groups must not be empty"));
    }
    let inventory: HashMap<_, _> = files
        .iter()
        .map(|file| (file["path"].as_str().unwrap_or_default(), file))
        .collect();
    let mut group_ids = HashSet::new();
    let mut paths = HashSet::new();
    for group in groups {
        let group = object(group, &["groupId", "files"])?;
        let id = nonblank(group.get("groupId"), "groupId")?;
        if !valid_id(id) || !group_ids.insert(id) {
            return Err(invalid("Invalid or duplicate group identifier"));
        }
        let members = array(group.get("files"), "files")?;
        // These bounds preserve space for guidance during review compaction.
        // Ref: skills/ss-review/docs/design.md, "Review coverage".
        if members.is_empty() || members.len() > 20 {
            return Err(invalid("Invalid group file count"));
        }
        let mut bytes = 0_u128;
        for member in members {
            let member = object(member, &["path", "sizeBytes", "change"])?;
            let path = nonblank(member.get("path"), "path")?;
            let original = inventory
                .get(path)
                .ok_or_else(|| invalid("Group names a file outside inventory"))?;
            if !paths.insert(path)
                || member.get("sizeBytes") != original.get("sizeBytes")
                || member.get("change") != original.get("change")
            {
                return Err(invalid("Duplicate file or changed inventory metadata"));
            }
            bytes += u128::from(
                member
                    .get("sizeBytes")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| invalid("Invalid size"))?,
            );
        }
        // Oversized individual files remain reviewable as singleton groups.
        // Ref: skills/ss-review/docs/design.md, "Review coverage".
        if members.len() > 1 && bytes > 100_000 {
            return Err(invalid("Group exceeds byte limit"));
        }
    }
    if paths.len() != inventory.len() {
        return Err(invalid("Groups omit inventory files"));
    }
    Ok(())
}

/// Opens a private journal or resumes it without discarding existing work.
///
/// # Errors
///
/// Returns an error for stale inputs, invalid state, or an unauthorized reset.
pub fn initialize_queue(
    inventory: &Path,
    groups: &Path,
    queue: &Path,
    resume: bool,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let inputs = read_inputs(inventory, groups, clock)?;
    let path = full_path(queue)?;
    let transaction = FileTransaction::open(&path, clock)?;
    let mut queue = match transaction.read_bytes()? {
        Some(bytes) => {
            if !resume {
                return Err(invalid(
                    "Journal already exists; explicit resume is required",
                ));
            }
            let queue = parse_json(decode_text(&bytes)?.as_bytes())?;
            findings::validate_queue(&queue, false, clock)?;
            if !queue.get("snapshot").is_none_or(Value::is_null) {
                assert_snapshot(queue.get("snapshot"), &inputs.snapshot)?;
            }
            validate_coverage(&queue, &inputs.groups, false)?;
            queue
        }
        None => {
            json!({"schemaVersion":3, "kind":"fileGroups", "items":inputs.groups, "inProgress":null, "completedGroupIds":[], "assessments":[], "snapshot":inputs.snapshot, "failure":null, "batchReceipts":[]})
        }
    };
    let exists = transaction.read_bytes()?.is_some();
    if !exists
        || queue
            .get("failure")
            .is_some_and(|failure| !failure.is_null())
    {
        queue["failure"] = Value::Null;
        publish(&transaction, &queue)?;
    }
    Ok(json!({
        "queuePath":path_text(&path)?,
        "groupCount":array(Some(&inputs.groups), "groups")?.len(),
        "fileCount":array(Some(&inputs.files), "files")?.len(),
        "completedGroupCount":array(queue.get("completedGroupIds"), "completedGroupIds")?.len(),
        "inProgressGroupId":queue.get("inProgress").and_then(|group| group.get("groupId")),
        "remainingCount":array(queue.get("items"), "items")?.len(),
        "snapshot":inputs.snapshot
    }))
}

/// Claims the next work item, replaying an unfinished private claim.
///
/// # Errors
///
/// Returns an error for missing, failed, or malformed queue state.
pub fn next(queue: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let path = full_path(queue)?;
    let transaction = FileTransaction::open(&path, clock)?;
    let mut queue = read_queue(&transaction, clock)?;
    ensure_not_failed(&queue)?;
    if queue["kind"] == "fileGroups" && !queue["inProgress"].is_null() {
        return Ok(
            json!({"exhausted":false,"item":queue["inProgress"],"remainingCount":array(queue.get("items"), "items")?.len(),"snapshot":queue.get("snapshot")}),
        );
    }
    let items = queue["items"]
        .as_array_mut()
        .ok_or_else(|| invalid("Missing queue items"))?;
    if items.is_empty() {
        return Ok(json!({"exhausted":true, "item":null, "remainingCount":0}));
    }
    let item = items.remove(0);
    if queue["kind"] == "fileGroups" {
        queue["inProgress"] = item.clone();
    }
    publish(&transaction, &queue)?;
    Ok(
        json!({"exhausted":false,"item":item,"remainingCount":array(queue.get("items"), "items")?.len(),"snapshot":queue.get("snapshot")}),
    )
}

/// Records a failure without modifying claims, assessments, or findings.
///
/// # Errors
///
/// Returns an error for invalid state or an empty reason.
pub fn set_failure(queue: &Path, reason: &str, clock: &dyn Clock) -> Result<Value, AppError> {
    if reason.trim().is_empty() {
        return Err(invalid("Failure requires a reason"));
    }
    let path = full_path(queue)?;
    let transaction = FileTransaction::open(&path, clock)?;
    let mut queue = read_queue(&transaction, clock)?;
    if queue["kind"] != "fileGroups" {
        return Err(invalid("Failure requires a private journal"));
    }
    if queue["failure"] != reason {
        queue["failure"] = json!(reason);
        publish(&transaction, &queue)?;
    }
    Ok(json!({"status":"Failed","queuePath":path_text(&path)?,"reason":reason}))
}

/// Completes a claimed group after its individual assessments were recorded.
///
/// # Errors
///
/// Returns an error when the claim or its assessments are incomplete.
pub fn complete_group(queue: &Path, group_id: &str, clock: &dyn Clock) -> Result<Value, AppError> {
    let path = full_path(queue)?;
    let transaction = FileTransaction::open(&path, clock)?;
    let mut queue = read_queue(&transaction, clock)?;
    if queue["kind"] != "fileGroups" {
        return Err(invalid("Expected a file-group journal"));
    }
    ensure_not_failed(&queue)?;
    let completed = array(queue.get("completedGroupIds"), "completedGroupIds")?;
    let already = completed.iter().any(|id| id.as_str() == Some(group_id));
    if !already {
        let group = queue
            .get("inProgress")
            .ok_or_else(|| invalid("Missing claim"))?;
        if group.get("groupId").and_then(Value::as_str) != Some(group_id) {
            return Err(invalid("Group is not the current claim"));
        }
        assert_assessments(&queue, group)?;
        queue["completedGroupIds"]
            .as_array_mut()
            .ok_or_else(|| invalid("Missing completed groups"))?
            .push(json!(group_id));
        queue["inProgress"] = Value::Null;
        publish(&transaction, &queue)?;
    }
    Ok(
        json!({"groupId":group_id,"alreadyComplete":already,"completedGroupCount":array(queue.get("completedGroupIds"), "completedGroupIds")?.len(),"remainingCount":array(queue.get("items"), "items")?.len()}),
    )
}

/// Reads a strict assessment draft before recording it in the journal.
///
/// # Errors
///
/// Returns an error when the draft is missing, malformed, or cannot be recorded.
pub fn set_assessment_from_file(
    queue: &Path,
    inventory: &Path,
    groups: &Path,
    assessment_path: &Path,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let text = read_text(assessment_path, clock)?;
    let assessment = parse_json(text.as_bytes())?;
    set_assessment(queue, inventory, groups, &assessment, clock)?;
    Ok(json!({
        "status": "Recorded",
        "path": assessment["path"],
        "disposition": assessment["disposition"]
    }))
}

/// Records or repairs an assessment without discarding the rest of the journal.
///
/// # Errors
///
/// Returns an error when the assignment, snapshot, or journal is invalid.
pub fn set_assessment(
    queue: &Path,
    inventory: &Path,
    groups: &Path,
    assessment: &Value,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    validate_assessment(assessment)?;
    let inputs = read_inputs(inventory, groups, clock)?;
    let group_id = nonblank(assessment.get("groupId"), "groupId")?;
    let path = nonblank(assessment.get("path"), "path")?;
    let group = find_group(&inputs.groups, group_id)?;
    if !group_contains(group, path)? {
        return Err(invalid("Assessment does not belong to its group"));
    }
    let transaction = FileTransaction::open(&full_path(queue)?, clock)?;
    let mut queue = read_queue(&transaction, clock)?;
    if !queue.get("snapshot").is_none_or(Value::is_null) {
        assert_snapshot(queue.get("snapshot"), &inputs.snapshot)?;
    }
    if !array(queue.get("completedGroupIds"), "completedGroupIds")?
        .iter()
        .any(|id| id.as_str() == Some(group_id))
        && queue
            .get("inProgress")
            .and_then(|value| value.get("groupId"))
            .and_then(Value::as_str)
            != Some(group_id)
    {
        return Err(invalid("Assessment requires a claimed or completed group"));
    }
    let assessments = queue["assessments"]
        .as_array_mut()
        .ok_or_else(|| invalid("Missing assessments"))?;
    assessments.retain(|entry| entry["path"].as_str() != Some(path));
    assessments.push(assessment.clone());
    validate_coverage(&queue, &inputs.groups, true)?;
    publish(&transaction, &queue)?;
    Ok(Value::Null)
}

/// Reads a strict batch draft before committing its assessments and receipt.
///
/// # Errors
///
/// Returns an error when the draft is missing, malformed, or cannot be committed.
pub fn complete_batch_from_file(
    queue: &Path,
    inventory: &Path,
    groups: &Path,
    artifact: &Path,
    batch_path: &Path,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let text = read_text(batch_path, clock)?;
    let batch = parse_json(text.as_bytes())?;
    complete_batch(queue, inventory, groups, artifact, &batch, clock)
}

/// Binds completion to all assessments and durable finding history.
///
/// # Errors
///
/// Returns an error for stale snapshots, partial work, or conflicting retries.
pub fn complete_batch(
    queue: &Path,
    inventory: &Path,
    groups: &Path,
    artifact: &Path,
    batch: &Value,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let batch = object(
        batch,
        &[
            "schemaVersion",
            "snapshot",
            "groupId",
            "assessments",
            "findingIds",
        ],
    )?;
    if batch.get("schemaVersion") != Some(&json!(1)) {
        return Err(invalid("Invalid batch schema"));
    }
    let group_id = nonblank(batch.get("groupId"), "groupId")?;
    if !valid_id(group_id) {
        return Err(invalid("Invalid group identifier"));
    }
    let batch_assessments = array(batch.get("assessments"), "assessments")?;
    let finding_ids = array(batch.get("findingIds"), "findingIds")?;
    let mut unique_ids = HashSet::new();
    for id in finding_ids {
        let id = nonblank(Some(id), "findingId")?;
        if !unique_ids.insert(id) {
            return Err(invalid("Duplicate finding identifier"));
        }
    }
    let inputs = read_inputs(inventory, groups, clock)?;
    assert_snapshot(batch.get("snapshot"), &inputs.snapshot)?;
    let group = find_group(&inputs.groups, group_id)?;
    let artifact_path = full_path(artifact)?;
    let queue_path = full_path(queue)?;
    if same_lock(&artifact_path, &queue_path) {
        return Err(invalid("Artifact and journal paths must differ"));
    }
    // Finding writes take the same artifact-before-journal lock order.
    let artifact_transaction = FileTransaction::open(&artifact_path, clock)?;
    let artifact_bytes = artifact_transaction
        .read_bytes()?
        .ok_or_else(|| invalid("Finding artifact is missing"))?;
    let artifact_value = parse_json(decode_text(&artifact_bytes)?.as_bytes())?;
    findings::validate_artifact(&artifact_value)?;
    assert_findings(&artifact_value, finding_ids)?;
    let transaction = FileTransaction::open(&queue_path, clock)?;
    let mut queue = read_queue(&transaction, clock)?;
    ensure_not_failed(&queue)?;
    assert_snapshot(queue.get("snapshot"), &inputs.snapshot)?;
    validate_coverage(&queue, &inputs.groups, false)?;
    let already = array(queue.get("completedGroupIds"), "completedGroupIds")?
        .iter()
        .any(|id| id.as_str() == Some(group_id));
    if !already
        && queue
            .get("inProgress")
            .and_then(|group| group.get("groupId"))
            .and_then(Value::as_str)
            != Some(group_id)
    {
        return Err(invalid("Batch requires the current claimed group"));
    }
    let mut records: HashMap<String, Value> = HashMap::new();
    for entry in array(queue.get("assessments"), "assessments")? {
        if entry["groupId"] == group_id {
            records.insert(
                nonblank(entry.get("path"), "path")?.to_owned(),
                entry.clone(),
            );
        }
    }
    let artifact_path = path_text(&artifact_path)?;
    let previous = if already {
        Some(batch_hash(group, &records, artifact_path, finding_ids)?)
    } else {
        None
    };
    let mut seen = HashSet::new();
    for entry in batch_assessments {
        validate_assessment(entry)?;
        let path = nonblank(entry.get("path"), "path")?;
        if entry["groupId"] != group_id || !group_contains(group, path)? || !seen.insert(path) {
            return Err(invalid(
                "Batch assessments must name distinct files in the group",
            ));
        }
        if !already && records.contains_key(path) {
            return Err(invalid("Assessment already persisted; use per-file repair"));
        }
        records.insert(path.to_owned(), entry.clone());
    }
    let hash = batch_hash(group, &records, artifact_path, finding_ids)?;
    if already {
        let receipts = array(queue.get("batchReceipts"), "batchReceipts")?;
        let existing: Vec<_> = receipts
            .iter()
            .filter(|receipt| receipt["groupId"] == group_id)
            .collect();
        if existing.len() != 1
            || existing[0]["sha256"] != hash
            || previous.as_deref() != Some(hash.as_str())
        {
            return Err(invalid("Conflicting batch retry"));
        }
    } else {
        let assessments = queue["assessments"]
            .as_array_mut()
            .ok_or_else(|| invalid("Missing assessments"))?;
        assessments.retain(|assessment| assessment["groupId"] != group_id);
        for file in array(group.get("files"), "files")? {
            let path = nonblank(file.get("path"), "path")?;
            assessments.push(
                records
                    .get(path)
                    .ok_or_else(|| invalid("Partial assessment batch"))?
                    .clone(),
            );
        }
        queue["completedGroupIds"]
            .as_array_mut()
            .ok_or_else(|| invalid("Missing completed groups"))?
            .push(json!(group_id));
        queue["inProgress"] = Value::Null;
        queue
            .as_object_mut()
            .ok_or_else(|| invalid("Invalid journal"))?
            .entry("batchReceipts")
            .or_insert_with(|| json!([]));
        queue["batchReceipts"].as_array_mut().ok_or_else(|| invalid("Missing receipts"))?.push(json!({"groupId":group_id,"sha256":hash,"artifactPath":artifact_path,"findingIds":finding_ids}));
        findings::validate_queue(&queue, false, clock)?;
        publish(&transaction, &queue)?;
    }
    Ok(
        json!({"groupId":group_id,"alreadyComplete":already,"assessmentCount":records.len(),"remainingCount":array(queue.get("items"), "items")?.len()}),
    )
}

/// Validates persisted active and removed finding identifiers for a receipt.
fn assert_findings(artifact: &Value, ids: &[Value]) -> Result<(), AppError> {
    if artifact["kind"] != "reviewer" {
        return Err(invalid("Batch requires a reviewer artifact"));
    }
    let mut persisted = HashSet::new();
    for finding in array(artifact.get("findings"), "findings")? {
        let id = nonblank(finding.get("id"), "finding id")?;
        if !persisted.insert(id) {
            return Err(invalid("Duplicate persisted finding identifier"));
        }
    }
    if let Some(removed) = artifact.get("removed") {
        for entry in array(Some(removed), "removed")? {
            let id = nonblank(
                entry.get("finding").and_then(|finding| finding.get("id")),
                "finding id",
            )?;
            if !persisted.insert(id) {
                return Err(invalid("Duplicate persisted finding identifier"));
            }
        }
    }
    for id in ids {
        if !persisted.contains(nonblank(Some(id), "findingId")?) {
            return Err(invalid("Receipt names an unpersisted finding"));
        }
    }
    Ok(())
}

/// Applies the assessment schema before inserting records into a journal.
fn validate_assessment(value: &Value) -> Result<(), AppError> {
    let fields = object(
        value,
        &["groupId", "path", "disposition", "scope", "reason"],
    )?;
    if !valid_id(nonblank(fields.get("groupId"), "groupId")?) {
        return Err(invalid("Invalid group identifier"));
    }
    absolute_path(fields.get("path"), "path")?;
    let scope = fields
        .get("scope")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Invalid assessment scope"))?;
    let reason = fields
        .get("reason")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Invalid assessment reason"))?;
    match fields.get("disposition").and_then(Value::as_str) {
        Some("reviewed") => Ok(()),
        Some("excluded") if !scope.trim().is_empty() && !reason.trim().is_empty() => Ok(()),
        _ => Err(invalid("Invalid assessment disposition")),
    }
}

/// Confirms every original group remains represented without changed file metadata.
fn validate_coverage(
    queue: &Value,
    groups: &Value,
    allow_incomplete: bool,
) -> Result<(), AppError> {
    if queue.get("kind") != Some(&json!("fileGroups")) {
        return Err(invalid("Expected file-group journal"));
    }
    let groups = array(Some(groups), "groups")?;
    let expected: HashMap<_, _> = groups
        .iter()
        .map(|group| (group["groupId"].as_str().unwrap_or_default(), group))
        .collect();
    let mut ids = HashSet::new();
    for id in array(queue.get("completedGroupIds"), "completedGroupIds")? {
        let id = nonblank(Some(id), "completed group")?;
        if !ids.insert(id) || !expected.contains_key(id) {
            return Err(invalid("Invalid completed group"));
        }
    }
    for group in array(queue.get("items"), "items")?
        .iter()
        .chain(queue.get("inProgress").filter(|item| !item.is_null()))
    {
        let id = nonblank(group.get("groupId"), "groupId")?;
        if !ids.insert(id)
            || expected
                .get(id)
                .is_none_or(|original| original.get("files") != group.get("files"))
        {
            return Err(invalid("Journal changed or duplicated an assigned group"));
        }
    }
    if ids.len() != expected.len() {
        return Err(invalid("Journal omits assigned groups"));
    }
    for assessment in array(queue.get("assessments"), "assessments")? {
        let id = nonblank(assessment.get("groupId"), "groupId")?;
        let path = nonblank(assessment.get("path"), "path")?;
        if !group_contains(
            expected
                .get(id)
                .ok_or_else(|| invalid("Assessment names unknown group"))?,
            path,
        )? {
            return Err(invalid("Assessment names a file outside its group"));
        }
    }
    if !allow_incomplete {
        for id in array(queue.get("completedGroupIds"), "completedGroupIds")? {
            let id = nonblank(Some(id), "completed group")?;
            assert_assessments(
                queue,
                expected.get(id).ok_or_else(|| invalid("Unknown group"))?,
            )?;
        }
    }
    Ok(())
}

/// Requires an assessment for every file before a group becomes complete.
fn assert_assessments(queue: &Value, group: &Value) -> Result<(), AppError> {
    let group_id = nonblank(group.get("groupId"), "groupId")?;
    let paths: HashSet<_> = array(queue.get("assessments"), "assessments")?
        .iter()
        .filter(|entry| entry["groupId"] == group_id)
        .filter_map(|entry| entry["path"].as_str())
        .collect();
    for file in array(group.get("files"), "files")? {
        if !paths.contains(nonblank(file.get("path"), "path")?) {
            return Err(invalid("Group lacks an assessment"));
        }
    }
    Ok(())
}

/// Selects an immutable group by identifier.
fn find_group<'a>(groups: &'a Value, id: &str) -> Result<&'a Value, AppError> {
    array(Some(groups), "groups")?
        .iter()
        .find(|group| group["groupId"] == id)
        .ok_or_else(|| invalid("Unknown group"))
}

/// Checks assignment using ordinal path spelling.
fn group_contains(group: &Value, path: &str) -> Result<bool, AppError> {
    Ok(array(group.get("files"), "files")?
        .iter()
        .any(|file| file["path"] == path))
}

/// Rejects stale snapshots rather than reinitializing a journal.
fn assert_snapshot(expected: Option<&Value>, actual: &Value) -> Result<(), AppError> {
    if expected != Some(actual) {
        return Err(invalid("Stale or missing review snapshot"));
    }
    Ok(())
}

/// Ensures a recorded failure blocks progress until explicit resume.
fn ensure_not_failed(queue: &Value) -> Result<(), AppError> {
    if queue
        .get("failure")
        .is_some_and(|failure| !failure.is_null())
    {
        return Err(invalid("Failed reviewer requires explicit resume"));
    }
    Ok(())
}

/// Reads and validates either supported queue without treating missing data as exhaustion.
fn read_queue(transaction: &FileTransaction<'_>, clock: &dyn Clock) -> Result<Value, AppError> {
    let bytes = transaction
        .read_bytes()?
        .ok_or_else(|| invalid("Review queue is missing"))?;
    let queue = parse_json(decode_text(&bytes)?.as_bytes())?;
    match queue.get("kind").and_then(Value::as_str) {
        Some("fileGroups") => findings::validate_queue(&queue, false, clock)?,
        Some("focusAreas") if queue.get("schemaVersion") == Some(&json!(1)) => {
            for item in array(queue.get("items"), "items")? {
                if !item.is_object() {
                    return Err(invalid("Queue item must be an object"));
                }
            }
        }
        _ => return Err(invalid("Invalid review queue")),
    }
    Ok(queue)
}

/// Hashes canonical assessment order using PowerShell's indented JSON form.
fn batch_hash(
    group: &Value,
    records: &HashMap<String, Value>,
    artifact: &str,
    ids: &[Value],
) -> Result<String, AppError> {
    let mut assessments = Vec::new();
    for file in array(group.get("files"), "files")? {
        let path = nonblank(file.get("path"), "path")?;
        let record = records
            .get(path)
            .ok_or_else(|| invalid("Partial assessment batch"))?;
        assessments.push(json!({"groupId":record["groupId"],"path":record["path"],"disposition":record["disposition"],"scope":record["scope"],"reason":record["reason"]}));
    }
    let mut ids: Vec<_> = ids
        .iter()
        .map(|id| nonblank(Some(id), "findingId"))
        .collect::<Result<_, _>>()?;
    ids.sort_unstable_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    let value = json!({"assessments":assessments,"artifactPath":artifact,"findingIds":ids});
    let compact = powershell_json(&value)?;
    Ok(bytes_hash(pretty_json(&compact).as_bytes()))
}

/// Adds PowerShell indentation to already serialized JSON without changing its strings.
fn pretty_json(compact: &str) -> String {
    let mut output = String::new();
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escape = false;
    let mut chars = compact.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            output.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                output.push(ch);
            }
            '{' | '[' => {
                output.push(ch);
                depth += 1;
                if !matches!(chars.peek(), Some('}' | ']')) {
                    output.push('\n');
                    output.push_str(&"  ".repeat(depth));
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if !matches!(output.chars().last(), Some('{' | '[')) {
                    output.push('\n');
                    output.push_str(&"  ".repeat(depth));
                }
                output.push(ch);
            }
            ',' => {
                output.push(ch);
                output.push('\n');
                output.push_str(&"  ".repeat(depth));
            }
            ':' => {
                output.push_str(": ");
            }
            _ => output.push(ch),
        }
    }
    output
}

/// Publishes complete JSON through the transaction's atomic replacement.
fn publish(transaction: &FileTransaction<'_>, value: &Value) -> Result<(), AppError> {
    transaction.write_bytes(powershell_json(value)?.as_bytes())
}

/// Reads complete workflow text using byte-order-mark-aware decoding.
fn read_text(path: &Path, clock: &dyn Clock) -> Result<String, AppError> {
    let bytes = read_bytes(path, clock)?.ok_or_else(|| invalid("Required input is missing"))?;
    decode_text(&bytes)
}

/// Requires an object and rejects properties outside its schema.
fn object<'a>(value: &'a Value, fields: &[&str]) -> Result<&'a Map<String, Value>, AppError> {
    let value = value
        .as_object()
        .ok_or_else(|| invalid("Expected an object"))?;
    allowed_fields(value, fields)?;
    Ok(value)
}

/// Requires an array instead of coercing missing data into an empty collection.
fn array<'a>(value: Option<&'a Value>, name: &str) -> Result<&'a Vec<Value>, AppError> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(&format!("Expected {name} array")))
}

/// Resolves a path without requiring the final entry to exist.
fn full_path(path: &Path) -> Result<PathBuf, AppError> {
    let path =
        std::path::absolute(path).map_err(|error| caused_by("io", "Cannot resolve path", error))?;
    Ok(normalize_path(&path))
}

/// Normalizes dot components without changing filesystem case or resolving links.
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => result.push(component.as_os_str()),
        }
    }
    result
}

/// Keeps non-Unicode paths out of JSON records.
fn path_text(path: &Path) -> Result<&str, AppError> {
    path.to_str().ok_or_else(|| invalid("Path is not Unicode"))
}

/// Avoids recursively taking the same retained sibling lock through an alias.
fn same_lock(first: &Path, second: &Path) -> bool {
    let identity = |path: &Path| {
        Some((
            fs::canonicalize(path.parent()?).ok()?,
            path.file_name()?.to_owned(),
        ))
    };
    first == second
        || identity(first)
            .zip(identity(second))
            .is_some_and(|(first, second)| first == second)
}

/// Reads the complete manifest before another operation relies on its assignments.
pub(crate) fn read_manifest(path: &Path, clock: &dyn Clock) -> Result<ValidatedManifest, AppError> {
    let manifest = read_bytes(path, clock)?
        .ok_or_else(|| fail("invalidInput", "Review-work manifest is missing"))?;
    let manifest = decode_text(&manifest)?;
    let manifest = parse_json(manifest.as_bytes())?;
    validate_manifest(&manifest)?;
    Ok(ValidatedManifest(manifest))
}

/// Applies the manifest schema and assignment uniqueness checks.
fn validate_manifest(manifest: &Value) -> Result<(), AppError> {
    let object = manifest
        .as_object()
        .ok_or_else(|| invalid("Manifest must be an object"))?;
    allowed_fields(
        object,
        &[
            "schemaVersion",
            "inventoryPath",
            "fileGroupsPath",
            "focusAreaQueuePath",
            "reviewers",
            "repositoryRoot",
            "snapshot",
        ],
    )?;
    if object.get("schemaVersion") != Some(&json!(1)) {
        return Err(invalid("Invalid manifest schema version"));
    }
    for field in ["inventoryPath", "fileGroupsPath", "focusAreaQueuePath"] {
        absolute_path(object.get(field), field)?;
    }
    let has_snapshot = object.contains_key("snapshot");
    if has_snapshot != object.contains_key("repositoryRoot") {
        return Err(invalid("Manifest snapshot requires repositoryRoot"));
    }
    if has_snapshot {
        nonblank(object.get("repositoryRoot"), "repositoryRoot")?;
        validate_snapshot(object.get("snapshot"))?;
    }
    let reviewers = object
        .get("reviewers")
        .and_then(Value::as_array)
        .filter(|reviewers| !reviewers.is_empty())
        .ok_or_else(|| invalid("Manifest requires reviewers"))?;
    let mut ids = HashSet::new();
    let mut areas = HashSet::new();
    let mut queues = HashSet::new();
    for reviewer in reviewers {
        let reviewer = reviewer
            .as_object()
            .ok_or_else(|| invalid("Reviewer assignment must be an object"))?;
        allowed_fields(
            reviewer,
            &[
                "reviewerId",
                "focusArea",
                "fileGroupQueuePath",
                "dispatch",
                "focusAreaSha256",
                "artifactPath",
            ],
        )?;
        let id = nonblank(reviewer.get("reviewerId"), "reviewerId")?;
        if !valid_id(id) || !ids.insert(id) {
            return Err(invalid("Invalid or duplicate reviewer identifier"));
        }
        let area = absolute_path(reviewer.get("focusArea"), "focusArea")?;
        let queue = absolute_path(reviewer.get("fileGroupQueuePath"), "fileGroupQueuePath")?;
        if !areas.insert(area) || !queues.insert(queue) {
            return Err(invalid("Reviewer assignments must use distinct paths"));
        }
        let has_dispatch = reviewer.contains_key("dispatch");
        if has_dispatch != has_snapshot
            || has_dispatch != reviewer.contains_key("focusAreaSha256")
            || has_dispatch != reviewer.contains_key("artifactPath")
        {
            return Err(invalid("Incomplete applicability assignment"));
        }
        if has_dispatch {
            if !matches!(
                reviewer.get("dispatch").and_then(Value::as_str),
                Some("review" | "excluded")
            ) {
                return Err(invalid("Invalid applicability dispatch"));
            }
            validate_hash(reviewer.get("focusAreaSha256"))?;
            absolute_path(reviewer.get("artifactPath"), "artifactPath")?;
        }
    }
    Ok(())
}

/// Keeps unknown properties from silently changing the assignment contract.
fn allowed_fields(object: &Map<String, Value>, fields: &[&str]) -> Result<(), AppError> {
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err(invalid("Unexpected manifest property"));
    }
    Ok(())
}

/// Checks the immutable inputs that bind a journal to its inventory.
fn validate_snapshot(snapshot: Option<&Value>) -> Result<(), AppError> {
    let snapshot = snapshot
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("Invalid manifest snapshot"))?;
    allowed_fields(snapshot, &["inventorySha256", "fileGroupsSha256"])?;
    validate_hash(snapshot.get("inventorySha256"))?;
    validate_hash(snapshot.get("fileGroupsSha256"))
}

/// Restricts stored digests to the uppercase SHA-256 representation.
fn validate_hash(hash: Option<&Value>) -> Result<(), AppError> {
    let hash = hash
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Missing SHA-256 digest"))?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
    {
        return Err(invalid("Invalid SHA-256 digest"));
    }
    Ok(())
}

/// Validates the shared identifier vocabulary without interpreting case.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Requires schema text rather than converting non-string JSON values.
fn nonblank<'a>(value: Option<&'a Value>, name: &str) -> Result<&'a str, AppError> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| invalid(&format!("Invalid {name}")))
}

/// Preserves path spelling because reviewer ownership uses ordinal comparison.
fn absolute_path<'a>(value: Option<&'a Value>, name: &str) -> Result<&'a str, AppError> {
    let value = nonblank(value, name)?;
    if !Path::new(value).is_absolute() {
        return Err(invalid(&format!("Expected absolute {name}")));
    }
    Ok(value)
}

/// Decodes BOM-detected text without repairing malformed workflow records.
fn decode_text(bytes: &[u8]) -> Result<String, AppError> {
    if let Some(bytes) = bytes.strip_prefix(&[0x00, 0x00, 0xfe, 0xff]) {
        decode_utf32(bytes, false)
    } else if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe, 0x00, 0x00]) {
        decode_utf32(bytes, true)
    } else if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        decode_utf16(bytes, true)
    } else if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        decode_utf16(bytes, false)
    } else {
        String::from_utf8(
            bytes
                .strip_prefix(&[0xef, 0xbb, 0xbf])
                .unwrap_or(bytes)
                .to_vec(),
        )
        .map_err(|error| caused_by("invalidInput", "Invalid UTF-8 workflow data", error).into())
    }
}

/// Rejects invalid UTF-16 rather than substituting replacement characters.
fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String, AppError> {
    let mut words = Vec::new();
    let (chunks, remainder) = bytes.as_chunks::<2>();
    for &word in chunks {
        words.push(if little_endian {
            u16::from_le_bytes(word)
        } else {
            u16::from_be_bytes(word)
        });
    }
    if !remainder.is_empty() {
        return Err(invalid("Invalid UTF-16 workflow data"));
    }
    String::from_utf16(&words)
        .map_err(|error| caused_by("invalidInput", "Invalid UTF-16 workflow data", error).into())
}

/// Rejects incomplete and invalid UTF-32 code points.
fn decode_utf32(bytes: &[u8], little_endian: bool) -> Result<String, AppError> {
    let mut text = String::new();
    let (chunks, remainder) = bytes.as_chunks::<4>();
    for &word in chunks {
        let code = if little_endian {
            u32::from_le_bytes(word)
        } else {
            u32::from_be_bytes(word)
        };
        text.push(char::from_u32(code).ok_or_else(|| invalid("Invalid UTF-32 workflow data"))?);
    }
    if !remainder.is_empty() {
        return Err(invalid("Invalid UTF-32 workflow data"));
    }
    Ok(text)
}

/// Classifies invalid workflow data at the application boundary.
fn invalid(message: &str) -> AppError {
    fail("invalidInput", message.to_owned())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Value, json};

    use super::{
        batch_hash, decode_text, match_segment, match_segments, validate_manifest,
        validate_path_scope,
    };

    /// Supplies a manifest with no applicability assignment.
    fn manifest() -> Value {
        #[cfg(windows)]
        let path = r"C:\review";
        #[cfg(not(windows))]
        let path = "/review";
        json!({
            "schemaVersion": 1,
            "inventoryPath": path,
            "fileGroupsPath": path,
            "focusAreaQueuePath": path,
            "reviewers": [{"reviewerId": "focus-001", "focusArea": path, "fileGroupQueuePath": path}]
        })
    }

    #[test]
    fn validates_manifest_and_snapshot_assignments() {
        let mut value = manifest();
        validate_manifest(&value).unwrap();
        value["repositoryRoot"] = json!("root");
        value["snapshot"] =
            json!({"inventorySha256": "A".repeat(64), "fileGroupsSha256": "B".repeat(64)});
        value["reviewers"][0]["dispatch"] = json!("excluded");
        value["reviewers"][0]["focusAreaSha256"] = json!("C".repeat(64));
        value["reviewers"][0]["artifactPath"] = value["inventoryPath"].clone();
        validate_manifest(&value).unwrap();
        value["snapshot"]["inventorySha256"] = json!("a".repeat(64));
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn rejects_incomplete_duplicate_and_unknown_assignments() {
        let mut value = manifest();
        value["reviewers"][0]["dispatch"] = json!("review");
        assert!(validate_manifest(&value).is_err());
        let mut value = manifest();
        let duplicate = value["reviewers"][0].clone();
        value["reviewers"].as_array_mut().unwrap().push(duplicate);
        assert!(validate_manifest(&value).is_err());
        let mut value = manifest();
        value["extra"] = json!(true);
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn reads_bom_encodings_as_text() {
        assert_eq!(decode_text(&[0xef, 0xbb, 0xbf, b'a']).unwrap(), "a");
        assert_eq!(decode_text(&[0xff, 0xfe, b'a', 0]).unwrap(), "a");
        assert_eq!(
            decode_text(&[0, 0, 0xfe, 0xff, 0, 0, 0, b'a']).unwrap(),
            "a"
        );
    }

    #[test]
    fn rejects_invalid_unicode_in_each_supported_encoding() {
        for bytes in [
            &[0xff][..],
            &[0xef, 0xbb, 0xbf, 0xff],
            &[0xff, 0xfe, 0x00, 0xd8],
            &[0xfe, 0xff, 0xd8],
            &[0xff, 0xfe, 0x00, 0x00, 0x00, 0x00, 0x11, 0x00],
            &[0x00, 0x00, 0xfe, 0xff, 0x00],
        ] {
            decode_text(bytes).unwrap_err();
        }
    }

    #[test]
    fn receipt_hash_matches_powershell_indented_json() {
        let group = json!({"groupId":"g","files":[{"path":"/a"}]});
        let records = HashMap::from([(
            "/a".to_owned(),
            json!({"groupId":"g","path":"/a","disposition":"reviewed","scope":"","reason":""}),
        )]);
        let ids = vec![json!("b"), json!("a")];
        assert_eq!(
            batch_hash(&group, &records, "/x", &ids).unwrap(),
            "82AE1472681A7E9E6BBC55F415E3A964C98A7977AA334C30B95F04964451B01E"
        );
    }

    #[test]
    fn scope_globs_retain_or_exclude_by_segments() {
        assert!(match_segments(
            &["src", "**", "*.rs"],
            &["src", "inner", "lib.rs"]
        ));
        assert!(match_segments(&["src", "**", "*.rs"], &["src", "LIB.RS"]));
        assert!(!match_segments(&["src", "**", "*.rs"], &["docs", "lib.rs"]));
    }

    #[test]
    fn reusable_match_rows_do_not_retain_prior_matches() {
        for (pattern, path, expected) in [
            ("", "", true),
            ("*", "", true),
            ("*a", "", false),
            ("*a*", "bbb", false),
            ("*a*b", "acb", true),
            ("*a*b", "ac", false),
            ("?*.RS", "A.rs", true),
            ("?", "", false),
            ("Ä?", "äX", true),
        ] {
            assert_eq!(
                match_segment(pattern, path),
                expected,
                "{pattern:?} {path:?}"
            );
        }

        for (pattern, path, expected) in [
            (&[][..], &[][..], true),
            (&["**"][..], &[][..], true),
            (&["**", "src"][..], &[][..], false),
            (&["**", "src", "**"][..], &["docs", "lib"][..], false),
            (&["**", "src", "**"][..], &["src"][..], true),
            (
                &["**", "src", "**", "*.rs"][..],
                &["root", "src", "inner", "file.rs"][..],
                true,
            ),
            (
                &["**", "src", "**", "*.rs"][..],
                &["root", "src", "inner", "file.txt"][..],
                false,
            ),
        ] {
            assert_eq!(
                match_segments(pattern, path),
                expected,
                "{pattern:?} {path:?}"
            );
        }
    }

    #[test]
    fn validates_path_scope_alternatives() {
        validate_path_scope(&json!({"base": ".", "include": ["src/**/*.rs", "*.md"]})).unwrap();
        for scope in [
            json!({"base": "src//lib", "include": ["*.rs"]}),
            json!({"base": "src", "include": []}),
            json!({"base": "src", "include": [""]}),
            json!({"base": "src", "include": ["*.rs", "*.rs"]}),
            json!({"base": "src", "include": ["[a].rs"]}),
            json!({"base": ".", "include": ["*.rs"], "unknown": true}),
        ] {
            assert!(validate_path_scope(&scope).is_err(), "{scope}");
        }
    }
}
