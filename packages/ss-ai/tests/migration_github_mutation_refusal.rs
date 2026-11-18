//! Preserves uncertain GraphQL mutation outcomes without repeating provider writes.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai::{Clock, Http, HttpResponse, Process, ProcessOutput, run};
use tempfile::Builder;

/// Returns one scripted mutation response after the read-only tracker preflight.
struct Provider {
    success: bool,
    body: Vec<u8>,
    calls: RefCell<Vec<String>>,
}

impl Process for Provider {
    fn execute(&self, program: &str, _: &[String], input: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "gh");
        let request: Value = serde_json::from_slice(input).unwrap();
        let query = request.get("query").unwrap().as_str().unwrap();
        let mut calls = self.calls.borrow_mut();
        if query.contains("createIssue(") {
            assert_eq!(calls.as_slice(), ["preflight"]);
            calls.push("create".into());
            Ok(ProcessOutput::new(self.success, self.body.clone(), vec![]))
        } else {
            assert!(calls.is_empty(), "unexpected repeated preflight");
            calls.push("preflight".into());
            Ok(ProcessOutput::new(
                true,
                br#"HTTP/2 200

{"data":{"repository":{"id":"repo","nameWithOwner":"acme/tracker","hasIssuesEnabled":true,"isArchived":false,"viewerPermission":"WRITE"}}}"#.to_vec(),
                vec![],
            ))
        }
    }
}

impl Http for Provider {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        panic!("GitHub uses the CLI")
    }
}

/// Makes replay attempts observable without reaching a provider.
struct Offline(Cell<bool>);

impl Process for Offline {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        self.0.set(true);
        Err(io::Error::other("unexpected provider"))
    }
}

impl Http for Offline {
    fn send(&self, _: &str, _: &str, _: &str, _: Option<&[u8]>) -> Result<HttpResponse, AppError> {
        self.0.set(true);
        Err(io::Error::other("unexpected provider").into())
    }
}

/// Prevents real time or a retry from hiding a duplicated mutation.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected retry or contention")
    }
}

#[test]
fn incomplete_mutation_evidence_remains_ambiguous_without_replay() {
    for (success, body) in [
        (
            false,
            r#"{"data":{"createIssue":{"id":"created"}},"errors":[{"message":"sibling failed"}]}"#,
        ),
        (
            false,
            r#"{"data":null,"errors":[{"message":"execution failed"}]}"#,
        ),
        (
            true,
            r#"{"data":{"createIssue":{"id":"created"}},"errors":[{"message":"sibling failed"}]}"#,
        ),
        (false, ""),
        (false, "{"),
        (
            false,
            r#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#,
        ),
        (
            true,
            r#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#,
        ),
        (
            false,
            r#"{"errors":[{"message":"Field does not exist on type Query"}]}"#,
        ),
    ] {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let directory = Builder::new()
            .prefix(".github-mutation-refusal-")
            .tempdir_in(root)
            .unwrap();
        let approved = directory.path().join("approved.json");
        let diagnostics = directory.path().join("diagnostics.json");
        let output = directory.path().join("out");
        fs::write(
            &approved,
            serde_json::to_vec(&json!({
                "schemaVersion":2,"target":null,"reviewedSourceCommit":null,
                "reviewedMergeBaseCommit":null,"reviewedContextHash":null,
                "findings":[{"schemaVersion":1,"id":"F","sourceFindingIds":["F"],
                    "origin":"reviewer","title":"A title","description":"Description",
                    "reasoning":"Reason","consequence":"Impact","recommendedAction":"Fix",
                    "anchor":{"kind":"file","path":"src/lib.rs"}}]
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(&diagnostics, b"{}").unwrap();
        let mut args: Vec<OsString> = [
            "ss-ai",
            "review",
            "publish-tracker-items",
            "--url",
            "https://github.com/acme/tracker",
            "--now-utc",
            "2026-01-01T00:00:00Z",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        for (flag, path) in [
            ("--approved-findings-path", approved),
            ("--diagnostics-path", diagnostics),
            ("--output-directory", output.clone()),
        ] {
            args.push(flag.into());
            args.push(path.into_os_string());
        }
        let provider = Provider {
            success,
            body: body.as_bytes().to_vec(),
            calls: RefCell::new(Vec::new()),
        };
        let result: Value =
            serde_json::from_str(&run(&args, &provider, &provider, &NoWait).unwrap()).unwrap();
        assert_eq!(
            result.get("ambiguousOperationCount"),
            Some(&json!(1)),
            "{body}"
        );
        let journal: Value = serde_json::from_slice(
            &fs::read(output.join("tracker-publication-result.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            journal.pointer("/operations/0/outcome"),
            Some(&json!("ambiguous")),
            "{body}"
        );
        assert_eq!(*provider.calls.borrow(), ["preflight", "create"]);
        let offline = Offline(Cell::new(false));
        run(&args, &offline, &offline, &NoWait).unwrap_err();
        assert!(!offline.0.get(), "ambiguous mutation must not be replayed");
    }
}
