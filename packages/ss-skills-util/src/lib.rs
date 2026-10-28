//! Command composition for the internal skills utility; not a supported SDK.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod cargo;
mod git;

pub use cargo::run as run_cargo;
pub use git::run as run_git;
pub use ss_skills_review::{
    Clock, Http, HttpResponse, NativeHttp, NativeProcess, Process, ProcessOutput, SystemClock,
    classification,
};

use std::ffi::OsString;
use std::path::Path;

use ohno::AppError;

/// Runs a complete command with injected external boundaries.
pub fn run(
    arguments: &[OsString],
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    dispatch(arguments, process, http, clock, None)
}

/// Runs a command with an explicit directory for provider-owned state.
pub fn run_in(
    arguments: &[OsString],
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
    directory: &Path,
) -> Result<String, AppError> {
    dispatch(arguments, process, http, clock, Some(directory))
}

/// Routes local commands independently of the provider-backed workflows.
fn dispatch(
    arguments: &[OsString],
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
    directory: Option<&Path>,
) -> Result<String, AppError> {
    if arguments.get(1).is_some_and(|argument| argument == "cargo") {
        return cargo::run(&local_arguments(arguments), clock);
    }
    if arguments.get(1).is_some_and(|argument| argument == "git") {
        return git::run(&local_arguments(arguments), process);
    }
    match directory {
        Some(directory) => ss_skills_review::run_in(arguments, process, http, clock, directory),
        None => ss_skills_review::run(arguments, process, http, clock),
    }
}

/// Retains the executable name while removing the selected command namespace.
fn local_arguments(arguments: &[OsString]) -> Vec<OsString> {
    arguments
        .first()
        .into_iter()
        .chain(arguments.iter().skip(2))
        .cloned()
        .collect()
}
