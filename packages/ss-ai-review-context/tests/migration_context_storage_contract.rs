//! Exercises capture publication, summary and path boundaries without provider access.
#![allow(
    clippy::indexing_slicing,
    reason = "The tests inspect fixed capture and summary shapes"
)]

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{prepare_capture, read_capture, snapshot_id, write_comment_authorship};
use ss_ai_runtime::Clock;

/// Makes unexpected lock contention fail without waiting.
struct NoWait;
impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("independent capture paths must not wait")
    }
}

/// Reserves independent files directly in the persistent build directory.
struct Files {
    reservation: PathBuf,
    baseline: PathBuf,
    current: PathBuf,
    owns_outputs: bool,
}

impl Files {
    fn new() -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("target");
        // Cargo's output may be redirected, leaving the checkout without this directory.
        fs::create_dir_all(&target).unwrap();
        for id in 0..1024 {
            let stem = target.join(format!("context-storage-{}-{id}", std::process::id()));
            let reservation = stem.with_extension("reservation");
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&reservation)
            {
                Ok(_) => {
                    let mut files = Self {
                        reservation,
                        baseline: stem.with_extension("baseline.json"),
                        current: stem.with_extension("current.json"),
                        owns_outputs: false,
                    };
                    if files.outputs().iter().all(|path| !path.exists()) {
                        files.owns_outputs = true;
                        return files;
                    }
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot reserve capture test files: {error}"),
            }
        }
        panic!("cannot reserve independent capture test files");
    }

    fn outputs(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for output in [&self.baseline, &self.current] {
            for suffix in ["", ".lock", ".authorship.json", ".authorship.json.lock"] {
                let mut name = output.as_os_str().to_os_string();
                name.push(suffix);
                paths.push(PathBuf::from(name));
            }
        }
        paths
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let paths = if self.owns_outputs {
            self.outputs()
        } else {
            Vec::new()
        };
        for path in paths.into_iter().chain([self.reservation.clone()]) {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) if std::thread::panicking() => eprintln!("cleanup failed: {error}"),
                Err(error) => panic!("cleanup failed: {error}"),
            }
        }
    }
}

/// Supplies a complete provider capture without any provider requests.
fn context() -> Value {
    serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-context.json"
    ))
    .unwrap()
}

#[test]
fn published_summary_and_capture_preserve_body_encoding_and_evidence() {
    let files = Files::new();
    let mut input = context();
    let body = "first\r\nsecond 🤖 — “café”\n2026-08-04T10:00:00+03:00";
    input["threads"][0]["comments"][0]["content"] = json!(body);
    let target = input["target"]["webUrl"].as_str().unwrap();
    let result = prepare_capture(&files.baseline, None, target, &NoWait)
        .unwrap()
        .publish(&input, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    assert_eq!(result["threadCount"], 1);
    assert_eq!(result["commentCount"], 1);
    assert_eq!(result["reviewerCount"], 0);
    assert_eq!(result["indeterminateCommentCount"], 1);
    assert_eq!(result["contextPath"], files.baseline.to_str().unwrap());
    assert_eq!(
        result["reviewedSourceCommit"],
        input["pullRequest"]["reviewedSourceCommit"]
    );
    assert_eq!(
        result["reviewedMergeBaseCommit"],
        input["pullRequest"]["reviewedMergeBaseCommit"]
    );
    assert!(result["delta"].is_null());
    assert!(
        !result
            .as_object()
            .unwrap()
            .contains_key("indeterminateComments")
    );
    let expected = [
        "platform",
        "pullRequestUrl",
        "repositoryUrl",
        "contextPath",
        "fetchedUtc",
        "reviewedSourceCommit",
        "reviewedMergeBaseCommit",
        "threadCount",
        "commentCount",
        "reviewerCount",
        "indeterminateCommentCount",
        "reusedClassificationCount",
        "delta",
    ];
    assert_eq!(result.as_object().unwrap().len(), expected.len());
    assert!(expected.iter().all(|key| result.get(key).is_some()));
    let text = fs::read(&files.baseline).unwrap();
    assert!(!text.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert!(!text.contains(&b'\r'));
    let restored = read_capture(&files.baseline, &NoWait).unwrap();
    assert_eq!(restored["schemaVersion"], 3);
    assert_eq!(restored["threads"][0]["comments"][0]["content"], body);
}

#[test]
fn empty_and_settled_discussions_report_complete_counts() {
    let files = Files::new();
    let mut input = context();
    input["threads"] = json!([]);
    input["reviewers"] = json!([]);
    let target = input["target"]["webUrl"].as_str().unwrap().to_owned();
    let empty = prepare_capture(&files.baseline, None, &target, &NoWait)
        .unwrap()
        .publish(&input, "2026-08-04T12:00:00Z")
        .unwrap();
    for key in [
        "threadCount",
        "commentCount",
        "reviewerCount",
        "indeterminateCommentCount",
    ] {
        assert_eq!(empty[key], 0);
    }
    let mut settled = context();
    settled["threads"][0]["comments"][0]["authorship"] = json!("agent");
    let result = prepare_capture(&files.current, Some(&files.baseline), &target, &NoWait)
        .unwrap()
        .publish(&settled, "2026-08-04T12:00:00Z")
        .unwrap();
    assert_eq!(result["commentCount"], 1);
    assert_eq!(result["indeterminateCommentCount"], 0);
    assert_eq!(result["delta"]["threadsAddedCount"], 1);
    assert_eq!(result["delta"]["commentsAddedCount"], 1);
    assert_eq!(result["delta"]["hasChanges"], true);
    assert!(
        result["delta"]
            .as_object()
            .unwrap()
            .values()
            .all(|value| !value.is_array())
    );
}

#[test]
fn schema_and_comparison_path_guards_reject_without_replacing_evidence() {
    let files = Files::new();
    let input = context();
    let target = input["target"]["webUrl"].as_str().unwrap();
    prepare_capture(&files.baseline, None, target, &NoWait)
        .unwrap()
        .publish(&input, "2026-08-04T12:00:00Z")
        .unwrap();
    let before = fs::read(&files.baseline).unwrap();
    prepare_capture(&files.baseline, Some(&files.baseline), target, &NoWait).unwrap_err();
    let alias = files
        .baseline
        .parent()
        .unwrap()
        .join(".")
        .join(files.baseline.file_name().unwrap());
    prepare_capture(&files.baseline, Some(&alias), target, &NoWait).unwrap_err();
    let target_dir = files.baseline.parent().unwrap();
    let relative_alias = target_dir
        .join("..")
        .join(target_dir.file_name().unwrap())
        .join(files.baseline.file_name().unwrap());
    prepare_capture(&files.baseline, Some(&relative_alias), target, &NoWait).unwrap_err();
    assert_eq!(fs::read(&files.baseline).unwrap(), before);
    let different =
        prepare_capture(&files.current, Some(&files.baseline), target, &NoWait).unwrap();
    drop(different);

    for value in [
        json!({"threads":[]}),
        json!({"schemaVersion":99,"threads":[]}),
    ] {
        fs::write(&files.current, serde_json::to_vec(&value).unwrap()).unwrap();
        read_capture(&files.current, &NoWait).unwrap_err();
    }
}

#[test]
fn classification_keeps_case_distinct_thread_and_comment_identifiers_apart() {
    let files = Files::new();
    let mut input = context();
    input["threads"][0]["id"] = json!("T");
    input["threads"][0]["openingCommentId"] = json!("C");
    input["threads"][0]["comments"][0]["id"] = json!("C");
    let mut lower_comment = input["threads"][0]["comments"][0].clone();
    lower_comment["id"] = json!("c");
    input["threads"][0]["comments"]
        .as_array_mut()
        .unwrap()
        .push(lower_comment);
    let mut lower_thread = input["threads"][0].clone();
    lower_thread["id"] = json!("t");
    lower_thread["comments"] = json!([lower_thread["comments"][0]]);
    input["threads"].as_array_mut().unwrap().push(lower_thread);
    let target = input["target"]["webUrl"].as_str().unwrap();
    prepare_capture(&files.baseline, None, target, &NoWait)
        .unwrap()
        .publish(&input, "2026-08-04T12:00:00Z")
        .unwrap();
    let before = fs::read(&files.baseline).unwrap();
    let original = read_capture(&files.baseline, &NoWait).unwrap();
    let original_evidence = original["threads"][0]["comments"][0]["authorshipEvidence"]
        .as_array()
        .unwrap()
        .clone();
    let summary = write_comment_authorship(
        &files.baseline,
        &snapshot_id(&original).unwrap(),
        &json!([{"threadId":"T","commentId":"C","kind":"human"}]),
        &NoWait,
    )
    .unwrap();
    assert_eq!(summary["applied"], 1);
    assert_eq!(summary["indeterminate"], 2);
    assert_eq!(summary["isComplete"], false);
    let effective = read_capture(&files.baseline, &NoWait).unwrap();
    let classified_evidence = effective["threads"][0]["comments"][0]["authorshipEvidence"]
        .as_array()
        .unwrap();
    assert!(
        original_evidence
            .iter()
            .all(|item| classified_evidence.contains(item))
    );
    assert!(classified_evidence.contains(&json!("classification:reviewed")));
    assert_eq!(
        effective["threads"][0]["comments"][0]["authorship"],
        "human"
    );
    assert_eq!(
        effective["threads"][0]["comments"][1]["authorship"],
        "indeterminate"
    );
    assert_eq!(
        effective["threads"][1]["comments"][0]["authorship"],
        "indeterminate"
    );
    assert_eq!(fs::read(&files.baseline).unwrap(), before);
}
