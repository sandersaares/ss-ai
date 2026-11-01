//! Exercises native workflow-record entry points and their publication boundaries.
#![allow(
    clippy::indexing_slicing,
    reason = "The tests inspect fixed JSON fixture shapes"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::{audit, review_work};
use ss_ai_runtime::Clock;

/// Isolated fixture preparation must not wait on wall-clock time or contention.
struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::MIN
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Unexpected contention in an isolated workflow fixture")
    }
}

/// Isolates CLI outputs and locks under the active checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".workflow-data-cli-{}-{sequence}",
            std::process::id()
        ));
        #[expect(
            clippy::create_dir,
            reason = "The fixture must not reuse existing state"
        )]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_owned()
    }

    fn put(&self, name: &str, value: &Value) -> String {
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

/// Captures both streams so failed commands cannot masquerade as JSON results.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .arg("workflow")
        .args(args)
        .output()
        .unwrap()
}

fn success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Plan authors and consumers share the same index and item validation.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn candidates_cli_validates_empty_indexes_and_nonempty_plans() {
    let workspace = Workspace::new();
    let plan = workspace.path("plan.json");
    let index = workspace.put("index.json", &json!({"schemaVersion":1,"findings":[]}));
    let init = run(&[
        "plan-initialize",
        "--kind",
        "mergeCandidates",
        "--path",
        &plan,
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let validate = || {
        run(&[
            "validate-candidates",
            "--path",
            &plan,
            "--index-path",
            &index,
        ])
    };
    assert_eq!(success(&validate())["clusterCount"], 0);

    for malformed in [json!({}), json!({"schemaVersion":1,"findings":null})] {
        workspace.put("index.json", &malformed);
        let output = validate();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
    let item = workspace.put("item.json", &json!({"findingIds":["a-1","b-1"]}));
    let append = run(&[
        "plan-append",
        "--kind",
        "mergeCandidates",
        "--path",
        &plan,
        "--item-path",
        &item,
    ]);
    assert!(
        append.status.success(),
        "{}",
        String::from_utf8_lossy(&append.stderr)
    );
    let entry = |id, reviewer| {
        json!({"id":id,"reviewerId":reviewer,"focusArea":"correctness",
        "title":"Problem","description":"What fails","recommendedAction":"Fix",
        "anchorPath":"src/a.rs","impactedPaths":[]})
    };
    let a = entry("a-1", "a");
    let b = entry("b-1", "b");
    workspace.put("index.json", &json!({"schemaVersion":1,"findings":[a,b]}));
    assert_eq!(success(&validate())["clusterCount"], 1);
    workspace.put("index.json", &json!({"schemaVersion":1,"findings":[a]}));
    let output = validate();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown finding"));
}

/// Published inputs feed the same persistent assessment journal used by reviewers.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn input_and_assessment_cli_publish_validated_records() {
    let workspace = Workspace::new();
    let files = json!([{"path":workspace.path("a.rs"),"sizeBytes":1,"change":"modified"},
        {"path":workspace.path("deleted.rs"),"sizeBytes":1,"change":"deleted"}]);
    let inventory_draft = workspace.put("inventory-draft.json", &files);
    let group_draft = workspace.put(
        "groups-draft.json",
        &json!([{"groupId":"group-001","files":files}]),
    );
    let inventory = workspace.path("inventory.json");
    let groups = workspace.path("groups.json");
    let output = run(&[
        "set-review-input",
        "--kind",
        "inventory",
        "--input-path",
        &inventory_draft,
        "--path",
        &inventory,
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&inventory).unwrap())
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let output = run(&[
        "set-review-input",
        "--kind",
        "groups",
        "--input-path",
        &group_draft,
        "--path",
        &groups,
        "--inventory-path",
        &inventory,
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let queue = workspace.path("queue.json");
    let clock = TestClock;
    review_work::initialize_queue(
        inventory.as_ref(),
        groups.as_ref(),
        queue.as_ref(),
        false,
        &clock,
    )
    .unwrap();
    review_work::next(queue.as_ref(), &clock).unwrap();
    let draft = workspace.put("assessment.json", &json!({"groupId":"group-001","path":files[0]["path"],"disposition":"reviewed","scope":"","reason":""}));
    let output = run(&[
        "set-review-assessment",
        "--queue-path",
        &queue,
        "--inventory-path",
        &inventory,
        "--file-groups-path",
        &groups,
        "--assessment-path",
        &draft,
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&fs::read(queue).unwrap()).unwrap();
    assert_eq!(result["assessments"].as_array().unwrap().len(), 1);
}

/// Expansion worker outcomes cross the command boundary as typed JSON records.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn expansion_cli_handles_empty_discovery_and_typed_worker_results() {
    let workspace = Workspace::new();
    let empty = workspace.path("empty");
    fs::create_dir_all(&empty).unwrap();
    for root in ["instruction-set-source", "instruction-set-raw"] {
        fs::create_dir_all(PathBuf::from(&empty).join(root)).unwrap();
    }
    let expansion = |session: &str, operation: &str, extra: &[&str]| {
        let mut args = vec![
            "update-focus-area-expansion",
            "--session-state",
            session,
            "--operation",
            operation,
        ];
        args.extend_from_slice(extra);
        run(&args)
    };
    success(&expansion(&empty, "initialize", &[]));
    assert_eq!(
        success(&expansion(&empty, "finalize", &[]))["state"],
        "Complete"
    );
    let read = expansion(&empty, "read", &[]);
    let result = success(&read);
    assert_eq!(result["referenceCount"], 0);
    for key in ["contexts", "failedReferences", "pendingReferenceIds"] {
        assert_eq!(result[key], json!([]));
    }
    assert_eq!(String::from_utf8_lossy(&read.stdout).lines().count(), 1);

    let session = workspace.path("session");
    fs::create_dir_all(&session).unwrap();
    for root in ["instruction-set-source", "instruction-set-raw"] {
        fs::create_dir_all(PathBuf::from(&session).join(root)).unwrap();
    }
    fs::write(
        PathBuf::from(&session).join("instruction-set-source/source.md"),
        "Source",
    )
    .unwrap();
    for path in ["parent.md", "child.md"] {
        fs::write(
            PathBuf::from(&session)
                .join("instruction-set-raw")
                .join(path),
            "Guidance",
        )
        .unwrap();
    }
    success(&expansion(&session, "initialize", &[]));
    let bad = workspace.put("bad-reference.json", &json!({"id":"bad","uri":"https://example.invalid/bad","referrers":[{
        "parentId":null,"source":{"root":"instruction-set-raw","path":"parent.md","startLine":1,"endLine":1},
        "scope":"Published APIs","declarations":[]}]}));
    let manifest = PathBuf::from(&session).join("focus-area-expansion-status.json");
    let before = fs::read(&manifest).unwrap();
    let rejected = expansion(&session, "register", &["--input-path", &bad]);
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("Direct referrer"));
    assert_eq!(fs::read(&manifest).unwrap(), before);
    let registration = workspace.put("reference.json", &json!({"id":"parent","uri":"https://example.invalid/parent","referrers":[{
        "parentId":null,"source":{"root":"instruction-set-source","path":"source.md","startLine":1,"endLine":1},
        "scope":"Published APIs","declarations":[]}]}));
    success(&expansion(
        &session,
        "register",
        &["--input-path", &registration],
    ));
    let draft = workspace.put(
        "result.json",
        &json!({"referenceId":"parent","outputs":["parent.md","child.md"],"error":null}),
    );
    success(&expansion(&session, "complete", &["--input-path", &draft]));
    let record: Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert_eq!(
        record["references"][0]["outputs"],
        json!(["parent.md", "child.md"])
    );
    assert_eq!(
        success(&expansion(&session, "finalize", &[]))["state"],
        "Complete"
    );
}

/// The decision command loads human feedback from its record rather than a shell argument.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn decision_cli_loads_accumulated_feedback_verbatim() {
    let workspace = Workspace::new();
    let finding = json!({"schemaVersion":1,"id":"F-1","sourceFindingIds":[],"origin":"reviewer",
        "title":"Title","description":"Description","reasoning":"Reasoning",
        "consequence":"Consequence","recommendedAction":"Action","references":[],
        "impactedLocations":[],"anchor":{"kind":"file","path":"src/lib.rs"}});
    let findings = workspace.put(
        "findings.json",
        &json!({"schemaVersion":1,"kind":"consolidated","findings":[finding],"removed":[]}),
    );
    let feedback = workspace.path("feedback.json");
    let text = workspace.path("text.txt");
    fs::write(&text, "Preserve $this exactly.").unwrap();
    audit::add_feedback(
        feedback.as_ref(),
        findings.as_ref(),
        "response-1",
        "F-1",
        text.as_ref(),
        &TestClock,
    )
    .unwrap();
    let decisions = workspace.path("decisions.json");
    let item = workspace.put(
        "decision.json",
        &json!({"findingId":"F-1","decision":"approved"}),
    );
    let output = run(&[
        "audit-add-decisions",
        "--path",
        &decisions,
        "--item-path",
        &item,
        "--audit-feedback-path",
        &feedback,
        "--recorded-utc",
        "2026-01-01T00:00:00Z",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: Value = serde_json::from_slice(&fs::read(decisions).unwrap()).unwrap();
    assert_eq!(
        record["decisions"][0]["feedback"],
        "Preserve $this exactly."
    );
}
