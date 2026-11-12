//! Freezes a local HTML overview and its deterministic attachment archive.

use std::io::{Cursor, ErrorKind, Write};
use std::path::{Path, PathBuf};

use ohno::AppError;
use same_file::is_same_file;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, from_slice, to_vec};
use sha2::{Digest, Sha256};
use ss_ai_runtime::{
    Clock, FileTransaction, LockMode, Process, ResourceLock, caused_by, fail, read_bytes,
};
use tempfile::{Builder, TempDir};
use zip::{CompressionMethod, DateTime, ZipWriter, write::SimpleFileOptions};

use crate::report_html::ReportResult;
use crate::report_tools::{ReportTools, assert_ready, discover, tools_lock};

/// The immutable paths and hashes needed to publish a frozen overview.
#[derive(Debug)]
#[non_exhaustive]
pub struct OverviewArtifact {
    /// Path to the HTML report.
    pub html_path: PathBuf,
    /// SHA-256 of the HTML bytes.
    pub html_hash: String,
    /// Path to the ZIP attachment.
    pub archive_path: PathBuf,
    /// SHA-256 of the ZIP bytes.
    pub archive_hash: String,
}

/// Distinguishes a prepared artifact from a consent-dependent tool setup.
#[derive(Debug)]
#[non_exhaustive]
pub enum Preparation {
    /// Existing or newly prepared local files.
    Ready(OverviewArtifact),
    /// Discovery results, including missing tools and diagnostics.
    MissingTools(ReportTools),
}

/// Matches the report renderer's JSON and the overview publication reader.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    source_path: PathBuf,
    source_hash: String,
    html_path: PathBuf,
    html_uri: String,
    html_hash: String,
    diagram_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive_path: Option<PathBuf>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// Identifies a missing frozen input or output without relying on diagnostic text.
#[ohno::error]
#[display("A frozen overview file is missing")]
struct MissingArtifact;

/// Identifies bytes that no longer match their recorded snapshot.
#[ohno::error]
#[display("A frozen overview artifact does not match its recorded hash")]
struct ModifiedArtifact;

/// Identifies a manifest path outside the allowed artifact locations.
#[ohno::error]
#[display("An overview manifest artifact path is unexpected")]
struct UnexpectedArtifactPath;

/// Freezes an overview without installing tools or publishing remotely.
///
/// `source_hash` is the frozen Markdown snapshot's lowercase SHA-256. Paths
/// must be absolute. The renderer must publish its HTML at the supplied path
/// and return the normal report-rendering result. The shared tool lock stays
/// held during discovery and rendering. Existing artifacts are verified and
/// never overwritten when their contents have changed.
///
/// # Errors
///
/// Returns an error if input, rendering, archive creation or verification fails.
#[expect(
    clippy::too_many_arguments,
    reason = "The renderer, tool discovery and snapshot each require separate inputs"
)]
pub fn freeze(
    source: &Path,
    source_hash: &str,
    output_directory: &Path,
    skill_root: &Path,
    tools_directory: &Path,
    process: &dyn Process,
    clock: &dyn Clock,
    render: impl FnOnce(&ReportTools, &ResourceLock, &Path, &Path) -> Result<ReportResult, AppError>,
) -> Result<Preparation, AppError> {
    if !source.is_absolute() || !output_directory.is_absolute() {
        return Err(invalid("Overview paths must be absolute"));
    }
    let default_html_path = output_directory.join("report.html");
    let default_archive_path = output_directory.join("report.zip");
    let manifest_path = output_directory.join("html.json");
    if [
        default_html_path.as_path(),
        default_archive_path.as_path(),
        manifest_path.as_path(),
    ]
    .contains(&source)
    {
        return Err(invalid("Overview source must not be an artifact"));
    }
    for path in [&default_html_path, &default_archive_path, &manifest_path] {
        reject_alias(source, path)?;
    }
    let manifest_file = FileTransaction::open(&manifest_path, clock)?;
    let _source_guard = ResourceLock::open(source, LockMode::Shared, clock)?;
    let source_bytes = read_required(source, clock)?;
    verify_hash(
        &source_bytes,
        source_hash,
        "Frozen overview Markdown was modified",
    )?;
    let existing = manifest_file.read_bytes()?;
    let (mut manifest, html_path, archive_path) = if let Some(bytes) = existing {
        let manifest: Manifest = from_slice(&bytes).map_err(|error| {
            caused_by("invalidInput", "Malformed overview HTML manifest", error)
        })?;
        if manifest.source_hash != source_hash {
            return Err(modified("Frozen HTML does not match the Markdown snapshot"));
        }
        let archive_path = manifest_archive_path(output_directory, &manifest)?;
        let html_path = manifest.html_path.clone();
        (manifest, html_path, archive_path)
    } else {
        let html_exists = default_html_path.try_exists()?;
        let archive_exists = default_archive_path.try_exists()?;
        let guard = tools_lock(tools_directory, LockMode::Shared, clock)?;
        let tools = discover(skill_root, tools_directory, process, &guard)?;
        if !tools.missing.is_empty() {
            return Ok(Preparation::MissingTools(tools));
        }
        assert_ready(&tools, false)?;
        let directory =
            select_render_directory(output_directory, html_exists, archive_exists, || {
                // An unrecorded artifact may belong to an interrupted attempt. Keep
                // it untouched and publish the retry at a fresh, exclusive path.
                Builder::new()
                    .prefix(".overview-attempt-")
                    .tempdir_in(output_directory)
                    .map(TempDir::keep)
                    .map_err(|error| {
                        caused_by("io", "Cannot create overview retry directory", error).into()
                    })
            })?;
        let html_path = directory.join("report.html");
        let archive_path = directory.join("report.zip");
        reject_alias(source, &html_path)?;
        reject_alias(source, &archive_path)?;
        let result = render(&tools, &guard, source, &html_path)?;
        reject_alias(source, &html_path)?;
        if result.source_hash != source_hash || result.html_path != html_path {
            return Err(invalid(
                "Renderer returned a different overview snapshot or path",
            ));
        }
        (
            Manifest {
                source_path: result.source_path,
                source_hash: result.source_hash,
                html_path: result.html_path,
                html_uri: result.html_uri,
                html_hash: result.html_hash,
                diagram_count: result.diagram_count,
                archive_hash: None,
                archive_path: Some(archive_path.clone()),
                extra: Map::new(),
            },
            html_path,
            archive_path,
        )
    };
    reject_alias(source, &html_path)?;
    reject_alias(source, &archive_path)?;
    // Keep the report and archive locked while verifying and recording their hashes.
    let html_file = FileTransaction::open(&html_path, clock)?;
    let html = html_file
        .read_bytes()?
        .ok_or_else(|| missing("Frozen HTML is missing"))?;
    verify_hash(&html, &manifest.html_hash, "Frozen HTML was modified")?;
    verify_hash(
        &read_required(source, clock)?,
        source_hash,
        "Frozen overview Markdown was modified",
    )?;
    let archive_file = FileTransaction::open(&archive_path, clock)?;
    let archive = match (&manifest.archive_hash, archive_file.read_bytes()?) {
        (Some(expected), Some(bytes)) => {
            verify_hash(&bytes, expected, "Frozen HTML archive was modified")?;
            bytes
        }
        (Some(_), None) => return Err(missing("Frozen HTML archive was modified or is missing")),
        (None, None) => {
            let bytes = make_archive(&html)?;
            archive_file.write_bytes(&bytes)?;
            bytes
        }
        (None, Some(_)) => return Err(invalid("Unrecorded overview archive already exists")),
    };
    let archive_hash = hash(&archive);
    if manifest.archive_hash.is_none() {
        manifest.archive_hash = Some(archive_hash.clone());
        let bytes = to_vec(&manifest)
            .map_err(|error| caused_by("invalidInput", "Cannot encode overview manifest", error))?;
        manifest_file.write_bytes(&bytes)?;
    }
    Ok(Preparation::Ready(OverviewArtifact {
        html_path,
        html_hash: manifest.html_hash,
        archive_path,
        archive_hash,
    }))
}

/// Keeps incomplete output while choosing a separate location for a retry.
fn select_render_directory(
    output: &Path,
    html_exists: bool,
    archive_exists: bool,
    create_retry: impl FnOnce() -> Result<PathBuf, AppError>,
) -> Result<PathBuf, AppError> {
    if html_exists || archive_exists {
        create_retry()
    } else {
        Ok(output.to_owned())
    }
}

/// Restricts manifest paths to the generation or one of its retry directories.
fn manifest_archive_path(output: &Path, manifest: &Manifest) -> Result<PathBuf, AppError> {
    let html = &manifest.html_path;
    let directory = html
        .parent()
        .ok_or_else(|| unexpected_path("Overview HTML path has no parent"))?;
    let retry_directory = directory.parent() == Some(output)
        && directory
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".overview-attempt-"));
    if html.file_name().is_none_or(|name| name != "report.html")
        || (directory != output && !retry_directory)
    {
        return Err(unexpected_path(
            "Overview manifest has an unexpected HTML path",
        ));
    }
    let archive = directory.join("report.zip");
    if manifest
        .archive_path
        .as_ref()
        .is_some_and(|path| path != &archive)
    {
        return Err(unexpected_path(
            "Overview manifest has an unexpected archive path",
        ));
    }
    Ok(archive)
}

/// Requires the complete snapshot instead of treating absent input as finished.
fn read_required(path: &Path, clock: &dyn Clock) -> Result<Vec<u8>, AppError> {
    read_bytes(path, clock)?.ok_or_else(|| missing("Frozen overview file is missing"))
}

/// Prevents a hard link or symbolic link from turning an artifact into its input.
fn reject_alias(source: &Path, output: &Path) -> Result<(), AppError> {
    match is_same_file(source, output) {
        Ok(true) => Err(invalid("Overview source must not be an artifact")),
        Ok(false) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(caused_by("io", "Cannot compare overview paths", error).into()),
    }
}

/// Uses stored compression and the ZIP epoch to make the exact archive reproducible.
fn make_archive(html: &[u8]) -> Result<Vec<u8>, AppError> {
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(DateTime::default());
    archive
        .start_file("report.html", options)
        .map_err(|error| caused_by("io", "Cannot create overview archive entry", error))?;
    archive
        .write_all(html)
        .map_err(|error| caused_by("io", "Cannot write overview archive", error))?;
    let bytes = archive
        .finish()
        .map_err(|error| caused_by("io", "Cannot finish overview archive", error))?;
    Ok(bytes.into_inner())
}

/// Uses the same lowercase SHA-256 representation as the PowerShell reader.
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Rejects changes rather than silently replacing an immutable artifact.
fn verify_hash(bytes: &[u8], expected: &str, message: &str) -> Result<(), AppError> {
    if hash(bytes) != expected {
        return Err(modified(message));
    }
    Ok(())
}

/// Classifies malformed or inconsistent frozen state at the workflow boundary.
fn invalid(message: &str) -> AppError {
    fail("invalidInput", message)
}

/// Preserves the recovery category while retaining a testable missing-file cause.
fn missing(message: &str) -> AppError {
    caused_by("invalidInput", message, MissingArtifact::new()).into()
}

/// Preserves the recovery category while retaining a testable content cause.
fn modified(message: &str) -> AppError {
    caused_by("invalidInput", message, ModifiedArtifact::new()).into()
}

/// Preserves the recovery category while retaining a testable manifest-path cause.
fn unexpected_path(message: &str) -> AppError {
    caused_by("invalidInput", message, UnexpectedArtifactPath::new()).into()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::io::{Cursor, Read};

    use zip::ZipArchive;

    use std::path::Path;

    use serde_json::{from_slice, to_value};
    use ss_ai_runtime::classification;

    use super::{
        Manifest, MissingArtifact, ModifiedArtifact, UnexpectedArtifactPath, hash, make_archive,
        manifest_archive_path, missing, select_render_directory, verify_hash,
    };

    #[test]
    fn archive_contains_only_exact_unicode_html_and_is_reproducible() {
        let html = "<html>Résumé 雪 🦀</html>".as_bytes();
        let bytes = make_archive(html).unwrap();
        assert_eq!(bytes, make_archive(html).unwrap());
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 1);
        let mut entry = archive.by_index(0).unwrap();
        assert_eq!(entry.name(), "report.html");
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content, html);
    }

    #[test]
    fn manifest_preserves_interoperable_hash_fields() {
        let json = br#"{"sourcePath":"/report.md","sourceHash":"abc","htmlPath":"/report.html","htmlUri":"file:///report.html","htmlHash":"def","diagramCount":0,"other":"preserved"}"#;
        let mut manifest: Manifest = from_slice(json).unwrap();
        manifest.archive_hash = Some(hash(b"archive"));
        let manifest = to_value(manifest).unwrap();
        assert_eq!(manifest.get("sourceHash").unwrap(), "abc");
        assert_eq!(manifest.get("htmlHash").unwrap(), "def");
        assert_eq!(manifest.get("archiveHash").unwrap(), &hash(b"archive"));
        assert_eq!(manifest.get("other").unwrap(), "preserved");
    }

    #[test]
    fn modified_frozen_content_is_rejected() {
        let expected = hash(b"original");
        verify_hash(b"original", &expected, "changed").unwrap();
        let error = verify_hash(b"modified", &expected, "changed").unwrap_err();
        assert!(error.find_source::<ModifiedArtifact>().is_some());
        assert_eq!(classification(&error), "invalidInput");
        assert_eq!(
            hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn missing_and_unexpected_paths_keep_classified_causes() {
        let error = missing("missing");
        assert!(error.find_source::<MissingArtifact>().is_some());
        assert_eq!(classification(&error), "invalidInput");

        let json = br#"{"sourcePath":"/report.md","sourceHash":"abc","htmlPath":"/other/report.html","htmlUri":"file:///report.html","htmlHash":"def","diagramCount":0}"#;
        let manifest: Manifest = from_slice(json).unwrap();
        let error = manifest_archive_path(Path::new("/generation"), &manifest).unwrap_err();
        assert!(error.find_source::<UnexpectedArtifactPath>().is_some());
        assert_eq!(classification(&error), "invalidInput");
    }

    #[test]
    fn interrupted_render_uses_new_directory_and_preserves_original_artifacts() {
        let output = Path::new("/generation");
        let retry = output.join(".overview-attempt-example");
        for (html_exists, archive_exists) in [(true, false), (true, true), (false, true)] {
            let selected =
                select_render_directory(output, html_exists, archive_exists, || Ok(retry.clone()))
                    .unwrap();
            assert_eq!(selected, retry);
        }
        assert_eq!(
            select_render_directory(output, false, false, || panic!("retry is unnecessary"))
                .unwrap(),
            output
        );
        let json = br#"{"sourcePath":"/report.md","sourceHash":"abc","htmlPath":"/generation/.overview-attempt-example/report.html","htmlUri":"file:///report.html","htmlHash":"def","diagramCount":0}"#;
        let manifest: Manifest = from_slice(json).unwrap();
        assert_eq!(
            manifest_archive_path(output, &manifest).unwrap(),
            retry.join("report.zip")
        );
    }
}
