//! Exercises batch and report publication against real workflow files.
#![expect(
    clippy::indexing_slicing,
    reason = "The tests read named fields and fixed rows from their own generated JSON."
)]

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, LockMode, Process, ProcessOutput, ResourceLock};
use ss_ai_test_audit::{advance, initialize, next_batch, record_batch, report, submit};
use tempfile::TempDir;

/// Keeps ordinary fixture commands independent of wall-clock time.
struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Unexpected contention in an isolated audit fixture")
    }
}

/// Coordinates concurrent exports at an already held destination lock.
struct GatedClock {
    waiting: Sender<()>,
    release: Receiver<()>,
}

impl Clock for GatedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        self.waiting.send(()).unwrap();
        self.release.recv().unwrap();
        Ok(())
    }
}

/// Supplies a fixed Git identity for a fixture inside the project directory.
struct FixtureGit<'a>(&'a Path);

impl Process for FixtureGit<'_> {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        panic!("The auditor must not start a real Git process in this fixture")
    }

    fn isolated_git(&self, _: &Path, arguments: &[String]) -> io::Result<ProcessOutput> {
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        let stdout = match arguments.as_slice() {
            ["rev-parse", "--show-toplevel"] => self.0.display().to_string(),
            ["rev-parse", "HEAD"] => "0123456789abcdef0123456789abcdef01234567".to_owned(),
            _ => panic!("Unexpected Git arguments: {arguments:?}"),
        };
        Ok(ProcessOutput::new(true, stdout.into_bytes(), Vec::new()))
    }
}

/// Isolates every source fixture and its audit state under the repository.
struct Audit {
    _root: TempDir,
    state: PathBuf,
    inventory: Value,
}

impl Audit {
    /// Initializes a named source-file scope for meaningful provenance assertions.
    fn new(source: &str) -> Self {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        fs::create_dir_all(&fixtures).unwrap();
        let root = TempDir::new_in(fixtures).unwrap();
        let repository = root.path().join("repository");
        let state = root.path().join("state");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::write(repository.join("lib.rs"), source).unwrap();
        initialize(
            &repository,
            &[PathBuf::from("lib.rs")],
            &state,
            &FixtureGit(&repository),
            &TestClock,
        )
        .unwrap();
        let inventory = read_json(state.join("inventory.json"));
        Self {
            _root: root,
            state,
            inventory,
        }
    }

    /// Exports a page and returns the immutable generation's receipt.
    fn export(&self, name: &str, count: usize) -> Value {
        parse(next_batch(&self.state, &self.state.join(name), count, &TestClock).unwrap())
    }

    /// Writes a low-level classification input for the initialized snapshot.
    fn classification(&self, row: &Value) -> PathBuf {
        let state = read_json(self.state.join("audit-state.json"));
        let path = self.state.join("classify.json");
        write_json(
            &path,
            &json!({
                "schemaVersion": 1,
                "auditId": state["auditId"],
                "snapshotSha256": state["snapshotSha256"],
                "assertions": [row],
            }),
        );
        path
    }

    /// Returns exact journal bytes to check that rejected inputs change nothing.
    fn journal(&self) -> Vec<u8> {
        fs::read(self.state.join("classifications.json")).unwrap()
    }
}

/// Provides distinct sites so exports can share an ID despite different progress.
const TWO_ASSERTIONS: &str = "#[test]\nfn checks() { assert!(true); assert_eq!(1, 1); }\n";

#[test]
fn compact_submissions_cannot_replay_against_another_batch_with_the_same_ids() {
    let audit = Audit::new(TWO_ASSERTIONS);
    let first = audit.export("first.json", 1);
    let first_batch = read_json(artifact(&first, "batchPath"));
    let first_id = &first_batch["assertions"][0]["id"];

    let second_id = &audit.inventory["assertions"][1]["id"];
    let classification = audit.classification(&incidental(second_id, "citations"));
    record_batch(&audit.state, &classification, &TestClock).unwrap();
    let later = audit.export("later.json", 1);
    let later_batch = read_json(artifact(&later, "batchPath"));
    assert_eq!(later_batch["assertions"][0]["id"], *first_id);
    assert_eq!(later["snapshotSha256"], first["snapshotSha256"]);
    assert_ne!(later["batchFingerprint"], first["batchFingerprint"]);

    let submission = audit.state.join("submission.json");
    let row = incidental(first_id, "citationHandles");
    let output = audit.state.join("next.json");
    let journal = audit.journal();
    write_json(&submission, &json!({"schemaVersion": 1, "rows": [row]}));
    let submit_first = || {
        submit(
            &audit.state,
            artifact(&first, "batchPath"),
            &submission,
            &output,
            1,
            &TestClock,
        )
    };
    assert!(submit_first().is_err(), "the fingerprint is mandatory");
    write_submission(&submission, &first, &row);
    let error = submit(
        &audit.state,
        artifact(&later, "batchPath"),
        &submission,
        &output,
        1,
        &TestClock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("batchFingerprint"));
    assert_eq!(audit.journal(), journal);
    assert_eq!(parse(submit_first().unwrap())["nextAction"], "report");
}

#[test]
fn overlapping_exports_keep_each_returned_pair_coherent() {
    let audit = Audit::new(TWO_ASSERTIONS);
    let output = audit.state.join("reused.json");
    let lock = ResourceLock::open(&output, LockMode::Exclusive, &TestClock).unwrap();
    let state = &audit.state;
    let output = &output;

    let (first, second) = thread::scope(|scope| {
        let (waiting, first_waiting) = mpsc::channel();
        let (release_first, release) = mpsc::channel();
        let first = scope.spawn(move || {
            parse(next_batch(state, output, 1, &GatedClock { waiting, release }).unwrap())
        });
        first_waiting.recv().unwrap();

        let (waiting, second_waiting) = mpsc::channel();
        let (release_second, release) = mpsc::channel();
        let second = scope.spawn(move || {
            parse(next_batch(state, output, 2, &GatedClock { waiting, release }).unwrap())
        });
        second_waiting.recv().unwrap();

        // Both invocations have loaded distinct batch selections before either
        // can publish. Inspect both results only after both writes have finished.
        drop(lock);
        release_first.send(()).unwrap();
        let first = first.join().unwrap();
        release_second.send(()).unwrap();
        (first, second.join().unwrap())
    });

    for (receipt, count) in [(&first, 1), (&second, 2)] {
        let machine = read_json(artifact(receipt, "batchPath"));
        let display = fs::read_to_string(artifact(receipt, "displayPath")).unwrap();
        assert_eq!(machine["assertions"].as_array().unwrap().len(), count);
        assert_eq!(display.matches(" | assertion | ").count(), count);
        let fingerprint = receipt["batchFingerprint"].as_str().unwrap();
        assert!(
            display
                .lines()
                .any(|line| line == format!("batchFingerprint: {fingerprint}"))
        );
    }
}

#[test]
fn a_large_single_assertion_can_be_exported_and_submitted() {
    let large_literal = "é".repeat(600_000);
    let audit = Audit::new(&format!(
        "#[test]\nfn checks() {{ assert!(true, \"{large_literal}\"); }}\n"
    ));
    let receipt = audit.export("large.json", 25);
    let batch = read_json(artifact(&receipt, "batchPath"));
    let id = &batch["assertions"][0]["id"];
    assert_eq!(receipt["batchCount"], 1);
    assert_eq!(id, &audit.inventory["assertions"][0]["id"]);
    let submission = audit.state.join("submission.json");
    write_submission(&submission, &receipt, &incidental(id, "citationHandles"));
    let result = submit(
        &audit.state,
        artifact(&receipt, "batchPath"),
        &submission,
        &audit.state.join("next.json"),
        25,
        &TestClock,
    )
    .unwrap();
    assert_eq!(parse(result)["nextAction"], "report");
}

#[test]
fn next_output_cannot_alias_either_input_before_the_journal_is_written() {
    let audit = Audit::new(TWO_ASSERTIONS);
    let batch = audit.export("current.json", 1);
    let batch_path = artifact(&batch, "batchPath");
    let batch_bytes = fs::read(batch_path).unwrap();
    let batch_record = read_json(batch_path);
    let id = &batch_record["assertions"][0]["id"];
    let submission = audit.state.join("submission.json");
    write_submission(&submission, &batch, &incidental(id, "citationHandles"));
    let submission_bytes = fs::read(&submission).unwrap();
    let journal = audit.journal();
    let other_path = audit.state.join("alias.json");
    fs::hard_link(batch_path, &other_path).unwrap();
    let companion_output = audit.state.join("companion.json");
    fs::hard_link(&submission, audit.state.join("companion.json.txt")).unwrap();

    for output in [batch_path, &submission, &other_path, &companion_output] {
        let error =
            submit(&audit.state, batch_path, &submission, output, 1, &TestClock).unwrap_err();
        assert!(error.to_string().contains("aliases input"));
    }
    let classification_path = audit.classification(&incidental(id, "citations"));
    let error = advance(
        &audit.state,
        &classification_path,
        &classification_path,
        1,
        &TestClock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("aliases input"));
    assert_eq!(fs::read(batch_path).unwrap(), batch_bytes);
    assert_eq!(fs::read(submission).unwrap(), submission_bytes);
    assert_eq!(audit.journal(), journal);
}

#[test]
fn bounded_inputs_reject_nonregular_files_and_valid_json_over_the_limit() {
    let audit = Audit::new(TWO_ASSERTIONS);
    let journal = audit.journal();
    let error = record_batch(&audit.state, &audit.state, &TestClock).unwrap_err();
    assert!(error.to_string().contains("regular file"));

    let path = audit.classification(&incidental(
        &audit.inventory["assertions"][0]["id"],
        "citations",
    ));
    let mut bytes = fs::read(&path).unwrap();
    bytes.resize(1024 * 1024, b' ');
    bytes.push(b' ');
    fs::write(&path, &bytes).unwrap();
    let error = record_batch(&audit.state, &path, &TestClock).unwrap_err();
    assert!(error.to_string().contains("bounded input size"));
    assert_eq!(audit.journal(), journal);
    bytes.pop().unwrap();
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        parse(record_batch(&audit.state, &path, &TestClock).unwrap())["completed"],
        1
    );
}

#[test]
fn record_transitions_require_nonblank_reasons_and_never_allow_a_final_response() {
    let audit = Audit::new(
        "#[cfg(test)]\nfn unlinked() { assert!(true); }\n#[test]\nfn linked() { assert!(true); }\n",
    );
    let unlinked_id = &audit.inventory["assertions"][0]["id"];
    let mut row = incidental(unlinked_id, "citations");
    row["incidentalReason"] = json!(" \t\n");
    let journal = audit.journal();
    let error = record_batch(&audit.state, &audit.classification(&row), &TestClock).unwrap_err();
    assert!(error.to_string().contains("incidentalReason"));
    row["incidentalReason"] = json!("The helper checks its own input.");
    row["unlinkedReason"] = json!(" \t\n");
    let error = record_batch(&audit.state, &audit.classification(&row), &TestClock).unwrap_err();
    assert!(error.to_string().contains("unlinkedReason"));
    assert_eq!(audit.journal(), journal);

    row["unlinkedReason"] = json!("No inventoried test calls the helper.");
    let progress =
        parse(record_batch(&audit.state, &audit.classification(&row), &TestClock).unwrap());
    assert_eq!(progress["nextAction"], "continue");
    assert_eq!(progress["finalResponseAllowed"], false);
    let path = audit.classification(&incidental(
        &audit.inventory["assertions"][1]["id"],
        "citations",
    ));
    let terminal = parse(record_batch(&audit.state, &path, &TestClock).unwrap());
    assert_eq!(terminal["nextAction"], "report");
    assert_eq!(terminal["finalResponseAllowed"], false);
}

#[test]
fn failures_at_the_last_details_or_summary_write_do_not_publish_a_report() {
    let audit = Audit::new("");
    let reports = audit.state.join("reports");
    // A directory blocks just the selected artifact write. Neither details
    // failure nor the last write failing may leave a completion artifact.
    for name in ["details.md", "summary.json"] {
        fs::create_dir_all(reports.join(".staging").join(name)).unwrap();
        report(&audit.state, &TestClock).unwrap_err();
        assert!(published_summaries(&reports).is_empty());
    }

    let completed = parse(report(&audit.state, &TestClock).unwrap());
    let summary = artifact(&completed, "summaryJsonPath");
    let summary_bytes = fs::read(summary).unwrap();
    fs::create_dir_all(reports.join(".staging").join("details.md")).unwrap();
    report(&audit.state, &TestClock).unwrap_err();
    assert_eq!(published_summaries(&reports).len(), 2);
    assert_eq!(fs::read(summary).unwrap(), summary_bytes);
    assert!(artifact(&completed, "detailsPath").is_file());
}

#[test]
fn details_match_the_schema_and_attribute_all_callers_in_both_views() {
    let audit = Audit::new(
        "
#[cfg(test)]
fn helper() { assert_eq!(1, 1); }
#[test]
fn native_caller() { helper(); }
#[test]
fn agent_caller() { let check = helper; check(); }
",
    );
    let assertion = &audit.inventory["assertions"][0];
    let id = &assertion["id"];
    let test_id = |name| {
        audit.inventory["tests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|test| test["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
    };
    let native = test_id("native_caller");
    let agent = test_id("agent_caller");
    assert_eq!(assertion["callingTestIds"], json!([native]));
    let mut row = incidental(id, "citations");
    row["callingTestIds"] = json!([agent]);
    record_batch(&audit.state, &audit.classification(&row), &TestClock).unwrap();

    let result = parse(report(&audit.state, &TestClock).unwrap());
    let details = read_json(artifact(&result, "detailsJsonPath"));
    assert_details_schema(&details);
    assert_eq!(details["repositoryRoot"], audit.inventory["repositoryRoot"]);
    assert_eq!(details["scopePaths"], json!(["lib.rs"]));
    let callers = [agent, native].into_iter().collect::<BTreeSet<_>>();
    for caller in &callers {
        let tests = details["testInventory"].as_array().unwrap();
        let test = tests.iter().find(|test| test["id"] == *caller).unwrap();
        assert_eq!(test["sharedAssertionIds"], json!([id]));
    }
    let markdown = fs::read_to_string(artifact(&result, "detailsPath")).unwrap();
    let expected_callers = format!(
        "- Calling tests: {}",
        callers.into_iter().collect::<Vec<_>>().join(", ")
    );
    assert!(markdown.lines().any(|line| line == expected_callers));
}

/// Checks the generated details document against the published schema shape.
fn assert_details_schema(details: &Value) {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../skills/ss-test-audit/schemas/details.schema.json"
    ))
    .unwrap();
    let required = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let actual = details
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(required, actual);
    assert_eq!(
        details["schemaVersion"],
        schema["properties"]["schemaVersion"]["const"]
    );
    for key in ["auditId", "repositoryRoot", "repositoryRevision"] {
        assert!(!details[key].as_str().unwrap().is_empty(), "{key}");
    }
    let snapshot = details["snapshotSha256"].as_str().unwrap();
    assert_eq!(snapshot.len(), 64);
    assert!(
        snapshot
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
    assert!(
        details["scopePaths"]
            .as_array()
            .unwrap()
            .iter()
            .all(Value::is_string)
    );
    for key in ["assertionTrace", "testInventory", "diagnostics"] {
        assert!(
            details[key]
                .as_array()
                .unwrap()
                .iter()
                .all(Value::is_object)
        );
    }
    for diagnostic in details["diagnostics"].as_array().unwrap() {
        for field in ["kind", "path", "detail"] {
            assert!(diagnostic.get(field).is_some());
        }
    }
}

/// Locates every machine summary that could claim durable completion.
fn published_summaries(directory: &Path) -> Vec<PathBuf> {
    let mut summaries = Vec::new();
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            summaries.extend(published_summaries(&path));
        } else if entry.file_name() == "summary.json" || entry.file_name() == "summary.md" {
            summaries.push(path);
        }
    }
    summaries
}

/// Builds an incidental row in either the compact or low-level format.
fn incidental(id: &Value, evidence_field: &str) -> Value {
    json!({
        "assertionId": id,
        "classification": "Incidental",
        "evidenceStatus": "Confirmed",
        "actualOutcome": "The fixture observes a local value.",
        "rationale": "The value is local to the test.",
        (evidence_field): [],
        "incidentalReason": "No public contract specifies this value.",
        "callingTestIds": [],
    })
}

/// Persists a compact submission explicitly bound to its exported batch.
fn write_submission(path: &Path, receipt: &Value, row: &Value) {
    write_json(
        path,
        &json!({
            "schemaVersion": 1,
            "batchFingerprint": receipt["batchFingerprint"],
            "rows": [row],
        }),
    );
}

/// Resolves an artifact using its returned path rather than guessing a generation.
fn artifact<'a>(receipt: &'a Value, key: &str) -> &'a Path {
    Path::new(receipt[key].as_str().unwrap())
}

/// Decodes a JSON command result.
fn parse(text: impl AsRef<str>) -> Value {
    serde_json::from_str(text.as_ref()).unwrap()
}

/// Reads a generated JSON document from its workflow path.
fn read_json(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Writes a JSON input fixture.
fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
