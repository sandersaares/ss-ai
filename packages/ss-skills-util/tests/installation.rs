//! Isolated Cargo installation and same-version upgrade of the real utility.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Copies only source inputs into a test-owned workspace, never editing the checkout.
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

fn successful(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cargo_installs_and_rebuilds_changed_source_at_the_same_version() {
    let temporary = tempfile::Builder::new()
        .prefix("skills cargo installation ")
        .tempdir()
        .unwrap();
    let source = temporary.path().join("source");
    let installed = temporary.path().join("configured-install-root");
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
    ] {
        fs::copy(checkout.join(name), source.join(name)).unwrap();
    }
    let package = source.join("packages").join("ss-skills-util");
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
    let scripts = source.join("scripts");
    fs::create_dir_all(scripts.join("lib")).unwrap();
    fs::copy(checkout.join("Install.ps1"), source.join("Install.ps1")).unwrap();
    copy_directory(&checkout.join("scripts").join("lib"), &scripts.join("lib"));
    let mut paths = vec![installed.join("bin")];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(paths).unwrap();
    let install = || {
        Command::new("cargo")
            .args(["install", "--path"])
            .arg(&package)
            .args(["--locked", "--offline", "--quiet"])
            .current_dir(&source)
            .env("CARGO_INSTALL_ROOT", &installed)
            .env("CARGO_TARGET_DIR", temporary.path().join("build"))
            // Match setup's host optimization so alternating callers reuse compiled dependencies.
            .env("CARGO_ENCODED_RUSTFLAGS", "-C\u{1f}target-cpu=native")
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let invoke = |argument| {
        let output = Command::new("ss-skills-util")
            .arg(argument)
            .env("PATH", &path)
            .current_dir(temporary.path())
            .output()
            .unwrap();
        successful(&output);
        String::from_utf8(output.stdout).unwrap()
    };
    // Run only the real installer function; full machine configuration remains out of scope.
    // Capture errors without host formatting so wrapping cannot change the assertions.
    let setup = |path: &OsStr| {
        Command::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "try {{ . '{}'; Install-SkillsUtility }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}",
                source
                    .join("Install.ps1")
                    .to_str()
                    .unwrap()
                    .replace('\'', "''")
            ))
            .current_dir(temporary.path())
            .env("CARGO_INSTALL_ROOT", &installed)
            .env("CARGO_TARGET_DIR", temporary.path().join("build"))
            .env("CARGO_NET_OFFLINE", "true")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTFLAGS")
            .env("PATH", path)
            .output()
            .unwrap()
    };
    // Cargo, rather than setup, rejects an existing binary owned by another source.
    fs::create_dir_all(installed.join("bin")).unwrap();
    let binary = installed.join("bin").join(if cfg!(windows) {
        "ss-skills-util.exe"
    } else {
        "ss-skills-util"
    });
    fs::write(&binary, "unrelated binary").unwrap();
    let conflict = install();
    assert!(!conflict.status.success());
    assert_eq!(fs::read(&binary).unwrap(), b"unrelated binary");
    fs::remove_file(&binary).unwrap();
    successful(&install());
    successful(&setup(&path));
    let version = invoke("--version");
    assert!(version.starts_with("ss-skills-util "));
    let original_help = invoke("--help");
    let old_directory = temporary.path().join("old installation");
    fs::create_dir_all(&old_directory).unwrap();
    fs::copy(&binary, old_directory.join(binary.file_name().unwrap())).unwrap();
    let command_path = package.join("src").join("command.rs");
    let original = fs::read_to_string(&command_path).unwrap();
    let changed = original.replace(
        "Shared native operations for ss-ai skills",
        "Test-owned replacement from changed local source",
    );
    assert_ne!(changed, original);
    fs::write(&command_path, changed).unwrap();
    successful(&install());
    assert_eq!(invoke("--version"), version);
    let replaced_help = invoke("--help");
    assert_ne!(original_help, replaced_help);
    assert!(replaced_help.contains("Test-owned replacement from changed local source"));
    for include_new in [false, true] {
        let mut shadowed = vec![old_directory.clone()];
        if include_new {
            shadowed.push(installed.join("bin"));
        }
        shadowed.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let result = setup(&std::env::join_paths(shadowed).unwrap());
        assert!(!result.status.success());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains("does not match Cargo's artifact"), "{error}");
        assert!(error.contains("before older copies"), "{error}");
    }
    successful(&setup(&path));
    assert_eq!(
        fs::read(source.join("Cargo.lock")).unwrap(),
        fs::read(checkout.join("Cargo.lock")).unwrap()
    );
}
