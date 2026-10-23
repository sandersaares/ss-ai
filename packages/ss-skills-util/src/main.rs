//! Entry point for the globally installed skills utility.

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use ohno::AppError;
use ss_skills_util::{NativeHttp, NativeProcess, SystemClock, classification, run};

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
    let process = NativeProcess::new(&env::current_dir()?);
    let http = NativeHttp::new()?;
    let output = run(
        &env::args_os().collect::<Vec<_>>(),
        &process,
        &http,
        &SystemClock::default(),
    )?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(output.as_bytes())?;
    stdout.write_all(b"\n")?;
    Ok(())
}
