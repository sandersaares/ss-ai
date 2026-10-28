//! Local Cargo inventory command and its file publication boundary.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use ohno::AppError;
use ss_ai_cargo::{Inventory, generate_inventory};
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail};

/// Parses the local commands independently of provider initialization.
#[derive(Debug, Parser)]
#[command(name = "ss-ai cargo", about = "Inspect Cargo workspaces")]
struct CargoCli {
    #[command(subcommand)]
    command: CargoCommand,
}

/// The workspace inventory is a read-only scan with optional atomic publication.
#[derive(Debug, Subcommand)]
enum CargoCommand {
    /// Inventory dependency declarations in a Cargo workspace.
    Inventory {
        #[arg(long, default_value = "./Cargo.toml")]
        workspace_manifest: PathBuf,
        #[arg(long)]
        output_path: Option<PathBuf>,
    },
}

/// Runs the local command without constructing a provider or network client.
pub fn run<I, T>(arguments: I, clock: &dyn Clock) -> Result<String, AppError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli =
        match CargoCli::try_parse_from(crate::command::namespace_arguments(arguments, "cargo")) {
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
        CargoCommand::Inventory {
            workspace_manifest,
            output_path,
        } => {
            let inventory = generate_inventory(&workspace_manifest, clock.now())
                .map_err(|error| caused_by("invalidInput", "Cannot inventory workspace", error))?;
            let bytes = serde_json::to_vec_pretty(&inventory)?;
            let Some(output_path) = output_path else {
                return Ok(String::from_utf8(bytes)?);
            };
            let output_path = absolute_destination(&output_path)?;
            let output_text = output_path
                .to_str()
                .ok_or_else(|| fail("invalidInput", "Output path must be UTF-8"))?;
            let transaction = FileTransaction::open(&output_path, clock)?;
            reject_manifest_alias(&inventory, &output_path)?;
            transaction.write_bytes(&bytes)?;
            Ok(serde_json::json!({
                "outputPath": output_text,
                "memberCount": inventory.member_manifests.len(),
                "dependencyCount": inventory.dependencies.len(),
            })
            .to_string())
        }
    }
}

/// Resolves the containing directory so the reported destination is absolute.
fn absolute_destination(path: &Path) -> Result<PathBuf, AppError> {
    let name = path
        .file_name()
        .ok_or_else(|| fail("invalidInput", "Output path requires a file name"))?;
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute
        .parent()
        .ok_or_else(|| fail("invalidInput", "Output path requires a parent directory"))?;
    fs::create_dir_all(parent)?;
    Ok(fs::canonicalize(parent)?.join(name))
}

/// Protects every scanned manifest, including symlink and hard-link aliases.
fn reject_manifest_alias(inventory: &Inventory, output: &Path) -> Result<(), AppError> {
    if !output.try_exists()? {
        return Ok(());
    }
    let root = Path::new(&inventory.workspace_root);
    for path in std::iter::once(&inventory.workspace_manifest.path)
        .chain(inventory.member_manifests.iter().map(|member| &member.path))
    {
        if same_file::is_same_file(output, root.join(path))? {
            return Err(fail(
                "invalidInput",
                "Output path aliases a scanned manifest",
            ));
        }
    }
    Ok(())
}
