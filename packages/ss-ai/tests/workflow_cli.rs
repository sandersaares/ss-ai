//! Exercises local workflow commands through the installed executable boundary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use ss_ai_review_workflow::{focus_area_expansion, focus_area_generation, focus_area_map};
use ss_ai_runtime::{LockMode, ResourceLock, SystemClock};
use tempfile::Builder;

/// Keeps review initialization fixtures inside the active checkout.
struct ReviewFixture(PathBuf);

impl ReviewFixture {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".review-init-cli-{}", std::process::id()));
        #[expect(
            clippy::create_dir,
            reason = "The fixture must not reuse existing state"
        )]
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for ReviewFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// CLI callers must supply the caller-held identity and receive private focus snapshots.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn review_initialization_cli_binds_the_preparation_identity() {
    let fixture = ReviewFixture::new();
    let session = &fixture.0;
    let clock = SystemClock::default();
    let source = session.join("instruction-set-source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("guide.md"), "# Rule\nKeep it.\n").unwrap();
    let identity = {
        let _operation = ResourceLock::open(
            &session.join("focus-area-cache-operation"),
            LockMode::Exclusive,
            &clock,
        )
        .unwrap();
        focus_area_generation::invalidate_locked(session, &clock).unwrap();
        focus_area_generation::invalidate_snapshot_locked(session, &clock).unwrap();
        focus_area_generation::bind_snapshot_locked(session, &clock).unwrap();
        focus_area_generation::bind_restore_locked(session, &source, &clock).unwrap()
    };
    let generation = focus_area_generation::begin(session, &identity, &clock).unwrap();
    let preparation = Path::new(&generation.session_state);
    focus_area_expansion::update(
        preparation,
        focus_area_expansion::Operation::Initialize,
        &clock,
    )
    .unwrap();
    focus_area_expansion::update(
        preparation,
        focus_area_expansion::Operation::Finalize,
        &clock,
    )
    .unwrap();
    let span = |root| json!({"root":root,"path":"guide.md","startLine":1,"endLine":2});
    let map = json!({"schemaVersion":1,"categories":[{"id":"rules","title":"Rules"}],
        "chapters":[{"id":"rule","categoryId":"rules","content":span("instruction-set-raw"),
            "sources":[span("instruction-set-source")],"applicability":[{
                "effectiveScope":"No explicit restriction; interpret the guideline in context.",
                "declarations":[],"origins":[],"uncertainties":[]}]}]});
    let map_path = preparation.join("focus-area-map.json");
    focus_area_map::update(
        preparation,
        &map_path,
        focus_area_map::MapOperation::Append(serde_json::to_vec(&map).unwrap()),
        &clock,
    )
    .unwrap();
    focus_area_map::assemble(preparation, &map_path, &clock).unwrap();
    focus_area_generation::publish(session, &generation.generation_id, &clock).unwrap();

    let files = json!([{"path":session.join("file.rs"),"sizeBytes":1,"change":"modified"}]);
    let inventory = session.join("inventory.json");
    let groups = session.join("groups.json");
    fs::write(&inventory, serde_json::to_vec(&files).unwrap()).unwrap();
    fs::write(
        &groups,
        serde_json::to_vec(&json!([{"groupId":"group-1","files":files}])).unwrap(),
    )
    .unwrap();
    let output = session.join("work");
    let run = |session_arg: bool, identity_arg: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ss-ai"));
        command
            .args(["workflow", "create-review-work", "--inventory-path"])
            .arg(&inventory)
            .arg("--file-groups-path")
            .arg(&groups)
            .arg("--focus-area-directory")
            .arg(session.join("focus-areas"))
            .arg("--output-directory")
            .arg(&output);
        if session_arg {
            command.arg("--session-state").arg(session);
        }
        if let Some(identity) = identity_arg {
            command.arg("--preparation-identity").arg(identity);
        }
        command.output().unwrap()
    };
    for (has_session, token) in [
        (false, None),
        (true, None),
        (false, Some(identity.as_str())),
        (true, Some("stale")),
    ] {
        let result = run(has_session, token);
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(!output.exists());
    }
    let result = run(true, Some(&identity));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result: Value = serde_json::from_slice(&result.stdout).unwrap();
    let manifest: Value = serde_json::from_slice(
        &fs::read(result.get("manifestPath").unwrap().as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let snapshot = Path::new(
        manifest
            .get("reviewers")
            .unwrap()
            .get(0)
            .unwrap()
            .get("focusArea")
            .unwrap()
            .as_str()
            .unwrap(),
    );
    assert!(snapshot.starts_with(output));
    assert_eq!(
        fs::read(snapshot).unwrap(),
        fs::read(session.join("focus-areas/focus-rules.md")).unwrap()
    );
}

/// Local finding plans must not require provider authentication or executables.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn plan_creation_runs_without_provider_tools() {
    let directory = Builder::new()
        .prefix(".workflow-cli-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let path = directory.path().join("finding plan.json");
    let output = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["workflow", "plan-initialize", "--kind", "updates", "--path"])
        .arg(&path)
        .env("PATH", "")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("status"), Some(&Value::from("Created")));
    let plan: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(plan.get("updates"), Some(&serde_json::json!([])));
}

/// Decision plans use the same validated native authoring boundary.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn decision_plan_creation_and_append_use_native_writer() {
    let directory = Builder::new()
        .prefix(".decision-plan-cli-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let path = directory.path().join("decisions.json");
    let item = directory.path().join("decision.json");
    fs::write(&item, r#"{"findingId":"F1","decision":"needs-audit"}"#).unwrap();
    for (command, extra) in [
        ("plan-initialize", None),
        ("plan-append", Some(item.as_path())),
    ] {
        let mut invocation = Command::new(env!("CARGO_BIN_EXE_ss-ai"));
        invocation
            .args(["workflow", command, "--kind", "decisions", "--path"])
            .arg(&path);
        if let Some(item) = extra {
            invocation.arg("--item-path").arg(item);
        }
        let output = invocation.env("PATH", "").output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let plan: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(
        plan.get("decisions")
            .and_then(|decisions| decisions.get(0))
            .and_then(|decision| decision.get("decision")),
        Some(&Value::from("needs-audit"))
    );
}

/// Publication adapters validate complete snapshots through the workflow command.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn approved_snapshot_request_validates_at_cli_boundary() {
    let directory = Builder::new()
        .prefix(".approved-check-cli-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let request = directory.path().join("request.json");
    let approved = serde_json::json!({
        "schemaVersion": 2, "target": null, "reviewedSourceCommit": null,
        "reviewedMergeBaseCommit": null, "reviewedContextHash": null, "findings": []
    });
    fs::write(
        &request,
        serde_json::to_vec(&serde_json::json!({"approved": approved, "context": null})).unwrap(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["workflow", "approved", "check-request", "--request-path"])
        .arg(&request)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("status"), Some(&Value::from("Valid")));
}

/// Empty plans are valid authoring state but not valid mutation requests.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn empty_mutation_plans_fail_without_changing_the_artifact() {
    let directory = Builder::new()
        .prefix(".workflow-cli-mutation-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let artifact = directory.path().join("reviewer.json");
    let finding = directory.path().join("finding.json");
    fs::write(&finding, r#"{"title":"Problem","description":"What fails","reasoning":"Why","consequence":"Impact","recommendedAction":"Fix","anchor":{"kind":"file","path":"src/lib.rs"}}"#).unwrap();
    let initialize = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["workflow", "initialize-finding", "--output-directory"])
        .arg(directory.path())
        .args(["--reviewer-id", "reviewer", "--focus-area", "correctness"])
        .output()
        .unwrap();
    assert!(
        initialize.status.success(),
        "{}",
        String::from_utf8_lossy(&initialize.stderr)
    );
    let add = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .args(["workflow", "add-finding", "--artifact-path"])
        .arg(&artifact)
        .arg("--finding-path")
        .arg(&finding)
        .args(["--origin", "reviewer"])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let before = fs::read(&artifact).unwrap();
    for (kind, command) in [
        ("updates", "update-finding"),
        ("removals", "remove-finding"),
    ] {
        let plan = directory.path().join(format!("{kind}.json"));
        let initialized = Command::new(env!("CARGO_BIN_EXE_ss-ai"))
            .args(["workflow", "plan-initialize", "--kind", kind, "--path"])
            .arg(&plan)
            .output()
            .unwrap();
        assert!(
            initialized.status.success(),
            "{}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        let contents: Value = serde_json::from_slice(&fs::read(&plan).unwrap()).unwrap();
        assert_eq!(contents.get(kind), Some(&serde_json::json!([])));
        let mut invocation = Command::new(env!("CARGO_BIN_EXE_ss-ai"));
        invocation
            .args(["workflow", command, "--artifact-path"])
            .arg(&artifact)
            .arg("--plan-path")
            .arg(&plan);
        if kind == "removals" {
            invocation.args(["--now-utc", "2024-05-06T07:08:09Z"]);
        }
        let output = invocation.output().unwrap();
        assert!(!output.status.success(), "{kind} accepted an empty plan");
        assert!(
            output.stdout.is_empty(),
            "{kind} emitted a successful result"
        );
        assert!(!output.stderr.is_empty(), "{kind} gave no diagnostic");
        assert_eq!(
            fs::read(&artifact).unwrap(),
            before,
            "{kind} changed the artifact"
        );
    }
}
