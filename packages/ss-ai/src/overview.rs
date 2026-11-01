//! CLI composition for the initial review overview publication.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf, absolute};

use clap::Parser;
use clap::error::ErrorKind;
use ohno::AppError;
use serde_json::{Value, from_slice, to_string};
use ss_ai_review::{OverviewInput, OverviewRender, publish_overview};
use ss_ai_review_context::read_capture;
use ss_ai_review_workflow::{overview_artifact, report_html, report_tools};
use ss_ai_runtime::{Clock, Process, caused_by, fail, read_bytes};

use crate::Http;

/// Parses the overview command separately to keep clap's stack use bounded.
#[derive(Debug, Parser)]
#[command(name = "ss-ai review publish-overview")]
struct OverviewCli {
    #[arg(long)]
    context_path: PathBuf,
    #[arg(long)]
    overview_path: PathBuf,
    #[arg(long)]
    output_directory: PathBuf,
    #[arg(long)]
    skill_root: PathBuf,
    #[arg(long)]
    comment_prefix_path: PathBuf,
    #[arg(long)]
    tenant: Option<String>,
}

/// Validates capture input before composing the provider transaction.
pub(crate) fn run(
    arguments: &[OsString],
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let arguments = arguments
        .first()
        .into_iter()
        .chain(arguments.iter().skip(3));
    let cli = match OverviewCli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return Ok(error.to_string());
        }
        Err(error) => {
            return Err(caused_by("invalidInput", "Invalid command arguments", error).into());
        }
    };
    let context_path = path(&cli.context_path)?;
    let overview_path = path(&cli.overview_path)?;
    let output_directory = path(&cli.output_directory)?;
    let skill_root = path(&cli.skill_root)?;
    let prefix_path = path(&cli.comment_prefix_path)?;
    let capture = read_capture(&context_path, clock)?;
    let prefix = read_prefix(&prefix_path)?;
    let input = OverviewInput {
        capture: &capture,
        overview_path: &overview_path,
        output_directory: &output_directory,
        skill_root: &skill_root,
        comment_prefix: &prefix,
        tenant: cli.tenant.as_deref(),
    };
    let result = publish_overview(&input, process, http, clock, |snapshot| {
        let markdown = snapshot
            .get("markdownPath")
            .and_then(Value::as_str)
            .ok_or_else(|| fail("invalidState", "Snapshot has no Markdown path"))?;
        let hash = snapshot
            .get("markdownHash")
            .and_then(Value::as_str)
            .ok_or_else(|| fail("invalidState", "Snapshot has no Markdown hash"))?;
        let tools_directory = path(&report_tools::tools_directory()?)?;
        let artifact_directory = artifact_directory(&output_directory, snapshot, hash, clock)?;
        fs::create_dir_all(&artifact_directory)?;
        let artifact = overview_artifact::freeze(
            Path::new(markdown),
            hash,
            &artifact_directory,
            &skill_root,
            &tools_directory,
            process,
            clock,
            |tools, guard, source, output| {
                report_html::write_report(
                    source,
                    output,
                    &skill_root,
                    "Pull request overview",
                    tools,
                    guard,
                    process,
                    clock,
                )
            },
        )?;
        Ok(match artifact {
            overview_artifact::Preparation::Ready(artifact) => OverviewRender::Ready {
                html_path: artifact.html_path,
                html_hash: artifact.html_hash,
                archive_path: artifact.archive_path,
                archive_hash: artifact.archive_hash,
            },
            overview_artifact::Preparation::MissingTools(tools) => OverviewRender::MissingTools {
                missing: tools.missing,
                diagnostics: tools.diagnostics,
            },
            _ => return Err(fail("invalidState", "Unknown overview preparation outcome")),
        })
    })?;
    Ok(to_string(&result)?)
}

/// Keeps generations separate, while honoring a matching existing manifest.
fn artifact_directory(
    output: &Path,
    snapshot: &Value,
    markdown_hash: &str,
    clock: &dyn Clock,
) -> Result<PathBuf, AppError> {
    let run_id = snapshot
        .get("runId")
        .and_then(Value::as_str)
        .filter(|value| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| fail("invalidState", "Snapshot has an invalid run identifier"))?;
    let root = output.join("overview-publication");
    if let Some(bytes) = read_bytes(&root.join("html.json"), clock)? {
        let manifest: Value = from_slice(&bytes).map_err(|error| {
            caused_by("invalidState", "Malformed overview HTML manifest", error)
        })?;
        let hash = manifest
            .get("sourceHash")
            .and_then(Value::as_str)
            .ok_or_else(|| fail("invalidState", "Overview HTML manifest has no source hash"))?;
        if hash == markdown_hash {
            return Ok(root);
        }
    }
    Ok(root.join(run_id))
}

/// Normalizes paths relative to this process without losing Unicode.
fn path(value: &Path) -> Result<PathBuf, AppError> {
    let path = absolute(value)?;
    if path.to_str().is_none() {
        return Err(fail("invalidInput", "Overview paths must be Unicode"));
    }
    Ok(path)
}

/// Limits attribution input while preserving every accepted UTF-8 byte.
fn read_prefix(path: &Path) -> Result<String, AppError> {
    // Keep this discretionary read bound aligned with the publication boundary:
    // attribution is embedded in both a provider comment and its journal.
    const MAX_PREFIX_BYTES: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_PREFIX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PREFIX_BYTES {
        return Err(fail("invalidInput", "Overview comment prefix is too large"));
    }
    String::from_utf8(bytes).map_err(|error| {
        caused_by(
            "invalidInput",
            "Overview comment prefix is not UTF-8",
            error,
        )
        .into()
    })
}
