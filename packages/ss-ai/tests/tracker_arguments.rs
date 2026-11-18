//! Tracker command parsing must not contact a provider.

use std::ffi::OsString;
use std::io;

use ohno::AppError;
use ss_ai::{Http, HttpResponse, Process, ProcessOutput, SystemClock, run};

/// Any external operation is unexpected while the command rejects syntax.
struct Offline;
impl Process for Offline {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        panic!("unexpected process")
    }
}
impl Http for Offline {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("unexpected HTTP request")
    }
}

#[test]
fn tracker_help_exposes_named_compatibility_inputs() {
    let args: Vec<OsString> = ["ss-ai", "review", "publish-tracker-items", "--help"]
        .into_iter()
        .map(Into::into)
        .collect();
    let help = run(&args, &Offline, &Offline, &SystemClock::default()).unwrap();
    for flag in [
        "--url",
        "--platform",
        "--organization",
        "--project",
        "--repository",
        "--approved-findings-path",
        "--context-path",
        "--diagnostics-path",
        "--output-directory",
        "--work-item-type",
        "--area-path",
        "--description-field",
        "--parent-work-item-id",
        "--accept-stored-rewrite",
        "--accept-work-item-id",
        "--accept-expected-body-hash",
        "--accept-stored-body-hash",
        "--tenant",
        "--now-utc",
    ] {
        assert!(help.contains(flag), "missing {flag}: {help}");
    }
}

#[test]
fn malformed_command_is_rejected_without_provider_access() {
    let args: Vec<OsString> = [
        "ss-ai",
        "review",
        "publish-tracker-items",
        "--parent-work-item-id",
        "not-an-integer",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    run(&args, &Offline, &Offline, &SystemClock::default()).unwrap_err();
}
