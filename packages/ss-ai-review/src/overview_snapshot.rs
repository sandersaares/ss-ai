//! Freezes a preparation overview before any provider publication.

use std::any::type_name;
use std::env;
use std::error::Error as StdError;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{Map, Value, from_str, from_value, json, to_vec_pretty};
use sha2::{Digest, Sha256};
use ss_ai_runtime::{Clock, FileTransaction, decode_text_with_bom, read_bytes};
use uuid::Uuid;

use crate::target::Target;

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
    run_id: String,
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

    /// Returns the frozen report's run identity.
    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
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
        let run_id = record
            .get("runId")
            .and_then(Value::as_str)
            .filter(|value| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(invalid)?
            .to_owned();
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
            run_id,
            markdown_path,
            markdown_hash,
            source_commit,
            merge_base_commit,
            source_hash,
        })
    }
}

/// Carries a frozen snapshot only after the ADO HTML and archive are prepared.
pub(crate) struct PreparedAdoSnapshot {
    frozen: FrozenSnapshot,
    html_path: String,
    html_hash: String,
    archive_path: String,
    archive_hash: String,
}

impl PreparedAdoSnapshot {
    /// Adds the renderer's complete artifact identity to the interoperable record.
    pub(crate) fn from_render(
        frozen: FrozenSnapshot,
        html_path: String,
        html_hash: String,
        archive_path: String,
        archive_hash: String,
    ) -> Result<Self, SnapshotError> {
        let invalid = || SnapshotError::invalid_state("Invalid prepared overview artifacts");
        if html_path.is_empty()
            || archive_path.is_empty()
            || !hash_shape(&html_hash)
            || !hash_shape(&archive_hash)
        {
            return Err(invalid());
        }
        Ok(Self {
            frozen,
            html_path,
            html_hash,
            archive_path,
            archive_hash,
        })
    }

    /// Returns the frozen Markdown snapshot used by GitHub-side checks.
    pub(crate) fn frozen(&self) -> &FrozenSnapshot {
        &self.frozen
    }

    /// Returns the unchanged journal record plus ADO artifact identity.
    pub(crate) fn record(&self) -> Value {
        let mut record = self.frozen.record.clone();
        record.insert("htmlPath".into(), json!(&self.html_path));
        record.insert("htmlHash".into(), json!(&self.html_hash));
        record.insert("archivePath".into(), json!(&self.archive_path));
        record.insert("archiveHash".into(), json!(&self.archive_hash));
        Value::Object(record)
    }

    /// Returns the prepared archive path.
    pub(crate) fn archive_path(&self) -> &str {
        &self.archive_path
    }

    /// Returns the prepared archive digest.
    pub(crate) fn archive_hash(&self) -> &str {
        &self.archive_hash
    }
}

/// Distinguishes provider-ready snapshots while retaining one journal record shape.
pub(crate) enum PreparedSnapshot {
    /// A frozen Markdown report is sufficient for GitHub publication.
    GitHub(FrozenSnapshot),
    /// Azure DevOps requires both rendered HTML and its prepared archive.
    AzureDevOps(PreparedAdoSnapshot),
}

impl PreparedSnapshot {
    /// Returns the exact record to persist at the existing JSON boundary.
    pub(crate) fn record(&self) -> Value {
        match self {
            Self::GitHub(snapshot) => snapshot.record(),
            Self::AzureDevOps(snapshot) => snapshot.record(),
        }
    }
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

/// Supplies a fully prepared ADO snapshot to provider boundary tests.
#[cfg(test)]
pub(crate) fn prepared_ado_snapshot_for_test(
    run_id: &str,
    markdown_path: &str,
    markdown_hash: &str,
    archive_path: &str,
    archive_hash: &str,
) -> PreparedAdoSnapshot {
    PreparedAdoSnapshot::from_render(
        frozen_snapshot_for_test(run_id, markdown_path, markdown_hash),
        format!("{archive_path}.html"),
        "0".repeat(64),
        archive_path.to_owned(),
        archive_hash.to_owned(),
    )
    .expect("test ADO snapshot has complete artifact identities")
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
        || !target
            .organization
            .eq_ignore_ascii_case(&parsed.organization)
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
    // PowerShell's `-match` is case-insensitive by default.
    let placeholder =
        Regex::new(r"\{\{[A-Za-z_][A-Za-z_0-9]*\}\}").expect("Fixed placeholder expression");
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
            return Err(SnapshotError::invalid_input(format!(
                "Missing, empty, or unordered {name} section"
            )));
        };
        if previous.is_some_and(|index| *position <= index) || !has_content {
            return Err(SnapshotError::invalid_input(format!(
                "Missing, empty, or unordered {name} section"
            )));
        }
        previous = Some(*position);
    }
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
        FrozenSnapshot, InvalidInput, InvalidState, PreparedAdoSnapshot, SnapshotError, SnapshotIo,
        commit, frozen_snapshot_for_test, hash, hash_shape, make_report, parse_json,
        validate_context, validate_overview,
    };
    use crate::{OverviewInput, OverviewRender};

    static_assertions::assert_impl_all!(OverviewInput<'static>: UnwindSafe, RefUnwindSafe);
    static_assertions::assert_impl_all!(OverviewRender: UnwindSafe, RefUnwindSafe);

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
        assert_eq!(snapshot.run_id(), "0123456789abcdef0123456789abcdef");
        assert_eq!(snapshot.markdown_path(), "report.md");
        assert_eq!(snapshot.markdown_hash(), "0".repeat(64));
        assert_eq!(snapshot.source_commit(), "a".repeat(40));
        assert_eq!(snapshot.merge_base_commit(), "b".repeat(40));
        assert_eq!(snapshot.source_hash(), "c".repeat(64));
    }

    #[test]
    fn ado_preparation_requires_complete_artifact_identity_and_keeps_journal_shape() {
        let frozen = frozen_snapshot_for_test(
            "0123456789abcdef0123456789abcdef",
            "report.md",
            &"0".repeat(64),
        );
        for (html_path, html_hash, archive_path, archive_hash) in [
            (
                String::new(),
                "0".repeat(64),
                "report.zip".to_owned(),
                "1".repeat(64),
            ),
            (
                "report.html".to_owned(),
                "bad".to_owned(),
                "report.zip".to_owned(),
                "1".repeat(64),
            ),
            (
                "report.html".to_owned(),
                "0".repeat(64),
                String::new(),
                "1".repeat(64),
            ),
            (
                "report.html".to_owned(),
                "0".repeat(64),
                "report.zip".to_owned(),
                "bad".to_owned(),
            ),
        ] {
            let frozen = FrozenSnapshot::from_record(json!({
                "schemaVersion": 1,
                "runId": "0123456789abcdef0123456789abcdef",
                "markdownPath": "report.md",
                "markdownHash": "0".repeat(64),
                "sourceCommit": "a".repeat(40),
                "mergeBaseCommit": "b".repeat(40),
                "sourceHash": "c".repeat(64),
            }))
            .unwrap();
            assert!(
                PreparedAdoSnapshot::from_render(
                    frozen,
                    html_path,
                    html_hash,
                    archive_path,
                    archive_hash,
                )
                .is_err()
            );
        }

        let prepared = PreparedAdoSnapshot::from_render(
            frozen,
            "report.html".to_owned(),
            "0".repeat(64),
            "report.zip".to_owned(),
            "1".repeat(64),
        )
        .unwrap();
        let record = prepared.record();
        assert_eq!(
            record
                .get("schemaVersion")
                .expect("schema version is recorded"),
            1
        );
        assert_eq!(
            record
                .get("markdownPath")
                .and_then(Value::as_str)
                .expect("Markdown path is recorded"),
            "report.md"
        );
        assert_eq!(
            record
                .get("markdownHash")
                .and_then(Value::as_str)
                .expect("Markdown hash is recorded"),
            "0".repeat(64)
        );
        assert_eq!(
            record
                .get("htmlPath")
                .and_then(Value::as_str)
                .expect("HTML path is recorded"),
            "report.html"
        );
        assert_eq!(
            record
                .get("htmlHash")
                .and_then(Value::as_str)
                .expect("HTML hash is recorded"),
            "0".repeat(64)
        );
        assert_eq!(
            record
                .get("archivePath")
                .and_then(Value::as_str)
                .expect("archive path is recorded"),
            "report.zip"
        );
        assert_eq!(
            record
                .get("archiveHash")
                .and_then(Value::as_str)
                .expect("archive hash is recorded"),
            "1".repeat(64)
        );
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
        drop(format!(
            "{:?}",
            OverviewRender::MissingTools {
                missing: vec!["tool".into()],
                diagnostics: vec!["diagnostic".into()],
            }
        ));
        drop(format!(
            "{:?}",
            OverviewRender::Ready {
                html_path: "/report.html".into(),
                html_hash: "hash".into(),
                archive_path: "/report.zip".into(),
                archive_hash: "hash".into(),
            }
        ));
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
}
