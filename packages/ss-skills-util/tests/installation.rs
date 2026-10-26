//! Explicit release installation preparation and compiler-free evidence checks.
#![allow(
    clippy::indexing_slicing,
    reason = "Integration evidence uses bounded, asserted fixture shapes"
)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Retains immutable inputs and outputs after the preparation process exits.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: PathBuf,
    sha256: String,
}

impl Artifact {
    fn new(path: PathBuf) -> Self {
        assert!(path.is_absolute());
        Self {
            sha256: format!("{:X}", Sha256::digest(fs::read(&path).unwrap())),
            path,
        }
    }

    fn verify(&self, directory: &Path) {
        assert!(self.path.is_absolute() && self.path.starts_with(directory));
        assert!(
            !self
                .path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        );
        assert_eq!(Self::new(self.path.clone()).sha256, self.sha256);
    }
}

/// Preserves actual command outcomes and Cargo freshness, not success-shaped flags.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Outcome {
    exit_code: i32,
    stdout: String,
    stderr: String,
    cargo_artifacts: Vec<Value>,
}

impl Outcome {
    fn new(output: Output, messages: &str) -> Self {
        let cargo_artifacts = messages
            .lines()
            .filter_map(|line| {
                let message: Value = serde_json::from_str(line).unwrap();
                (message["reason"] == "compiler-artifact").then_some(message)
            })
            .collect();
        Self {
            exit_code: output.status.code().unwrap(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
            cargo_artifacts,
        }
    }

    fn verify(&self, success: bool) {
        assert_eq!(
            self.exit_code == 0,
            success,
            "{}\n{}",
            self.stdout,
            self.stderr
        );
    }

    /// A changed utility must compile; subsequent setup calls must reuse every artifact.
    fn verify_freshness(&self, changed: bool) {
        assert!(!self.cargo_artifacts.is_empty());
        let mut utility = false;
        for artifact in &self.cargo_artifacts {
            let is_utility = artifact["target"]["name"] == "ss-skills-util"
                || artifact["target"]["name"] == "ss_skills_util";
            if is_utility {
                utility = true;
                assert_eq!(artifact["profile"]["opt_level"], "3");
            }
            assert_eq!(
                artifact["fresh"],
                !(changed && is_utility),
                "Unexpected compilation/reuse: {artifact}"
            );
        }
        assert!(utility);
    }
}

/// Binds the verified lifecycle to separately owned baseline and changed-source bytes.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Evidence {
    schema_version: u32,
    status: String,
    artifacts: Vec<Artifact>,
    commands: BTreeMap<String, Outcome>,
    native_flags: String,
    baseline: Observation,
    changed: Observation,
}

impl Evidence {
    /// Both producer and runtime consumer enforce the same outcome and source contracts.
    fn verify(&self, directory: &Path) {
        assert_eq!(self.schema_version, 1);
        assert_eq!(self.status, "complete");
        assert!(self.native_flags.ends_with("-C\u{1f}target-cpu=native"));
        assert_eq!(self.artifacts.len(), 6);
        for artifact in &self.artifacts {
            artifact.verify(directory);
        }
        let [
            baseline,
            changed,
            original_source,
            changed_source,
            original_lock,
            final_lock,
        ] = self.artifacts.as_slice()
        else {
            panic!("Incomplete installation artifacts");
        };
        assert_ne!(baseline.sha256, changed.sha256);
        assert_ne!(original_source.sha256, changed_source.sha256);
        assert_eq!(
            fs::read_to_string(&original_source.path).unwrap(),
            include_str!("../src/command.rs")
        );
        assert_eq!(
            fs::read_to_string(&changed_source.path).unwrap(),
            include_str!("../src/command.rs").replace(
                "Shared native operations for ss-ai skills",
                "Test-owned replacement from changed local source"
            )
        );
        assert_eq!(original_lock.sha256, final_lock.sha256);
        assert_eq!(
            fs::read(&original_lock.path).unwrap(),
            include_bytes!("../../../Cargo.lock")
        );
        assert_eq!(self.commands.len(), 8);
        for name in [
            "conflict",
            "baseline",
            "baselineSetup",
            "changed",
            "stale",
            "shadowed",
            "missing",
            "finalSetup",
        ] {
            self.commands[name].verify(matches!(
                name,
                "baseline" | "baselineSetup" | "changed" | "finalSetup"
            ));
        }
        for name in ["stale", "shadowed"] {
            assert!(
                self.commands[name]
                    .stderr
                    .contains("does not match Cargo's artifact")
            );
            assert!(self.commands[name].stderr.contains("before older copies"));
        }
        assert!(self.commands["missing"].stderr.contains("is not on PATH"));
        self.commands["changed"].verify_freshness(true);
        for name in [
            "baselineSetup",
            "stale",
            "shadowed",
            "missing",
            "finalSetup",
        ] {
            self.commands[name].verify_freshness(false);
        }
        self.baseline.verify(&baseline.path);
        self.changed.verify(&changed.path);
        assert!(self.baseline.version.starts_with("ss-skills-util "));
        assert_eq!(self.baseline.version, self.changed.version);
        assert_ne!(self.baseline.help, self.changed.help);
        assert!(
            self.changed
                .help
                .contains("Test-owned replacement from changed local source")
        );
    }
}

/// Captures executable behavior at each installation boundary and checks it again at runtime.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    version: String,
    help: String,
}

impl Observation {
    fn new(binary: &Path) -> Self {
        Self {
            version: invoke(binary, "--version"),
            help: invoke(binary, "--help"),
        }
    }

    fn verify(&self, binary: &Path) {
        assert_eq!(self.version, invoke(binary, "--version"));
        assert_eq!(self.help, invoke(binary, "--help"));
    }
}

/// Copies only source inputs, keeping all writable compilation state invocation-private.
fn copy_directory(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &target.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}

/// Uses exact executable paths, so the consumer needs neither Cargo nor rustc on PATH.
fn invoke(binary: &Path, argument: &str) -> String {
    let output = Command::new(binary).arg(argument).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Quotes test-owned paths for the real PowerShell installer invocation.
fn quote(path: &Path) -> String {
    path.to_str().unwrap().replace('\'', "''")
}

/// Explicit compiler-bearing integration, invoked only by the canonical preparation phase.
#[test]
#[ignore = "Run by Invoke-Tests.ps1 preparation; ordinary execution validates its retained evidence"]
fn prepare_real_release_installation() {
    let directory = PathBuf::from(
        std::env::var_os("SS_AI_INSTALLATION_PREPARATION_DIRECTORY")
            .expect("Explicit installation preparation directory is required"),
    );
    assert!(directory.is_absolute());
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
    let source = directory.join("source");
    let installed = directory.join("configured-install-root");
    let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    fs::create_dir_all(&source).unwrap();
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "clippy.toml",
        "Install.ps1",
    ] {
        fs::copy(checkout.join(name), source.join(name)).unwrap();
    }
    let package = source.join("packages/ss-skills-util");
    fs::create_dir_all(&package).unwrap();
    for name in ["Cargo.toml", "README.md"] {
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(name),
            package.join(name),
        )
        .unwrap();
    }
    copy_directory(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &package.join("src"),
    );
    copy_directory(&checkout.join("scripts/lib"), &source.join("scripts/lib"));
    // Exclude ambient installed utilities so missing/stale PATH cases cannot hit the user's copy.
    // Preserve cargo/rustup even when an installation shares their directory.
    let tools = directory.join("tools");
    fs::create_dir_all(&tools).unwrap();
    let utility_name = if cfg!(windows) {
        "ss-skills-util.exe"
    } else {
        "ss-skills-util"
    };
    let ambient: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|path| {
            if !path.join(utility_name).exists() {
                return true;
            }
            for tool in ["cargo", "rustup", "rustc", "rustdoc"] {
                let name = if cfg!(windows) {
                    format!("{tool}.exe")
                } else {
                    tool.to_owned()
                };
                if path.join(&name).exists() {
                    fs::copy(path.join(&name), tools.join(&name)).unwrap();
                }
            }
            false
        })
        .collect();
    let path_for = |prefix: &[PathBuf]| {
        std::env::join_paths(prefix.iter().chain([&tools]).chain(ambient.iter())).unwrap()
    };
    let path = path_for(&[installed.join("bin")]);
    // Match the production installer's append/precedence rules, including caller flags.
    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_else(|_| {
        std::env::var("RUSTFLAGS")
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("\u{1f}")
    });
    let native_flags = if flags.is_empty() {
        "-C\u{1f}target-cpu=native".to_owned()
    } else {
        format!("{flags}\u{1f}-C\u{1f}target-cpu=native")
    };
    let install = || {
        let output = Command::new("cargo")
            .args(["install", "--path"])
            .arg(&package)
            .args([
                "--locked",
                "--offline",
                "--message-format=json-render-diagnostics",
            ])
            .current_dir(&source)
            .env("CARGO_INSTALL_ROOT", &installed)
            .env("CARGO_TARGET_DIR", directory.join("build"))
            .env("CARGO_ENCODED_RUSTFLAGS", &native_flags)
            .env("PATH", &path)
            .output()
            .unwrap();
        let messages = String::from_utf8(output.stdout.clone()).unwrap();
        Outcome::new(output, &messages)
    };
    // Instrument only the Cargo message stream. The real installer and Cargo still perform all work.
    let setup = |name: &str, path: &OsStr| {
        let log = directory.join(format!("{name}-cargo.jsonl"));
        let output = Command::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "try {{ . '{}'; $script:realCargo = ${{function:Invoke-SkillsUtilityCargo}}; function Invoke-SkillsUtilityCargo {{ param([string] $UtilityPath) $messages = @(& $script:realCargo -UtilityPath $UtilityPath); $messages | Set-Content -LiteralPath '{}'; $messages }}; Install-SkillsUtility }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}",
                quote(&source.join("Install.ps1")), quote(&log)
            ))
            .current_dir(&directory)
            .env("CARGO_INSTALL_ROOT", &installed)
            .env("CARGO_TARGET_DIR", directory.join("build"))
            .env("CARGO_NET_OFFLINE", "true")
            .env("PATH", path)
            .output().unwrap();
        Outcome::new(
            output,
            &fs::read_to_string(log).expect("Installer must report Cargo artifacts"),
        )
    };
    fs::create_dir_all(installed.join("bin")).unwrap();
    let binary = installed.join("bin").join(utility_name);
    fs::write(&binary, "unrelated binary").unwrap();
    let mut commands = BTreeMap::new();
    let conflict = install();
    conflict.verify(false);
    assert_eq!(fs::read(&binary).unwrap(), b"unrelated binary");
    commands.insert("conflict".to_owned(), conflict);
    fs::remove_file(&binary).unwrap();
    let baseline = install();
    baseline.verify(true);
    commands.insert("baseline".to_owned(), baseline);
    let baseline_setup = setup("baselineSetup", &path);
    baseline_setup.verify(true);
    commands.insert("baselineSetup".to_owned(), baseline_setup);
    let old_directory = directory.join("old installation");
    fs::create_dir_all(&old_directory).unwrap();
    let old_binary = old_directory.join(utility_name);
    fs::copy(&binary, &old_binary).unwrap();
    let baseline_observation = Observation::new(&old_binary);
    let command_path = package.join("src/command.rs");
    let original_path = directory.join("baseline-command.rs");
    fs::copy(&command_path, &original_path).unwrap();
    let original = fs::read_to_string(&command_path).unwrap();
    let changed = original.replace(
        "Shared native operations for ss-ai skills",
        "Test-owned replacement from changed local source",
    );
    assert_ne!(changed, original);
    fs::write(&command_path, changed).unwrap();
    let changed = install();
    changed.verify(true);
    commands.insert("changed".to_owned(), changed);
    let changed_observation = Observation::new(&binary);
    for (name, prefix) in [
        ("stale", vec![old_directory.clone()]),
        ("shadowed", vec![old_directory, installed.join("bin")]),
        ("missing", vec![]),
        ("finalSetup", vec![installed.join("bin")]),
    ] {
        commands.insert(name.to_owned(), setup(name, &path_for(&prefix)));
    }
    let evidence = Evidence {
        schema_version: 1,
        status: "complete".to_owned(),
        artifacts: [
            old_binary,
            binary,
            original_path,
            command_path,
            checkout.join("Cargo.lock"),
            source.join("Cargo.lock"),
        ]
        .into_iter()
        .map(Artifact::new)
        .collect(),
        commands,
        native_flags,
        baseline: baseline_observation,
        changed: changed_observation,
    };
    // Retain an owned baseline lockfile rather than referencing mutable checkout files.
    let mut evidence = evidence;
    let lock = directory.join("baseline-Cargo.lock");
    fs::copy(&evidence.artifacts[4].path, &lock).unwrap();
    evidence.artifacts[4] = Artifact::new(lock);
    evidence.verify(&directory);
    let temporary = directory.join("evidence.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    fs::rename(temporary, directory.join("evidence.json")).unwrap();
}

#[test]
fn cargo_installs_and_rebuilds_changed_source_at_the_same_version() {
    let record = PathBuf::from(
        std::env::var_os("SS_AI_RUST_TEST_PREREQUISITES")
            .expect("Run scripts/Invoke-Tests.ps1 -Suite Rust preparation first"),
    );
    assert!(record.is_absolute());
    let record: Value = serde_json::from_slice(&fs::read(record).unwrap()).unwrap();
    assert_eq!(record["schemaVersion"], 1);
    assert!(record["status"] == "complete" || record["status"] == "providerFailed");
    let evidence = &record["installation"];
    let path = Path::new(evidence["path"].as_str().unwrap());
    let owned = Path::new(record["ownedDirectory"].as_str().unwrap()).join("installation");
    assert_eq!(path, owned.join("evidence.json"));
    assert_eq!(Artifact::new(path.to_owned()).sha256, evidence["sha256"]);
    let evidence: Evidence = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    evidence.verify(&owned);
}
