//! Composes the overview transaction with provider boundaries.

use std::any::type_name;
use std::fmt;
use std::fs;
use std::path::Path;

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::FileTransaction;

use crate::error::{caused_by, fail};
use crate::overview_snapshot::{self, ContextInput, FrozenSnapshot};
use crate::overview_transaction::{self, EffectError, OverviewEffects};
use crate::target::{Platform, Target};
use crate::transport::Transport;
use crate::{Clock, Http, Process, classification, overview_ado, overview_github};

/// Input to the internal initial overview publication operation.
#[expect(
    clippy::exhaustive_structs,
    reason = "This is an internal command composition interface"
)]
pub struct OverviewInput<'a> {
    /// Validated capture, including inspected revisions.
    pub capture: &'a Value,
    /// Authored overview Markdown.
    pub overview_path: &'a Path,
    /// Directory for the publication journal and frozen report.
    pub output_directory: &'a Path,
    /// Active review skill root.
    pub skill_root: &'a Path,
    /// Attribution text, retained exactly.
    pub comment_prefix: &'a str,
    /// Optional Azure OAuth tenant.
    pub tenant: Option<&'a str>,
}

impl fmt::Debug for OverviewInput<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(type_name::<Self>())
            .field(
                "has_pull_request_url",
                &self.capture.get("pullRequestUrl").is_some(),
            )
            .field("overview_path", &self.overview_path)
            .field("output_directory", &self.output_directory)
            .field("skill_root", &self.skill_root)
            .field("has_comment_prefix", &!self.comment_prefix.is_empty())
            .field("has_tenant", &self.tenant.is_some())
            .finish()
    }
}

// These discretionary byte budgets bound persisted input rather than enforce
// provider limits. The CLI prefix reader matches this invocation bound.
const MAX_PREFIX_BYTES: usize = 64 * 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_URL_BYTES: usize = 4096;
const MAX_ERROR_CHARS: usize = 4096;

/// Publishes an initial overview using injected provider boundaries.
///
/// The capture must already have passed the capture reader's strict validation.
///
/// # Errors
///
/// Rejects invalid target coordinates and inaccessible output directories.
pub fn publish_overview(
    input: &OverviewInput<'_>,
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    if !input.output_directory.is_absolute()
        || !input.overview_path.is_absolute()
        || !input.skill_root.is_absolute()
        || input.output_directory.to_str().is_none()
        || input.overview_path.to_str().is_none()
        || input.skill_root.to_str().is_none()
    {
        return Err(fail(
            "invalidInput",
            "Overview paths must be absolute Unicode paths",
        ));
    }
    if input.comment_prefix.len() > MAX_PREFIX_BYTES
        || [
            input.output_directory,
            input.overview_path,
            input.skill_root,
        ]
        .iter()
        .any(|path| {
            path.to_str()
                .is_none_or(|value| value.len() > MAX_PATH_BYTES)
        })
    {
        return Err(fail("invalidInput", "Overview input is too large"));
    }
    let context = overview_snapshot::validate_context(input.capture)
        .map_err(|error| caused_by("invalidInput", error.to_string(), error))?;
    if context.url.len() > MAX_URL_BYTES {
        return Err(fail(
            "invalidInput",
            "Overview pull request URL is too large",
        ));
    }
    let url = context.url;
    let target = context.target;
    if let Err(error) = fs::create_dir_all(input.output_directory) {
        let message = error
            .to_string()
            .chars()
            .take(MAX_ERROR_CHARS)
            .collect::<String>();
        return Ok(json!({
            "schemaVersion": 2,
            "pullRequestUrl": url,
            "status": "journalFailure",
            "stage": "preflight",
            "resultPath": input.output_directory.join("overview-publication-result.json"),
            "snapshot": null,
            "comments": [],
            "operations": [],
            "error": message
        }));
    }
    let mut effects = Effects {
        transport: Transport::new(&target, input.tenant, process, http, clock),
        target,
        input,
        clock,
        snapshot_lock: None,
    };
    Ok(overview_transaction::publish(
        &input
            .output_directory
            .join("overview-publication-result.json"),
        clock,
        &mut effects,
        url,
        input.comment_prefix,
    ))
}

/// Retains snapshot ownership across all provider effects.
struct Effects<'a, 'b> {
    transport: Transport<'a>,
    target: Target,
    input: &'b OverviewInput<'b>,
    clock: &'a dyn Clock,
    snapshot_lock: Option<FileTransaction<'a>>,
}

impl OverviewEffects for Effects<'_, '_> {
    fn is_open(&mut self) -> Result<bool, EffectError> {
        match self.target.platform {
            Platform::GitHub => overview_github::is_open(&mut self.transport, &self.target),
            Platform::AzureDevOps => overview_ado::is_open(&mut self.transport, &self.target),
        }
        .map_err(effect_error)
    }

    fn freeze(&mut self, _clock: &dyn Clock) -> Result<FrozenSnapshot, EffectError> {
        let lock = FileTransaction::open(
            &self.input.output_directory.join("overview-snapshot.json"),
            self.clock,
        )
        .map_err(effect_error)?;
        let snapshot = overview_snapshot::freeze_overview(
            ContextInput::Json(self.input.capture),
            self.input.overview_path,
            self.input.output_directory,
            self.input.skill_root,
            true,
            &lock,
            self.clock,
        )
        .map_err(|error| local_error(error.to_string()))?;
        self.snapshot_lock = Some(lock);
        Ok(snapshot)
    }

    fn comment_bodies(
        &mut self,
        snapshot: &FrozenSnapshot,
        prefix: &str,
        clock: &dyn Clock,
    ) -> Result<Vec<String>, EffectError> {
        overview_snapshot::comment_bodies(snapshot, prefix, clock)
            .map_err(|error| local_error(error.to_string()))
    }

    fn comment(
        &mut self,
        _snapshot: &FrozenSnapshot,
        payload: &Value,
    ) -> Result<Value, EffectError> {
        match self.target.platform {
            Platform::GitHub => {
                overview_github::comment(&mut self.transport, &self.target, payload)
            }
            Platform::AzureDevOps => {
                overview_ado::comment(&mut self.transport, &self.target, payload)
            }
        }
        .map_err(|error| write_error(error, self.target.platform))
    }

    fn verify_comment(
        &mut self,
        _snapshot: &FrozenSnapshot,
        payload: &Value,
    ) -> Result<(), EffectError> {
        match self.target.platform {
            Platform::GitHub => {
                overview_github::verify_comment(&mut self.transport, &self.target, payload)
            }
            Platform::AzureDevOps => {
                overview_ado::verify_comment(&mut self.transport, &self.target, payload)
            }
        }
        .map_err(effect_error)
    }
}

/// Keeps unclassified failures from authorizing a repeated write.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err supplies owned errors"
)]
fn effect_error(error: AppError) -> EffectError {
    local_error(error.to_string())
}

/// Only classifications backed by an actual HTTP refusal permit another write.
fn write_error(error: AppError, platform: Platform) -> EffectError {
    let kind = classification(&error);
    let refusal = (platform == Platform::AzureDevOps && kind == "rateLimited")
        || (platform == Platform::GitHub && kind == "explicitRateLimited");
    let mut error = effect_error(error);
    error.definite_refusal = refusal;
    error
}

/// Converts local failures into transaction diagnostics.
fn local_error(message: String) -> EffectError {
    EffectError {
        message,
        definite_refusal: false,
    }
}
