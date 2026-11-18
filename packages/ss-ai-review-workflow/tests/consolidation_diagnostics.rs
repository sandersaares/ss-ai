//! Actionable diagnostics for invalid reviewer and merge evidence.

#![allow(
    clippy::indexing_slicing,
    reason = "The test uses fixed JSON fixture shapes"
)]

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::findings;
use ss_ai_review_workflow::overview_validation;
use ss_ai_runtime::{Clock, classification, fail};

/// Rejects unexpected contention without introducing real-time waits.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected lock contention"))
    }
}

/// Keeps each test's artifacts and atomic staging files in the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".consolidation-diagnostics-{}-{name}",
            std::process::id()
        ));
        #[expect(clippy::create_dir, reason = "Fixture collisions must fail")]
        fs::create_dir(&path).unwrap();
        #[expect(clippy::create_dir, reason = "The directory must start empty")]
        fs::create_dir(path.join("reviewers")).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write(&self, name: &str, value: &Value) -> PathBuf {
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

/// Creates a complete reviewer finding for validation and consolidation.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Callers construct each fixture inline"
)]
fn finding(id: &str, anchor: Value) -> Value {
    json!({"schemaVersion":1,"id":id,"origin":"reviewer","sourceFindingIds":[],
        "title":"Problem","description":"Details","reasoning":"Why",
        "consequence":"Impact","recommendedAction":"Fix","anchor":anchor,
        "references":[],"impactedLocations":[]})
}

/// Wraps findings in a complete reviewer artifact.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Callers construct each fixture inline"
)]
fn reviewer(id: &str, findings: Value) -> Value {
    json!({"schemaVersion":1,"kind":"reviewer","reviewerId":id,
        "focusArea":"correctness","status":"complete","findings":findings})
}

/// Checks both the structured failure category and the actionable details.
fn assert_invalid(error: &AppError, details: &[&str]) {
    assert_eq!(classification(error), "invalidInput");
    let message = error.to_string();
    for detail in details {
        assert!(message.contains(detail), "missing {detail:?}: {message}");
    }
}

/// Duplicate IDs name the conflicting finding without changing existing output.
#[test]
fn duplicate_reviewer_and_history_identifiers_are_named() {
    let workspace = Workspace::new("duplicate");
    let source = finding("g1-1", json!({"kind":"file","path":"src/a.rs"}));
    let first = reviewer("g1", json!([source]));
    let first_path = workspace.write("reviewers/g1.json", &first);
    workspace.write("reviewers/g2.json", &reviewer("g2", json!([source])));
    let output = workspace.path("index.json");
    let original = b"existing index bytes";
    fs::write(&output, original).unwrap();
    let source_before = fs::read(&first_path).unwrap();

    let error =
        findings::build_index(&workspace.path("reviewers"), &output, &FixedClock).unwrap_err();
    assert_invalid(&error, &["Duplicate", "'g1-1'"]);
    assert_eq!(fs::read(&output).unwrap(), original);
    assert_eq!(fs::read(&first_path).unwrap(), source_before);

    let duplicate = reviewer("g1", json!([source, source]));
    let error = findings::validate_artifact(&duplicate).unwrap_err();
    assert_invalid(&error, &["Duplicate", "'g1-1'"]);

    let mut history = first;
    history["removed"] = json!([{"finding":source,"reason":"Superseded",
        "removedUtc":"2024-05-06T09:10:11Z"}]);
    let error = findings::validate_artifact(&history).unwrap_err();
    assert_invalid(&error, &["Duplicate", "'g1-1'", "history"]);
}

/// Invalid stored prose identifies the artifact, finding, field and code point.
#[test]
fn malformed_stored_finding_identifies_its_artifact_and_identifier() {
    let workspace = Workspace::new("malformed");
    let mut bad = finding("g1-2", json!({"kind":"file","path":"src/a.rs"}));
    bad["description"] = json!("before\u{001b}after");
    let path = workspace.write("reviewers/g1.json", &reviewer("g1", json!([bad])));
    let source_before = fs::read(&path).unwrap();
    let output = workspace.path("consolidated.json");
    let original = b"existing consolidated bytes";
    fs::write(&output, original).unwrap();

    let error = findings::consolidate(&workspace.path("reviewers"), None, &output, &FixedClock)
        .unwrap_err();
    assert_invalid(&error, &["g1.json", "'g1-2'", "'description'", "U+001B"]);
    assert_eq!(fs::read(&path).unwrap(), source_before);
    assert_eq!(fs::read(&output).unwrap(), original);

    let report = findings::validate_artifacts(&path, &[], &FixedClock).unwrap();
    assert_eq!(report["isValid"], false);
    let diagnostics = report["errors"].to_string();
    for detail in ["g1.json", "'g1-2'", "'description'", "U+001B"] {
        assert!(diagnostics.contains(detail), "{diagnostics}");
    }

    bad["description"] = json!("Details");
    bad.as_object_mut().unwrap().remove("id");
    let error = findings::validate_artifact(&reviewer("g1", json!([bad]))).unwrap_err();
    assert_invalid(&error, &["missing or invalid identifier", "'id'"]);
}

/// Malformed artifact diagnostics identify both the input and the parsing failure.
#[test]
fn malformed_artifact_identifies_json_failure() {
    let workspace = Workspace::new("json-cause");
    let path = workspace.path("broken.json");
    fs::write(&path, b"{").unwrap();
    let error = findings::read_artifact(&path, &FixedClock).unwrap_err();
    assert_invalid(&error, &["broken.json", "Invalid JSON"]);
}

/// Preparation propagates the supplied validator's classification and cause.
#[test]
fn overview_validation_retains_injected_error() {
    let workspace = Workspace::new("overview-cause");
    let path = workspace.path("overview.md");
    fs::write(&path, "# Content overview\n").unwrap();
    let validator = |_: &str| {
        Err(ss_ai_runtime::caused_by(
            "invalidInput",
            "Invalid structure",
            std::io::Error::other("validator cause"),
        )
        .into())
    };
    let error = overview_validation::validate(&path, None, &validator, &FixedClock).unwrap_err();
    assert_invalid(&error, &["Invalid structure", "validator cause"]);
    assert!(error.find_source::<std::io::Error>().is_some());
}

/// Merge diagnostics identify unknown sources and each kind of uncovered location.
#[test]
fn merge_diagnostics_identify_unknown_sources_and_lost_locations() {
    let workspace = Workspace::new("merge");
    let first = finding(
        "g1-1",
        json!({"kind":"line","path":"src/retry.rs","startLine":42}),
    );
    workspace.write("reviewers/g1.json", &reviewer("g1", json!([first])));
    let mut second = finding(
        "g2-1",
        json!({"kind":"line","path":"src/backoff.rs","startLine":8}),
    );
    let second_path = workspace.write("reviewers/g2.json", &reviewer("g2", json!([second])));
    workspace.write(
        "merged.json",
        &json!({"title":"Merged","description":"Details","reasoning":"Why",
            "consequence":"Impact","recommendedAction":"Fix",
            "anchor":{"kind":"line","path":"src/retry.rs","startLine":42}}),
    );
    let plan = workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"groups":[
            {"sourceFindingIds":["g1-1","g9-4"],"findingPath":"merged.json"}]}),
    );
    let output = workspace.path("consolidated.json");
    let original = b"existing consolidated bytes";
    fs::write(&output, original).unwrap();
    let error = findings::consolidate(
        &workspace.path("reviewers"),
        Some(&plan),
        &output,
        &FixedClock,
    )
    .unwrap_err();
    assert_invalid(&error, &["unknown", "'g9-4'"]);
    assert_eq!(fs::read(&output).unwrap(), original);

    workspace.write(
        "plan.json",
        &json!({"schemaVersion":1,"groups":[
            {"sourceFindingIds":["g1-1","g2-1"],"findingPath":"merged.json"}]}),
    );
    for (anchor, location, side) in [
        (
            json!({"kind":"line","path":"src/backoff.rs","startLine":8}),
            "src/backoff.rs:8",
            "right",
        ),
        (
            json!({"kind":"line","path":"src/backoff.rs","startLine":8,"endLine":10,"side":"left"}),
            "src/backoff.rs:8-10",
            "left",
        ),
        (
            json!({"kind":"file","path":"src/backoff.rs"}),
            "src/backoff.rs",
            "",
        ),
    ] {
        second["anchor"] = anchor;
        workspace.write("reviewers/g2.json", &reviewer("g2", json!([second])));
        let source_before = fs::read(&second_path).unwrap();
        let error = findings::consolidate(
            &workspace.path("reviewers"),
            Some(&plan),
            &output,
            &FixedClock,
        )
        .unwrap_err();
        assert_invalid(&error, &[location, "'g2-1'", side]);
        assert_eq!(fs::read(&second_path).unwrap(), source_before);
        assert_eq!(fs::read(&output).unwrap(), original);
    }

    second["anchor"] = json!({"kind":"line","path":"src/retry.rs","startLine":42});
    second["impactedLocations"] = json!([{"path":"src/backoff.rs","startLine":8}]);
    workspace.write("reviewers/g2.json", &reviewer("g2", json!([second])));
    let source_before = fs::read(&second_path).unwrap();
    let error = findings::consolidate(
        &workspace.path("reviewers"),
        Some(&plan),
        &output,
        &FixedClock,
    )
    .unwrap_err();
    assert_invalid(&error, &["src/backoff.rs:8", "'g2-1'"]);
    assert_eq!(fs::read(&second_path).unwrap(), source_before);
    assert_eq!(fs::read(&output).unwrap(), original);
}
