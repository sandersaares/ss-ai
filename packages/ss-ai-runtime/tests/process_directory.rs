//! Child-only working directory and environment for process invocations.

use std::env;
#[cfg(windows)]
use std::fs;
#[cfg(windows)]
use std::io;
#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
use std::process::Command;

use ss_ai_runtime::{NativeProcess, Process};
use tempfile::Builder;

/// Child-only overrides do not change the invoking process.
#[test]
#[cfg_attr(miri, ignore = "Miri does not support launching child processes")]
fn execute_in_keeps_directory_and_path_in_the_child() {
    let original = env::current_dir().unwrap();
    let workspace = Builder::new()
        .prefix(".process-test-")
        .tempdir_in(&original)
        .unwrap();
    let executable = env::current_exe().unwrap();
    let process = NativeProcess::new(&original);
    let arguments = ["--exact", "execute_in_child", "--ignored", "--nocapture"].map(str::to_owned);
    let output = process
        .execute_in(
            executable.to_str().unwrap(),
            &arguments,
            &[],
            workspace.path(),
            Some(workspace.path().as_os_str()),
        )
        .unwrap();
    assert!(
        output.success,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(&format!("directory={}", workspace.path().display())));
    assert!(text.contains(&format!("path={}", workspace.path().display())));
    assert!(text.contains("1 passed"));
    assert_eq!(env::current_dir().unwrap(), original);

    let process = NativeProcess::new(workspace.path());
    let output = process
        .execute_with_path(
            executable.to_str().unwrap(),
            &arguments,
            &[],
            workspace.path().as_os_str(),
        )
        .unwrap();
    assert!(
        output.success,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(&format!("directory={}", workspace.path().display())));
    assert!(text.contains(&format!("path={}", workspace.path().display())));
}

/// Reports child state to the parent test without changing global parent state.
#[test]
#[ignore = "launched by execute_in_keeps_directory_and_path_in_the_child"]
fn execute_in_child() {
    println!("directory={}", env::current_dir().unwrap().display());
    println!("path={}", env::var_os("PATH").unwrap().to_string_lossy());
    #[cfg(windows)]
    println!("executable={}", env::current_exe().unwrap().display());
}

/// A separate parent process supplies an inherited PATH without changing this process.
#[cfg(windows)]
#[test]
#[cfg_attr(miri, ignore = "Miri does not support launching child processes")]
fn windows_name_lookup_uses_effective_child_path() {
    let workspace = Builder::new()
        .prefix(".process-path-test-")
        .tempdir_in(env::current_dir().unwrap())
        .unwrap();
    let inherited = workspace.path().join("inherited");
    let selected = workspace.path().join("selected");
    fs::create_dir(&inherited).unwrap();
    fs::create_dir(&selected).unwrap();
    let executable = env::current_exe().unwrap();
    fs::copy(&executable, inherited.join("path-fixture.exe")).unwrap();
    fs::copy(&executable, selected.join("path-fixture.exe")).unwrap();
    fs::copy(&executable, selected.join("child-only-fixture.exe")).unwrap();

    let output = Command::new(executable)
        .args([
            "--exact",
            "windows_path_lookup_child",
            "--ignored",
            "--nocapture",
        ])
        .env("PATH", &inherited)
        .env("SS_AI_TEST_SELECTED_PATH", &selected)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

/// Exercises name lookup against a controlled inherited and child-specific PATH.
#[cfg(windows)]
#[test]
#[ignore = "launched by windows_name_lookup_uses_effective_child_path"]
fn windows_path_lookup_child() {
    let inherited = env::var_os("PATH").unwrap();
    let selected = env::var_os("SS_AI_TEST_SELECTED_PATH").unwrap();
    let directory = env::current_dir().unwrap();
    let process = NativeProcess::new(&directory);
    let arguments = ["--exact", "execute_in_child", "--ignored", "--nocapture"].map(str::to_owned);

    let inherited_output = process.execute("path-fixture", &arguments, &[]).unwrap();
    let inherited_in = process
        .execute_in("path-fixture", &arguments, &[], &directory, None)
        .unwrap();
    let selected_in = process
        .execute_in("path-fixture", &arguments, &[], &directory, Some(&selected))
        .unwrap();
    let selected_with_path = process
        .execute_with_path("child-only-fixture", &arguments, &[], &selected)
        .unwrap();
    let explicit = Path::new(&inherited).join("path-fixture.exe");
    let explicit_output = process
        .execute_with_path(explicit.to_str().unwrap(), &arguments, &[], &selected)
        .unwrap();

    for (output, executable) in [
        (inherited_output, explicit.clone()),
        (inherited_in, explicit.clone()),
        (selected_in, Path::new(&selected).join("path-fixture.exe")),
        (
            selected_with_path,
            Path::new(&selected).join("child-only-fixture.exe"),
        ),
        (explicit_output, explicit),
    ] {
        assert!(
            output.success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("1 passed"), "{text}");
        assert!(
            text.contains(&format!("executable={}", executable.display())),
            "{text}"
        );
    }

    assert_eq!(
        process
            .execute("child-only-fixture", &arguments, &[])
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}
