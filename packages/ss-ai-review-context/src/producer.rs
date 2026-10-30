//! Builds and publishes immutable captures from provider-independent evidence.

use std::any::type_name;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, FileTransaction, fail};

use crate::capture::{
    CAPTURE_VERSION, POLICY_VERSION, apply_decisions, array_property, comment_input_hash,
    last_matching, parse_file, property, selected, string_id, validate_capture_keys,
    validate_capture_structure, validate_decisions,
};
use crate::query::capture_delta;
use crate::request::{full_path, path_string};
use crate::{document_hash, powershell_json, read_capture};

/// Retains the destination lock and the checked baseline across provider reads.
pub struct CapturePreparation<'a> {
    path: PathBuf,
    sidecar: FileTransaction<'a>,
    capture: FileTransaction<'a>,
    previous: Option<Value>,
    decisions: Option<Value>,
    target_url: String,
}

impl fmt::Debug for CapturePreparation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(type_name::<Self>())
            .field("path", &self.path)
            .field("has_baseline", &self.previous.is_some())
            .finish_non_exhaustive()
    }
}

/// Checks immutable output and comparison evidence before provider access.
pub fn prepare_capture<'a>(
    path: &Path,
    compare: Option<&Path>,
    target_url: &str,
    clock: &'a dyn Clock,
) -> Result<CapturePreparation<'a>, AppError> {
    let path = full_path(path)?;
    let output_sidecar = sidecar_path(&path);
    if let Some(compare) = compare {
        let compare = full_path(compare)?;
        if compare == path
            || compare == output_sidecar
            || fs::canonicalize(&compare)
                .ok()
                .zip(fs::canonicalize(&path).ok())
                .is_some_and(|(a, b)| a == b)
        {
            return Err(fail(
                "invalidInput",
                "Give the capture a distinct -OutputPrefix",
            ));
        }
    }
    let (previous, decisions) = if let Some(compare) = compare {
        let compare = full_path(compare)?;
        let lock = FileTransaction::open(&sidecar_path(&compare), clock)?;
        let previous = read_capture(&compare, clock)?;
        if !matches_target(&previous, target_url) {
            return Err(fail(
                "invalidInput",
                "Cannot compare captures of different pull requests",
            ));
        }
        let decisions = lock
            .read_bytes()?
            .map(|bytes| parse_file(&bytes))
            .transpose()?;
        (Some(previous), decisions)
    } else {
        (None, None)
    };
    let sidecar = FileTransaction::open(&output_sidecar, clock)?;
    let capture = FileTransaction::open(&path, clock)?;
    if sidecar.read_bytes()?.is_some() || capture.read_bytes()?.is_some() {
        return Err(fail(
            "invalidInput",
            "Capture output already exists; choose a new prefix",
        ));
    }
    Ok(CapturePreparation {
        path,
        sidecar,
        capture,
        previous,
        decisions,
        target_url: target_url.to_owned(),
    })
}

impl CapturePreparation<'_> {
    /// Validates and publishes the sidecar before making the capture visible.
    pub fn publish(self, context: &Value, fetched_utc: &str) -> Result<Value, AppError> {
        let mut document = document(context, fetched_utc)?;
        validate_capture_keys(&document)?;
        validate_capture_structure(&document)?;
        if !self.target_url.contains("/_git/pullrequest/")
            && property(&document, "pullRequestUrl").as_str() != Some(&self.target_url)
        {
            return Err(fail(
                "invalidInput",
                "Provider returned a different pull request",
            ));
        }
        if let Some(previous) = &self.previous
            && property(previous, "pullRequestUrl") != property(&document, "pullRequestUrl")
        {
            return Err(fail(
                "invalidInput",
                "Provider returned a different pull request",
            ));
        }
        let raw_text = powershell_json(&document)?;
        let mut decisions = json!({"schemaVersion": 1, "policyVersion": POLICY_VERSION, "captureHash": document_hash(&document)?, "decisions": []});
        let mut reused = Vec::new();
        if let Some(previous) = &self.decisions
            && property(previous, "policyVersion").as_str() == Some(POLICY_VERSION)
        {
            for entry in array_property(previous, "decisions") {
                let thread_id = property(&entry, "threadId").as_str().unwrap_or_default();
                let comment_id = property(&entry, "commentId").as_str().unwrap_or_default();
                let Some((is_array, index)) =
                    last_matching(property(&document, "threads"), thread_id)
                else {
                    continue;
                };
                let thread = selected(property(&document, "threads"), is_array, index);
                let Some((is_array, index)) =
                    last_matching(property(thread, "comments"), comment_id)
                else {
                    continue;
                };
                let comment = selected(property(thread, "comments"), is_array, index);
                if property(comment, "authorship").as_str() == Some("indeterminate")
                    && property(&entry, "inputHash").as_str()
                        == Some(&comment_input_hash(&document, thread_id, comment)?)
                {
                    let mut entry = entry;
                    entry
                        .as_object_mut()
                        .ok_or_else(|| fail("invalidInput", "Invalid authorship decision"))?
                        .insert("origin".to_owned(), json!("reused"));
                    reused.push(entry);
                }
            }
        }
        decisions
            .as_object_mut()
            .ok_or_else(|| fail("invalidInput", "Invalid authorship record"))?
            .insert("decisions".to_owned(), json!(reused));
        validate_decisions(&decisions)?;
        apply_decisions(&mut document, &decisions)?;
        let delta = self
            .previous
            .as_ref()
            .map(|previous| capture_delta(previous, &document))
            .transpose()?;
        let threads = array_property(&document, "threads");
        let comments: Vec<_> = threads
            .iter()
            .flat_map(|thread| array_property(thread, "comments"))
            .collect();
        let pull_request = property(&document, "pullRequest");
        let result = json!({
            "platform": property(&document, "platform"),
            "pullRequestUrl": property(&document, "pullRequestUrl"),
            "repositoryUrl": property(&document, "repositoryUrl"),
            "contextPath": path_string(&self.path)?,
            "fetchedUtc": property(&document, "fetchedUtc"),
            "reviewedSourceCommit": property(pull_request, "reviewedSourceCommit"),
            "reviewedMergeBaseCommit": property(pull_request, "reviewedMergeBaseCommit"),
            "threadCount": threads.len(),
            "commentCount": comments.len(),
            "reviewerCount": array_property(&document, "reviewers").len(),
            "indeterminateCommentCount": comments.iter().filter(|comment| property(comment, "authorship").as_str() == Some("indeterminate")).count(),
            "reusedClassificationCount": reused.len(),
            "delta": delta
        });
        self.sidecar
            .write_bytes(powershell_json(&decisions)?.as_bytes())?;
        self.capture.write_bytes(raw_text.as_bytes())?;
        Ok(result)
    }
}

/// Matches identifier-only Azure targets within their globally scoped collection.
fn matches_target(previous: &Value, target_url: &str) -> bool {
    if property(previous, "pullRequestUrl").as_str() == Some(target_url) {
        return true;
    }
    let Some((collection, id)) = target_url.rsplit_once("/_git/pullrequest/") else {
        return false;
    };
    let target = property(previous, "target");
    property(previous, "platform").as_str() == Some("azureDevOps")
        && property(target, "collectionUrl").as_str() == Some(collection)
        && string_id(property(target, "pullRequestId")) == id
}

/// Derives the shared sidecar path without altering the capture extension.
fn sidecar_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".authorship.json");
    name.into()
}

/// Converts provider evidence into the ordered canonical capture model.
fn document(context: &Value, fetched_utc: &str) -> Result<Value, AppError> {
    for name in ["reviewers", "revisions", "threads"] {
        if !property(context, name).is_array() {
            return Err(fail(
                "invalidInput",
                format!("Provider context requires an array for {name}"),
            ));
        }
    }
    if !property(context, "authenticatedIdentity").is_object() {
        return Err(fail(
            "invalidInput",
            "Provider context requires an authenticated identity",
        ));
    }
    let target = property(context, "target");
    let url = property(target, "webUrl")
        .as_str()
        .ok_or_else(|| fail("invalidInput", "Provider target has no URL"))?;
    let platform = property(target, "platform")
        .as_str()
        .ok_or_else(|| fail("invalidInput", "Provider target has no platform"))?;
    let suffix = if platform == "gitHub" {
        "/pull/"
    } else {
        "/pullrequest/"
    };
    let repository_url = url
        .rsplit_once(suffix)
        .map(|(repository, _)| repository)
        .ok_or_else(|| fail("invalidInput", "Provider target URL is incomplete"))?;
    let identity = property(context, "authenticatedIdentity");
    let mut pull_request = property(context, "pullRequest").clone();
    let self_authored = identity_matches(property(&pull_request, "author"), identity);
    pull_request
        .as_object_mut()
        .ok_or_else(|| fail("invalidInput", "Missing pull request"))?
        .insert("isAuthoredBySelf".to_owned(), json!(self_authored));
    let mut threads = array_property(context, "threads");
    for thread in &mut threads {
        if !property(thread, "comments").is_array() {
            return Err(fail(
                "invalidInput",
                "Provider thread requires a comments array",
            ));
        }
        let mut comments = array_property(thread, "comments");
        comments.sort_by_key(|comment| sort_key(comment, "publishedUtc"));
        for comment in &mut comments {
            let authored = identity_matches(property(comment, "author"), identity);
            comment
                .as_object_mut()
                .ok_or_else(|| fail("invalidInput", "Invalid comment"))?
                .insert("isAuthoredBySelf".to_owned(), json!(authored));
        }
        let opener = string_id(property(thread, "openingCommentId"));
        let author = comments
            .iter()
            .find(|comment| !opener.is_empty() && string_id(property(comment, "id")) == opener)
            .map_or_else(
                || property(property(thread, "decision"), "by"),
                |comment| property(comment, "author"),
            );
        let authored = identity_matches(author, identity);
        let object = thread
            .as_object_mut()
            .ok_or_else(|| fail("invalidInput", "Invalid thread"))?;
        object.insert("comments".to_owned(), json!(comments));
        object.insert("isAuthoredBySelf".to_owned(), json!(authored));
    }
    threads.sort_by_key(|thread| sort_key(thread, "eventUtc"));
    Ok(json!({
        "schemaVersion": CAPTURE_VERSION, "fetchedUtc": fetched_utc, "platform": platform,
        "repositoryUrl": repository_url, "pullRequestUrl": url, "target": target,
        "authenticatedIdentity": identity, "pullRequest": pull_request,
        "reviewers": array_property(context, "reviewers"), "revisions": array_property(context, "revisions"), "threads": threads
    }))
}

/// Orders timestamped events deterministically, placing missing times last.
fn sort_key(value: &Value, timestamp: &str) -> String {
    let stamp = property(value, timestamp)
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("9999-12-31T23:59:59.9999999Z");
    let id = string_id(property(value, "id"));
    let id = if !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{id:0>20}")
    } else {
        id
    };
    format!("{stamp}|{id}")
}

/// Uses the strongest identity field shared by both account records.
fn identity_matches(left: &Value, right: &Value) -> bool {
    for name in ["descriptor", "id", "uniqueName"] {
        let left = property(left, name)
            .as_str()
            .filter(|s| !s.trim().is_empty());
        let right = property(right, name)
            .as_str()
            .filter(|s| !s.trim().is_empty());
        if let (Some(left), Some(right)) = (left, right) {
            return left
                .chars()
                .map(ordinal_upper)
                .eq(right.chars().map(ordinal_upper));
        }
    }
    false
}

/// Uses simple uppercase mapping without introducing multi-character expansions.
fn ordinal_upper(character: char) -> char {
    let mut uppercase = character.to_uppercase();
    let first = uppercase.next().unwrap_or(character);
    if uppercase.next().is_some() {
        character
    } else {
        first
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::identity_matches;

    #[test]
    fn identity_uses_strongest_shared_field_with_simple_case_matching() {
        assert!(identity_matches(
            &json!({"id":"Émilie"}),
            &json!({"id":"émilie"})
        ));
        assert!(!identity_matches(
            &json!({"id":"one","uniqueName":"same"}),
            &json!({"id":"two","uniqueName":"same"})
        ));
        assert!(!identity_matches(&json!({"id":"ß"}), &json!({"id":"SS"})));
    }
}
