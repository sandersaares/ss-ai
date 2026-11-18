//! Exercises semantic authorship reuse and selective reading of a large refresh.
#![allow(
    clippy::indexing_slicing,
    reason = "The test constructs and inspects a known capture fixture"
)]

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::{
    evidence_hash, prepare_capture, query, read_capture, snapshot_id, write_comment_authorship,
};
use ss_ai_runtime::Clock;

/// Makes unexpected transaction contention fail instead of waiting.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("isolated authorship fixture must not wait")
    }
}

/// Owns unique file names in the checkout's existing build-output directory.
struct Files {
    reservation: PathBuf,
    baseline: PathBuf,
    current: PathBuf,
    owns_paths: bool,
}

impl Files {
    fn new() -> Self {
        let package = Path::new(env!("CARGO_MANIFEST_DIR"));
        let target = package.parent().unwrap().parent().unwrap().join("target");
        fs::create_dir_all(&target).unwrap();
        // A bounded search detects an exhausted namespace without hanging.
        for id in 0..1024 {
            let stem = target.join(format!(
                "migration-authorship-volume-{}-{id}",
                std::process::id()
            ));
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
                        owns_paths: false,
                    };
                    if files.paths().iter().all(|path| !path.exists()) {
                        files.owns_paths = true;
                        return files;
                    }
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot reserve authorship fixture: {error}"),
            }
        }
        panic!("cannot allocate authorship fixture names");
    }

    fn paths(&self) -> Vec<PathBuf> {
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

impl Drop for Files {
    fn drop(&mut self) {
        if self.owns_paths {
            for path in self.paths() {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) if std::thread::panicking() => {
                        eprintln!("Cannot clean authorship fixture: {error}");
                    }
                    Err(error) => panic!("Cannot clean authorship fixture: {error}"),
                }
            }
        }
        fs::remove_file(&self.reservation).unwrap();
    }
}

#[test]
fn refresh_reuses_unchanged_decisions_and_pages_only_changed_comments() {
    let files = Files::new();
    let clock = NoWait;
    let mut capture: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-context.json"
    ))
    .unwrap();
    let target = capture["target"]["webUrl"].as_str().unwrap().to_owned();
    let thread_id = capture["threads"][0]["id"].as_str().unwrap().to_owned();
    let template = capture["threads"][0]["comments"][0].clone();
    let make_comment = |id: usize| {
        let mut comment = template.clone();
        comment["id"] = json!(id.to_string());
        comment["content"] = json!(format!("Human comment {id}"));
        comment
    };
    // The baseline is an arbitrary large set. Removing and editing one each
    // leaves reusable decisions; additions span several filtered query pages.
    capture["threads"][0]["comments"] = json!((1..=135).map(make_comment).collect::<Vec<_>>());
    capture["threads"][0]["openingCommentId"] = json!("1");
    prepare_capture(&files.baseline, None, &target, &clock)
        .unwrap()
        .publish(&capture, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let baseline_bytes = fs::read(&files.baseline).unwrap();
    let baseline = read_capture(&files.baseline, &clock).unwrap();
    let decisions: Vec<_> = (1..=135)
        .map(|id| json!({"threadId":thread_id,"commentId":id.to_string(),"kind":"human"}))
        .collect();
    write_comment_authorship(
        &files.baseline,
        &snapshot_id(&baseline).unwrap(),
        &json!(decisions),
        &clock,
    )
    .unwrap();
    let comments = capture["threads"][0]["comments"].as_array_mut().unwrap();
    comments.remove(0);
    comments[0]["content"] = json!("Edited without changing the provider timestamp");
    comments.extend((136..=146).map(make_comment));
    capture["threads"][0]["openingCommentId"] = json!("2");
    let summary = prepare_capture(&files.current, Some(&files.baseline), &target, &clock)
        .unwrap()
        .publish(&capture, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    assert_eq!(fs::read(&files.baseline).unwrap(), baseline_bytes);
    assert_eq!(summary["reusedClassificationCount"], 133);
    assert_eq!(summary["indeterminateCommentCount"], 12);
    assert_eq!(summary["delta"]["commentsAddedCount"], 11);
    assert_eq!(summary["delta"]["commentsEditedCount"], 1);
    assert_eq!(summary["delta"]["commentsRemovedCount"], 1);

    let current = read_capture(&files.current, &clock).unwrap();
    let options = json!({"query":"comments","authorship":"indeterminate","pageSize":5});
    let mut cursor = None;
    let mut ids = Vec::new();
    // The expected page count bounds progression even if a cursor is broken.
    for page_number in 0..3 {
        let output = query(&current, &options, None, cursor.as_deref()).unwrap();
        assert!(output.len() < 16 * 1024);
        let page: Value = serde_json::from_str(&output).unwrap();
        ids.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["commentId"].as_str().unwrap().to_owned()),
        );
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        assert_eq!(page["complete"], page_number == 2);
        assert_eq!(cursor.is_none(), page_number == 2);
    }
    assert_eq!(ids.len(), 12);
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 12);
    assert!(ids.contains(&"2".to_owned()));
    assert!(!ids.contains(&"1".to_owned()));
    assert!(!ids.contains(&"3".to_owned()));
}

#[test]
fn changed_authorship_inputs_are_reconsidered_on_refresh() {
    for changed_field in [
        "content",
        "author-id",
        "display-name",
        "account-type",
        "self-attribution",
        "self-unset",
        "provider-evidence",
    ] {
        let files = Files::new();
        let clock = NoWait;
        let mut capture: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/ss-review-native-capture/github-context.json"
        ))
        .unwrap();
        let target = capture["target"]["webUrl"].as_str().unwrap().to_owned();
        let thread_id = capture["threads"][0]["id"].clone();
        let comment_id = capture["threads"][0]["comments"][0]["id"].clone();
        let other_identity = capture["authenticatedIdentity"].clone();
        if changed_field == "self-unset" {
            capture["authenticatedIdentity"] =
                capture["threads"][0]["comments"][0]["author"].clone();
        }

        prepare_capture(&files.baseline, None, &target, &clock)
            .unwrap()
            .publish(&capture, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        let baseline = read_capture(&files.baseline, &clock).unwrap();
        write_comment_authorship(
            &files.baseline,
            &snapshot_id(&baseline).unwrap(),
            &json!([{"threadId":thread_id,"commentId":comment_id,"kind":"human"}]),
            &clock,
        )
        .unwrap();
        match changed_field {
            "content" => capture["threads"][0]["comments"][0]["content"] = json!("Edited"),
            "author-id" => capture["threads"][0]["comments"][0]["author"]["id"] = json!("other"),
            "display-name" => {
                capture["threads"][0]["comments"][0]["author"]["displayName"] = json!("Other");
            }
            "account-type" => {
                capture["threads"][0]["comments"][0]["author"]["accountType"] = json!("Mannequin");
            }
            "self-attribution" => {
                capture["authenticatedIdentity"] =
                    capture["threads"][0]["comments"][0]["author"].clone();
            }
            "self-unset" => {
                capture["authenticatedIdentity"] = other_identity;
            }
            "provider-evidence" => {
                capture["threads"][0]["comments"][0]["authorshipEvidence"] =
                    json!(["changed-proof"]);
            }
            _ => unreachable!(),
        }
        let summary = prepare_capture(&files.current, Some(&files.baseline), &target, &clock)
            .unwrap()
            .publish(&capture, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        assert_eq!(summary["reusedClassificationCount"], 0, "{changed_field}");
        let current = read_capture(&files.current, &clock).unwrap();
        assert_eq!(
            current["threads"][0]["comments"][0]["authorship"], "indeterminate",
            "{changed_field}"
        );
        assert_eq!(summary["delta"]["hasChanges"], true, "{changed_field}");
    }
}

#[test]
fn unchanged_decisions_survive_refresh_without_mutating_the_capture() {
    for changed_commit in [false, true] {
        let files = Files::new();
        let clock = NoWait;
        let mut capture: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/ss-review-native-capture/github-context.json"
        ))
        .unwrap();
        let mut second = capture["threads"][0]["comments"][0].clone();
        second["id"] = json!("second");
        capture["threads"][0]["comments"]
            .as_array_mut()
            .unwrap()
            .push(second);
        let thread_id = capture["threads"][0]["id"].clone();
        let comment_id = capture["threads"][0]["comments"][0]["id"].clone();
        let target = capture["target"]["webUrl"].as_str().unwrap().to_owned();
        prepare_capture(&files.baseline, None, &target, &clock)
            .unwrap()
            .publish(&capture, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        let baseline_bytes = fs::read(&files.baseline).unwrap();
        let baseline = read_capture(&files.baseline, &clock).unwrap();
        write_comment_authorship(
            &files.baseline,
            &snapshot_id(&baseline).unwrap(),
            &json!([
                {"threadId":thread_id,"commentId":comment_id,"kind":"human"},
                {"threadId":thread_id,"commentId":"second","kind":"agent"}
            ]),
            &clock,
        )
        .unwrap();
        if changed_commit {
            capture["pullRequest"]["reviewedSourceCommit"] = json!("f".repeat(40));
        }
        let summary = prepare_capture(&files.current, Some(&files.baseline), &target, &clock)
            .unwrap()
            .publish(&capture, "2026-08-05T12:00:00.0000000Z")
            .unwrap();
        assert_eq!(fs::read(&files.baseline).unwrap(), baseline_bytes);
        assert_eq!(summary["reusedClassificationCount"], 2);
        assert_eq!(summary["indeterminateCommentCount"], 0);
        let current = read_capture(&files.current, &clock).unwrap();
        let selected = query(
            &current,
            &json!({"query":"comments","authorship":"indeterminate"}),
            None,
            None,
        )
        .unwrap();
        let page: Value = serde_json::from_str(&selected).unwrap();
        assert!(page["items"].as_array().unwrap().is_empty());
        assert_eq!(page["complete"], true);
        let raw: Value = serde_json::from_slice(&fs::read(&files.current).unwrap()).unwrap();
        assert_eq!(
            raw["threads"][0]["comments"][0]["authorship"],
            "indeterminate"
        );
        if !changed_commit {
            assert_eq!(summary["delta"]["hasChanges"], false);
            assert_eq!(
                evidence_hash(&current).unwrap(),
                evidence_hash(&baseline).unwrap()
            );
        }
    }
}

#[test]
fn decisions_do_not_transfer_across_pull_request_repository_or_provider_identity() {
    for changed in ["pull-request", "repository", "provider"] {
        let files = Files::new();
        let clock = NoWait;
        let mut capture: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/ss-review-native-capture/github-context.json"
        ))
        .unwrap();
        let target = capture["target"]["webUrl"].as_str().unwrap().to_owned();
        let thread_id = capture["threads"][0]["id"].clone();
        let comment_id = capture["threads"][0]["comments"][0]["id"].clone();
        prepare_capture(&files.baseline, None, &target, &clock)
            .unwrap()
            .publish(&capture, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        let baseline = read_capture(&files.baseline, &clock).unwrap();
        write_comment_authorship(
            &files.baseline,
            &snapshot_id(&baseline).unwrap(),
            &json!([{"threadId":thread_id,"commentId":comment_id,"kind":"human"}]),
            &clock,
        )
        .unwrap();
        match changed {
            "pull-request" => {
                capture["target"]["webUrl"] = json!("https://github.com/octo/widget/pull/8");
            }
            "repository" => capture["target"]["repository"] = json!("other"),
            "provider" => {
                capture["target"]["platform"] = json!("azureDevOps");
                capture["target"]["collectionUrl"] = json!("https://dev.azure.com/org");
                capture["target"]["project"] = json!("project");
            }
            _ => unreachable!(),
        }
        let preparation =
            prepare_capture(&files.current, Some(&files.baseline), &target, &clock).unwrap();
        let result = preparation.publish(&capture, "2026-08-04T12:00:00.0000000Z");
        if changed == "pull-request" {
            assert!(result.is_err());
            assert!(!files.current.exists());
        } else {
            let summary = result.unwrap();
            assert_eq!(summary["reusedClassificationCount"], 0, "{changed}");
            let current = read_capture(&files.current, &clock).unwrap();
            assert_eq!(
                current["threads"][0]["comments"][0]["authorship"],
                "indeterminate"
            );
        }
    }
}

#[test]
fn decisions_from_an_old_policy_are_not_reused_and_can_be_reclassified() {
    let files = Files::new();
    let clock = NoWait;
    let mut capture: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-context.json"
    ))
    .unwrap();
    let mut second = capture["threads"][0]["comments"][0].clone();
    second["id"] = json!("second");
    capture["threads"][0]["comments"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let target = capture["target"]["webUrl"].as_str().unwrap().to_owned();
    let thread_id = capture["threads"][0]["id"].clone();
    let comment_id = capture["threads"][0]["comments"][0]["id"].clone();
    prepare_capture(&files.baseline, None, &target, &clock)
        .unwrap()
        .publish(&capture, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let baseline = read_capture(&files.baseline, &clock).unwrap();
    write_comment_authorship(
        &files.baseline,
        &snapshot_id(&baseline).unwrap(),
        &json!([
            {"threadId":thread_id,"commentId":comment_id,"kind":"human"},
            {"threadId":thread_id,"commentId":"second","kind":"agent"}
        ]),
        &clock,
    )
    .unwrap();
    let mut sidecar = files.baseline.as_os_str().to_os_string();
    sidecar.push(".authorship.json");
    let sidecar = PathBuf::from(sidecar);
    let mut record: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    record["policyVersion"] = json!("legacy-policy");
    fs::write(sidecar, serde_json::to_vec(&record).unwrap()).unwrap();
    let stale = read_capture(&files.baseline, &clock).unwrap();
    assert_eq!(
        stale["threads"][0]["comments"][0]["authorship"],
        "indeterminate"
    );
    assert_eq!(
        stale["threads"][0]["comments"][1]["authorship"],
        "indeterminate"
    );
    let result = prepare_capture(&files.current, Some(&files.baseline), &target, &clock)
        .unwrap()
        .publish(&capture, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    assert_eq!(result["reusedClassificationCount"], 0);
    assert_eq!(result["indeterminateCommentCount"], 2);
    write_comment_authorship(
        &files.baseline,
        &snapshot_id(&stale).unwrap(),
        &json!([{"threadId":thread_id,"commentId":comment_id,"kind":"agent"}]),
        &clock,
    )
    .unwrap();
    let classified = read_capture(&files.baseline, &clock).unwrap();
    assert_eq!(
        classified["threads"][0]["comments"][0]["authorship"],
        "agent"
    );
}

#[test]
fn current_provider_proof_overrides_the_prior_semantic_decision() {
    for proof in ["marker", "service"] {
        let files = Files::new();
        let clock = NoWait;
        let mut capture: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/ss-review-native-capture/github-context.json"
        ))
        .unwrap();
        let mut second = capture["threads"][0]["comments"][0].clone();
        second["id"] = json!("second");
        capture["threads"][0]["comments"]
            .as_array_mut()
            .unwrap()
            .push(second);
        let target = capture["target"]["webUrl"].as_str().unwrap().to_owned();
        let thread_id = capture["threads"][0]["id"].clone();
        let comment_id = capture["threads"][0]["comments"][0]["id"].clone();
        prepare_capture(&files.baseline, None, &target, &clock)
            .unwrap()
            .publish(&capture, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        let baseline = read_capture(&files.baseline, &clock).unwrap();
        write_comment_authorship(
            &files.baseline,
            &snapshot_id(&baseline).unwrap(),
            &json!([
                {"threadId":thread_id,"commentId":comment_id,"kind":"human"},
                {"threadId":thread_id,"commentId":"second","kind":"agent"}
            ]),
            &clock,
        )
        .unwrap();
        let changed = &mut capture["threads"][0]["comments"][0];
        changed["authorship"] = json!("agent");
        if proof == "marker" {
            changed["content"] = json!("[Copilot speaking] Updated.");
            changed["authorshipEvidence"] = json!(["marker:copilot"]);
        } else {
            changed["author"]["descriptor"] = json!("aadsp.SERVICE");
            changed["authorshipEvidence"] = json!(["identity:service"]);
        }
        let summary = prepare_capture(&files.current, Some(&files.baseline), &target, &clock)
            .unwrap()
            .publish(&capture, "2026-08-04T12:00:00.0000000Z")
            .unwrap();
        assert_eq!(summary["reusedClassificationCount"], 1, "{proof}");
        let current = read_capture(&files.current, &clock).unwrap();
        assert_eq!(current["threads"][0]["comments"][0]["authorship"], "agent");
    }
}

#[test]
fn newly_classified_comments_do_not_change_removed_baseline_evidence() {
    let files = Files::new();
    let clock = NoWait;
    let mut capture: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-context.json"
    ))
    .unwrap();
    let mut removed = capture["threads"][0].clone();
    removed["id"] = json!("removed-thread");
    removed["openingCommentId"] = json!("removed");
    removed["comments"][0]["id"] = json!("removed");
    capture["threads"].as_array_mut().unwrap().push(removed);
    let target = capture["target"]["webUrl"].as_str().unwrap().to_owned();
    let thread_id = capture["threads"][0]["id"].clone();
    let comment_id = capture["threads"][0]["comments"][0]["id"].clone();
    prepare_capture(&files.baseline, None, &target, &clock)
        .unwrap()
        .publish(&capture, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let baseline = read_capture(&files.baseline, &clock).unwrap();
    write_comment_authorship(
        &files.baseline,
        &snapshot_id(&baseline).unwrap(),
        &json!([
            {"threadId":thread_id,"commentId":comment_id,"kind":"human"},
            {"threadId":"removed-thread","commentId":"removed","kind":"agent"}
        ]),
        &clock,
    )
    .unwrap();
    capture["threads"].as_array_mut().unwrap().pop();
    let mut added = capture["threads"][0]["comments"][0].clone();
    added["id"] = json!("added");
    capture["threads"][0]["comments"]
        .as_array_mut()
        .unwrap()
        .push(added);
    let summary = prepare_capture(&files.current, Some(&files.baseline), &target, &clock)
        .unwrap()
        .publish(&capture, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    assert_eq!(summary["reusedClassificationCount"], 1);
    assert_eq!(summary["delta"]["commentsAddedCount"], 1);
    assert_eq!(summary["delta"]["commentsRemovedCount"], 1);
    let current = read_capture(&files.current, &clock).unwrap();
    let baseline = read_capture(&files.baseline, &clock).unwrap();
    let changes = query(&current, &json!({"query":"changes"}), Some(&baseline), None).unwrap();
    let changes: Value = serde_json::from_str(&changes).unwrap();
    let items = changes["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["change"] == "commentsAdded" && item["commentId"] == "added")
    );
    assert!(
        items
            .iter()
            .any(|item| item["change"] == "commentsRemoved" && item["commentId"] == "removed")
    );
    let applied = write_comment_authorship(
        &files.current,
        &snapshot_id(&current).unwrap(),
        &json!([{"threadId":thread_id,"commentId":"added","kind":"human"}]),
        &clock,
    )
    .unwrap();
    assert_eq!(applied["applied"], 1);
    assert_eq!(applied["isComplete"], true);
    let baseline = read_capture(&files.baseline, &clock).unwrap();
    assert_eq!(baseline["threads"][1]["comments"][0]["authorship"], "agent");
    let mut sidecar = files.current.as_os_str().to_os_string();
    sidecar.push(".authorship.json");
    let record: Value = serde_json::from_slice(&fs::read(PathBuf::from(sidecar)).unwrap()).unwrap();
    assert_eq!(record["decisions"][0]["origin"], "reused");
    assert_eq!(record["decisions"][1]["origin"], "classified");
}

#[test]
fn classification_invalidates_the_old_filtered_cursor_and_snapshot() {
    let files = Files::new();
    let clock = NoWait;
    let mut capture: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-context.json"
    ))
    .unwrap();
    let mut second = capture["threads"][0]["comments"][0].clone();
    second["id"] = json!("second");
    capture["threads"][0]["comments"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let target = capture["target"]["webUrl"].as_str().unwrap().to_owned();
    prepare_capture(&files.baseline, None, &target, &clock)
        .unwrap()
        .publish(&capture, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let baseline = read_capture(&files.baseline, &clock).unwrap();
    let options = json!({"query":"comments","authorship":"indeterminate","pageSize":1});
    let first: Value =
        serde_json::from_str(&query(&baseline, &options, None, None).unwrap()).unwrap();
    let cursor = first["nextCursor"].as_str().unwrap();
    let snapshot = first["snapshotId"].as_str().unwrap();
    write_comment_authorship(
        &files.baseline,
        snapshot,
        &json!([{"threadId":first["items"][0]["threadId"],"commentId":first["items"][0]["commentId"],"kind":"human"}]),
        &clock,
    )
    .unwrap();
    let effective = read_capture(&files.baseline, &clock).unwrap();
    let error = query(&effective, &options, None, Some(cursor)).unwrap_err();
    assert!(error.to_string().contains("Stale"));
    let error =
        write_comment_authorship(&files.baseline, snapshot, &json!([]), &clock).unwrap_err();
    assert!(error.to_string().contains("snapshot changed"));
    let next: Value =
        serde_json::from_str(&query(&effective, &options, None, None).unwrap()).unwrap();
    assert_eq!(next["items"][0]["commentId"], "second");
    assert_eq!(next["complete"], true);
}

#[test]
fn malformed_sidecars_and_capture_replacement_are_rejected() {
    let files = Files::new();
    let clock = NoWait;
    let capture: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-context.json"
    ))
    .unwrap();
    let target = capture["target"]["webUrl"].as_str().unwrap().to_owned();
    let thread_id = capture["threads"][0]["id"].clone();
    let comment_id = capture["threads"][0]["comments"][0]["id"].clone();
    prepare_capture(&files.baseline, None, &target, &clock)
        .unwrap()
        .publish(&capture, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let baseline = read_capture(&files.baseline, &clock).unwrap();
    write_comment_authorship(
        &files.baseline,
        &snapshot_id(&baseline).unwrap(),
        &json!([{"threadId":thread_id,"commentId":comment_id,"kind":"human"}]),
        &clock,
    )
    .unwrap();
    let mut sidecar = files.baseline.as_os_str().to_os_string();
    sidecar.push(".authorship.json");
    let sidecar = PathBuf::from(sidecar);
    let original = fs::read(&sidecar).unwrap();
    let mut record: Value = serde_json::from_slice(&original).unwrap();
    record["schemaVersion"] = json!(99);
    fs::write(&sidecar, serde_json::to_vec(&record).unwrap()).unwrap();
    read_capture(&files.baseline, &clock).unwrap_err();
    record["schemaVersion"] = json!(1);
    record["decisions"][0]["extra"] = json!("not permitted");
    fs::write(&sidecar, serde_json::to_vec(&record).unwrap()).unwrap();
    read_capture(&files.baseline, &clock).unwrap_err();
    fs::write(&sidecar, &original).unwrap();
    let Err(_) = prepare_capture(&files.baseline, None, &target, &clock) else {
        panic!("existing capture must not be replaced");
    };
    let mut replacement: Value =
        serde_json::from_slice(&fs::read(&files.baseline).unwrap()).unwrap();
    replacement["threads"][0]["comments"][0]["content"] = json!("replaced snapshot");
    fs::write(&files.baseline, serde_json::to_vec(&replacement).unwrap()).unwrap();
    let error = read_capture(&files.baseline, &clock).unwrap_err();
    assert!(error.to_string().contains("do not belong"));
}

#[test]
fn malformed_decision_drafts_do_not_change_the_existing_sidecar() {
    let files = Files::new();
    let clock = NoWait;
    let capture: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-context.json"
    ))
    .unwrap();
    let target = capture["target"]["webUrl"].as_str().unwrap().to_owned();
    let thread_id = capture["threads"][0]["id"].clone();
    let comment_id = capture["threads"][0]["comments"][0]["id"].clone();
    prepare_capture(&files.baseline, None, &target, &clock)
        .unwrap()
        .publish(&capture, "2026-08-04T12:00:00.0000000Z")
        .unwrap();
    let baseline = read_capture(&files.baseline, &clock).unwrap();
    let snapshot = snapshot_id(&baseline).unwrap();
    let mut sidecar = files.baseline.as_os_str().to_os_string();
    sidecar.push(".authorship.json");
    let sidecar = PathBuf::from(sidecar);
    let before = fs::read(&sidecar).unwrap();
    for entry in [
        json!({"threadId":thread_id,"commentId":comment_id}),
        json!({"threadId":thread_id,"commentId":comment_id,"kind":"human","unexpected":true}),
        json!({"threadId":[thread_id],"commentId":comment_id,"kind":"human"}),
        json!({"threadId":thread_id,"commentId":"","kind":"human"}),
    ] {
        write_comment_authorship(&files.baseline, &snapshot, &json!([entry]), &clock).unwrap_err();
        assert_eq!(fs::read(&sidecar).unwrap(), before);
    }
}
