//! Tests review-work input boundaries independently of retired scripts.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixtures use fixed record shapes and assert input failures"
)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::review_work;
use ss_ai_runtime::{Clock, fail};

/// Makes unexpected lock contention fail without waiting.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected test lock contention"))
    }
}

/// Isolates each input matrix from other test processes.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".migration-inputs-{}-{name}", process::id()));
        #[expect(clippy::create_dir, reason = "Existing test state must not be reused")]
        fs::create_dir(&path).unwrap();
        fs::create_dir_all(path.join("focus")).unwrap();
        fs::write(path.join("focus/area.md"), "Focus").unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn put(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Exercises creation so invalid inputs cannot leave a work directory.
fn create(workspace: &Workspace) -> bool {
    create_result(workspace).is_ok()
}

/// Exposes the diagnostic from review-work creation for input repair.
fn create_result(workspace: &Workspace) -> Result<Value, AppError> {
    review_work::create(
        &workspace.path("inventory.json"),
        &workspace.path("groups.json"),
        &workspace.path("focus"),
        &workspace.path("work"),
        None,
        None,
        &FixedClock,
    )
}

#[test]
fn migration_rejects_bad_inventories_without_creating_work() {
    let workspace = Workspace::new("inventory");
    let file = json!({"path":workspace.path("a.rs"),"sizeBytes":1,"change":"modified"});
    workspace.put("groups.json", &json!([{"groupId":"one","files":[file]}]));
    for text in ["[]", "{}", "null", "{broken"] {
        fs::write(workspace.path("inventory.json"), text).unwrap();
        assert!(!create(&workspace), "{text}");
        assert!(!workspace.path("work").exists());
    }
    for file in [
        json!({"path":"relative.rs","sizeBytes":1,"change":"modified"}),
        json!({"path":workspace.path("a.rs"),"sizeBytes":-1,"change":"modified"}),
        json!({"path":workspace.path("a.rs"),"sizeBytes":"1","change":"modified"}),
        json!({"path":workspace.path("a.rs"),"sizeBytes":1,"change":"ignored"}),
    ] {
        workspace.put("inventory.json", &json!([file]));
        assert!(!create(&workspace), "{file}");
        assert!(!workspace.path("work").exists());
    }
    workspace.put("inventory.json", &json!([file, file]));
    assert!(
        create_result(&workspace)
            .unwrap_err()
            .to_string()
            .contains("repeats")
    );
    assert!(!workspace.path("work").exists());
}

#[test]
fn migration_rejects_malformed_and_inexact_group_coverage() {
    let workspace = Workspace::new("groups");
    let first = json!({"path":workspace.path("new.rs"),"sizeBytes":1,"change":"new"});
    let second = json!({"path":workspace.path("deleted.rs"),"sizeBytes":3,"change":"deleted"});
    workspace.put("inventory.json", &json!([first, second]));
    for text in ["[]", "{}", "null", "{broken"] {
        fs::write(workspace.path("groups.json"), text).unwrap();
        assert!(!create(&workspace), "{text}");
        assert!(!workspace.path("work").exists());
    }
    for (groups, diagnostic) in [
        (json!([{"groupId":"one","files":[]}]), "file count"),
        (json!([{"groupId":"one","files":{}}]), "files"),
        (json!([{"files":[]}]), "groupId"),
        (json!([{"groupId":"one","files":[first]}]), "deleted.rs"),
        (
            json!([{"groupId":"one","files":[first,first,second]}]),
            "Duplicate",
        ),
        (
            json!([{"groupId":"one","files":[first]},{"groupId":"two","files":[first,second]}]),
            "Duplicate",
        ),
        (
            json!([{"groupId":"one","files":[first]},{"groupId":"one","files":[second]}]),
            "group identifier",
        ),
        (
            json!([{"groupId":"one","files":[first]},{"groupId":"../group","files":[second]}]),
            "group identifier",
        ),
        (
            json!([{"groupId":"one","files":[first]},{"groupId":"","files":[second]}]),
            "groupId",
        ),
        (
            json!([{"groupId":"one","files":[first]},{"groupId":"two","files":[{"path":workspace.path("other.rs"),"sizeBytes":3,"change":"deleted"}]}]),
            "outside inventory",
        ),
        (
            json!([{"groupId":"one","files":[first]},{"groupId":"two","files":[{"path":second["path"],"sizeBytes":4,"change":"deleted"}]}]),
            "changed inventory metadata",
        ),
        (
            json!([{"groupId":"one","files":[first]},{"groupId":"two","files":[{"path":second["path"],"sizeBytes":"3","change":"deleted"}]}]),
            "changed inventory metadata",
        ),
        (
            json!([{"groupId":"one","files":[first]},{"groupId":"two","files":[{"path":second["path"],"sizeBytes":3,"change":"modified"}]}]),
            "changed inventory metadata",
        ),
    ] {
        workspace.put("groups.json", &groups);
        let error = create_result(&workspace).unwrap_err();
        assert!(error.to_string().contains(diagnostic), "{groups}: {error}");
        assert!(!workspace.path("work").exists());
    }
}

#[test]
fn migration_omitted_paths_are_identified_at_creation_and_queue_initialization() {
    let workspace = Workspace::new("omitted-diagnostic");
    let files = json!([
        {"path":workspace.path("new.rs"),"sizeBytes":1,"change":"new"},
        {"path":workspace.path("deleted.rs"),"sizeBytes":3,"change":"deleted"}
    ]);
    let inventory = workspace.put("inventory.json", &files);
    let groups = workspace.put(
        "groups.json",
        &json!([{"groupId":"one","files":[files[0]]}]),
    );
    let error = review_work::create(
        &inventory,
        &groups,
        &workspace.path("focus"),
        &workspace.path("work"),
        None,
        None,
        &FixedClock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("deleted.rs"), "{error}");
    assert!(!workspace.path("work").exists());
    let queue = workspace.path("queue.json");
    let error =
        review_work::initialize_queue(&inventory, &groups, &queue, false, &FixedClock).unwrap_err();
    assert!(error.to_string().contains("deleted.rs"), "{error}");
    assert!(!queue.exists());
}

#[test]
fn migration_empty_focus_and_missing_queue_are_not_completed_work() {
    let workspace = Workspace::new("empty-focus");
    let file = json!({"path":workspace.path("file.rs"),"sizeBytes":1,"change":"modified"});
    let inventory = workspace.put("inventory.json", &json!([file]));
    let groups = workspace.put("groups.json", &json!([{"groupId":"one","files":[file]}]));
    fs::write(workspace.path("focus/area.md"), " ").unwrap();
    assert!(!create(&workspace));
    fs::remove_file(workspace.path("focus/area.md")).unwrap();
    assert!(!create(&workspace));
    let queue = workspace.path("queue.json");
    let result =
        review_work::initialize_queue(&inventory, &groups, &queue, true, &FixedClock).unwrap();
    assert_eq!(result["completedGroupCount"], 0);
    assert!(result["inProgressGroupId"].is_null());
    assert_eq!(result["remainingCount"], 1);
}

#[test]
fn migration_group_limits_accept_boundaries_and_oversized_singletons() {
    let workspace = Workspace::new("limits");
    for (count, accepted) in [(20, true), (21, false)] {
        let files: Vec<_> = (0..count)
            .map(|index| json!({"path":workspace.path(&format!("{index}.rs")),"sizeBytes":1,"change":"modified"}))
            .collect();
        let inventory = workspace.put("inventory.json", &json!(files));
        let groups = workspace.put("groups.json", &json!([{"groupId":"one","files":files}]));
        let output = workspace.path("published.json");
        let result = review_work::set_input_groups(&inventory, &groups, &output, true, &FixedClock);
        assert_eq!(result.is_ok(), accepted, "count {count}");
        if let Err(error) = &result {
            assert!(error.to_string().contains("file count"), "{error}");
        }
        let work = review_work::create(
            &inventory,
            &groups,
            &workspace.path("focus"),
            &workspace.path("work"),
            None,
            None,
            &FixedClock,
        );
        assert_eq!(work.is_ok(), accepted, "count {count}");
        if let Ok(work) = work {
            assert_eq!(work["groupCount"], 1);
            assert_eq!(work["fileCount"], count);
            fs::remove_dir_all(workspace.path("work")).unwrap();
        }
    }
    for (bytes, accepted) in [(100_000, true), (100_001, false)] {
        let files = json!([
            {"path":workspace.path("a.rs"),"sizeBytes":bytes - 1,"change":"modified"},
            {"path":workspace.path("b.rs"),"sizeBytes":1,"change":"modified"}
        ]);
        let inventory = workspace.put("inventory.json", &files);
        let groups = workspace.put("groups.json", &json!([{"groupId":"one","files":files}]));
        let result = review_work::set_input_groups(
            &inventory,
            &groups,
            &workspace.path("published.json"),
            true,
            &FixedClock,
        );
        assert_eq!(result.is_ok(), accepted, "bytes {bytes}");
        if let Err(error) = &result {
            assert!(error.to_string().contains("byte limit"), "{error}");
        }
        assert_eq!(create(&workspace), accepted, "bytes {bytes}");
        if accepted {
            fs::remove_dir_all(workspace.path("work")).unwrap();
        }
    }
    let file = json!({"path":workspace.path("large.rs"),"sizeBytes":100_001,"change":"modified"});
    let inventory = workspace.put("inventory.json", &json!([file]));
    let groups = workspace.put("groups.json", &json!([{"groupId":"one","files":[file]}]));
    let clock = FixedClock;
    let work = review_work::create(
        &inventory,
        &groups,
        &workspace.path("focus"),
        &workspace.path("work"),
        None,
        None,
        &clock,
    )
    .unwrap();
    assert_eq!(work["groupCount"], 1);
    let worker = review_work::next(
        &PathBuf::from(work["focusAreaQueuePath"].as_str().unwrap()),
        &clock,
    )
    .unwrap();
    let queue = PathBuf::from(worker["item"]["fileGroupQueuePath"].as_str().unwrap());
    review_work::initialize_queue(
        &PathBuf::from(work["inventoryPath"].as_str().unwrap()),
        &PathBuf::from(work["fileGroupsPath"].as_str().unwrap()),
        &queue,
        false,
        &clock,
    )
    .unwrap();
    let claim = review_work::next(&queue, &clock).unwrap();
    assert_eq!(claim["item"]["files"], json!([file]));
    assert_eq!(claim["exhausted"], false);
    let assessment = json!({"groupId":"one","path":file["path"],
        "disposition":"reviewed","scope":"","reason":""});
    review_work::set_assessment(
        &queue,
        &PathBuf::from(work["inventoryPath"].as_str().unwrap()),
        &PathBuf::from(work["fileGroupsPath"].as_str().unwrap()),
        &assessment,
        &clock,
    )
    .unwrap();
    review_work::complete_group(&queue, "one", &clock).unwrap();
    assert_eq!(
        review_work::next(&queue, &clock).unwrap()["exhausted"],
        true
    );
}
