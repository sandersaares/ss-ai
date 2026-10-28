//! Routes command namespaces to local and provider-backed operations.

use std::ffi::OsString;
use std::path::Path;

use ohno::AppError;

use crate::{Clock, Http, Process, cargo, git};

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
        return cargo::run(local_arguments(arguments), clock);
    }
    if arguments.get(1).is_some_and(|argument| argument == "git") {
        return git::run(local_arguments(arguments), process);
    }
    match directory {
        Some(directory) => ss_ai_review::run_in(arguments, process, http, clock, directory),
        None => ss_ai_review::run(arguments, process, http, clock),
    }
}

/// Retains the executable name while removing the selected command namespace.
fn local_arguments(arguments: &[OsString]) -> impl Iterator<Item = &OsString> {
    arguments
        .first()
        .into_iter()
        .chain(arguments.iter().skip(2))
}
