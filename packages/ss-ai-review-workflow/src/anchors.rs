//! Resolves authored finding anchors without changing the authored record.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::Path;

use ohno::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
use ss_ai_git::{Comparison, FileDiff};
use ss_ai_runtime::{Clock, fail, read_bytes};

use crate::workflow_plan::parse_json;

/// Resolves a file-backed request from the publication adapter.
///
/// The request contains an anchor and either a diff or one diff file.
///
/// # Errors
///
/// Rejects unreadable or malformed requests and unresolvable anchors.
pub fn resolve_request_path(path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let bytes = read_bytes(path, clock)?.ok_or_else(|| invalid("Anchor request is missing"))?;
    resolve_request(&parse_json(&bytes)?)
}

/// Resolves the structured request without starting another process.
///
/// # Errors
///
/// Rejects malformed requests, diff records and unresolvable anchors.
pub fn resolve_request(request: &Value) -> Result<Value, AppError> {
    let object = request
        .as_object()
        .ok_or_else(|| invalid("Anchor request must be an object"))?;
    if object
        .keys()
        .any(|key| !["anchor", "anchors", "diff", "diffFile"].contains(&key.as_str()))
    {
        return Err(invalid("Unknown anchor request field"));
    }
    if let Some(anchors) = object.get("anchors") {
        if object.contains_key("anchor") || object.contains_key("diffFile") {
            return Err(invalid("Supply anchors and diff only"));
        }
        let anchors = anchors
            .as_array()
            .ok_or_else(|| invalid("Anchors must be an array"))?;
        let files = read_diff(object.get("diff").ok_or_else(|| invalid("Missing diff"))?)?;
        let paths: HashMap<_, _> = files.iter().map(|file| (file.path(), file)).collect();
        let results: Vec<_> = anchors
            .iter()
            .map(|anchor| {
                // PowerShell's dictionary parameter rejects missing or non-object anchors
                // before resolution; freshness must not discard those as stale findings.
                if !anchor.is_object() {
                    return json!({"argumentError": "Anchor must be a non-null object"});
                }
                let result = (|| {
                    let path = canonical_path(text(anchor, "path")?)?;
                    let file = paths.get(path.as_str()).ok_or_else(|| {
                        invalid(format!(
                            "The reviewed diff does not change '{path}', so a finding cannot be anchored to it."
                        ))
                    })?;
                    resolve_anchor(anchor, file)
                })();
                match result {
                    Ok(anchor) => json!({"anchor": anchor}),
                    Err(error) => json!({"error": format!("Unresolvable anchor: {error}")}),
                }
            })
            .collect();
        return Ok(json!({"results": results}));
    }
    let anchor = object
        .get("anchor")
        .ok_or_else(|| invalid("Missing anchor"))?;
    match (object.get("diff"), object.get("diffFile")) {
        (Some(diff), None) => {
            let parsed = read_diff(diff)?;
            resolve_anchor_in_files(anchor, &parsed)
                .map_err(|error| invalid(format!("Unresolvable anchor: {error}")))
        }
        (None, Some(file)) => {
            let file = read_file(file)?;
            resolve_anchor(anchor, &file)
                .map_err(|error| invalid(format!("Unresolvable anchor: {error}")))
        }
        _ => Err(invalid("Supply either diff or diffFile")),
    }
}

/// Validates the entire diff, including files no anchor happens to name.
fn read_diff(diff: &Value) -> Result<Vec<FileDiff>, AppError> {
    let files = diff
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Diff requires a files array"))?;
    let mut paths = HashSet::new();
    let mut parsed = Vec::with_capacity(files.len());
    for value in files {
        let file = read_file(value)?;
        if !paths.insert(file.path().to_owned()) {
            return Err(invalid("Diff contains duplicate file paths"));
        }
        parsed.push(file);
    }
    Ok(parsed)
}

/// Validates serialized Git evidence before using it for publication.
fn read_file(value: &Value) -> Result<FileDiff, AppError> {
    let file = FileDiff::deserialize(value)
        .map_err(|error| invalid(format!("Invalid diff file: {error}")))?;
    file.validate()?;
    Ok(file)
}

/// Resolves an anchor using the exact comparison used by publication.
///
/// A range outside a hunk becomes a file-level anchor; an absent file fails.
///
/// # Errors
///
/// Returns an error for an invalid anchor or a path absent from the comparison.
pub fn resolve_anchor_in_diff(anchor: &Value, comparison: &Comparison) -> Result<Value, AppError> {
    resolve_anchor_in_files(anchor, comparison.files())
}

/// Looks up the exact post-change path in validated Git evidence.
fn resolve_anchor_in_files(anchor: &Value, files: &[FileDiff]) -> Result<Value, AppError> {
    let path = canonical_path(text(anchor, "path")?)?;
    let file = files
        .iter()
        .find(|file| file.path() == path)
        .ok_or_else(|| {
            invalid(format!(
                "The reviewed diff does not change '{path}', so a finding cannot be anchored to it."
            ))
        })?;
    resolve_anchor(anchor, file)
}

/// Resolves an anchor against one changed file.
///
/// # Errors
///
/// Returns an error for an invalid anchor or a different file.
pub fn resolve_anchor(anchor: &Value, file: &FileDiff) -> Result<Value, AppError> {
    file.validate()?;
    let object = anchor
        .as_object()
        .ok_or_else(|| invalid("Anchor must be an object"))?;
    if object
        .keys()
        .any(|key| !["kind", "path", "startLine", "endLine", "side"].contains(&key.as_str()))
    {
        return Err(invalid("Unknown anchor field"));
    }
    let path = canonical_path(text(anchor, "path")?)?;
    // The single-file PowerShell helper accepts case-insensitive pairs; whole
    // diff lookup remains exact, so no other changed file can be substituted.
    if path.to_lowercase() != file.path().to_lowercase() {
        return Err(invalid(format!(
            "An anchor on '{path}' cannot be resolved against the diff entry for '{}'.",
            file.path()
        )));
    }
    let kind = match anchor.get("kind") {
        None => "file",
        Some(Value::String(kind)) => kind,
        _ => return Err(invalid("Invalid anchor kind")),
    };
    if kind == "file" {
        if ["startLine", "endLine", "side"]
            .iter()
            .any(|key| object.contains_key(*key))
        {
            return Err(invalid("File anchor cannot name a position"));
        }
        return Ok(json!({"kind": "file", "path": file.path(), "wasDowngraded": false}));
    }
    if kind != "line" {
        return Err(invalid(format!(
            "An anchor of kind '{kind}' cannot be resolved."
        )));
    }
    let start = line(anchor, "startLine")?
        .ok_or_else(|| invalid(format!("A line anchor on '{path}' names no line.")))?;
    let end = line(anchor, "endLine")?.unwrap_or(start);
    if end < start {
        return Err(invalid("Line anchor ends before it starts"));
    }
    let side = match anchor.get("side") {
        None | Some(Value::Null) => "right".to_owned(),
        Some(Value::String(value)) if value.trim().is_empty() => "right".to_owned(),
        Some(Value::String(value)) => value.to_ascii_lowercase(),
        _ => return Err(invalid("Invalid anchor side")),
    };
    if side != "left" && side != "right" {
        return Err(invalid(format!(
            "A line anchor on '{path}' names side '{side}', which is neither the post-change nor the pre-change side."
        )));
    }
    if file.contains_range(start.get(), end.get(), side == "left") {
        return Ok(
            json!({"kind": "line", "path": file.path(), "startLine": start,
            "endLine": end, "side": side, "wasDowngraded": false}),
        );
    }
    let range = if start == end {
        format!("line {start}")
    } else {
        format!("lines {start}-{end}")
    };
    Ok(
        json!({"kind": "file", "path": file.path(), "wasDowngraded": true,
        "downgradeReason": format!("No hunk of the reviewed diff covers {range} on the {side} side of '{}', so the finding is reported against the file.", file.path())}),
    )
}

/// Normalizes the repository-relative spelling accepted by finding records.
pub(crate) fn canonical_path(path: &str) -> Result<String, AppError> {
    if path.trim().is_empty() {
        return Err(invalid("A finding names no file"));
    }
    if path.starts_with('/')
        || (path.as_bytes().get(1) == Some(&b':')
            && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
    {
        return Err(invalid("Invalid repository-relative anchor path"));
    }
    if path.contains('\\') {
        return Err(invalid("Anchor path uses backslashes"));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            ".." => return Err(invalid("Anchor path leaves the repository")),
            "" | "." => {}
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(invalid("Anchor path names no file"));
    }
    Ok(parts.join("/"))
}

/// Reads a positive line number without silently converting malformed JSON.
fn line(anchor: &Value, name: &str) -> Result<Option<NonZeroU32>, AppError> {
    match anchor.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let value = value
                .as_u64()
                .filter(|value| *value <= i32::MAX as u64)
                .and_then(|value| u32::try_from(value).ok());
            match value {
                Some(0) if name == "endLine" => Ok(None),
                Some(value) => NonZeroU32::new(value)
                    .map(Some)
                    .ok_or_else(|| invalid(format!("Invalid anchor {name}"))),
                _ => Err(invalid(format!("Invalid anchor {name}"))),
            }
        }
    }
}

/// Requires a nonempty string at the anchor boundary.
fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str, AppError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("Missing or invalid {name}")))
}

/// Marks malformed authored data as invalid input.
fn invalid(message: impl Into<String>) -> AppError {
    fail("invalidInput", message.into())
}
