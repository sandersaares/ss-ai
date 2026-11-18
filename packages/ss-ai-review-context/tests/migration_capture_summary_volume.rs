//! Exercises the published capture summary with many newly added discussions.
#![allow(
    clippy::indexing_slicing,
    reason = "The test constructs and inspects fixed JSON fixture shapes"
)]

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::prepare_capture;
use ss_ai_runtime::Clock;

/// Makes unexpected transaction contention fail without waiting on real time.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("capture-summary fixture must not wait")
    }
}

/// Reserves unique output names without creating a temporary directory.
struct Outputs {
    reservation: PathBuf,
    baseline: PathBuf,
    current: PathBuf,
    owns_created_files: bool,
}

impl Outputs {
    /// Allocates names in the checkout's existing build-output directory.
    fn new() -> Self {
        let package = Path::new(env!("CARGO_MANIFEST_DIR"));
        std::env::set_current_dir(package.parent().unwrap().parent().unwrap()).unwrap();
        fs::create_dir_all("target").unwrap();
        // The bound detects a namespace allocation failure without looping forever.
        for id in 0..1024 {
            let stem = format!("target/migration-summary-{}-{id}", std::process::id());
            let reservation = PathBuf::from(format!("{stem}.reservation"));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&reservation)
            {
                Ok(_) => {
                    let mut result = Self {
                        reservation,
                        baseline: PathBuf::from(format!("{stem}-baseline.json")),
                        current: PathBuf::from(format!("{stem}-current.json")),
                        owns_created_files: false,
                    };
                    if result.output_paths().iter().all(|path| !path.exists()) {
                        result.owns_created_files = true;
                        return result;
                    }
                    // This reservation belongs to this attempt, but the
                    // existing outputs may belong to a prior interrupted run.
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot reserve summary fixture: {error}"),
            }
        }
        panic!("cannot allocate unique summary fixture names");
    }

    /// Lists only the files owned by these distinct capture outputs.
    fn output_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for output in [&self.baseline, &self.current] {
            for suffix in ["", ".lock", ".authorship.json", ".authorship.json.lock"] {
                let mut path = output.as_os_str().to_os_string();
                path.push(suffix);
                paths.push(PathBuf::from(path));
            }
        }
        paths
    }
}

impl Drop for Outputs {
    fn drop(&mut self) {
        let owned_outputs = if self.owns_created_files {
            self.output_paths()
        } else {
            Vec::new()
        };
        for path in owned_outputs.into_iter().chain([self.reservation.clone()]) {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) if std::thread::panicking() => {
                    eprintln!("Cannot clean summary fixture: {error}");
                }
                Err(error) => panic!("Cannot clean summary fixture: {error}"),
            }
        }
    }
}

#[test]
#[cfg_attr(
    miri,
    ignore = "the required large file-backed capture is too slow under Miri"
)]
fn capture_summary_stays_bounded_for_thousand_added_indeterminate_comments() {
    let outputs = Outputs::new();
    let clock = NoWait;
    let mut context: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-context.json"
    ))
    .unwrap();
    let template = context["threads"][0].clone();
    let target = context["target"]["webUrl"].as_str().unwrap().to_owned();
    context["threads"] = json!([]);
    prepare_capture(&outputs.baseline, None, &target, &clock)
        .unwrap()
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();

    // Match the predecessor's volume and body size while keeping every
    // identifier distinct, so the delta counts represent separate evidence.
    let threads: Vec<_> = (0..1000)
        .map(|id| {
            let mut thread = template.clone();
            thread["id"] = json!(format!("thread-{id}"));
            thread["openingCommentId"] = json!(format!("comment-{id}"));
            thread["comments"][0]["id"] = json!(format!("comment-{id}"));
            thread["comments"][0]["content"] = json!("x".repeat(1000));
            thread
        })
        .collect();
    context["threads"] = json!(threads);
    let result = prepare_capture(&outputs.current, Some(&outputs.baseline), &target, &clock)
        .unwrap()
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    assert_eq!(result["indeterminateCommentCount"], 1000);
    assert_eq!(result["delta"]["commentsAddedCount"], 1000);
    assert_eq!(result["delta"]["threadsAddedCount"], 1000);
    let serialized = serde_json::to_string(&result).unwrap();
    assert!(serialized.len() < 16 * 1024);
    for property in ["indeterminateComments", "commentId", "threadId"] {
        assert!(!serialized.contains(property));
    }
}
