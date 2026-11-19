//! Freezes a preparation overview before any provider publication.

use std::any::type_name;
use std::env;
use std::error::Error as StdError;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use ohno::AppError;
use regex::Regex;
use serde_json::{Map, Value, from_str, from_value, json, to_vec_pretty};
use sha2::{Digest, Sha256};
use ss_ai_runtime::{Clock, FileTransaction, decode_text_with_bom, fail, read_bytes};
use uuid::Uuid;

use crate::target::{Platform, Target};

/// Selects an already loaded capture or a complete capture file.
#[derive(Clone, Copy)]
pub(crate) enum ContextInput<'a> {
    Json(&'a Value),
    #[allow(
        dead_code,
        reason = "The requested path input is not yet constructed by an integration caller"
    )]
    Path(&'a Path),
}

/// Preserves snapshot failures and their underlying causes until reporting.
#[ohno::error]
pub(crate) struct SnapshotError;

macro_rules! snapshot_failure {
    ($name:ident, $method:ident, $with_cause:ident) => {
        #[ohno::error]
        #[display("{message}")]
        struct $name {
            message: String,
        }

        impl SnapshotError {
            fn $method(message: impl Into<String>) -> Self {
                Self::caused_by($name::new(message.into()))
            }

            fn $with_cause(
                message: impl Into<String>,
                cause: impl Into<Box<dyn StdError + Send + Sync>>,
            ) -> Self {
                Self::caused_by($name::caused_by(message.into(), cause))
            }
        }
    };
}

snapshot_failure!(InvalidInput, invalid_input, invalid_input_caused_by);
snapshot_failure!(InvalidState, invalid_state, invalid_state_caused_by);
snapshot_failure!(SnapshotIo, io, io_caused_by);

/// Retains the validated target and original JSON for snapshot identity checks.
pub(crate) struct ValidatedContext<'a> {
    pub(crate) url: &'a str,
    pub(crate) target: Target,
    target_json: &'a Value,
    source: &'a str,
    base: &'a str,
}

impl fmt::Debug for ValidatedContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(type_name::<Self>())
            .field("url", &self.url)
            .field("source", &self.source)
            .field("base", &self.base)
            .finish()
    }
}

/// Carries a frozen Markdown record after its stored bytes have been verified.
///
/// The provider boundary uses this type so path, run identity, and content hash
/// come from a snapshot validated before any publication intent is journaled.
pub(crate) struct FrozenSnapshot {
    record: Map<String, Value>,
    markdown_path: String,
    markdown_hash: String,
    source_commit: String,
    merge_base_commit: String,
    source_hash: String,
}

impl FrozenSnapshot {
    /// Returns the unchanged JSON record used by the publication journal.
    pub(crate) fn record(&self) -> Value {
        Value::Object(self.record.clone())
    }

    /// Returns the path of the immutable Markdown bytes.
    pub(crate) fn markdown_path(&self) -> &str {
        &self.markdown_path
    }

    /// Returns the digest of the immutable Markdown bytes.
    pub(crate) fn markdown_hash(&self) -> &str {
        &self.markdown_hash
    }

    /// Returns the inspected source commit bound to this snapshot.
    pub(crate) fn source_commit(&self) -> &str {
        &self.source_commit
    }

    /// Returns the inspected merge-base commit bound to this snapshot.
    pub(crate) fn merge_base_commit(&self) -> &str {
        &self.merge_base_commit
    }

    /// Returns the hash of the source overview text.
    pub(crate) fn source_hash(&self) -> &str {
        &self.source_hash
    }

    /// Builds typed fields only from a structurally valid snapshot record.
    fn from_record(record: Value) -> Result<Self, SnapshotError> {
        let invalid = || SnapshotError::invalid_state("Invalid or modified overview snapshot");
        let Value::Object(record) = record else {
            return Err(invalid());
        };
        record
            .get("runId")
            .and_then(Value::as_str)
            .filter(|value| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(invalid)?;
        let markdown_path = record
            .get("markdownPath")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(invalid)?;
        let markdown_hash = record
            .get("markdownHash")
            .and_then(Value::as_str)
            .filter(|value| hash_shape(value))
            .ok_or_else(invalid)?
            .to_owned();
        let source_commit = record
            .get("sourceCommit")
            .and_then(Value::as_str)
            .filter(|value| commit(value))
            .ok_or_else(invalid)?
            .to_owned();
        let merge_base_commit = record
            .get("mergeBaseCommit")
            .and_then(Value::as_str)
            .filter(|value| commit(value))
            .ok_or_else(invalid)?
            .to_owned();
        let source_hash = record
            .get("sourceHash")
            .and_then(Value::as_str)
            .filter(|value| hash_shape(value))
            .ok_or_else(invalid)?
            .to_owned();
        Ok(Self {
            record,
            markdown_path,
            markdown_hash,
            source_commit,
            merge_base_commit,
            source_hash,
        })
    }
}

// A conservative common UTF-8 ceiling keeps every GitHub and ADO body within 64 KiB.
const MAX_COMMENT_BYTES: usize = 64 * 1024;

/// Reserves enough bytes for the largest part label that a `usize` counter can produce.
const MAX_PART_LABEL_BYTES: usize =
    "**Pull request overview (auto-generated, part 18446744073709551615 of 18446744073709551615).**\n\n"
        .len();

/// Builds exact, attributed inline comments from the frozen Markdown report.
pub(crate) fn comment_bodies(
    snapshot: &FrozenSnapshot,
    prefix: &str,
    clock: &dyn Clock,
) -> Result<Vec<String>, SnapshotError> {
    let bytes = required_bytes(Path::new(snapshot.markdown_path()), clock)?;
    let actual_hash = hash(&bytes);
    if actual_hash != snapshot.markdown_hash() {
        return Err(SnapshotError::invalid_state(
            "Frozen overview Markdown was modified",
        ));
    }
    let report = str::from_utf8(&bytes).map_err(|error| {
        SnapshotError::invalid_state_caused_by("Frozen overview is not UTF-8", error)
    })?;
    split_comment_bodies(report, prefix)
}

/// Adds the required attribution and splits Markdown only at UTF-8 boundaries.
fn split_comment_bodies(report: &str, prefix: &str) -> Result<Vec<String>, SnapshotError> {
    let attribution = if prefix.is_empty() {
        "[Copilot speaking]".to_owned()
    } else if prefix.starts_with("[Copilot speaking]") {
        prefix.to_owned()
    } else {
        format!("[Copilot speaking]\n\n{prefix}")
    };
    let report = report
        .strip_prefix("[Copilot speaking]\n\n")
        .unwrap_or(report);
    let single = format!("{attribution}\n\n{report}");
    if single.len() <= MAX_COMMENT_BYTES {
        return Ok(vec![single]);
    }

    let overhead = attribution
        .len()
        .checked_add("\n\n".len())
        .and_then(|length| length.checked_add(MAX_PART_LABEL_BYTES))
        .ok_or_else(|| SnapshotError::invalid_input("Overview attribution is too large"))?;
    let capacity = MAX_COMMENT_BYTES
        .checked_sub(overhead)
        .filter(|capacity| *capacity > 0)
        .ok_or_else(|| {
            SnapshotError::invalid_input("Overview attribution leaves no comment content space")
        })?;
    let chunks = split_at_boundaries(report, capacity)?;
    let count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let part = index.checked_add(1).ok_or_else(|| {
                SnapshotError::invalid_state("Overview has too many comment parts")
            })?;
            let label =
                format!("**Pull request overview (auto-generated, part {part} of {count}).**\n\n");
            let body = format!("{attribution}\n\n{label}{chunk}");
            if body.len() > MAX_COMMENT_BYTES {
                return Err(SnapshotError::invalid_state(
                    "Overview comment chunk exceeded the provider limit",
                ));
            }
            Ok(body)
        })
        .collect()
}

/// Preserves exact report text while favoring paragraph and line boundaries.
fn split_at_boundaries(mut remaining: &str, capacity: usize) -> Result<Vec<&str>, SnapshotError> {
    let mut chunks = Vec::new();
    while remaining.len() > capacity {
        let mut end = capacity;
        while !remaining.is_char_boundary(end) {
            // `capacity` is positive and a smaller boundary always exists at zero.
            end = end
                .checked_sub(1)
                .ok_or_else(|| SnapshotError::invalid_state("Cannot split overview Markdown"))?;
        }
        let candidate = remaining
            .get(..end)
            .ok_or_else(|| SnapshotError::invalid_state("Cannot split overview Markdown"))?;
        let paragraph_end = candidate
            .rfind("\n\n")
            .and_then(|position| position.checked_add("\n\n".len()));
        let line_end = match candidate.rfind('\n') {
            Some(position) => position.checked_add('\n'.len_utf8()),
            None => None,
        };
        let end = paragraph_end
            .or(line_end)
            .filter(|position| *position > 0)
            .unwrap_or(end);
        if end == 0 {
            return Err(SnapshotError::invalid_state(
                "Overview Markdown made no splitting progress",
            ));
        }
        let (chunk, rest) = remaining.split_at(end);
        chunks.push(chunk);
        remaining = rest;
    }
    chunks.push(remaining);
    Ok(chunks)
}

/// Supplies a structurally valid frozen snapshot to transaction boundary tests.
#[cfg(test)]
pub(crate) fn frozen_snapshot_for_test(
    run_id: &str,
    markdown_path: &str,
    markdown_hash: &str,
) -> FrozenSnapshot {
    FrozenSnapshot::from_record(json!({
        "schemaVersion": 1,
        "runId": run_id,
        "markdownPath": markdown_path,
        "markdownHash": markdown_hash,
        "sourceCommit": "a".repeat(40),
        "mergeBaseCommit": "b".repeat(40),
        "sourceHash": "c".repeat(64),
    }))
    .expect("test frozen snapshot has valid identity fields")
}

/// Publishes or reuses the immutable report under the caller's retained lock.
///
/// The transaction must own `output_directory/overview-snapshot.json` for the
/// entire publication operation. The caller may permit refresh only after
/// establishing that no remote write completed or could have reached a service.
pub(crate) fn freeze_overview(
    context: ContextInput<'_>,
    overview_path: &Path,
    output_directory: &Path,
    skill_root: &Path,
    refresh_unpublished: bool,
    snapshot_transaction: &FileTransaction<'_>,
    clock: &dyn Clock,
) -> Result<FrozenSnapshot, SnapshotError> {
    let loaded;
    let context = match context {
        ContextInput::Json(context) => context,
        ContextInput::Path(path) => {
            let bytes = required_bytes(path, clock)?;
            loaded = parse_json(&bytes, false)?;
            &loaded
        }
    };
    let context = validate_context(context)?;
    let output_directory = full_path(output_directory)?;
    let source_path = full_path(overview_path)?;
    let skill_root = full_path(skill_root)?;
    let existing = snapshot_transaction
        .read_bytes()
        .map_err(|error| SnapshotError::io_caused_by(error.to_string(), error.into_std_error()))?;
    let overview = decode_text_with_bom(&required_bytes(&source_path, clock)?);
    let source_hash = hash(overview.as_bytes());
    if let Some(bytes) = existing {
        let snapshot = parse_json(&bytes, true)?;
        let snapshot = validate_snapshot(snapshot, context.url, context.target_json, clock)?;
        if !refresh_unpublished
            || (snapshot.source_hash() == source_hash
                && snapshot.source_commit() == context.source
                && snapshot.merge_base_commit() == context.base)
        {
            return Ok(snapshot);
        }
    }

    validate_overview(&overview, &output_directory, &skill_root)?;
    let directory = output_directory.join("overview-publication");
    fs::create_dir_all(&directory)
        .map_err(|error| SnapshotError::io_caused_by(error.to_string(), error))?;
    let run_id = Uuid::new_v4().simple().to_string();
    // A distinct report per generation leaves the previous snapshot readable
    // if publishing its replacement fails after writing the new report.
    let markdown_path = directory.join(format!("report-{run_id}.md"));
    let report = make_report(context.url, context.source, context.base, &overview);
    let report_transaction = FileTransaction::open(&markdown_path, clock)
        .map_err(|error| SnapshotError::io_caused_by(error.to_string(), error.into_std_error()))?;
    report_transaction
        .write_bytes(report.as_bytes())
        .map_err(|error| SnapshotError::io_caused_by(error.to_string(), error.into_std_error()))?;
    let path = markdown_path
        .to_str()
        .ok_or_else(|| SnapshotError::invalid_input("Report path is not Unicode"))?;
    let snapshot = json!({
        "schemaVersion": 1,
        "runId": run_id,
        "pullRequestUrl": context.url,
        "target": context.target_json,
        "sourceCommit": context.source,
        "mergeBaseCommit": context.base,
        "createdUtc": powershell_timestamp(clock),
        "sourceHash": source_hash,
        "markdownPath": path,
        "markdownHash": hash(report.as_bytes())
    });
    let bytes = to_vec_pretty(&snapshot)
        .map_err(|error| SnapshotError::invalid_state_caused_by(error.to_string(), error))?;
    snapshot_transaction
        .write_bytes(&bytes)
        .map_err(|error| SnapshotError::io_caused_by(error.to_string(), error.into_std_error()))?;
    FrozenSnapshot::from_record(snapshot)
}

/// Keeps the overview's internal line endings and Unicode in the frozen report.
fn make_report(url: &str, source: &str, base: &str, overview: &str) -> String {
    format!(
        "[Copilot speaking]\n\nGenerated during preparation, before the review. Completeness questions are preliminary, not verified findings or a merge recommendation. This snapshot is not updated by later review work.\n\nPull request: {url}\n\nInspected source commit: {source}\n\nInspected merge-base commit: {base}\n\n{}",
        overview.trim_end()
    )
}

/// Establishes that the capture names the same concrete PR and inspected commits.
pub(crate) fn validate_context(context: &Value) -> Result<ValidatedContext<'_>, SnapshotError> {
    let invalid = || SnapshotError::invalid_input("Invalid pull request capture");
    if context.get("schemaVersion").and_then(Value::as_u64) != Some(3) {
        return Err(invalid());
    }
    let url = context
        .get("pullRequestUrl")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let target_value = context.get("target").ok_or_else(invalid)?;
    let target: Target = from_value(target_value.clone()).map_err(|error| {
        SnapshotError::invalid_input_caused_by(
            format!("Invalid pull request target: {error}"),
            error,
        )
    })?;
    let parsed = Target::pull_request(url, Some(target.platform)).map_err(|error| {
        SnapshotError::invalid_input_caused_by(
            format!("Invalid pull request URL: {error}"),
            error.into_std_error(),
        )
    })?;
    if target.pull_request_id == 0
        || target.pull_request_id != parsed.pull_request_id
        || target.web_url != url
        || parsed.web_url != url
        // Azure captures route through the explicit collection, not a hostname-derived label.
        || (target.platform == Platform::GitHub
            && !target.organization.eq_ignore_ascii_case(&parsed.organization))
        || !target.repository.eq_ignore_ascii_case(&parsed.repository)
        || target.project.as_deref().map(str::to_lowercase)
            != parsed.project.as_deref().map(str::to_lowercase)
        || target.collection_url.as_deref().map(str::to_lowercase)
            != parsed.collection_url.as_deref().map(str::to_lowercase)
        || context.get("platform") != target_value.get("platform")
    {
        return Err(invalid());
    }
    let pr = context.get("pullRequest").ok_or_else(invalid)?;
    let pr_id = pr
        .get("id")
        .and_then(|id| id.as_u64().or_else(|| id.as_str()?.parse().ok()));
    if pr_id != Some(u64::from(target.pull_request_id)) {
        return Err(invalid());
    }
    let source = pr
        .get("reviewedSourceCommit")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let base = pr
        .get("reviewedMergeBaseCommit")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    if !commit(source) || !commit(base) {
        return Err(SnapshotError::invalid_input(
            "Overview requires inspected source and merge-base commits",
        ));
    }
    Ok(ValidatedContext {
        url,
        target,
        target_json: target_value,
        source,
        base,
    })
}

/// Checks a complete stored record and hashes the actual frozen bytes.
fn validate_snapshot(
    snapshot: Value,
    url: &str,
    target: &Value,
    clock: &dyn Clock,
) -> Result<FrozenSnapshot, SnapshotError> {
    let invalid = || SnapshotError::invalid_state("Invalid or modified overview snapshot");
    if snapshot.get("schemaVersion").and_then(Value::as_u64) != Some(1)
        || snapshot.get("pullRequestUrl").and_then(Value::as_str) != Some(url)
        || snapshot.get("target") != Some(target)
        || !snapshot
            .get("sourceCommit")
            .and_then(Value::as_str)
            .is_some_and(commit)
        || !snapshot
            .get("mergeBaseCommit")
            .and_then(Value::as_str)
            .is_some_and(commit)
        || !snapshot
            .get("runId")
            .and_then(Value::as_str)
            .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        || !snapshot
            .get("createdUtc")
            .and_then(Value::as_str)
            .is_some_and(|s| s.parse::<jiff::Timestamp>().is_ok())
        || !snapshot
            .get("sourceHash")
            .and_then(Value::as_str)
            .is_some_and(hash_shape)
        || !snapshot
            .get("markdownHash")
            .and_then(Value::as_str)
            .is_some_and(hash_shape)
    {
        return Err(invalid());
    }
    let path = snapshot
        .get("markdownPath")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(invalid)?;
    let bytes = required_bytes(Path::new(path), clock)?;
    if snapshot.get("markdownHash").and_then(Value::as_str) != Some(&hash(&bytes)) {
        return Err(invalid());
    }
    FrozenSnapshot::from_record(snapshot)
}

/// Applies the same ordered heading, placeholder, and private-path gate as PowerShell.
fn validate_overview(text: &str, output: &Path, skill: &Path) -> Result<(), SnapshotError> {
    validate_overview_structure(text).map_err(|error| {
        SnapshotError::invalid_input_caused_by("Invalid overview structure", error.into_std_error())
    })?;
    // PowerShell's `-match` is case-insensitive by default.
    let placeholder =
        Regex::new(r"\{\{[A-Za-z_][A-Za-z_0-9]*\}\}").expect("Fixed placeholder expression");
    let mut paths = vec![
        output.to_string_lossy().into_owned(),
        skill.to_string_lossy().into_owned(),
    ];
    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        paths.push(home.to_string_lossy().into_owned());
    }
    let folded = text.to_lowercase();
    if paths.iter().filter(|path| !path.is_empty()).any(|path| {
        folded.contains(&path.to_lowercase())
            || folded.contains(&path.replace('\\', "/").to_lowercase())
    }) {
        return Err(SnapshotError::invalid_input(
            "Overview contains a private path",
        ));
    }
    if placeholder.is_match(text) {
        return Err(SnapshotError::invalid_input(
            "Overview contains a workflow placeholder",
        ));
    }
    Ok(())
}

/// Checks the section structure shared by preparation and publication.
///
/// # Errors
///
/// Rejects missing, duplicate, empty, or unordered required sections.
#[doc(hidden)]
pub fn validate_overview_structure(text: &str) -> Result<(), AppError> {
    let required = [
        "Mission",
        "Substance",
        "Design changes",
        "Significant changes",
        "Structure",
        "Mechanism",
        "Completeness assessment",
        "Related history",
        "Related work",
    ];
    let mut sections: Vec<(&str, bool)> = Vec::new();
    let mut open: Option<String> = None;
    for line in text.split(['\r', '\n']) {
        if let Some((run, rest)) = fence_marker(line) {
            if let Some(previous) = &open {
                if run.as_bytes().first() == previous.as_bytes().first()
                    && run.len() >= previous.len()
                    && rest.trim().is_empty()
                {
                    open = None;
                }
            } else {
                open = Some(run.to_owned());
            }
        } else if open.is_none()
            && let Some(found) = line.strip_prefix("## ").filter(|name| !name.is_empty())
        {
            sections.push((found.trim_end(), false));
            continue;
        }
        if let Some((_, has_content)) = sections.last_mut() {
            *has_content |= !line.trim().is_empty();
        }
    }
    let mut previous = None;
    for name in required {
        let matches: Vec<_> = sections
            .iter()
            .enumerate()
            .filter(|(_, section)| section.0 == name)
            .collect();
        let [(position, (_, has_content))] = matches.as_slice() else {
            return Err(fail(
                "invalidInput",
                format!("Missing, empty, or unordered {name} section"),
            ));
        };
        if previous.is_some_and(|index| *position <= index) || !has_content {
            return Err(fail(
                "invalidInput",
                format!("Missing, empty, or unordered {name} section"),
            ));
        }
        previous = Some(*position);
    }
    Ok(())
}

/// Checks authored overview text using the publication validation rules.
///
/// This internal-library entry point lets offline tooling exercise the same
/// validator without preparing files or contacting a provider.
#[doc(hidden)]
#[must_use]
pub fn overview_text_is_publishable(text: &str, output: &Path, skill: &Path) -> bool {
    validate_overview(text, output, skill).is_ok()
}

/// Recognizes Markdown fences with the same indentation and run rules as PowerShell.
fn fence_marker(line: &str) -> Option<(&str, &str)> {
    let indent = line.chars().take_while(|ch| ch.is_whitespace()).count();
    if indent > 3 {
        return None;
    }
    let line = line.trim_start_matches(char::is_whitespace);
    let first = line.as_bytes().first().copied()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let length = line.bytes().take_while(|byte| *byte == first).count();
    if length < 3 {
        return None;
    }
    Some(line.split_at(length))
}

/// Uses the shared reader so concurrent atomic replacements expose complete bytes.
fn required_bytes(path: &Path, clock: &dyn Clock) -> Result<Vec<u8>, SnapshotError> {
    read_bytes(path, clock)
        .map_err(|error| SnapshotError::io_caused_by(error.to_string(), error.into_std_error()))?
        .ok_or_else(|| SnapshotError::io(format!("Missing file {}", path.display())))
}

/// Distinguishes malformed source input from a malformed retained snapshot.
fn parse_json(bytes: &[u8], state: bool) -> Result<Value, SnapshotError> {
    let text = decode_text_with_bom(bytes);
    from_str(&text).map_err(|error| {
        if state {
            SnapshotError::invalid_state_caused_by(error.to_string(), error)
        } else {
            SnapshotError::invalid_input_caused_by(error.to_string(), error)
        }
    })
}

/// Resolves relative caller paths before recording a report path.
fn full_path(path: &Path) -> Result<PathBuf, SnapshotError> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| SnapshotError::io_caused_by(error.to_string(), error))
    }
}

/// Preserves the report hash representation used by the PowerShell reader.
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Restricts stored hashes to canonical lowercase SHA-256 text.
fn hash_shape(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Uses `DateTimeOffset`'s round-trip UTC representation for PowerShell readers.
fn powershell_timestamp(clock: &dyn Clock) -> String {
    let instant = clock.now();
    let whole = instant.strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let ticks = instant
        .subsec_nanosecond()
        .checked_div(100)
        .expect("A fixed nonzero divisor");
    format!("{whole}.{ticks:07}+00:00")
}

/// Recognizes the full inspected Git object IDs required by publication.
fn commit(value: &str) -> bool {
    (40..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::fmt::Write;
    use std::io::{Error, ErrorKind};
    use std::panic::{RefUnwindSafe, UnwindSafe};
    use std::path::Path;

    use ohno::AppError;
    use serde_json::{Error as JsonError, Value, json};

    use super::{
        FrozenSnapshot, InvalidInput, InvalidState, MAX_COMMENT_BYTES, SnapshotError, SnapshotIo,
        commit, frozen_snapshot_for_test, hash, hash_shape, make_report, parse_json,
        split_comment_bodies, validate_context, validate_overview,
    };
    use crate::OverviewInput;

    static_assertions::assert_impl_all!(OverviewInput<'static>: UnwindSafe, RefUnwindSafe);

    fn overview() -> String {
        [
            "Mission",
            "Substance",
            "Design changes",
            "Significant changes",
            "Structure",
            "Mechanism",
            "Completeness assessment",
            "Related history",
            "Related work",
        ]
        .iter()
        .fold(String::new(), |mut text, name| {
            write!(text, "## {name}\nContent\n").unwrap();
            text
        })
    }

    fn capture() -> Value {
        json!({
            "schemaVersion": 3,
            "platform": "gitHub",
            "pullRequestUrl": "https://github.com/owner/repo/pull/42",
            "target": {
                "platform": "gitHub",
                "collectionUrl": null,
                "organization": "owner",
                "project": null,
                "repository": "repo",
                "pullRequestId": 42,
                "webUrl": "https://github.com/owner/repo/pull/42"
            },
            "pullRequest": {
                "id": 42,
                "reviewedSourceCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "reviewedMergeBaseCommit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            }
        })
    }

    #[test]
    fn required_sections_are_ordered_unique_nonempty_and_outside_fences() {
        let output = Path::new("/private/output");
        let skill = Path::new("/private/skill");
        let valid = overview();
        validate_overview(&valid, output, skill).unwrap();
        assert!(
            validate_overview(
                &valid.replace("## Mission", "```\n## Mission\n```"),
                output,
                skill
            )
            .is_err()
        );
        assert!(
            validate_overview(
                &valid.replace("## Mission", "## Mission\nText\n## Mission"),
                output,
                skill
            )
            .is_err()
        );
        assert!(
            validate_overview(
                &valid.replace("## Mission\nContent", "## Mission\n  "),
                output,
                skill
            )
            .is_err()
        );
        let swapped = valid
            .replace("## Mission", "## TEMP")
            .replace("## Substance", "## Mission")
            .replace("## TEMP", "## Substance");
        assert!(validate_overview(&swapped, output, skill).is_err());
        validate_overview(&format!("{valid}\n~~~\n## Mission\n~~~"), output, skill).unwrap();
    }

    #[test]
    fn publication_rejects_private_paths_and_placeholders() {
        for value in ["/PRIVATE/OUTPUT/x", "/private/skill/x", "{{TODO_2}}"] {
            assert!(
                validate_overview(
                    &format!("{}\n{value}", overview()),
                    Path::new("/private/output"),
                    Path::new("/private/skill")
                )
                .is_err()
            );
        }
        assert!(
            validate_overview(
                &format!("{}\n{{{{lowercase}}}}", overview()),
                Path::new("/private/output"),
                Path::new("/private/skill")
            )
            .is_err()
        );
    }

    #[test]
    fn capture_binds_url_target_pr_and_inspected_commits() {
        let valid = capture();
        let context = validate_context(&valid).unwrap();
        let debug = format!("{context:?}");
        assert!(debug.contains("url:"));
        assert!(debug.contains("source:"));
        assert!(!debug.contains("target_json"));
        for (pointer, value) in [
            ("/pullRequest/id", json!(43)),
            ("/target/pullRequestId", json!(43)),
            ("/pullRequest/reviewedSourceCommit", json!("bad")),
            ("/pullRequest/reviewedMergeBaseCommit", json!("")),
            ("/schemaVersion", json!(1)),
            ("/platform", json!("azureDevOps")),
        ] {
            let mut changed = valid.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(validate_context(&changed).is_err(), "{pointer}");
        }
        let mut changed = valid;
        *changed.pointer_mut("/pullRequestUrl").unwrap() =
            json!("https://example.com/owner/repo/pull/42");
        *changed.pointer_mut("/target/webUrl").unwrap() =
            json!("https://example.com/owner/repo/pull/42");
        validate_context(&changed).unwrap_err();
    }

    #[test]
    fn frozen_snapshot_requires_typed_identity_fields() {
        for record in [
            json!({"markdownPath":"report.md","markdownHash":"0".repeat(64)}),
            json!({"runId":"0123456789abcdef0123456789abcdef","markdownHash":"0".repeat(64)}),
            json!({"runId":"0123456789abcdef0123456789abcdef","markdownPath":"report.md"}),
            json!({
                "runId":"0123456789abcdef0123456789abcdef","markdownPath":"report.md",
                "markdownHash":"0".repeat(64),"sourceCommit":"a".repeat(40),
                "mergeBaseCommit":"b".repeat(40)
            }),
        ] {
            assert!(FrozenSnapshot::from_record(record).is_err());
        }

        let snapshot = frozen_snapshot_for_test(
            "0123456789abcdef0123456789abcdef",
            "report.md",
            &"0".repeat(64),
        );
        assert_eq!(snapshot.markdown_path(), "report.md");
        assert_eq!(snapshot.markdown_hash(), "0".repeat(64));
        assert_eq!(snapshot.source_commit(), "a".repeat(40));
        assert_eq!(snapshot.merge_base_commit(), "b".repeat(40));
        assert_eq!(snapshot.source_hash(), "c".repeat(64));
    }

    #[test]
    fn validated_context_keeps_original_target_identity() {
        let mut capture = capture();
        *capture.pointer_mut("/target/organization").unwrap() = json!("OWNER");
        let context = validate_context(&capture).unwrap();
        assert_eq!(context.target.organization, "OWNER");
        assert_eq!(Some(context.target_json), capture.get("target"));
    }

    #[test]
    fn validated_context_accepts_explicit_azure_collection_coordinates() {
        let target = crate::capture_target(&json!({
            "platform": "azureDevOps",
            "organization": "contoso",
            "collectionUrl": "https://ado.example.com/tfs/DefaultCollection",
            "project": "Project",
            "repository": "Repo",
            "pullRequestId": 42
        }))
        .unwrap();
        let mut capture = capture();
        *capture.get_mut("platform").unwrap() = json!("azureDevOps");
        *capture.get_mut("pullRequestUrl").unwrap() = target.get("webUrl").unwrap().clone();
        *capture.get_mut("target").unwrap() = target;
        let context = validate_context(&capture).unwrap();
        assert_eq!(context.target.organization, "contoso");
        assert_eq!(Some(context.target_json), capture.get("target"));

        for field in ["collectionUrl", "project", "repository", "pullRequestId"] {
            let mut mismatched = capture.clone();
            *mismatched
                .get_mut("target")
                .unwrap()
                .get_mut(field)
                .unwrap() = if field == "pullRequestId" {
                json!(43)
            } else {
                json!("different")
            };
            assert!(validate_context(&mismatched).is_err(), "{field}");
        }
    }

    #[test]
    fn validated_context_rejects_different_github_owners() {
        let mut capture = capture();
        *capture.pointer_mut("/target/organization").unwrap() = json!("different");
        validate_context(&capture).unwrap_err();
    }

    #[test]
    fn snapshot_failures_preserve_error_causes() {
        let input: AppError = parse_json(b"{", false).unwrap_err().into();
        assert!(input.find_source::<InvalidInput>().is_some());
        assert!(input.find_source::<JsonError>().is_some());

        let state: AppError = parse_json(b"{", true).unwrap_err().into();
        assert!(state.find_source::<InvalidState>().is_some());
        assert!(state.find_source::<JsonError>().is_some());

        let io = Error::new(ErrorKind::PermissionDenied, "denied");
        let error: AppError = SnapshotError::io_caused_by("read failed", io).into();
        assert!(error.find_source::<SnapshotIo>().is_some());
        assert_eq!(
            error.find_source::<Error>().unwrap().kind(),
            ErrorKind::PermissionDenied
        );

        let error: AppError = validate_overview("", Path::new("output"), Path::new("skill"))
            .unwrap_err()
            .into();
        assert!(error.find_source::<InvalidInput>().is_some());
        assert_eq!(ss_ai_runtime::classification(&error), "invalidInput");
        assert!(
            error
                .to_string()
                .contains("Missing, empty, or unordered Mission section")
        );
    }

    #[test]
    fn overview_debug_formatters_accept_large_nested_values() {
        let capture = json!({"pullRequestUrl": {"nested": ["large"]}});
        let input = OverviewInput {
            capture: &capture,
            overview_path: Path::new("/overview"),
            output_directory: Path::new("/output"),
            skill_root: Path::new("/skill"),
            comment_prefix: "attribution",
            tenant: Some("tenant"),
        };
        drop(format!("{input:?}"));
    }

    #[test]
    fn generated_fences_hide_duplicate_headings_only_until_closed() {
        let output = Path::new("/private/output");
        let skill = Path::new("/private/skill");
        for marker in ['`', '~'] {
            for width in 3..=6 {
                for indent in 0..=3 {
                    let fence = marker.to_string().repeat(width);
                    let prefix = " ".repeat(indent);
                    let text = format!(
                        "{}\n{prefix}{fence}\n## Mission\n{prefix}{fence}\n",
                        overview()
                    );
                    validate_overview(&text, output, skill).unwrap();
                    assert!(
                        validate_overview(&format!("{text}\n## Mission\nContent"), output, skill)
                            .is_err()
                    );
                }
            }
        }
    }

    #[test]
    fn snapshot_hash_and_commit_helpers_validate_expected_shapes() {
        assert_eq!(
            hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(hash_shape(&hash(b"")));
        assert!(!hash_shape(&hash(b"").to_uppercase()));
        assert!(commit(&"f".repeat(40)));
        assert!(commit(&"A".repeat(64)));
        assert!(!commit(&"f".repeat(39)));
    }

    #[test]
    fn report_preserves_unicode_and_internal_crlf() {
        let overview = "# Content overview\r\n\r\n```mermaid\r\nA[\"ö\"] --> B\r\n```\r\n";
        let report = make_report("https://github.com/o/r/pull/1", "source", "base", overview);
        assert!(report.ends_with(overview.trim_end()));
        assert!(report.contains("```mermaid\r\nA[\"ö\"] --> B\r\n```"));
    }

    #[test]
    fn inline_comments_preserve_attribution_and_exact_unicode_report_bytes() {
        let report = format!(
            "[Copilot speaking]\n\nGenerated\n\n{}",
            "Résumé 雪 🦀\n\n".repeat(12000)
        );
        let bodies = split_comment_bodies(&report, "Attribution\nexact").unwrap();
        assert!(bodies.len() > 1);
        assert!(bodies.iter().all(|body| body.len() <= MAX_COMMENT_BYTES));
        assert!(
            bodies
                .iter()
                .all(|body| { body.starts_with("[Copilot speaking]\n\nAttribution\nexact\n\n") })
        );

        let prefix = "[Copilot speaking]\n\nAttribution\nexact\n\n";
        let mut restored = String::new();
        for body in &bodies {
            let content = body.strip_prefix(prefix).unwrap();
            let end = content.find(").**\n\n").unwrap() + ").**\n\n".len();
            restored.push_str(content.get(end..).unwrap());
        }
        assert_eq!(
            restored,
            report.strip_prefix("[Copilot speaking]\n\n").unwrap()
        );
    }

    #[test]
    fn single_inline_comment_uses_one_attributed_body() {
        let bodies = split_comment_bodies(
            "[Copilot speaking]\n\n# Content overview\n\nRésumé",
            "[Copilot speaking]",
        )
        .unwrap();
        assert_eq!(
            bodies,
            ["[Copilot speaking]\n\n# Content overview\n\nRésumé"]
        );
    }
}
