//! Exercises source identity and coverage against real filesystem paths.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use ss_ai_review_workflow::focus_area_expansion::{Operation, update};
#[cfg(unix)]
use ss_ai_review_workflow::focus_area_map::{MapOperation, assemble, update as update_map};
use ss_ai_review_workflow::focus_area_source::{SourceSnapshot, Span};
use ss_ai_runtime::Clock;

/// Unexpected contention fails immediately in an isolated fixture.
struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::MIN
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Unexpected contention in an isolated fixture")
    }
}

/// Keeps each source tree local to this checkout.
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // The counter only assigns unique fixture names.
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("focus-source-test-{}-{index}", std::process::id()));
        for root in ["instruction-set-source", "instruction-set-raw"] {
            fs::create_dir_all(path.join(root)).unwrap();
            fs::write(path.join(root).join("guide.md"), "# Rule\nKeep it.\n").unwrap();
        }
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Builds an already published, complete map for failure-preservation checks.
#[cfg(unix)]
fn assemble_fixture(fixture: &Fixture) -> PathBuf {
    let session = &fixture.0;
    update(session, Operation::Initialize, &TestClock).unwrap();
    update(session, Operation::Finalize, &TestClock).unwrap();
    let span = |root| json!({"root":root,"path":"guide.md","startLine":1,"endLine":2});
    let map = json!({"schemaVersion":1,"categories":[{"id":"rules","title":"Rules"}],
        "chapters":[{"id":"rule","categoryId":"rules","content":span("instruction-set-raw"),
            "sources":[span("instruction-set-source")],"applicability":[{
                "effectiveScope":"Unrestricted","declarations":[],"origins":[],"uncertainties":[]}]}]});
    let path = session.join("map.json");
    update_map(
        session,
        &path,
        MapOperation::Append(serde_json::to_vec(&map).unwrap()),
        &TestClock,
    )
    .unwrap();
    assemble(session, &path, &TestClock).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn colliding_files_are_rejected_before_publication() {
    for root in ["instruction-set-source", "instruction-set-raw"] {
        let fixture = Fixture::new();
        let map = assemble_fixture(&fixture);
        let output = fixture.0.join("focus-areas/focus-rules.md");
        let catalog = fixture.0.join("instruction-set-categories.md");
        let original_output = fs::read(&output).unwrap();
        let original_catalog = fs::read(&catalog).unwrap();
        let tree = fixture.0.join(root);
        fs::create_dir_all(tree.join("nested")).unwrap();
        let nested = tree.join("nested/rule.md");
        let literal = tree.join("nested\\rule.md");
        fs::write(&nested, "Nested source\n").unwrap();
        fs::write(&literal, "Different source\n").unwrap();

        let error = SourceSnapshot::read(&fixture.0, &TestClock).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("collid"), "{message}");
        assert!(message.contains(&format!("{nested:?}")), "{message}");
        assert!(message.contains(&format!("{literal:?}")), "{message}");
        assert!(message.contains("rename"), "{message}");
        assemble(&fixture.0, &map, &TestClock).unwrap_err();
        assert_eq!(fs::read(output).unwrap(), original_output);
        assert_eq!(fs::read(catalog).unwrap(), original_catalog);
        fs::remove_file(literal).unwrap();
        if root == "instruction-set-raw" {
            let error = assemble(&fixture.0, &map, &TestClock).unwrap_err();
            assert!(error.to_string().contains("Uncovered raw input line"));
        }
        fs::remove_file(nested).unwrap();
        assemble(&fixture.0, &map, &TestClock).unwrap();
    }
}

#[test]
fn separate_roots_and_separator_aliases_preserve_sources() {
    let fixture = Fixture::new();
    for root in ["instruction-set-source", "instruction-set-raw"] {
        let tree = fixture.0.join(root);
        fs::create_dir_all(tree.join("nested")).unwrap();
        fs::write(tree.join("nested/rule.md"), root).unwrap();
    }
    let snapshot = SourceSnapshot::read(&fixture.0, &TestClock).unwrap();
    for root in ["instruction-set-source", "instruction-set-raw"] {
        for path in ["nested/rule.md", "nested\\rule.md"] {
            let span = Span {
                root: root.into(),
                path: path.into(),
                start_line: 1,
                end_line: 1,
            };
            assert_eq!(snapshot.span(&span).unwrap(), [root]);
        }
    }
    assert_eq!(snapshot.files().count(), 4);
}

#[cfg(unix)]
#[test]
fn lone_literal_backslash_is_still_addressable() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("instruction-set-raw/nested\\rule.md"),
        "Literal name",
    )
    .unwrap();
    let snapshot = SourceSnapshot::read(&fixture.0, &TestClock).unwrap();
    assert_eq!(
        snapshot.raw_output("nested/rule.md").unwrap(),
        ["Literal name"]
    );
}

#[test]
fn distinct_case_is_preserved_when_the_filesystem_supports_it() {
    let fixture = Fixture::new();
    let tree = fixture.0.join("instruction-set-raw");
    fs::write(tree.join("Case.md"), "Upper").unwrap();
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tree.join("case.md"))
    {
        Ok(_) => fs::write(tree.join("case.md"), "Lower").unwrap(),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => return,
        Err(error) => panic!("Cannot probe filename case behavior: {error}"),
    }
    let snapshot = SourceSnapshot::read(&fixture.0, &TestClock).unwrap();
    assert_eq!(snapshot.raw_output("Case.md").unwrap(), ["Upper"]);
    assert_eq!(snapshot.raw_output("case.md").unwrap(), ["Lower"]);
}
