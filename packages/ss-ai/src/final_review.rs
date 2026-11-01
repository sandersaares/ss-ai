//! Composes local review validation and the provider publication transaction.

use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind as IoErrorKind, Write};
use std::path::{Path, PathBuf, absolute};
use std::time::Duration;

use clap::Parser;
use clap::error::ErrorKind;
use jiff::Timestamp;
use ohno::{AppError, EnrichableExt};
use serde_json::{Value, json};
use ss_ai_review::{
    FinalReviewInput, capture_target, final_review_check_prior, publish_final_review,
};
use ss_ai_review_context::{
    canonical_capture_from_evidence, evidence_hash, parse_workflow_json, read_capture,
};
use ss_ai_review_workflow::{audit, review_publication_plan};
use ss_ai_runtime::{Clock, FileTransaction, Process, caused_by, fail, read_bytes};

use crate::Http;

/// Parses the publication command independently to bound clap's stack use.
#[derive(Debug, Parser)]
#[command(name = "ss-ai review publish-final-review")]
struct FinalReviewCli {
    #[arg(long)]
    url: String,
    #[arg(long)]
    approved_findings_path: PathBuf,
    #[arg(long)]
    context_path: PathBuf,
    #[arg(long)]
    diagnostics_path: PathBuf,
    #[arg(long)]
    repository_path: PathBuf,
    #[arg(long)]
    output_directory: PathBuf,
    #[arg(long)]
    discard_pending_review: bool,
    #[arg(long)]
    publish_to_closed_pull_request: bool,
    #[arg(long)]
    merge_approval_path: Option<PathBuf>,
    #[arg(long)]
    tenant: Option<String>,
    #[arg(long)]
    now_utc: Option<Timestamp>,
}

/// Retains the result lock across preflight, provider effects, and final output.
pub(crate) fn run(
    arguments: &[OsString],
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let args = arguments
        .first()
        .into_iter()
        .chain(arguments.iter().skip(3));
    let cli = match FinalReviewCli::try_parse_from(args) {
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
    let fixed = FixedClock {
        timestamp: cli.now_utc,
        clock,
    };
    let clock: &dyn Clock = &fixed;
    let target = capture_target(&json!({"url": cli.url}))?;
    let output = absolute(&cli.output_directory)?;
    fs::create_dir_all(&output)?;
    let result_path = output.join("publication-result.json");
    for input in [
        &cli.approved_findings_path,
        &cli.context_path,
        &cli.diagnostics_path,
    ]
    .into_iter()
    .chain(cli.merge_approval_path.iter())
    {
        reject_alias(&result_path, input)?;
    }
    let mut sidecar = cli.context_path.as_os_str().to_os_string();
    sidecar.push(".authorship.json");
    reject_alias(&result_path, Path::new(&sidecar))?;
    let mut journal = FileTransaction::open(&result_path, clock)?;
    final_review_check_prior(&mut journal, &target)?;
    probe_output(&output)?;
    let approved = review_publication_plan::read_approved(&cli.approved_findings_path, clock)?;
    let context = read_capture(&cli.context_path, clock)?;
    let identity = context
        .get("authenticatedIdentity")
        .filter(|value| value.is_object())
        .ok_or_else(|| fail("invalidInput", "Capture has no authenticated identity"))?;
    if identity.get("platform") != target.get("platform")
        || !["id", "descriptor", "uniqueName"].iter().any(|field| {
            identity
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
    {
        return Err(fail(
            "invalidInput",
            "Capture has an invalid authenticated identity",
        ));
    }
    check_target(
        &target,
        approved
            .get("target")
            .ok_or_else(|| fail("invalidInput", "Approved finding artifact has no target"))?,
        "the approved finding artifact",
    )?;
    check_target(
        &target,
        context
            .get("target")
            .ok_or_else(|| fail("invalidInput", "Capture has no target"))?,
        "the captured discussion",
    )?;
    let context_url = text(&context, "pullRequestUrl")?;
    let url_target = capture_target(&json!({"url": context_url}))?;
    check_target(&target, &url_target, "the captured discussion URL")?;
    let source = text(&approved, "reviewedSourceCommit")?;
    let base = text(&approved, "reviewedMergeBaseCommit")?;
    let approve_merge = match &cli.merge_approval_path {
        Some(path) => audit::test_merge_approval(path, context_url, source, clock)?,
        None => false,
    };
    if approve_merge
        && context
            .get("pullRequest")
            .and_then(|pr| pr.get("isAuthoredBySelf"))
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err(fail(
            "invalidInput",
            "Merge approval requires a pull request not authored by the authenticated account",
        ));
    }
    let diagnostics = read_json(&cli.diagnostics_path, clock).map_err(|error| {
        error.enrich(format!(
            "Publication diagnostics at '{}' cannot be read",
            cli.diagnostics_path.display()
        ))
    })?;
    // Refuse locally knowable mismatches before launching Git or contacting a provider.
    review_publication_plan::preflight(&approved, &context, &diagnostics, approve_merge)?;
    let comparison = ss_ai_git::compare(process, &cli.repository_path, base, source).map_err(|error| {
        error.enrich(format!(
            "Ensure the checkout contains reviewed commits {base} and {source}; fetch missing commits with git fetch"
        ))
    })?;
    let prepared = review_publication_plan::prepare(
        &approved,
        &context,
        &comparison,
        &diagnostics,
        approve_merge,
    )?;
    let plan = prepared
        .get("plan")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("invalidState", "Publication plan is missing"))?;
    let summary = text(&prepared, "summaryBody")?;
    let hash = evidence_hash(&context)?;
    let input = FinalReviewInput {
        target: &target,
        expected_identity: identity,
        plan,
        summary_body: summary,
        expected_evidence_hash: &hash,
        source_commit: source,
        merge_base_commit: base,
        discard_pending_review: cli.discard_pending_review,
        publish_closed: cli.publish_to_closed_pull_request,
        approve_merge,
        tenant: cli.tenant.as_deref(),
    };
    let result = publish_final_review(
        &mut journal,
        &result_path,
        &input,
        process,
        http,
        clock,
        |evidence| evidence_hash(&canonical_capture_from_evidence(evidence)?),
    )?;
    Ok(serde_json::to_string(&result)?)
}

/// Confirms a sibling file can be created, flushed, and removed before effects.
fn probe_output(directory: &Path) -> Result<(), AppError> {
    // Create-new protects other files; a bounded collision retry accommodates
    // concurrent invocations without hiding unrelated I/O errors.
    for attempt in 0..100 {
        let path = directory.join(format!(
            ".publication-probe-{}-{attempt}",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let result = file.write_all(b"probe").and_then(|()| file.sync_all());
                drop(file);
                let cleanup = fs::remove_file(&path);
                result?;
                cleanup?;
                return Ok(());
            }
            Err(error) if error.kind() == IoErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(fail("io", "Cannot allocate a publication output probe"))
}

/// Applies an explicit timestamp without replacing the retry wait boundary.
struct FixedClock<'a> {
    timestamp: Option<Timestamp>,
    clock: &'a dyn Clock,
}
impl Clock for FixedClock<'_> {
    fn now(&self) -> Timestamp {
        self.timestamp.unwrap_or_else(|| self.clock.now())
    }
    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        self.clock.wait(duration)
    }
}

/// Rejects a result destination that could replace one of its own inputs.
fn reject_alias(output: &Path, input: &Path) -> Result<(), AppError> {
    let input = absolute(input)?;
    if output == input
        || (output.exists() && input.exists() && same_file::is_same_file(output, &input)?)
    {
        return Err(fail(
            "invalidInput",
            "Publication result must not replace an input",
        ));
    }
    Ok(())
}

/// Requires matching provider coordinates, not merely a matching display URL.
fn check_target(left: &Value, right: &Value, right_name: &str) -> Result<(), AppError> {
    for field in [
        "platform",
        "collectionUrl",
        "organization",
        "project",
        "repository",
    ] {
        let same = match (left.get(field), right.get(field)) {
            (Some(Value::Null), Some(Value::Null)) => true,
            (Some(Value::String(a)), Some(Value::String(b))) => a.eq_ignore_ascii_case(b),
            _ => false,
        };
        if !same {
            return Err(fail(
                "invalidInput",
                format!("Pull request target and {right_name} differ in {field}"),
            ));
        }
    }
    if left.get("pullRequestId").and_then(Value::as_u64)
        != right.get("pullRequestId").and_then(Value::as_u64)
        || left.get("pullRequestId").and_then(Value::as_u64).is_none()
    {
        return Err(fail(
            "invalidInput",
            format!(
                "The command line pull request is {} and {right_name} is {}",
                left.get("pullRequestId").unwrap_or(&Value::Null),
                right.get("pullRequestId").unwrap_or(&Value::Null)
            ),
        ));
    }
    Ok(())
}

/// Reads nonempty required text without coercing JSON values.
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, AppError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| fail("invalidInput", format!("Missing or invalid {field}")))
}

/// Reads a complete snapshot and refuses duplicate JSON object keys.
fn read_json(path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let bytes = read_bytes(path, clock)?
        .ok_or_else(|| fail("invalidInput", format!("Missing input: {}", path.display())))?;
    parse_workflow_json(&bytes)
}
