//! Local Cargo inventory command and its file publication boundary.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use ohno::AppError;
use ss_skills_cargo::{Inventory, generate_inventory};
use ss_skills_runtime::{Clock, FileTransaction, caused_by, fail};

/// Parses the local commands independently of provider initialization.
#[derive(Debug, Parser)]
#[command(name = "ss-skills-util cargo", about = "Inspect Cargo workspaces")]
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
pub fn run(arguments: &[OsString], clock: &dyn Clock) -> Result<String, AppError> {
    let cli = match CargoCli::try_parse_from(arguments) {
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use ss_skills_runtime::Clock;
    use tempfile::TempDir;

    /// Supplies a deterministic scan time without provider or wall-clock access.
    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            "2026-10-28T07:28:14Z".parse().unwrap()
        }

        fn wait(&self, _: Duration) -> Result<(), AppError> {
            panic!("A local scan must not wait")
        }
    }

    #[test]
    fn scan_timestamp_comes_from_the_injected_clock() {
        let directory = TempDir::new().unwrap();
        let manifest = directory.path().join("Cargo.toml");
        fs::write(&manifest, "[workspace]\nmembers = []\n").unwrap();
        let output = super::run(
            &[
                OsString::from("cargo"),
                OsString::from("inventory"),
                OsString::from("--workspace-manifest"),
                manifest.into_os_string(),
            ],
            &FixedClock,
        )
        .unwrap();
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            result.get("scannedAt").and_then(serde_json::Value::as_str),
            Some("2026-10-28T07:28:14Z")
        );
    }
}
