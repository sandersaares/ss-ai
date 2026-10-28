//! Executable contract for local, provider-independent Cargo inventory.
#![allow(
    clippy::indexing_slicing,
    reason = "Test fixtures assert known JSON fields"
)]

#[cfg(unix)]
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// Runs the installed command with no usable provider executable.
fn inventory(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .current_dir(root)
        .env("PATH", root)
        .args(["cargo", "inventory"])
        .args(arguments)
        .output()
        .unwrap()
}

/// Creates a workspace whose manifest bytes can be checked after publication.
fn workspace() -> TempDir {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.path().join("member")).unwrap();
    fs::write(
        root.path().join("member/Cargo.toml"),
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1.0\"\n",
    )
    .unwrap();
    root
}

#[test]
fn executable_reports_the_repository_name() {
    let executable = env!("CARGO_BIN_EXE_ss-ai");
    let version = Command::new(executable).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(
        String::from_utf8(version.stdout)
            .unwrap()
            .starts_with("ss-ai ")
    );

    let help = Command::new(executable).arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("ss-ai cargo --help"));
    assert!(help.contains("ss-ai git --help"));
}

#[test]
fn stdout_contains_inventory_without_provider_authentication() {
    let root = workspace();
    let output = inventory(root.path(), &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["schemaVersion"], 1);
    assert_eq!(record["memberManifests"].as_array().unwrap().len(), 1);
    assert_eq!(record["dependencies"].as_array().unwrap().len(), 1);
    assert!(record["scannedAt"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn output_is_atomically_replaced_and_summary_is_bounded() {
    let root = workspace();
    let output = root.path().join("reports/inventory.json");
    let result = inventory(
        root.path(),
        &[
            "--workspace-manifest",
            "Cargo.toml",
            "--output-path",
            "reports/inventory.json",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(summary["outputPath"], output.to_str().unwrap());
    assert_eq!(summary["memberCount"], 1);
    assert_eq!(summary["dependencyCount"], 1);
    let original = fs::read(&output).unwrap();
    let second = inventory(root.path(), &["--output-path", "reports/inventory.json"]);
    assert!(second.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&original).unwrap()["schemaVersion"],
        1
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&output).unwrap()).unwrap()["schemaVersion"],
        1
    );
    assert!(root.path().join("reports/inventory.json.lock").exists());
}

#[test]
fn manifests_and_hard_link_aliases_cannot_be_overwritten() {
    let root = workspace();
    let manifest = root.path().join("member/Cargo.toml");
    let original = fs::read(&manifest).unwrap();
    fs::hard_link(&manifest, root.path().join("alias")).unwrap();
    for path in ["member/Cargo.toml", "alias"] {
        let output = inventory(root.path(), &["--output-path", path]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(fs::read(&manifest).unwrap(), original);
    }
}

#[cfg(unix)]
#[test]
fn symlink_aliases_cannot_overwrite_manifests() {
    let root = workspace();
    symlink("Cargo.toml", root.path().join("alias")).unwrap();
    let original = fs::read(root.path().join("Cargo.toml")).unwrap();
    let output = inventory(root.path(), &["--output-path", "alias"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(root.path().join("Cargo.toml")).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn manifest_alias_checks_preserve_literal_backslashes() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = ['member\\name']\n",
    )
    .unwrap();
    fs::create_dir_all(root.path().join("member\\name")).unwrap();
    let manifest = root.path().join("member\\name/Cargo.toml");
    let original = b"[package]\nname = 'member'\nversion = '0.1.0'\n";
    fs::write(&manifest, original).unwrap();
    let output = inventory(root.path(), &["--output-path", "member\\name/Cargo.toml"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(manifest).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn non_utf8_output_path_fails_before_publication() {
    let root = workspace();
    let path = OsString::from_vec(b"invalid-\xff.json".to_vec());
    let output = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .current_dir(root.path())
        .args(["cargo", "inventory", "--output-path"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!root.path().join(path).exists());
}

#[test]
fn invalid_input_fails_without_success_json() {
    let root = workspace();
    let output = inventory(root.path(), &["--workspace-manifest", "missing.toml"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

mod timestamp {
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use ss_ai::{Http, HttpResponse, Process, ProcessOutput};
    use ss_ai_runtime::Clock;
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

    /// Rejects provider operations during local command routing.
    struct NoProvider;

    impl Process for NoProvider {
        fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
            panic!("Local command help must not launch a provider")
        }
    }

    impl Http for NoProvider {
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            panic!("Local command help must not contact a provider")
        }
    }

    #[test]
    fn scan_timestamp_comes_from_the_injected_clock() {
        let directory = TempDir::new().unwrap();
        let manifest = directory.path().join("Cargo.toml");
        fs::write(&manifest, "[workspace]\nmembers = []\n").unwrap();
        let output = ss_ai::run_cargo(
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

    #[test]
    fn borrowed_arguments_keep_the_original_executable_name_in_help() {
        let arguments = [
            OsString::from("custom-executable"),
            OsString::from("cargo"),
            OsString::from("inventory"),
            OsString::from("--help"),
        ];
        let local = arguments
            .first()
            .into_iter()
            .chain(arguments.iter().skip(2));
        let output = ss_ai::run_cargo(local, &FixedClock).unwrap();
        assert!(output.contains("custom-executable inventory"));
    }

    #[test]
    fn both_dispatch_entry_points_preserve_the_executable_name() {
        for (namespace, command) in [("cargo", "inventory"), ("git", "diff")] {
            let arguments = ["custom-executable", namespace, command, "--help"].map(OsString::from);
            let ordinary = ss_ai::run(&arguments, &NoProvider, &NoProvider, &FixedClock).unwrap();
            let with_directory = ss_ai::run_in(
                &arguments,
                &NoProvider,
                &NoProvider,
                &FixedClock,
                Path::new("unused"),
            )
            .unwrap();
            assert!(ordinary.contains(&format!("custom-executable {command}")));
            assert_eq!(ordinary, with_directory);
        }
    }
}
