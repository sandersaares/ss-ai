//! CLI translation for local Git comparisons and diff parsing.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use ohno::AppError;
use ss_ai_runtime::{Process, caused_by};

/// Keeps local Git syntax independent of provider operations.
#[derive(Debug, Parser)]
#[command(name = "ss-ai git", about = "Inspect exact Git changes")]
struct GitCli {
    #[command(subcommand)]
    command: GitCommand,
}

/// Selects complete immutable comparisons or diff interpretation.
#[derive(Debug, Subcommand)]
enum GitCommand {
    /// Compare exact immutable commits already available in a local repository.
    Diff {
        #[arg(long)]
        repository_path: PathBuf,
        #[arg(long)]
        old_commit: String,
        #[arg(long)]
        new_commit: String,
    },
    /// Parse complete unified diff text from UTF-8 standard input.
    Parse,
    /// Normalize a quoted or service-facing Git path.
    Path {
        #[arg(long)]
        value: String,
        #[arg(long)]
        side_prefix: bool,
    },
}

/// Produces the same JSON record without constructing provider state.
pub fn run<I, T>(arguments: I, process: &dyn Process) -> Result<String, AppError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match GitCli::try_parse_from(crate::command::namespace_arguments(arguments, "git")) {
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
    let value = match cli.command {
        GitCommand::Diff {
            repository_path,
            old_commit,
            new_commit,
        } => serde_json::to_value(ss_ai_git::compare(
            process,
            &repository_path,
            &old_commit,
            &new_commit,
        )?)?,
        GitCommand::Parse => {
            let mut text = String::new();
            io::stdin().read_to_string(&mut text)?;
            serde_json::json!({"files": ss_ai_git::parse(&text)?})
        }
        GitCommand::Path { value, side_prefix } => {
            serde_json::json!({"path": ss_ai_git::normalize_path(&value, side_prefix)?})
        }
    };
    Ok(serde_json::to_string(&value)?)
}
