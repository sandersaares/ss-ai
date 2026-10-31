//! Builds and publishes immutable captures from provider-independent evidence.

use std::any::type_name;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail, read_bytes};

use crate::capture::{
    CAPTURE_VERSION, POLICY_VERSION, apply_decisions, array_property, comment_input_hash,
    last_matching, parse_file, property, property_values, selected, string_id,
    validate_capture_keys, validate_capture_structure, validate_decisions,
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
        let compare_sidecar = sidecar_path(&compare);
        // Even an absent sidecar is reserved for the comparison's decisions.
        for baseline in [&compare, &compare_sidecar] {
            for output in [&path, &output_sidecar] {
                if same_existing_path(baseline, output)? {
                    return Err(fail(
                        "invalidInput",
                        "Give the capture a distinct -OutputPrefix",
                    ));
                }
            }
        }
    }
    let (previous, previous_raw, decisions) = if let Some(compare) = compare {
        let compare = full_path(compare)?;
        let compare_sidecar = sidecar_path(&compare);
        let lock = FileTransaction::open(&compare_sidecar, clock)?;
        // The retained lock exists even when the sidecar does not, so the
        // filesystem can resolve case and directory aliases without creating
        // or modifying the sidecar itself.
        for output in [&path, &output_sidecar] {
            if same_lock_path(&compare_sidecar, output)? {
                return Err(fail(
                    "invalidInput",
                    "Give the capture a distinct -OutputPrefix",
                ));
            }
        }
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
        let decisions = match lock.read_bytes()? {
            Some(bytes) => Some(parse_file(&bytes)?),
            None => {
                // A dangling link is not an absent sidecar: publishing its
                // target could make the baseline acquire unrelated decisions.
                match fs::symlink_metadata(&compare_sidecar) {
                    Ok(_) => {
                        return Err(fail(
                            "invalidInput",
                            "Comparison authorship sidecar points to a missing file",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(caused_by(
                            "io",
                            format!("Cannot inspect {}", compare_sidecar.display()),
                            error,
                        )
                        .into());
                    }
                }
            }
        };
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
        let threads = property_values(&document, "threads");
        let (comment_count, indeterminate_count) = threads
            .iter()
            .flat_map(|thread| property_values(thread, "comments"))
            .fold((0_usize, 0_usize), |(comments, indeterminate), comment| {
                (
                    comments
                        .checked_add(1)
                        .expect("comment count is bounded by the in-memory capture"),
                    indeterminate
                        .checked_add(usize::from(
                            property(comment, "authorship").as_str() == Some("indeterminate"),
                        ))
                        .expect("classification count is bounded by the in-memory capture"),
                )
            });
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
            "commentCount": comment_count,
            "reviewerCount": property_values(&document, "reviewers").len(),
            "indeterminateCommentCount": indeterminate_count,
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
        publish_new(&self.path, raw_text.as_bytes()).map_err(|error| {
            caused_by(
                "io",
                format!(
                    "Capture publication failed after publishing {}",
                    sidecar_path(&self.path).display()
                ),
                error,
            )
        })?;
        Ok(result)
    }
}

/// Detects aliases of existing files or absent names in the same directory.
fn same_existing_path(left: &Path, right: &Path) -> Result<bool, AppError> {
    if left == right {
        return Ok(true);
    }
    Ok(resolved_path(left)?
        .zip(resolved_path(right)?)
        .is_some_and(|(left, right)| left == right))
}

/// Resolves directory aliases while preserving actual filesystem errors.
fn resolved_path(path: &Path) -> Result<Option<PathBuf>, AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| fail("invalidInput", "File path requires a parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| fail("invalidInput", "File path requires a name"))?;
    let Some(parent) = canonical_path(parent)? else {
        return Ok(None);
    };
    let metadata = fs::metadata(&parent)
        .map_err(|error| caused_by("io", format!("Cannot inspect {}", parent.display()), error))?;
    // Some filesystems report a child of a regular file as missing rather than
    // not-a-directory; reject it before acquiring comparison locks either way.
    if !metadata.is_dir() {
        return Err(caused_by(
            "io",
            format!("Cannot resolve {}", path.display()),
            io::Error::from(io::ErrorKind::NotADirectory),
        )
        .into());
    }
    Ok(Some(
        canonical_path(path)?.unwrap_or_else(|| parent.join(name)),
    ))
}

/// Resolves existing paths and reports failures other than missing paths.
fn canonical_path(path: &Path) -> Result<Option<PathBuf>, AppError> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(caused_by("io", format!("Cannot resolve {}", path.display()), error).into())
        }
    }
}

/// Compares retained sibling locks using the filesystem's name resolution.
fn same_lock_path(baseline: &Path, output: &Path) -> Result<bool, AppError> {
    let lock_name = |path: &Path| {
        let mut name = path.as_os_str().to_os_string();
        name.push(".lock");
        PathBuf::from(name)
    };
    let baseline_lock_path = lock_name(baseline);
    let baseline_lock = canonical_path(&baseline_lock_path)?
        .ok_or_else(|| fail("io", "Comparison sidecar lock disappeared"))?;
    if canonical_path(&lock_name(output))?.as_ref() == Some(&baseline_lock) {
        return Ok(true);
    }

    // Appending ".lock" changes whether a trailing dot or space is significant.
    // Probe the existing lock before comparing the trimmed output name.
    let Some(name) = output.file_name().and_then(|name| name.to_str()) else {
        return Ok(false);
    };
    let trimmed = name.trim_end_matches(['.', ' ']);
    if trimmed == name || trimmed.is_empty() {
        return Ok(false);
    }
    let suffix = name
        .strip_prefix(trimmed)
        .ok_or_else(|| fail("io", "Cannot compare output lock name"))?;
    let mut probe = baseline_lock_path.as_os_str().to_os_string();
    probe.push(suffix);
    if canonical_path(Path::new(&probe))?.as_ref() != Some(&baseline_lock) {
        return Ok(false);
    }
    Ok(
        canonical_path(&lock_name(&output.with_file_name(trimmed)))?.as_ref()
            == Some(&baseline_lock),
    )
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
            Err(error) => {
                return Err(caused_by(
                    "io",
                    format!("Cannot create staging file for {}", path.display()),
                    error,
                )
                .into());
            }
        }
    }
    let (staging, mut file) = created.ok_or_else(|| {
        fail(
            "io",
            format!("Cannot allocate staging file for {}", path.display()),
        )
    })?;
    let result = (|| -> Result<(), AppError> {
        file.write_all(bytes).map_err(|error| {
            caused_by(
                "io",
                format!("Cannot write staging file for {}", path.display()),
                error,
            )
        })?;
        file.sync_all().map_err(|error| {
            caused_by(
                "io",
                format!("Cannot sync staging file for {}", path.display()),
                error,
            )
        })?;
        drop(file);
        fs::hard_link(&staging, path).map_err(|error| {
            caused_by("io", format!("Cannot publish {}", path.display()), error)
        })?;
        Ok(())
    })();
    let cleanup = fs::remove_file(&staging);
    finish_publication(path, result, cleanup)
}

/// Reports cleanup failures without obscuring whether publication succeeded.
fn finish_publication(
    path: &Path,
    result: Result<(), AppError>,
    cleanup: io::Result<()>,
) -> Result<(), AppError> {
    if let Err(error) = result {
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup) => Err(caused_by(
                "io",
                format!(
                    "Publication of {} failed, and staging cleanup also failed: {cleanup}",
                    path.display()
                ),
                error,
            )
            .into()),
        };
    }
    cleanup.map_err(|error| {
        caused_by(
            "io",
            format!(
                "Published {}, but cannot remove its staging file",
                path.display()
            ),
            error,
        )
    })?;
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
        // Padding preserves numeric order in text comparisons. The width exceeds
        // realistic provider identifiers, matching PullRequestModel.ps1.
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
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::io;
    use std::path::Path;

    use serde_json::json;
    use ss_ai_runtime::{caused_by, classification};

    use super::{finish_publication, identity_matches};

    #[test]
    fn publication_classifies_failures_before_and_after_link() {
        let path = Path::new("capture.json");
        let error = finish_publication(
            path,
            Err(caused_by(
                "io",
                "Cannot publish capture.json",
                io::Error::from(io::ErrorKind::AlreadyExists),
            )
            .into()),
            Err(io::ErrorKind::PermissionDenied.into()),
        )
        .unwrap_err();
        assert_eq!(classification(&error), "io");
        assert!(error.to_string().contains("cleanup also failed"));
        assert!(error.to_string().contains("Cannot publish capture.json"));
        assert_eq!(
            error.find_source::<io::Error>().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );

        let error = finish_publication(path, Ok(()), Err(io::ErrorKind::PermissionDenied.into()))
            .unwrap_err();
        assert_eq!(classification(&error), "io");
        assert!(error.to_string().contains("Published capture.json"));
        assert!(error.to_string().contains("staging file"));
        assert_eq!(
            error.find_source::<io::Error>().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

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
