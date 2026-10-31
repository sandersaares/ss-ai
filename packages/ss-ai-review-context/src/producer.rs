//! Builds and publishes immutable captures from provider-independent evidence.

use std::any::type_name;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, FileTransaction, fail, read_bytes};

use crate::capture::{
    CAPTURE_VERSION, POLICY_VERSION, apply_decisions, array_property, comment_input_hash,
    last_matching, parse_file, property, selected, string_id, validate_capture_keys,
    validate_capture_structure, validate_decisions,
};
use crate::query::capture_delta;
use crate::request::{full_path, path_string};
use crate::{document_hash, powershell_json};

/// Retains the destination lock and the checked baseline across provider reads.
pub struct CapturePreparation<'a> {
    path: PathBuf,
    sidecar: FileTransaction<'a>,
    capture: FileTransaction<'a>,
    previous: Option<Value>,
    previous_raw: Option<Value>,
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
        if same_existing_path(&compare, &path) || same_existing_path(&compare, &output_sidecar) {
            return Err(fail(
                "invalidInput",
                "Give the capture a distinct -OutputPrefix",
            ));
        }
    }
    let (previous, previous_raw, decisions) = if let Some(compare) = compare {
        let compare = full_path(compare)?;
        let lock = FileTransaction::open(&sidecar_path(&compare), clock)?;
        let bytes = read_bytes(&compare, clock)?
            .ok_or_else(|| fail("invalidInput", "Comparison capture is missing"))?;
        let raw = parse_file(&bytes)?;
        validate_capture_keys(&raw)?;
        if property(&raw, "schemaVersion").as_i64() != Some(CAPTURE_VERSION) {
            return Err(fail(
                "invalidInput",
                format!(
                    "Unsupported capture schema version {}. Capture the pull request again.",
                    property(&raw, "schemaVersion")
                ),
            ));
        }
        validate_capture_structure(&raw)?;
        let decisions = lock
            .read_bytes()?
            .map(|bytes| parse_file(&bytes))
            .transpose()?;
        let mut previous = raw.clone();
        if let Some(decisions) = &decisions {
            apply_decisions(&mut previous, decisions)?;
        }
        if !matches_target(&previous, target_url) {
            return Err(fail(
                "invalidInput",
                "Cannot compare captures of different pull requests",
            ));
        }
        (Some(previous), Some(raw), decisions)
    } else {
        (None, None, None)
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
        previous_raw,
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
        if !matches_target(&document, &self.target_url) {
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
            && let Some(previous_raw) = &self.previous_raw
            && property(previous, "policyVersion").as_str() == Some(POLICY_VERSION)
        {
            for entry in array_property(previous, "decisions") {
                let thread_id = property(&entry, "threadId").as_str().unwrap_or_default();
                let comment_id = property(&entry, "commentId").as_str().unwrap_or_default();
                // A stale decision must match both the baseline and the new capture.
                let Some((is_array, index)) =
                    last_matching(property(previous_raw, "threads"), thread_id)
                else {
                    continue;
                };
                let thread = selected(property(previous_raw, "threads"), is_array, index);
                let Some((is_array, index)) =
                    last_matching(property(thread, "comments"), comment_id)
                else {
                    continue;
                };
                let comment = selected(property(thread, "comments"), is_array, index);
                if property(comment, "authorship").as_str() != Some("indeterminate")
                    || property(&entry, "inputHash").as_str()
                        != Some(&comment_input_hash(previous_raw, thread_id, comment)?)
                {
                    continue;
                }
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
                    reused.push(json!({
                        "threadId": thread_id, "commentId": comment_id,
                        "kind": property(&entry, "kind"),
                        "inputHash": property(&entry, "inputHash"), "origin": "reused"
                    }));
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
        if self.sidecar.read_bytes()?.is_some() || self.capture.read_bytes()?.is_some() {
            return Err(fail(
                "invalidInput",
                "Capture output already exists; choose a new prefix",
            ));
        }
        publish_new(
            &sidecar_path(&self.path),
            powershell_json(&decisions)?.as_bytes(),
        )?;
        publish_new(&self.path, raw_text.as_bytes())?;
        Ok(result)
    }
}

/// Detects aliases when both existing paths refer to the same file.
fn same_existing_path(left: &Path, right: &Path) -> bool {
    left == right
        || fs::canonicalize(left)
            .ok()
            .zip(fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

/// Publishes complete bytes without replacing even a non-cooperating creator.
fn publish_new(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // A crashed process can leave a staging file whose PID is later reused.
    // Bound attempts while never deleting or replacing another creator's file.
    let mut created = None;
    for _ in 0..64 {
        // The counter only supplies distinct staging names; locks guard shared state.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".stage-{}-{id}", std::process::id()));
        let staging = PathBuf::from(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
        {
            Ok(file) => {
                created = Some((staging, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let (staging, mut file) =
        created.ok_or_else(|| fail("io", "Cannot allocate a unique staging file"))?;
    let result = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::hard_link(&staging, path)?;
        Ok(())
    })();
    let cleanup = fs::remove_file(&staging);
    result?;
    cleanup?;
    Ok(())
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
    let component = |name| -> Result<String, AppError> {
        let value = property(target, name)
            .as_str()
            .ok_or_else(|| fail("invalidInput", format!("Provider target requires {name}")))?;
        Ok(escape_uri_component(value))
    };
    let repository_url = if platform == "gitHub" {
        format!(
            "https://github.com/{}/{}",
            component("organization")?,
            component("repository")?
        )
    } else if platform == "azureDevOps" {
        let collection = property(target, "collectionUrl")
            .as_str()
            .ok_or_else(|| fail("invalidInput", "Provider target requires collectionUrl"))?;
        format!(
            "{collection}/{}/_git/{}",
            component("project")?,
            component("repository")?
        )
    } else {
        return Err(fail("invalidInput", "Unsupported provider platform"));
    };
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
        comments.sort_by_cached_key(|comment| sort_key(comment, "publishedUtc"));
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
            .map(|comment| property(comment, "author"))
            .filter(|author| !author.is_null())
            .unwrap_or_else(|| property(property(thread, "decision"), "by"));
        let authored = identity_matches(author, identity);
        let object = thread
            .as_object_mut()
            .ok_or_else(|| fail("invalidInput", "Invalid thread"))?;
        object.insert("comments".to_owned(), json!(comments));
        object.insert("isAuthoredBySelf".to_owned(), json!(authored));
    }
    threads.sort_by_cached_key(|thread| sort_key(thread, "eventUtc"));
    Ok(json!({
        "schemaVersion": CAPTURE_VERSION, "fetchedUtc": fetched_utc, "platform": platform,
        "repositoryUrl": repository_url, "pullRequestUrl": url, "target": target,
        "authenticatedIdentity": identity, "pullRequest": pull_request,
        "reviewers": array_property(context, "reviewers"), "revisions": array_property(context, "revisions"), "threads": threads
    }))
}

/// Orders timestamped events deterministically, placing missing times last.
fn sort_key(value: &Value, timestamp: &str) -> Vec<u16> {
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
    // .NET's ordinal comparison sorts UTF-16 code units, not UTF-8 bytes.
    format!("{stamp}|{id}").encode_utf16().collect()
}

/// Encodes path segments using the URI component rules used by the model.
fn escape_uri_component(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            result.push(char::from(byte));
        } else {
            write!(result, "%{byte:02X}").expect("writing to a string cannot fail");
        }
    }
    result
}

/// Uses the strongest identity field shared by both account records.
fn identity_matches(left: &Value, right: &Value) -> bool {
    for name in ["descriptor", "id", "uniqueName"] {
        let left = string_id(property(left, name));
        let right = string_id(property(right, name));
        if !left.trim().is_empty() && !right.trim().is_empty() {
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
