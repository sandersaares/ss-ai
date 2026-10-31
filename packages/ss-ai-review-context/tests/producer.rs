//! Compares native capture publication with retained PowerShell model fixtures.
#![allow(
    clippy::indexing_slicing,
    reason = "The fixtures have fixed, known shapes"
)]

use std::cell::RefCell;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{
    document_hash, prepare_capture, read_capture, snapshot_id, write_comment_authorship,
};
use ss_ai_runtime::{Clock, FileTransaction, SystemClock, classification};

/// Keeps each test's capture and retained locks in the active checkout.
struct Output(PathBuf);

impl Output {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The counter only separates names; it does not synchronize test data.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(
            std::env::current_dir()
                .unwrap()
                .join(format!(".capture-test-{}-{id}.json", std::process::id())),
        )
    }

    fn sidecar(&self) -> PathBuf {
        let mut path = self.0.as_os_str().to_os_string();
        path.push(".authorship.json");
        path.into()
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        for suffix in ["", ".lock", ".authorship.json", ".authorship.json.lock"] {
            let mut path = self.0.as_os_str().to_os_string();
            path.push(suffix);
            drop(fs::remove_file(path));
        }
    }
}

/// Loads a provider or canonical document supplied by the retained model.
fn fixture(name: &str) -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ss-review-native-capture");
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}

/// Exposes contention without allowing tests to depend on elapsed time.
struct ContendedClock {
    reached: mpsc::Sender<()>,
    release: RefCell<mpsc::Receiver<()>>,
}

impl Clock for ContendedClock {
    fn now(&self) -> Timestamp {
        panic!("capture preparation does not read the clock")
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        self.reached.send(()).unwrap();
        self.release.borrow_mut().recv().unwrap();
        Ok(())
    }
}

#[test]
fn publishes_identical_captures_for_both_providers() {
    let clock = SystemClock::default();
    for (source, expected) in [
        ("github-context.json", "github-expected.json"),
        ("azure-devops-context.json", "azure-devops-expected.json"),
        (
            "azure-devops-refresh-context.json",
            "azure-devops-refresh-expected.json",
        ),
    ] {
        let context = fixture(source);
        let expected = fixture(expected);
        let output = Output::new();
        let target = context["target"]["webUrl"].as_str().unwrap();
        let prepared = prepare_capture(&output.0, None, target, &clock).unwrap();
        let summary = prepared
            .publish(&context, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        let actual: Value = serde_json::from_slice(&fs::read(&output.0).unwrap()).unwrap();
        assert_eq!(actual, expected, "{source}");
        assert_eq!(
            document_hash(&actual).unwrap(),
            document_hash(&expected).unwrap(),
            "{source}"
        );
        assert_eq!(read_capture(&output.0, &clock).unwrap(), expected);
        assert_eq!(
            usize::try_from(summary["threadCount"].as_u64().unwrap()).unwrap(),
            expected["threads"].as_array().unwrap().len()
        );
        let comments: Vec<_> = expected["threads"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|thread| thread["comments"].as_array().unwrap())
            .collect();
        assert_eq!(summary["commentCount"], comments.len());
        assert_eq!(
            summary["indeterminateCommentCount"],
            comments
                .iter()
                .filter(|comment| comment["authorship"] == "indeterminate")
                .count()
        );
        assert_eq!(
            summary["reviewerCount"],
            expected["reviewers"].as_array().unwrap().len()
        );
        assert!(summary["delta"].is_null());
    }
}

#[test]
fn published_capture_and_classification_bind_powershell_serialized_numbers() {
    let clock = SystemClock::default();
    let mut context = fixture("azure-devops-context.json");
    // PowerShell's Double formatter emits this value using its compatibility spelling.
    context["threads"][0]["comments"][0]["author"]["nativeNumber"] =
        json!(f64::from_bits(0x3e60_0000_0000_0000));
    let target = context["target"]["webUrl"].as_str().unwrap();
    let output = Output::new();
    prepare_capture(&output.0, None, target, &clock)
        .unwrap()
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let raw: Value = serde_json::from_slice(&fs::read(&output.0).unwrap()).unwrap();
    let record: Value = serde_json::from_slice(&fs::read(output.sidecar()).unwrap()).unwrap();
    assert_eq!(record["captureHash"], document_hash(&raw).unwrap());
    let snapshot = snapshot_id(&read_capture(&output.0, &clock).unwrap()).unwrap();
    write_comment_authorship(
        &output.0,
        &snapshot,
        &json!([{"threadId":"101", "commentId":"1", "kind":"human"}]),
        &clock,
    )
    .unwrap();
    assert_eq!(
        read_capture(&output.0, &clock).unwrap()["threads"][0]["comments"][0]["authorship"],
        "human"
    );
}

#[test]
fn refresh_reuses_only_unchanged_decisions_and_preserves_baseline() {
    let clock = SystemClock::default();
    let context = fixture("azure-devops-context.json");
    let changed = fixture("azure-devops-refresh-context.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    let refresh = Output::new();
    prepare_capture(&baseline.0, None, target, &clock)
        .unwrap()
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let raw = fs::read(&baseline.0).unwrap();
    let document = read_capture(&baseline.0, &clock).unwrap();
    let snapshot = snapshot_id(&document).unwrap();
    write_comment_authorship(
        &baseline.0,
        &snapshot,
        &json!([
            {"threadId":"101", "commentId":"1", "kind":"human"},
            {"threadId":"101", "commentId":"2", "kind":"agent"}
        ]),
        &clock,
    )
    .unwrap();
    let result = prepare_capture(&refresh.0, Some(&baseline.0), target, &clock)
        .unwrap()
        .publish(&changed, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    assert_eq!(fs::read(&baseline.0).unwrap(), raw);
    assert_eq!(result["reusedClassificationCount"], 1);
    assert_eq!(result["indeterminateCommentCount"], 1);
    assert_eq!(result["delta"]["commentsEditedCount"], 1);
    assert_eq!(result["delta"]["hasChanges"], true);
    prepare_capture(&refresh.0, Some(&baseline.0), target, &clock).unwrap_err();
    prepare_capture(&baseline.0, Some(&baseline.0), target, &clock).unwrap_err();
    prepare_capture(
        &Output::new().0,
        Some(&baseline.0),
        "https://example.invalid/another",
        &clock,
    )
    .unwrap_err();
}

#[test]
fn concurrent_creators_do_not_replace_the_published_capture() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let output = Output::new();
    let url = context["target"]["webUrl"].as_str().unwrap();
    let first = prepare_capture(&output.0, None, url, &clock).unwrap();
    let path = output.0.clone();
    let url = url.to_owned();
    let (reached_send, reached_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let worker = thread::spawn(move || {
        let clock = ContendedClock {
            reached: reached_send,
            release: RefCell::new(release_receive),
        };
        prepare_capture(&path, None, &url, &clock).is_err()
    });
    reached_receive.recv().unwrap();
    first
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let original = fs::read(&output.0).unwrap();
    release_send.send(()).unwrap();
    assert!(worker.join().unwrap());
    assert_eq!(fs::read(&output.0).unwrap(), original);
}

#[test]
fn comparison_rejects_malformed_or_unbound_sidecar_before_provider_work() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    prepare_capture(&baseline.0, None, target, &clock)
        .unwrap()
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let sidecar = baseline.sidecar();
    let original = fs::read(&sidecar).unwrap();
    for bad in [
        b"not json".to_vec(),
        br#"{"schemaVersion":1,"policyVersion":"2","captureHash":"0000000000000000000000000000000000000000000000000000000000000000","decisions":[]}"#.to_vec(),
    ] {
        fs::write(&sidecar, bad).unwrap();
        let output = Output::new();
        prepare_capture(&output.0, Some(&baseline.0), target, &clock).unwrap_err();
        assert!(!output.0.exists());
        assert!(!output.sidecar().exists());
    }
    fs::write(sidecar, original).unwrap();
}

#[test]
fn comparison_reserves_absent_sidecar_and_accepts_distinct_output() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let baseline_document = fixture("github-expected.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    fs::write(&baseline.0, serde_json::to_vec(&baseline_document).unwrap()).unwrap();
    let baseline_bytes = fs::read(&baseline.0).unwrap();
    let output_path = baseline.sidecar();
    let mut output_sidecar = output_path.as_os_str().to_os_string();
    output_sidecar.push(".authorship.json");
    let mut baseline_sidecar_lock = output_path.as_os_str().to_os_string();
    baseline_sidecar_lock.push(".lock");
    let error = prepare_capture(&output_path, Some(&baseline.0), target, &clock).unwrap_err();
    assert_eq!(classification(&error), "invalidInput");
    assert!(!output_path.exists());
    assert!(!PathBuf::from(output_sidecar).exists());
    assert!(!PathBuf::from(baseline_sidecar_lock).exists());
    assert_eq!(fs::read(&baseline.0).unwrap(), baseline_bytes);
    assert_eq!(
        read_capture(&baseline.0, &clock).unwrap(),
        baseline_document
    );

    let output = Output::new();
    prepare_capture(&output.0, Some(&baseline.0), target, &clock)
        .unwrap()
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    assert_eq!(
        read_capture(&baseline.0, &clock).unwrap(),
        baseline_document
    );
    assert!(output.0.exists());
    assert!(output.sidecar().exists());
}

#[cfg(unix)]
#[test]
fn comparison_rejects_dangling_sidecar_before_publication() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let document = fixture("github-expected.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    for points_to_capture in [true, false] {
        let baseline = Output::new();
        let output = Output::new();
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&baseline.0, &bytes).unwrap();
        let link_target = if points_to_capture {
            output.0.clone()
        } else {
            output.sidecar()
        };
        symlink(&link_target, baseline.sidecar()).unwrap();

        let error = prepare_capture(&output.0, Some(&baseline.0), target, &clock).unwrap_err();
        assert_eq!(classification(&error), "invalidInput");
        assert_eq!(fs::read(&baseline.0).unwrap(), bytes);
        assert_eq!(fs::read_link(baseline.sidecar()).unwrap(), link_target);
        assert!(!output.0.exists());
        assert!(!output.sidecar().exists());
    }
}

#[cfg(unix)]
#[test]
fn comparison_reserves_absent_sidecar_through_directory_alias() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let baseline_document = fixture("github-expected.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    fs::write(&baseline.0, serde_json::to_vec(&baseline_document).unwrap()).unwrap();
    let alias = Output::new();
    symlink(baseline.0.parent().unwrap(), &alias.0).unwrap();
    let output_path = alias.0.join(baseline.sidecar().file_name().unwrap());
    let mut output_sidecar = output_path.as_os_str().to_os_string();
    output_sidecar.push(".authorship.json");
    let error = prepare_capture(&output_path, Some(&baseline.0), target, &clock).unwrap_err();
    assert_eq!(classification(&error), "invalidInput");
    assert!(!output_path.exists());
    assert!(!PathBuf::from(output_sidecar).exists());
    assert_eq!(
        read_capture(&baseline.0, &clock).unwrap(),
        baseline_document
    );
}

#[test]
fn comparison_reserves_sidecar_under_actual_case_rules() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let baseline_document = fixture("github-expected.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    fs::write(&baseline.0, serde_json::to_vec(&baseline_document).unwrap()).unwrap();

    // Probe the same directory rather than inferring case rules from the OS.
    let alternate_name = baseline
        .0
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_uppercase();
    let case_insensitive = baseline.0.with_file_name(alternate_name).exists();
    let alternate_sidecar_name = baseline
        .sidecar()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_uppercase();
    let output = Output(baseline.0.with_file_name(alternate_sidecar_name));
    let prepared = prepare_capture(&output.0, Some(&baseline.0), target, &clock);
    if case_insensitive {
        assert_eq!(classification(&prepared.unwrap_err()), "invalidInput");
        assert!(!baseline.sidecar().exists());
        assert!(!output.sidecar().exists());
    } else {
        prepared
            .unwrap()
            .publish(&context, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        assert!(output.0.exists());
        assert!(!baseline.sidecar().exists());
    }
    assert_eq!(
        read_capture(&baseline.0, &clock).unwrap(),
        baseline_document
    );
}

#[cfg(unix)]
#[test]
fn comparison_rejects_output_with_aliased_sidecar_lock() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    fs::write(
        &baseline.0,
        serde_json::to_vec(&fixture("github-expected.json")).unwrap(),
    )
    .unwrap();
    drop(FileTransaction::open(&baseline.sidecar(), &clock).unwrap());
    let mut baseline_lock = baseline.sidecar().into_os_string();
    baseline_lock.push(".lock");
    let output = Output::new();
    let mut output_lock = output.0.as_os_str().to_os_string();
    output_lock.push(".lock");
    symlink(baseline_lock, output_lock).unwrap();
    let error = prepare_capture(&output.0, Some(&baseline.0), target, &clock).unwrap_err();
    assert_eq!(classification(&error), "invalidInput");
    assert!(!baseline.sidecar().exists());
    assert!(!output.0.exists());
    assert!(!output.sidecar().exists());
}

#[test]
fn comparison_reserves_sidecar_under_actual_trailing_dot_rules() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let baseline_document = fixture("github-expected.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    fs::write(&baseline.0, serde_json::to_vec(&baseline_document).unwrap()).unwrap();

    let mut probe = baseline.0.as_os_str().to_os_string();
    probe.push(".");
    let ignores_trailing_dot = PathBuf::from(probe).exists();
    let mut output = baseline.sidecar().into_os_string();
    output.push(".");
    let output = Output(output.into());
    let prepared = prepare_capture(&output.0, Some(&baseline.0), target, &clock);
    if ignores_trailing_dot {
        assert_eq!(classification(&prepared.unwrap_err()), "invalidInput");
        assert!(!baseline.sidecar().exists());
        assert!(!output.sidecar().exists());
    } else {
        prepared
            .unwrap()
            .publish(&context, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        assert!(output.0.exists());
        assert!(!baseline.sidecar().exists());
    }
    assert_eq!(
        read_capture(&baseline.0, &clock).unwrap(),
        baseline_document
    );
}

#[test]
fn comparison_reports_path_resolution_failures_before_provider_work() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    fs::write(
        &baseline.0,
        serde_json::to_vec(&fixture("github-expected.json")).unwrap(),
    )
    .unwrap();
    let blocker = Output::new();
    fs::write(&blocker.0, b"not a directory").unwrap();
    let error = prepare_capture(
        &blocker.0.join("capture"),
        Some(&baseline.0),
        target,
        &clock,
    )
    .unwrap_err();
    assert_eq!(classification(&error), "io");
    assert_eq!(
        error.find_source::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::NotADirectory
    );
    assert_eq!(fs::read(&blocker.0).unwrap(), b"not a directory");
    assert_eq!(
        fs::read(&baseline.0).unwrap(),
        serde_json::to_vec(&fixture("github-expected.json")).unwrap()
    );
    let mut lock = baseline.sidecar().into_os_string();
    lock.push(".lock");
    assert!(!PathBuf::from(lock).exists());
}

#[test]
fn comparison_never_replaces_existing_hardlink_output() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let baseline_document = fixture("github-expected.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    fs::write(&baseline.0, serde_json::to_vec(&baseline_document).unwrap()).unwrap();
    let output = Output::new();
    fs::hard_link(&baseline.0, &output.0).unwrap();
    let error = prepare_capture(&output.0, Some(&baseline.0), target, &clock).unwrap_err();
    assert_eq!(classification(&error), "invalidInput");
    assert_eq!(
        read_capture(&baseline.0, &clock).unwrap(),
        baseline_document
    );
    assert!(!output.sidecar().exists());
}

#[test]
fn comparison_reads_sidecar_after_acquiring_baseline_lock() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let target = context["target"]["webUrl"].as_str().unwrap().to_owned();
    let baseline = Output::new();
    let output = Output::new();
    prepare_capture(&baseline.0, None, &target, &clock)
        .unwrap()
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let lock = FileTransaction::open(&baseline.sidecar(), &clock).unwrap();
    let (reached_send, reached_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let baseline_path = baseline.0.clone();
    let output_path = output.0.clone();
    let worker = thread::spawn(move || {
        let clock = ContendedClock {
            reached: reached_send,
            release: RefCell::new(release_receive),
        };
        prepare_capture(&output_path, Some(&baseline_path), &target, &clock).is_err()
    });
    reached_receive.recv().unwrap();
    lock.write_bytes(b"invalid sidecar").unwrap();
    drop(lock);
    release_send.send(()).unwrap();
    assert!(worker.join().unwrap());
    assert!(!output.0.exists());
    assert!(!output.sidecar().exists());
}

#[test]
fn reuse_requires_matching_baseline_inputs_even_when_new_inputs_match() {
    let clock = SystemClock::default();
    let context = fixture("azure-devops-context.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    let baseline = Output::new();
    let changed = Output::new();
    let refresh = Output::new();
    let mut changed_context = context.clone();
    changed_context["threads"][0]["comments"][0]["content"] = json!("new evidence");
    for (output, context) in [(&baseline, &context), (&changed, &changed_context)] {
        prepare_capture(&output.0, None, target, &clock)
            .unwrap()
            .publish(context, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        let snapshot = snapshot_id(&read_capture(&output.0, &clock).unwrap()).unwrap();
        write_comment_authorship(
            &output.0,
            &snapshot,
            &json!([{"threadId":"101", "commentId":"1", "kind":"human"}]),
            &clock,
        )
        .unwrap();
    }
    let mut old: Value = serde_json::from_slice(&fs::read(baseline.sidecar()).unwrap()).unwrap();
    let new: Value = serde_json::from_slice(&fs::read(changed.sidecar()).unwrap()).unwrap();
    assert_ne!(
        old["decisions"][0]["inputHash"],
        new["decisions"][0]["inputHash"]
    );
    old["decisions"][0]["inputHash"] = new["decisions"][0]["inputHash"].clone();
    fs::write(baseline.sidecar(), serde_json::to_vec(&old).unwrap()).unwrap();
    let summary = prepare_capture(&refresh.0, Some(&baseline.0), target, &clock)
        .unwrap()
        .publish(&changed_context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    assert_eq!(summary["reusedClassificationCount"], 0);
    assert_eq!(
        read_capture(&refresh.0, &clock).unwrap()["threads"][0]["comments"][0]["authorship"],
        "indeterminate"
    );
}

#[test]
fn publication_never_replaces_files_created_after_preparation() {
    let clock = SystemClock::default();
    let context = fixture("github-context.json");
    let target = context["target"]["webUrl"].as_str().unwrap();
    for sidecar in [false, true] {
        let output = Output::new();
        let prepared = prepare_capture(&output.0, None, target, &clock).unwrap();
        let path = if sidecar {
            output.sidecar()
        } else {
            output.0.clone()
        };
        fs::write(&path, b"other owner").unwrap();
        prepared
            .publish(&context, "2026-08-04T12:00:00.0000000Z")
            .unwrap_err();
        assert_eq!(fs::read(path).unwrap(), b"other owner");
    }
}

#[test]
fn canonical_model_uses_ordinal_chronology_target_fields_and_opener_fallback() {
    let clock = SystemClock::default();
    let mut context = fixture("github-context.json");
    let target = context["target"]["webUrl"].as_str().unwrap().to_owned();
    context["target"]["organization"] = json!("space owner");
    context["target"]["repository"] = json!("répo");
    let template = context["threads"][0].clone();
    let mut supplementary = template.clone();
    supplementary["id"] = json!("\u{10000}");
    supplementary["eventUtc"] = json!("same");
    let mut bmp = template;
    bmp["id"] = json!("\u{e000}");
    bmp["eventUtc"] = json!("same");
    bmp["comments"][0]["author"] = Value::Null;
    bmp["decision"] = json!({"by": context["authenticatedIdentity"]});
    context["threads"] = json!([bmp, supplementary]);
    let output = Output::new();
    prepare_capture(&output.0, None, &target, &clock)
        .unwrap()
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let document = read_capture(&output.0, &clock).unwrap();
    assert_eq!(
        document["repositoryUrl"],
        "https://github.com/space%20owner/r%C3%A9po"
    );
    assert_eq!(document["threads"][0]["id"], "\u{10000}");
    assert_eq!(document["threads"][1]["id"], "\u{e000}");
    assert_eq!(document["threads"][1]["isAuthoredBySelf"], true);
}

#[test]
fn azure_identifier_target_must_match_provider_result() {
    let clock = SystemClock::default();
    let context = fixture("azure-devops-context.json");
    let output = Output::new();
    let prepared = prepare_capture(
        &output.0,
        None,
        "https://example.visualstudio.com/_git/pullrequest/99",
        &clock,
    )
    .unwrap();
    prepared
        .publish(&context, "2026-08-04T12:00:00.0000000Z")
        .unwrap_err();
    assert!(!output.0.exists());
}
