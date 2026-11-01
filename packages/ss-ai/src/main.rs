//! Entry point for the globally installed skills utility.

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use ohno::AppError;
use ss_ai::{
    NativeHttp, NativeProcess, SystemClock, classification, run, run_cargo, run_context, run_git,
    run_workflow,
};
use ss_ai_runtime::fail;

/// Gives the installed executable the same allocator policy on both platforms.
///
/// Mimalloc is the utility's performance-oriented allocator; library consumers
/// such as package tests retain their own allocation environment.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Converts the complete command outcome into the process-level CLI contract.
fn main() -> ExitCode {
    match execute() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{}: {error}", classification(&error));
            ExitCode::FAILURE
        }
    }
}

/// Keeps environment discovery and output handling outside the testable operations.
fn execute() -> Result<ExitCode, AppError> {
    let arguments = env::args_os().collect::<Vec<_>>();
    let output = if arguments.get(1).is_some_and(|argument| argument == "cargo") {
        let cargo_arguments = arguments
            .first()
            .into_iter()
            .chain(arguments.iter().skip(2));
        run_cargo(cargo_arguments, &SystemClock::default())?
    } else if arguments.get(1).is_some_and(|argument| argument == "git") {
        let git_arguments = arguments
            .first()
            .into_iter()
            .chain(arguments.iter().skip(2));
        let process = NativeProcess::new(&env::current_dir()?);
        run_git(git_arguments, &process)?
    } else if arguments
        .get(1)
        .is_some_and(|argument| argument == "context")
    {
        if arguments
            .get(2)
            .is_some_and(|argument| argument == "create")
        {
            let process = NativeProcess::new(&env::current_dir()?);
            let http = NativeHttp::new()?;
            write_output(&run(&arguments, &process, &http, &SystemClock::default())?)?;
            return Ok(ExitCode::SUCCESS);
        }
        let context_arguments = arguments
            .first()
            .into_iter()
            .chain(arguments.iter().skip(2));
        run_context(context_arguments, &SystemClock::default())?
    } else if arguments
        .get(1)
        .is_some_and(|argument| argument == "workflow")
    {
        let workflow_arguments = arguments
            .first()
            .into_iter()
            .chain(arguments.iter().skip(2));
        let process = NativeProcess::new(&env::current_dir()?);
        run_workflow(workflow_arguments, &process, &SystemClock::default())?
    } else {
        let process = NativeProcess::new(&env::current_dir()?);
        let http = NativeHttp::new()?;
        run(&arguments, &process, &http, &SystemClock::default())?
    };
    write_output(&output)?;
    // Publication outcomes must remain available on stdout even when the
    // durable result requires intervention.
    if arguments.get(1).is_some_and(|arg| arg == "review")
        && arguments
            .get(2)
            .is_some_and(|arg| arg == "publish-final-review")
        && !arguments
            .iter()
            .any(|arg| arg == "--help" || arg == "-h" || arg == "--version" || arg == "-V")
    {
        let value: serde_json::Value = serde_json::from_str(&output)?;
        let status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| fail("invalidState", "Publication outcome has no status"))?;
        if status != "succeeded" {
            return Ok(ExitCode::FAILURE);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Writes one complete response after command execution succeeds.
fn write_output(output: &str) -> Result<(), AppError> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(output.as_bytes())?;
    stdout.write_all(b"\n")?;
    Ok(())
}
