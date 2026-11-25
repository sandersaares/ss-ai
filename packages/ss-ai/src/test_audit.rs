//! Local test-audit inventory, classification, and reporting commands.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use ohno::AppError;
use ss_ai_runtime::{Process, caused_by};
use ss_ai_test_audit::{
    advance, initialize, next_batch, record_batch, report, search_many, submit,
};

use crate::Clock;

/// Parses the local test-audit namespace without provider initialization.
#[derive(Debug, Parser)]
#[command(
    name = "ss-ai test-audit",
    about = "Inventory and classify test promises"
)]
struct TestAuditCli {
    #[command(subcommand)]
    command: TestAuditCommand,
}

/// Defines the deterministic native stages of one resumable test audit.
#[derive(Debug, Subcommand)]
enum TestAuditCommand {
    /// Create an immutable repository and scope snapshot.
    Initialize {
        #[arg(long)]
        repository_path: PathBuf,
        #[arg(long = "scope-path")]
        scope_paths: Vec<PathBuf>,
        #[arg(long)]
        state_directory: PathBuf,
    },
    /// Search the bounded line index for contract evidence.
    Search {
        #[arg(long)]
        state_directory: PathBuf,
        #[arg(long = "query", required = true)]
        queries: Vec<String>,
        #[arg(long)]
        kind: Option<String>,
        /// Uses a reviewable page while allowing narrower targeted searches.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Export the next bounded assertion batch without consuming it.
    NextBatch {
        #[arg(long)]
        state_directory: PathBuf,
        #[arg(long)]
        output_path: PathBuf,
        /// Matches the skill's cumulative assertion batch progress unit.
        #[arg(long, default_value_t = 25)]
        count: usize,
    },
    /// Validate and durably record agent-authored classifications.
    RecordBatch {
        #[arg(long)]
        state_directory: PathBuf,
        #[arg(long)]
        batch_path: PathBuf,
    },
    /// Record one classification batch and export the next batch.
    Advance {
        #[arg(long)]
        state_directory: PathBuf,
        #[arg(long)]
        batch_path: PathBuf,
        #[arg(long)]
        output_path: PathBuf,
        /// Matches the skill's cumulative assertion batch progress unit.
        #[arg(long, default_value_t = 25)]
        count: usize,
    },
    /// Resolve compact evidence handles and submit one assertion batch.
    Submit {
        #[arg(long)]
        state_directory: PathBuf,
        #[arg(long)]
        batch_path: PathBuf,
        #[arg(long)]
        submission_path: PathBuf,
        #[arg(long)]
        output_path: PathBuf,
        #[arg(long, default_value_t = 25)]
        count: usize,
    },
    /// Require complete coverage and write the final trace and reports.
    Report {
        #[arg(long)]
        state_directory: PathBuf,
    },
}

/// Runs local commands with an injected process and clock boundary.
pub fn run<I, T>(arguments: I, process: &dyn Process, clock: &dyn Clock) -> Result<String, AppError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match TestAuditCli::try_parse_from(crate::command::namespace_arguments(
        arguments,
        "test-audit",
    )) {
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
    match cli.command {
        TestAuditCommand::Initialize {
            repository_path,
            scope_paths,
            state_directory,
        } => initialize(
            &repository_path,
            &scope_paths,
            &state_directory,
            process,
            clock,
        ),
        TestAuditCommand::Search {
            state_directory,
            queries,
            kind,
            limit,
        } => search_many(&state_directory, &queries, kind.as_deref(), limit, clock),
        TestAuditCommand::NextBatch {
            state_directory,
            output_path,
            count,
        } => next_batch(&state_directory, &output_path, count, clock),
        TestAuditCommand::RecordBatch {
            state_directory,
            batch_path,
        } => record_batch(&state_directory, &batch_path, clock),
        TestAuditCommand::Advance {
            state_directory,
            batch_path,
            output_path,
            count,
        } => advance(&state_directory, &batch_path, &output_path, count, clock),
        TestAuditCommand::Submit {
            state_directory,
            batch_path,
            submission_path,
            output_path,
            count,
        } => submit(
            &state_directory,
            &batch_path,
            &submission_path,
            &output_path,
            count,
            clock,
        ),
        TestAuditCommand::Report { state_directory } => report(&state_directory, clock),
    }
}
