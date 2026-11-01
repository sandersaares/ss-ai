//! Exercises approved finding freshness with recorded Git evidence.
#![allow(
    clippy::indexing_slicing,
    reason = "The test fixtures have known JSON shapes"
)]

use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::approved;
use ss_ai_runtime::{Clock, Process, ProcessOutput};

/// Keeps file transaction contention deterministic for isolated fixtures.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("approved finding fixture must not wait")
    }
}

/// Supplies a reviewed diff without executing Git or accessing a provider.
struct RecordedGit;

impl Process for RecordedGit {
    fn execute(&self, _: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        let output = if arguments.iter().any(|arg| arg == "cat-file") {
            "commit\n"
        } else if arguments.iter().any(|arg| arg == "diff") {
            concat!(
                "diff --git a/src/lib.rs b/src/lib.rs\n",
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n",
                "@@ -10,2 +10,2 @@\n-old\n+new\n context\n",
                "diff --git a/src/old.rs b/src/new.rs\n",
                "similarity index 100%\nrename from src/old.rs\nrename to src/new.rs\n",
                "diff --git a/old/gone.txt b/old/gone.txt\n",
                "deleted file mode 100644\n--- a/old/gone.txt\n+++ /dev/null\n",
                "@@ -1 +0,0 @@\n-deleted\n"
            )
        } else {
            ""
        };
        Ok(ProcessOutput::new(
            true,
            output.as_bytes().to_vec(),
            Vec::new(),
        ))
    }
}

/// Produces an invalid diff or a process failure without running a child.
struct InvalidGit {
    process_failure: bool,
}

/// Detects any diff access before malformed finding input is rejected.
struct UnusedGit;

impl Process for UnusedGit {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        panic!("malformed finding must be rejected before Git")
    }
}

impl Process for InvalidGit {
    fn execute(&self, _: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        if self.process_failure {
            return Err(io::Error::other("recorded Git failure"));
        }
        let output = if arguments.iter().any(|arg| arg == "cat-file") {
            "commit\n"
        } else {
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ malformed @@\n"
        };
        Ok(ProcessOutput::new(
            true,
            output.as_bytes().to_vec(),
            Vec::new(),
        ))
    }
}

/// Reserves only this test's files in the checkout build-output directory.
struct Files {
    reservation: PathBuf,
    stem: String,
    owns_paths: bool,
}

impl Files {
    fn new() -> Self {
        let package = Path::new(env!("CARGO_MANIFEST_DIR"));
        std::env::set_current_dir(package.parent().unwrap().parent().unwrap()).unwrap();
        fs::create_dir_all("target").unwrap();
        // A bounded search handles process ID reuse without hanging.
        for id in 0..1024 {
            let stem = format!("target/migration-approved-{}-{id}", std::process::id());
            let reservation = PathBuf::from(format!("{stem}.reservation"));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&reservation)
            {
                Ok(_) => {
                    let mut files = Self {
                        reservation,
                        stem,
                        owns_paths: false,
                    };
                    if files.paths().iter().all(|path| !path.exists()) {
                        files.owns_paths = true;
                        return files;
                    }
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot reserve approved fixture: {error}"),
            }
        }
        panic!("cannot allocate approved fixture");
    }

    fn path(&self, name: &str) -> PathBuf {
        PathBuf::from(format!("{}-{name}.json", self.stem))
    }

    fn put(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }

    fn paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for name in ["findings", "decisions", "capture", "approved"] {
            let path = self.path(name);
            for suffix in ["", ".lock", ".authorship.json", ".authorship.json.lock"] {
                let mut candidate = path.as_os_str().to_os_string();
                candidate.push(suffix);
                paths.push(PathBuf::from(candidate));
            }
        }
        paths
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        if self.owns_paths {
            for path in self.paths() {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) if std::thread::panicking() => {
                        eprintln!("Cannot clean approved fixture: {error}");
                    }
                    Err(error) => panic!("Cannot clean approved fixture: {error}"),
                }
            }
        }
        fs::remove_file(&self.reservation).unwrap();
    }
}

/// Constructs valid consolidated findings with independently varied anchors.
#[expect(
    clippy::needless_pass_by_value,
    reason = "The fixtures construct anchor values inline"
)]
fn finding(id: &str, anchor: Value) -> Value {
    json!({"schemaVersion":1,"id":id,"sourceFindingIds":[id],"origin":"reviewer",
        "title":"Finding","description":"Problem","reasoning":"Reason",
        "consequence":"Impact","recommendedAction":"Fix","anchor":anchor})
}

/// Builds the shared audited capture and decision inputs for focused boundary tests.
fn approved_inputs(files: &Files, findings: &[Value]) -> (PathBuf, PathBuf, PathBuf) {
    let decisions: Vec<_> = findings
        .iter()
        .map(|finding| {
            json!({"findingId":finding["id"],"decision":"approved","recordedUtc":"2025-01-01T00:00:00Z"})
        })
        .collect();
    let artifact = files.put(
        "findings",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":findings}),
    );
    let decisions = files.put(
        "decisions",
        &json!({"schemaVersion":1,"decisions":decisions}),
    );
    let capture = files.put("capture", &json!({
        "schemaVersion":3,"platform":"gitHub","repositoryUrl":"https://example.test/repo",
        "pullRequestUrl":"https://example.test/pr/1","target":{"platform":"gitHub","pullRequestId":1},
        "pullRequest":{"title":"Title","description":"Body","reviewedSourceCommit":"b".repeat(40),
            "reviewedMergeBaseCommit":"a".repeat(40)},
        "reviewers":[],"revisions":[],"threads":[{"id":"T","openingCommentId":"C","kind":"text",
            "resolution":"active","isOutdated":false,"isAuthoredBySelf":false,
            "context":{"filePath":"src/lib.rs"},"decision":null,"url":null,"eventUtc":null,
            "lastUpdatedUtc":null,"comments":[]}]
    }));
    (artifact, decisions, capture)
}

#[test]
fn approved_projection_preserves_anchors_followups_and_unanchored_reasons() {
    let files = Files::new();
    let mut follow = finding("F4", json!({"kind":"file","path":"missing"}));
    follow["followUp"] = json!({"threadId":"T","commentId":"C"});
    let mut cited = finding("F6", json!({"kind":"file","path":"src/lib.rs"}));
    cited["impactedLocations"] = json!([{"path":"src/untouched.rs","startLine":12}]);
    let findings = vec![
        finding("F1", json!({"kind":"file","path":"./src/lib.rs"})),
        finding("F2", json!({"kind":"file","path":"SRC/lib.rs"})),
        finding(
            "F3",
            json!({"kind":"line","path":"src/lib.rs","startLine":300,"endLine":305}),
        ),
        follow,
        finding("F5", json!({"kind":"file","path":"./src/old.rs"})),
        cited,
        finding("F7", json!({"kind":"file","path":"src/new.rs"})),
        finding("F8", json!({"kind":"file","path":"old/gone.txt"})),
        finding(
            "F9",
            json!({"kind":"line","path":"src/lib.rs","startLine":10}),
        ),
        finding("F10", json!({"kind":"file","path":"src/untouched.rs"})),
    ];
    let artifact = files.put(
        "findings",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":findings}),
    );
    let decisions = files.put("decisions", &json!({"schemaVersion":1,"decisions":
        (1..=10).map(|id| json!({"findingId":format!("F{id}"),"decision":"approved","recordedUtc":"2025-01-01T00:00:00Z"})).collect::<Vec<_>>()
    }));
    let capture = files.put("capture", &json!({
        "schemaVersion":3,"platform":"gitHub","repositoryUrl":"https://example.test/repo",
        "pullRequestUrl":"https://example.test/pr/1","target":{"platform":"gitHub","pullRequestId":1},
        "pullRequest":{"title":"Title","description":"Body","reviewedSourceCommit":"b".repeat(40),
            "reviewedMergeBaseCommit":"a".repeat(40)},
        "reviewers":[],"revisions":[],"threads":[{"id":"T","openingCommentId":"C","kind":"text",
            "resolution":"active","isOutdated":false,"isAuthoredBySelf":false,
            "context":{"filePath":"src/lib.rs"},"decision":null,"url":null,"eventUtc":null,
            "lastUpdatedUtc":null,"comments":[]}]
    }));
    let output = files.path("approved");
    let result = approved::build_approved(
        &artifact,
        &decisions,
        &output,
        Some(&capture),
        Some(Path::new(".")),
        &NoWait,
        &RecordedGit,
    )
    .unwrap();
    assert_eq!(
        result["findingId"],
        json!(["F1", "F3", "F4", "F6", "F7", "F8", "F9"])
    );
    assert_eq!(result["unanchored"], 3);
    assert_eq!(result["unanchoredFinding"][0]["findingId"], "F2");
    assert_eq!(result["unanchoredFinding"][1]["findingId"], "F5");
    assert_eq!(result["unanchoredFinding"][1]["path"], "src/old.rs");
    assert!(
        result["unanchoredFinding"][1]["reason"]
            .as_str()
            .unwrap()
            .contains("does not change 'src/old.rs'")
    );
    assert_eq!(result["unanchoredFinding"][2]["findingId"], "F10");
    assert_eq!(result["unanchoredFinding"][2]["path"], "src/untouched.rs");
    assert!(
        result["unanchoredFinding"][2]["reason"]
            .as_str()
            .unwrap()
            .contains("does not change 'src/untouched.rs'")
    );
    let before = fs::read(&output).unwrap();
    let document: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(
        document["findings"][1]["anchor"],
        json!({"kind":"line","path":"src/lib.rs","startLine":300,"endLine":305})
    );
    assert_eq!(document["findings"][2]["followUp"]["threadId"], "T");
    assert_eq!(
        document["findings"][3]["impactedLocations"][0]["path"],
        "src/untouched.rs"
    );
    for process_failure in [false, true] {
        approved::build_approved(
            &artifact,
            &decisions,
            &output,
            Some(&capture),
            Some(Path::new(".")),
            &NoWait,
            &InvalidGit { process_failure },
        )
        .unwrap_err();
        assert_eq!(fs::read(&output).unwrap(), before);
    }

    let original_artifact: Value = serde_json::from_slice(&fs::read(&artifact).unwrap()).unwrap();
    for malformed in [
        Value::Null,
        json!("malformed"),
        json!({"kind":"line","path":"src/lib.rs","startLine":10,"side":"middle"}),
    ] {
        let mut changed = original_artifact.clone();
        changed["findings"][0]["anchor"] = malformed;
        fs::write(&artifact, serde_json::to_vec(&changed).unwrap()).unwrap();
        approved::build_approved(
            &artifact,
            &decisions,
            &output,
            Some(&capture),
            Some(Path::new(".")),
            &NoWait,
            &RecordedGit,
        )
        .unwrap_err();
        assert_eq!(fs::read(&output).unwrap(), before);
    }
}

#[test]
fn malformed_anchor_side_fails_validation_before_diff_and_preserves_output() {
    let files = Files::new();
    let findings = vec![
        finding("F1", json!({"kind":"file","path":"src/lib.rs"})),
        finding(
            "F2",
            json!({"kind":"line","path":"src/lib.rs","startLine":10,"side":"middle"}),
        ),
    ];
    let (artifact, decisions, capture) = approved_inputs(&files, &findings);
    let output = files.path("approved");
    fs::write(&output, b"existing audited output").unwrap();
    let error = approved::build_approved(
        &artifact,
        &decisions,
        &output,
        Some(&capture),
        Some(Path::new(".")),
        &NoWait,
        &UnusedGit,
    )
    .unwrap_err();
    assert!(error.to_string().contains("Invalid diff side"));
    assert_eq!(fs::read(output).unwrap(), b"existing audited output");
}

#[test]
fn follow_up_keeps_its_thread_but_still_requires_the_reviewed_diff() {
    let files = Files::new();
    let mut follow = finding("F1", json!({"kind":"file","path":"src/untouched.rs"}));
    follow["followUp"] = json!({"threadId":"T","commentId":"C"});
    let (artifact, decisions, capture) = approved_inputs(&files, &[follow]);
    let output = files.path("approved");
    let result = approved::build_approved(
        &artifact,
        &decisions,
        &output,
        Some(&capture),
        Some(Path::new(".")),
        &NoWait,
        &RecordedGit,
    )
    .unwrap();
    assert_eq!(result["findingId"], json!(["F1"]));
    assert_eq!(result["unanchored"], 0);
    let before = fs::read(&output).unwrap();
    approved::build_approved(
        &artifact,
        &decisions,
        &output,
        Some(&capture),
        Some(Path::new(".")),
        &NoWait,
        &InvalidGit {
            process_failure: true,
        },
    )
    .unwrap_err();
    assert_eq!(fs::read(&output).unwrap(), before);
}
