//! CLI translation for local captured review evidence.

use std::ffi::OsString;
use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use clap::builder::OsStringValueParser;
use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand};
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{
    increment_diagnostic, normalize_options, prepare, prepare_failure, query, read_capture,
    read_diagnostics, read_saved,
};
use ss_ai_runtime::{Clock, caused_by, fail};

/// Keeps local evidence operations independent of provider initialization.
#[derive(Debug, Parser)]
#[command(name = "ss-ai context", about = "Inspect captured review evidence")]
struct ContextCli {
    #[command(subcommand)]
    command: ContextCommand,
}

/// Exposes direct and saved queries, preparation, and diagnostic records.
#[derive(Debug, Subcommand)]
enum ContextCommand {
    /// Read a page directly from a capture.
    Read {
        #[arg(long)]
        path: PathBuf,
        #[command(flatten)]
        options: QueryOptions,
        #[arg(long)]
        files_json: Option<String>,
        #[arg(long, value_parser = OsStringValueParser::new())]
        files_path: Option<OsString>,
        #[arg(long)]
        compare_path: Option<PathBuf>,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read or advance a saved request.
    ReadSaved {
        #[arg(long)]
        request_path: PathBuf,
        #[arg(long)]
        next: bool,
        #[arg(long)]
        restart: bool,
    },
    /// Create a saved request.
    Prepare {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        request_path: PathBuf,
        #[arg(long)]
        diagnostics_path: PathBuf,
        #[command(flatten)]
        options: QueryOptions,
        #[arg(long)]
        file: Vec<String>,
        #[arg(long)]
        text_path: Option<PathBuf>,
        #[arg(long)]
        compare_path: Option<PathBuf>,
    },
    /// Read or increment input diagnostics.
    Diagnostics {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        record: Option<String>,
        #[arg(long, requires = "record")]
        count: Option<NonZeroU32>,
    },
}

/// Carries selectors in the same shape as persisted query requests.
#[derive(Debug, Args)]
struct QueryOptions {
    #[arg(long)]
    query: String,
    #[arg(long)]
    field: Option<String>,
    #[arg(long, default_value = "all")]
    scope: String,
    #[arg(long)]
    kind: Option<String>,
    #[arg(long)]
    resolution: Option<String>,
    #[arg(long)]
    thread_id: Option<String>,
    #[arg(long)]
    comment_id: Option<String>,
    #[arg(long)]
    authorship: Option<String>,
    #[arg(long)]
    index_only: bool,
    #[arg(long)]
    text: Option<String>,
    // Matches the query model's ordinary page size.
    #[arg(long, default_value_t = 20)]
    page_size: i32,
}

impl QueryOptions {
    /// Builds the shared query representation before validation.
    fn value(&self, files: &Value, text: Option<&str>) -> Value {
        json!({
            "query": self.query, "field": self.field.as_deref().unwrap_or(""),
            "files": files, "scope": self.scope,
            "kind": self.kind.as_deref().unwrap_or(""),
            "resolution": self.resolution.as_deref().unwrap_or(""),
            "threadId": self.thread_id.as_deref().unwrap_or(""),
            "commentId": self.comment_id.as_deref().unwrap_or(""),
            "authorship": self.authorship.as_deref().unwrap_or(""),
            "indexOnly": self.index_only, "text": text.unwrap_or(""),
            "pageSize": self.page_size
        })
    }
}

/// Runs local commands without constructing an HTTP client.
pub fn run<I, T>(arguments: I, clock: &dyn Clock) -> Result<String, AppError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli =
        match ContextCli::try_parse_from(crate::command::namespace_arguments(arguments, "context"))
        {
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
        ContextCommand::Read {
            path,
            options,
            files_json,
            files_path,
            compare_path,
            cursor,
        } => {
            let files = match (files_json, files_path) {
                (Some(_), Some(_)) => {
                    return Err(fail(
                        "invalidInput",
                        "Supply either -FilesJson or -FilesPath, not both.",
                    ));
                }
                (Some(text), None) => {
                    if text.trim().is_empty() {
                        return Err(fail(
                            "invalidInput",
                            "-FilesJson requires a nonempty JSON array of strings.",
                        ));
                    }
                    parse_files(&text)?
                }
                (None, Some(path)) => {
                    if path.is_empty() || path.to_str().is_some_and(|text| text.trim().is_empty()) {
                        return Err(fail(
                            "invalidInput",
                            "-FilesPath requires a nonempty file path.",
                        ));
                    }
                    parse_files(&read_utf8(Path::new(&path))?)?
                }
                (None, None) => json!([]),
            };
            let options = normalize_options(&options.value(&files, options.text.as_deref()))?;
            let document = read_capture(&path, clock)?;
            let previous = compare_path
                .as_deref()
                .map(|path| read_capture(path, clock))
                .transpose()?;
            query(&document, &options, previous.as_ref(), cursor.as_deref())
        }
        ContextCommand::ReadSaved {
            request_path,
            next,
            restart,
        } => read_saved(&request_path, next, restart, clock),
        ContextCommand::Prepare {
            path,
            request_path,
            diagnostics_path,
            options,
            file,
            text_path,
            compare_path,
        } => {
            let text = match (&options.text, text_path) {
                (Some(_), Some(_)) => Err(fail(
                    "invalidInput",
                    "Supply either Text or TextPath, not both",
                )),
                (_, Some(path)) => read_utf8(&path),
                (Some(text), None) => Ok(text.clone()),
                (None, None) => Ok(String::new()),
            };
            let text = match text {
                Ok(text) => text,
                Err(error) => {
                    prepare_failure(&request_path, &diagnostics_path, error, clock)?;
                    return Err(fail("invalidInput", "Query preparation failed"));
                }
            };
            let value = options.value(&json!(file), Some(&text));
            let result = prepare(
                &path,
                compare_path.as_deref(),
                &request_path,
                &diagnostics_path,
                &value,
                clock,
            )?;
            Ok(serde_json::to_string(&result)?)
        }
        ContextCommand::Diagnostics {
            path,
            record,
            count,
        } => {
            if let Some(record) = record {
                if path.is_dir() {
                    return Err(fail("invalidInput", "Record requires one diagnostics file"));
                }
                let count = count.unwrap_or(NonZeroU32::MIN);
                increment_diagnostic(&path, &record, count, clock)?;
            }
            let value = read_diagnostics(&path, clock)?;
            Ok(serde_json::to_string(&value)?)
        }
    }
}

/// Rejects empty explicit selectors instead of silently widening a query.
fn parse_files(text: &str) -> Result<Value, AppError> {
    let files: Value = serde_json::from_str(text)?;
    if !files.as_array().is_some_and(|files| !files.is_empty()) {
        return Err(fail(
            "invalidInput",
            "File selectors must be a nonempty JSON array of strings",
        ));
    }
    Ok(files)
}

/// Matches UTF-8 text file readers that consume an optional byte order mark.
fn read_utf8(path: &Path) -> Result<String, AppError> {
    let mut text = fs::read_to_string(path)?;
    if text.starts_with('\u{feff}') {
        text.drain(..'\u{feff}'.len_utf8());
    }
    Ok(text)
}
