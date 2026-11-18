//! Checks subprocess failure reporting when a child refuses input immediately.

use std::env;
use std::io::{Write, stdout};

use ss_ai_runtime::{NativeProcess, Process};

/// Exits without consuming stdin, independent of how the parent handles pipes.
#[test]
#[ignore = "launched by the parent process test"]
fn refusal_child() {
    panic!("recorded refusal");
}

#[test]
#[cfg_attr(miri, ignore = "Miri does not support launching subprocesses")]
fn early_failed_child_preserves_stderr_while_input_is_pending() {
    let executable = env::current_exe().unwrap();
    let directory = env::current_dir().unwrap();
    let process = NativeProcess::new(&directory);
    let arguments = ["--exact", "refusal_child", "--ignored", "--nocapture"].map(str::to_owned);
    // The child exits without reading, so the input writer must not hide refusal.
    let input = vec![b'x'; 1024 * 1024];
    let output = process
        .execute(executable.to_str().unwrap(), &arguments, &input)
        .unwrap();
    assert!(!output.success);
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("recorded refusal")
    );
}

/// Emits non-UTF-8 output without involving input or provider services.
#[test]
#[ignore = "launched by the parent process test"]
fn binary_output_child() {
    stdout().write_all(b"\xff\x80binary").unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "Miri does not support launching subprocesses")]
fn successful_child_stdout_remains_undecoded_bytes() {
    let executable = env::current_exe().unwrap();
    let directory = env::current_dir().unwrap();
    let process = NativeProcess::new(&directory);
    let arguments =
        ["--exact", "binary_output_child", "--ignored", "--nocapture"].map(str::to_owned);
    let output = process
        .execute(executable.to_str().unwrap(), &arguments, &[])
        .unwrap();
    assert!(output.success);
    assert!(
        output
            .stdout
            .windows(8)
            .any(|bytes| bytes == b"\xff\x80binary")
    );
}
