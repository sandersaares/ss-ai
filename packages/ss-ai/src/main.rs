//! Entry point for the globally installed skills utility.

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use ohno::AppError;
use ss_ai::{NativeHttp, NativeProcess, SystemClock, classification, run, run_cargo, run_git};

/// Gives the installed executable the same allocator policy on both platforms.
///
/// Mimalloc is the utility's performance-oriented allocator; library consumers
/// such as package tests retain their own allocation environment.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Converts the complete command outcome into the process-level CLI contract.
fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}: {error}", classification(&error));
            ExitCode::FAILURE
        }
    }
}

/// Keeps environment discovery and output handling outside the testable operations.
fn execute() -> Result<(), AppError> {
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
    } else {
        let process = NativeProcess::new(&env::current_dir()?);
        let http = NativeHttp::new()?;
        run(&arguments, &process, &http, &SystemClock::default())?
    };
    let mut stdout = io::stdout().lock();
    stdout.write_all(output.as_bytes())?;
    stdout.write_all(b"\n")?;
    Ok(())
}
