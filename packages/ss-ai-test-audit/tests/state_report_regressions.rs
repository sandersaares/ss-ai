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
use serde_json::{Value, from_slice, from_str, json, to_vec};
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

    let pairs = [(&first, &second, 1), (&second, &first, 2)];
    for (receipt, other, count) in pairs {
        let machine = read_json(artifact(receipt, "batchPath"));
        let display = fs::read_to_string(artifact(receipt, "displayPath")).unwrap();
        let crossed_display = fs::read_to_string(artifact(other, "displayPath")).unwrap();
        let assertions = machine["assertions"].as_array().unwrap();
        assert_eq!(assertions.len(), count);
        let matches = |display: &str| {
            assertions.iter().all(|row| {
                display.contains(row["id"].as_str().unwrap())
                    && display.contains(row["expression"].as_str().unwrap())
            })
        };
        assert!(matches(&display));
        if count == 2 {
            // The smaller page can share the first site's context but not the
            // second site's identity, so the complete page cannot use its display.
            assert!(!matches(&crossed_display));
        }
    }
}

#[test]
fn batch_exports_cannot_target_the_report_staging_area() {
    let audit = Audit::new(TWO_ASSERTIONS);
    let reports = audit.state.join("reports");
    let output = reports.join(".staging").join("batch.json");
    let row = incidental(&audit.inventory["assertions"][0]["id"], "citations");
    let input = audit.classification(&row);
    let journal = audit.journal();
    for error in [
        next_batch(&audit.state, &output, 1, &TestClock).unwrap_err(),
        advance(&audit.state, &input, &output, 1, &TestClock).unwrap_err(),
    ] {
        assert!(error.to_string().contains("report publication area"));
    }
    assert_eq!(audit.journal(), journal);
    assert!(!reports.exists());
}

#[cfg(unix)]
#[test]
fn batch_exports_reject_an_alias_into_report_staging() {
    use std::os::unix::fs::symlink;

    let audit = Audit::new(TWO_ASSERTIONS);
    let staging = audit.state.join("reports").join(".staging");
    fs::create_dir_all(&staging).unwrap();
    let alias = audit.state.join("alias");
    symlink(&staging, &alias).unwrap();
    let error = next_batch(&audit.state, &alias.join("batch.json"), 1, &TestClock).unwrap_err();
    assert!(error.to_string().contains("report publication area"));
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn non_utf8_output_paths_fail_before_any_publication_or_classification() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::symlink;

    let audit = Audit::new(TWO_ASSERTIONS);
    let row = incidental(&audit.inventory["assertions"][0]["id"], "citations");
    let input = audit.classification(&row);
    let journal = audit.journal();
    let name = OsString::from_vec(b"invalid-\xFF".to_vec());
    let output = audit.state.join("not-created").join(&name);
    let alias = audit._root.path().join(&name);
    symlink(&audit.state, &alias).unwrap();
    let uninitialized = audit._root.path().join("not-created").join(&name);
    let repository = audit._root.path().join("repository");
    for error in [
        initialize(
            &repository,
            &[],
            &uninitialized,
            &FixtureGit(&repository),
            &TestClock,
        )
        .unwrap_err(),
        next_batch(&audit.state, &output, 1, &TestClock).unwrap_err(),
        advance(&audit.state, &input, &output, 1, &TestClock).unwrap_err(),
        record_batch(&alias, &input, &TestClock).unwrap_err(),
    ] {
        assert!(error.to_string().contains("UTF-8"));
    }
    assert_eq!(audit.journal(), journal);
    assert!(!output.parent().unwrap().exists());
    assert!(!uninitialized.parent().unwrap().exists());

    // An empty audit can reach the report output boundary without recording.
    let empty = Audit::new("pub fn untouched() {}");
    let alias = empty._root.path().join(name);
    symlink(&empty.state, &alias).unwrap();
    assert!(
        report(&alias, &TestClock)
            .unwrap_err()
            .to_string()
            .contains("UTF-8")
    );
    assert!(!empty.state.join("reports").exists());
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
    let linked_id = &audit.inventory["assertions"][1]["id"];
    let mut linked = incidental(linked_id, "citations");
    linked["unlinkedReason"] = json!("This linked site has no caller.");
    let error = record_batch(&audit.state, &audit.classification(&linked), &TestClock).unwrap_err();
    assert!(error.to_string().contains("unlinkedReason"));
    linked.as_object_mut().unwrap().remove("unlinkedReason");
    linked["unresolvedCallerReason"] =
        json!("A callback registration may add a second indirect caller.");
    linked["reviewReason"] = json!(" \t");
    let error = record_batch(&audit.state, &audit.classification(&linked), &TestClock).unwrap_err();
    assert!(error.to_string().contains("reviewReason"));
    linked["reviewReason"] =
        json!("The public contract's applicability to the callback still needs review.");
    let path = audit.classification(&linked);
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
fn reports_preserve_caller_attribution_and_test_gaps_in_both_views() {
    let audit = Audit::new(
        "
#[cfg(test)]
fn helper() { assert_eq!(1, 1); }
#[test]
fn native_caller() { helper(); }
#[test]
fn agent_caller() { let check = helper; check(); }
#[test]
fn smoke_without_assertions() {}
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
    assert!(result.get("tracePath").is_none());
    assert!(result.get("testInventoryPath").is_none());
    let generation = artifact(&result, "detailsJsonPath").parent().unwrap();
    let artifact_names = fs::read_dir(generation)
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            (entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "lock"))
            .then(|| entry.file_name())
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        artifact_names,
        ["details.json", "details.md", "summary.json", "summary.md"]
            .map(Into::into)
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(details["repositoryRoot"], audit.inventory["repositoryRoot"]);
    assert_eq!(details["scopePaths"], json!(["lib.rs"]));
    let callers = [agent, native].into_iter().collect::<BTreeSet<_>>();
    for caller in &callers {
        let tests = details["testInventory"].as_array().unwrap();
        let test = tests.iter().find(|test| test["id"] == *caller).unwrap();
        assert_eq!(test["sharedAssertionIds"], json!([id]));
        assert_eq!(test["assertionCount"], 1);
        assert_eq!(test["incidentalOnly"], true);
    }
    let tests = details["testInventory"].as_array().unwrap();
    let smoke = tests
        .iter()
        .find(|test| test["name"] == "smoke_without_assertions")
        .unwrap();
    assert_eq!(smoke["assertionCount"], 0);
    assert_eq!(smoke["incidentalOnly"], false);
    let summary = read_json(artifact(&result, "summaryJsonPath"));
    assert_eq!(summary["incidentalOnlyCount"], 2);
    assert_eq!(summary["noCountedAssertionCount"], 1);
    let text = fs::read_to_string(artifact(&result, "summaryPath")).unwrap();
    assert!(
        text.contains("`smoke_without_assertions`") && text.contains("has no counted assertion")
    );
    assert!(text.contains("`agent_caller`") && text.contains("has only incidental checks"));
    let markdown = fs::read_to_string(artifact(&result, "detailsPath")).unwrap();
    assert!(
        markdown
            .lines()
            .any(|line| callers.iter().all(|id| line.contains(id)))
    );
    assert!(
        markdown
            .lines()
            .any(|line| line.contains("agent_caller") && line.contains(id.as_str().unwrap()))
    );
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
    from_str(text.as_ref()).unwrap()
}

/// Reads a generated JSON document from its workflow path.
fn read_json(path: impl AsRef<Path>) -> Value {
    from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Writes a JSON input fixture.
fn write_json(path: &Path, value: &Value) {
    fs::write(path, to_vec(value).unwrap()).unwrap();
}
