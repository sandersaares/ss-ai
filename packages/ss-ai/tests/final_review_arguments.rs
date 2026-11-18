//! Command parsing checks that do not access a provider or the filesystem.

use std::ffi::OsString;
use std::io;

use ohno::AppError;
use ss_ai::{Http, HttpResponse, Process, ProcessOutput, SystemClock, run};

/// Refuses any external operation so parsing cannot accidentally publish.
struct Offline;
impl Process for Offline {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        Err(io::Error::other("unexpected process"))
    }
}
impl Http for Offline {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        Err(io::Error::other("unexpected HTTP request").into())
    }
}

#[test]
fn final_review_help_exposes_compatibility_arguments() {
    let args: Vec<OsString> = ["ss-ai", "review", "publish-final-review", "--help"]
        .into_iter()
        .map(Into::into)
        .collect();
    let help = run(&args, &Offline, &Offline, &SystemClock::default()).unwrap();
    for flag in [
        "--url",
        "--approved-findings-path",
        "--context-path",
        "--diagnostics-path",
        "--repository-path",
        "--output-directory",
        "--discard-pending-review",
        "--publish-to-closed-pull-request",
        "--merge-approval-path",
        "--tenant",
        "--now-utc",
    ] {
        assert!(help.contains(flag), "missing {flag}: {help}");
    }
}

#[test]
fn merge_approval_cannot_be_granted_by_a_cli_switch() {
    let args: Vec<OsString> = ["ss-ai", "review", "publish-final-review", "--approve-merge"]
        .into_iter()
        .map(Into::into)
        .collect();
    run(&args, &Offline, &Offline, &SystemClock::default()).unwrap_err();
}
