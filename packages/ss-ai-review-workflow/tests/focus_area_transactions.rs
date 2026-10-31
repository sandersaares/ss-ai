//! Exercises coordinated focus-area transactions against actual files.
#![allow(
    clippy::indexing_slicing,
    reason = "The tests index small JSON fixtures whose shape is constructed in the test"
)]

use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::json;
use ss_ai_review_workflow::focus_area_expansion::{
    Operation, ReferenceInput, ReferenceResult, update,
};
use ss_ai_review_workflow::focus_area_map::{MapOperation, assemble, update as update_map};
use ss_ai_review_workflow::review_work;
use ss_ai_runtime::{Clock, LockMode, ResourceLock};

/// Unexpected contention fails immediately rather than sleeping in a test.
struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::MIN
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Unexpected contention in an isolated fixture")
    }
}

/// Reports contention and waits for publication before permitting a retry.
struct PublicationClock {
    events: Sender<bool>,
    release: Receiver<()>,
    waited: Cell<bool>,
}

impl Clock for PublicationClock {
    fn now(&self) -> Timestamp {
        Timestamp::MIN
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        assert!(!self.waited.replace(true), "unexpected repeated contention");
        self.events.send(true).unwrap();
        self.release.recv().unwrap();
        Ok(())
    }
}

/// Keeps each test's generated files in its own project-local directory.
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // The counter only supplies unique fixture names; it does not publish shared data.
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("focus-area-test-{}-{index}", std::process::id()));
        for directory in ["instruction-set-source", "instruction-set-raw"] {
            fs::create_dir_all(root.join(directory)).unwrap();
            fs::write(root.join(directory).join("guide.md"), "# Rule\nKeep it.\n").unwrap();
        }
        Self(root)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Produces an agent map with exact source line ranges.
fn map() -> Vec<u8> {
    let span = |root| json!({"root":root,"path":"guide.md","startLine":1,"endLine":2});
    serde_json::to_vec(
        &json!({"schemaVersion":1,"categories":[{"id":"rules","title":"Rules"}],
        "chapters":[{"id":"rule","categoryId":"rules","content":span("instruction-set-raw"),
            "sources":[span("instruction-set-source")],"applicability":[{
                "effectiveScope":"No explicit restriction; interpret the guideline in context.",
                "declarations":[],"origins":[],"uncertainties":[]}]}]}),
    )
    .unwrap()
}

#[test]
fn preparation_readers_wait_for_complete_source_publication() {
    for operation in ["validate", "update", "assemble", "expansion"] {
        let fixture = Fixture::new();
        let session = &fixture.0;
        let clock = TestClock;
        update(session, Operation::Initialize, &clock).unwrap();
        update(session, Operation::Finalize, &clock).unwrap();
        let map_path = session.join("map.json");
        update_map(session, &map_path, MapOperation::Append(map()), &clock).unwrap();
        let lock = ResourceLock::open(
            &session.join("focus-area-cache-operation"),
            LockMode::Exclusive,
            &clock,
        )
        .unwrap();
        let source = session.join("instruction-set-source");
        let backup = session.join("instruction-set-source.previous");
        fs::rename(&source, &backup).unwrap();
        thread::scope(|scope| {
            let (events_tx, events_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let worker = scope.spawn(move || {
                let clock = PublicationClock {
                    events: events_tx.clone(),
                    release: release_rx,
                    waited: Cell::new(false),
                };
                let result = match operation {
                    "validate" => ss_ai_review_workflow::focus_area_map::validate(
                        session, &map_path, &clock, false,
                    ),
                    "update" => update_map(
                        session,
                        &session.join("other-map.json"),
                        MapOperation::Append(map()),
                        &clock,
                    ),
                    "assemble" => assemble(session, &map_path, &clock),
                    "expansion" => update(session, Operation::Read(None), &clock),
                    _ => unreachable!(),
                };
                events_tx.send(false).unwrap();
                result
            });
            let waited = events_rx.recv().unwrap();
            fs::rename(&backup, &source).unwrap();
            drop(lock);
            if waited {
                release_tx.send(()).unwrap();
            }
            let result = worker.join().unwrap();
            assert!(
                waited,
                "{operation} completed without waiting for publication"
            );
            result.unwrap();
        });
    }
}

#[test]
fn assembles_complete_sources_and_keeps_old_artifacts_on_invalid_input() {
    let fixture = Fixture::new();
    let session = &fixture.0;
    let clock = TestClock;
    update(session, Operation::Initialize, &clock).unwrap();
    update(session, Operation::Finalize, &clock).unwrap();
    let path = session.join("map.json");
    update_map(session, &path, MapOperation::Append(map()), &clock).unwrap();
    assemble(session, &path, &clock).unwrap();
    let output = session.join("focus-areas/focus-rules.md");
    let original = fs::read(&output).unwrap();
    assert!(String::from_utf8_lossy(&original).contains("Keep it."));
    fs::write(
        session.join("instruction-set-raw/uncovered.md"),
        "Uncovered\n",
    )
    .unwrap();
    assemble(session, &path, &clock).unwrap_err();
    assert_eq!(fs::read(output).unwrap(), original);
}

#[test]
fn reference_transactions_retain_all_late_alternatives() {
    let fixture = Fixture::new();
    let session = &fixture.0;
    let clock = TestClock;
    update(session, Operation::Initialize, &clock).unwrap();
    let input = |scope| {
        serde_json::from_value::<ReferenceInput>(json!({
        "id":"ref", "uri":"https://example.invalid/ref", "referrers":[{
            "parentId":null,"source":{"root":"instruction-set-source","path":"guide.md","startLine":1,"endLine":2},
            "scope":scope,"declarations":[]
        }]
    })).unwrap()
    };
    update(session, Operation::Register(input("First")), &clock).unwrap();
    let completed = update(
        session,
        Operation::Complete(ReferenceResult {
            reference_id: "ref".into(),
            outputs: vec!["guide.md".into()],
            error: None,
        }),
        &clock,
    )
    .unwrap();
    assert_eq!(completed["contexts"].as_array().unwrap().len(), 1);
    update(session, Operation::Register(input("Second")), &clock).unwrap();
    let result = update(session, Operation::Read(Some("ref".into())), &clock).unwrap();
    assert_eq!(result["contexts"].as_array().unwrap().len(), 2);
    update(session, Operation::Finalize, &clock).unwrap();
    update(session, Operation::Register(input("Third")), &clock).unwrap_err();
}

#[test]
fn assembled_path_scope_is_accepted_by_the_review_consumer() {
    let fixture = Fixture::new();
    let session = &fixture.0;
    let clock = TestClock;
    for root in ["instruction-set-source", "instruction-set-raw"] {
        fs::write(
            session.join(root).join("guide.md"),
            "Scope: **/*.ps1\n# Rule\nKeep it.\n",
        )
        .unwrap();
    }
    update(session, Operation::Initialize, &clock).unwrap();
    update(session, Operation::Finalize, &clock).unwrap();
    let mut scoped: serde_json::Value = serde_json::from_slice(&map()).unwrap();
    scoped["chapters"][0]["content"]["startLine"] = json!(2);
    scoped["chapters"][0]["content"]["endLine"] = json!(3);
    scoped["chapters"][0]["sources"][0]["startLine"] = json!(2);
    scoped["chapters"][0]["sources"][0]["endLine"] = json!(3);
    scoped["chapters"][0]["applicability"][0]["effectiveScope"] = json!("PowerShell files");
    scoped["chapters"][0]["applicability"][0]["pathScope"] =
        json!({"base":".","include":["**/*.ps1"]});
    scoped["chapters"][0]["applicability"][0]["declarations"] = json!([{
        "root":"instruction-set-raw","path":"guide.md","startLine":1,"endLine":1
    }]);
    let map_path = session.join("map.json");
    update_map(
        session,
        &map_path,
        MapOperation::Append(serde_json::to_vec(&scoped).unwrap()),
        &clock,
    )
    .unwrap();
    assemble(session, &map_path, &clock).unwrap();
    let file = json!({"path":session.join("source.rs").to_str().unwrap(), "sizeBytes":1, "change":"modified"});
    let inventory = session.join("inventory.json");
    let groups = session.join("groups.json");
    fs::write(&inventory, serde_json::to_vec(&json!([file])).unwrap()).unwrap();
    fs::write(
        &groups,
        serde_json::to_vec(&json!([{"groupId":"group-1","files":[file]}])).unwrap(),
    )
    .unwrap();
    let result = review_work::create(
        &inventory,
        &groups,
        &session.join("focus-areas"),
        &session.join("work"),
        Some(session),
        Some(&session.join("findings")),
        &clock,
    )
    .unwrap();
    assert_eq!(result["excludedReviewerCount"], 1);
}

#[test]
fn assembly_rejects_late_referrer_loss_in_descendants() {
    let fixture = Fixture::new();
    let session = &fixture.0;
    let clock = TestClock;
    fs::write(
        session.join("instruction-set-raw/parent.md"),
        "# Parent\nChild link\n",
    )
    .unwrap();
    fs::write(session.join("instruction-set-raw/child.md"), "# Child\n").unwrap();
    update(session, Operation::Initialize, &clock).unwrap();
    let span =
        |root, path, start, end| json!({"root":root,"path":path,"startLine":start,"endLine":end});
    let direct = |line| {
        serde_json::from_value::<ReferenceInput>(json!({"id":"parent","uri":"https://example.invalid/parent",
            "referrers":[{"parentId":null,"source":span("instruction-set-source","guide.md",line,line),
                "scope":format!("scope {line}"),
                "declarations":[span("instruction-set-source","guide.md",line,line)]}]})).unwrap()
    };
    update(session, Operation::Register(direct(1)), &clock).unwrap();
    update(
        session,
        Operation::Complete(ReferenceResult {
            reference_id: "parent".into(),
            outputs: vec!["parent.md".into()],
            error: None,
        }),
        &clock,
    )
    .unwrap();
    let child: ReferenceInput =
        serde_json::from_value(json!({"id":"child","uri":"https://example.invalid/child",
        "referrers":[{"parentId":"parent","source":span("instruction-set-raw","parent.md",2,2),
            "scope":"Nested scope","declarations":[]}]}))
        .unwrap();
    update(session, Operation::Register(child), &clock).unwrap();
    update(
        session,
        Operation::Complete(ReferenceResult {
            reference_id: "child".into(),
            outputs: vec!["child.md".into()],
            error: None,
        }),
        &clock,
    )
    .unwrap();
    update(session, Operation::Register(direct(2)), &clock).unwrap();
    update(session, Operation::Finalize, &clock).unwrap();
    let alternative = |line, origins| {
        json!({"effectiveScope":"Agent-authored scope",
        "origins":origins,"declarations":[span("instruction-set-source","guide.md",line,line)],
        "uncertainties":[],"pathScope":null})
    };
    let parent_origins = json!(["https://example.invalid/parent"]);
    let child_origins = json!([
        "https://example.invalid/parent",
        "https://example.invalid/child"
    ]);
    let chapter = |id, path, end, alternatives| {
        json!({"id":id,"categoryId":"rules",
        "content":span("instruction-set-raw",path,1,end),
        "sources":[span("instruction-set-raw",path,1,end)],"applicability":alternatives})
    };
    let parent = chapter(
        "parent",
        "parent.md",
        2,
        json!([
            alternative(1, parent_origins.clone()),
            alternative(2, parent_origins)
        ]),
    );
    let mut child = chapter(
        "child",
        "child.md",
        1,
        json!([alternative(1, child_origins.clone())]),
    );
    let direct_chapter = chapter(
        "direct",
        "guide.md",
        2,
        json!([{
            "effectiveScope":"Unrestricted","declarations":[],"origins":[],"uncertainties":[],"pathScope":null
        }]),
    );
    let map_path = session.join("map.json");
    let map = json!({"schemaVersion":1,"categories":[{"id":"rules","title":"Rules"}],
        "chapters":[direct_chapter,parent,child]});
    update_map(
        session,
        &map_path,
        MapOperation::Append(serde_json::to_vec(&map).unwrap()),
        &clock,
    )
    .unwrap();
    fs::create_dir_all(session.join("focus-areas")).unwrap();
    fs::write(session.join("focus-areas/previous.md"), "Previous\n").unwrap();
    fs::write(
        session.join("instruction-set-categories.md"),
        "Previous catalog\n",
    )
    .unwrap();
    assemble(session, &map_path, &clock).unwrap_err();
    assert_eq!(
        fs::read_to_string(session.join("focus-areas/previous.md")).unwrap(),
        "Previous\n"
    );
    child["applicability"]
        .as_array_mut()
        .unwrap()
        .push(alternative(2, child_origins));
    let correction =
        json!({"schemaVersion":1,"categories":[{"id":"rules","title":"Rules"}],"chapters":[child]});
    update_map(
        session,
        &map_path,
        MapOperation::Replace(serde_json::to_vec(&correction).unwrap()),
        &clock,
    )
    .unwrap();
    assemble(session, &map_path, &clock).unwrap();
    assert!(!session.join("focus-areas/previous.md").exists());
}
