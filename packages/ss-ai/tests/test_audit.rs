//! Executable contract for the provider-independent test-audit namespace.

use std::process::Command;

#[test]
fn help_lists_the_native_workflow_without_provider_authentication() {
    let output = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .arg("test-audit")
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    for command in [
        "initialize",
        "search",
        "next-batch",
        "record-batch",
        "report",
    ] {
        assert!(stdout.contains(command), "{command}");
    }
    assert!(stdout.contains("test-audit <COMMAND>"));
}
