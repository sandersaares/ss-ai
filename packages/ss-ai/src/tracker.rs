//! Composes local tracker validation with the provider publication transaction.

use std::ffi::OsString;
use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf, absolute};

use clap::{Args, Parser, error::ErrorKind};
use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json, to_string};
use ss_ai_review::{
    TrackerAcceptance, TrackerPublicationInput, publish_tracker_items, resolve_tracker_target,
    tracker_publication_check_prior, validate_tracker_arguments,
};
use ss_ai_review_workflow::tracker_publication_plan;
use ss_ai_runtime::{Clock, FileTransaction, Process, caused_by, fail};

use crate::Http;
use crate::final_review::{FixedClock, probe_output, reject_alias};

/// Parses this operation separately from the review command hierarchy.
#[derive(Debug, Parser)]
#[command(name = "ss-ai review publish-tracker-items")]
struct TrackerCli {
    #[command(flatten)]
    destination: Destination,
    #[command(flatten)]
    files: Files,
    #[command(flatten)]
    metadata: Metadata,
    #[command(flatten)]
    acceptance: Acceptance,
    #[arg(long)]
    tenant: Option<String>,
    #[arg(long)]
    now_utc: Option<Timestamp>,
}

/// Tracker coordinates are independent of the reviewed pull request.
#[derive(Debug, Args)]
struct Destination {
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    platform: Option<String>,
    #[arg(long)]
    organization: Option<String>,
    #[arg(long)]
    project: Option<String>,
    #[arg(long)]
    repository: Option<String>,
}

/// Local artifacts supply the approved plan and checked evidence.
#[derive(Debug, Args)]
struct Files {
    #[arg(long)]
    approved_findings_path: PathBuf,
    #[arg(long)]
    diagnostics_path: PathBuf,
    #[arg(long)]
    output_directory: PathBuf,
    #[arg(long)]
    context_path: Option<PathBuf>,
}

/// Azure metadata identifies the requested work item representation.
#[derive(Debug, Args)]
struct Metadata {
    #[arg(long)]
    work_item_type: Option<String>,
    #[arg(long)]
    area_path: Option<String>,
    #[arg(long)]
    description_field: Option<String>,
    #[arg(long)]
    parent_work_item_id: Option<NonZeroU32>,
}

/// An explicit human decision names the exact paused rewrite.
#[derive(Debug, Args)]
struct Acceptance {
    #[arg(long = "accept-stored-rewrite")]
    stored_rewrite: bool,
    #[arg(long = "accept-work-item-id")]
    work_item_id: Option<String>,
    #[arg(long = "accept-expected-body-hash")]
    expected_body_hash: Option<String>,
    #[arg(long = "accept-stored-body-hash")]
    stored_body_hash: Option<String>,
}

/// Leaves help separate from execution and rejects malformed command syntax.
fn parse(arguments: &[OsString]) -> Result<Result<TrackerCli, String>, AppError> {
    let args = arguments
        .first()
        .into_iter()
        .chain(arguments.iter().skip(3));
    match TrackerCli::try_parse_from(args) {
        Ok(cli) => Ok(Ok(cli)),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            Ok(Err(error.to_string()))
        }

        Err(error) => Err(caused_by("invalidInput", "Invalid command arguments", error).into()),
    }
}

/// Resolves and validates the destination before any provider access.
fn resolve_inputs(cli: &TrackerCli) -> Result<(Value, Value), AppError> {
    let target = resolve_tracker_target(&json!({
        "url": cli.destination.url,
        "platform": cli.destination.platform,
        "organization": cli.destination.organization,
        "project": cli.destination.project,
        "repository": cli.destination.repository
    }))?;
    let metadata = json!({
        "workItemType": cli.metadata.work_item_type,
        "areaPath": cli.metadata.area_path,
        "descriptionField": cli.metadata.description_field,
        "parentWorkItemId": cli.metadata.parent_work_item_id
    });
    validate_tracker_arguments(&target, &metadata)?;
    Ok((target, metadata))
}

/// Requires an explicit decision and all coordinates of the paused rewrite.
fn acceptance(input: &Acceptance) -> Result<Option<TrackerAcceptance<'_>>, AppError> {
    let coordinates = [
        input.work_item_id.as_deref(),
        input.expected_body_hash.as_deref(),
        input.stored_body_hash.as_deref(),
    ];
    if !input.stored_rewrite {
        if coordinates.iter().any(Option::is_some) {
            return Err(fail(
                "invalidInput",
                "Acceptance coordinates require --accept-stored-rewrite",
            ));
        }
        return Ok(None);
    }
    match coordinates {
        [Some(item_id), Some(expected), Some(stored)]
            if [item_id, expected, stored]
                .iter()
                .all(|value| !value.trim().is_empty()) =>
        {
            Ok(Some(TrackerAcceptance {
                item_id,
                expected_body_hash: expected,
                stored_body_hash: stored,
            }))
        }
        _ => Err(fail(
            "invalidInput",
            "Accepting a rewrite requires the item ID and both body hashes",
        )),
    }
}

/// Retains the result lock across local preparation and provider publication.
pub(crate) fn run(
    arguments: &[OsString],
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let cli = match parse(arguments)? {
        Ok(cli) => cli,
        Err(help) => return Ok(help),
    };
    let fixed = FixedClock {
        timestamp: cli.now_utc,
        clock,
    };
    let clock: &dyn Clock = &fixed;
    let (target, metadata) = resolve_inputs(&cli)?;
    let acceptance = acceptance(&cli.acceptance)?;
    let output = absolute(&cli.files.output_directory)?;
    fs::create_dir_all(&output)?;
    let result_path = output.join("tracker-publication-result.json");
    for input in [
        &cli.files.approved_findings_path,
        &cli.files.diagnostics_path,
    ]
    .into_iter()
    .chain(cli.files.context_path.iter())
    {
        reject_alias(&result_path, input)?;
    }
    if let Some(context) = &cli.files.context_path {
        let mut sidecar = context.as_os_str().to_os_string();
        sidecar.push(".authorship.json");
        reject_alias(&result_path, Path::new(&sidecar))?;
    }
    let mut journal = FileTransaction::open(&result_path, clock)?;
    tracker_publication_check_prior(&mut journal, &target, acceptance.as_ref())?;
    probe_output(&output)?;
    let platform = target
        .get("platform")
        .and_then(Value::as_str)
        .ok_or_else(|| fail("invalidInput", "Tracker platform is missing"))?;
    let plan = tracker_publication_plan::prepare(
        &cli.files.approved_findings_path,
        &cli.files.diagnostics_path,
        cli.files.context_path.as_deref(),
        platform,
        clock,
    )?;
    let input = TrackerPublicationInput {
        target: &target,
        metadata: &metadata,
        plan: &plan,
        acceptance: acceptance.as_ref(),
        tenant: cli.tenant.as_deref(),
    };
    let result = publish_tracker_items(&mut journal, &result_path, &input, process, http, clock)?;
    Ok(to_string(&result)?)
}
