//! Exercises local workflow commands through the installed executable boundary.

use std::collections::BTreeSet;
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

/// Builds the normal CLI invocation with a restricted Linux process stack.
///
/// The limit is below the ungrouped debug parser's frame and leaves room for
/// startup and command execution; other platforms exercise their native limit.
fn workflow_command() -> Command {
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "ulimit -c 0; ulimit -s 768; exec \"$@\"",
            "sh",
            env!("CARGO_BIN_EXE_ss-ai"),
        ]);
        command
    }

    #[cfg(not(target_os = "linux"))]
    {
        Command::new(env!("CARGO_BIN_EXE_ss-ai"))
    }
}

/// Every command reaches its operation without exhausting the parser stack.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn all_workflow_commands_dispatch_with_a_bounded_stack() {
    let directory = Builder::new()
        .prefix(".workflow-stack-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let cases: &[(&[&str], bool)] = &[
        (
            &[
                "restore-focus-area-cache",
                "--session-state",
                "missing",
                "--input-directory",
                "missing",
                "--skill-root",
                "missing",
                "--recipe-path",
                "missing",
                "--cache-directory",
                "missing",
                "--generator-profile",
                "profile",
                "--freshness-days",
                "1",
                "--now-utc",
                "2024-05-06T07:08:09Z",
                "--snapshot-identity",
                "snapshot",
                "--refresh",
            ],
            false,
        ),
        (
            &[
                "begin-focus-area-generation",
                "--session-state",
                "missing",
                "--preparation-identity",
                "identity",
            ],
            false,
        ),
        (
            &[
                "publish-focus-area-generation",
                "--session-state",
                "missing",
                "--generation-id",
                "generation",
            ],
            false,
        ),
        (
            &[
                "save-focus-area-cache",
                "--session-state",
                "missing",
                "--input-directory",
                "missing",
                "--skill-root",
                "missing",
                "--recipe-path",
                "missing",
                "--cache-directory",
                "missing",
                "--generator-profile",
                "profile",
                "--freshness-days",
                "1",
                "--now-utc",
                "2024-05-06T07:08:09Z",
                "--preparation-identity",
                "identity",
            ],
            false,
        ),
        (
            &[
                "validate-focus-area-artifacts",
                "--session-state",
                "missing",
                "--preparation-identity",
                "identity",
            ],
            false,
        ),
        (
            &[
                "update-focus-area-expansion",
                "--session-state",
                "missing",
                "--operation",
                "register",
                "--input-path",
                "missing",
                "--reference-id",
                "reference",
            ],
            false,
        ),
        (
            &[
                "update-focus-area-map",
                "--session-state",
                "missing",
                "--path",
                "missing",
                "--operation",
                "regroup",
                "--input-path",
                "missing",
                "--category-id",
                "old",
                "--new-category-id",
                "new",
                "--title",
                "Title",
            ],
            false,
        ),
        (
            &[
                "validate-focus-area-map",
                "--session-state",
                "missing",
                "--map-path",
                "missing",
                "--complete",
            ],
            false,
        ),
        (
            &[
                "assemble-focus-areas",
                "--session-state",
                "missing",
                "--map-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "snapshot-instructions",
                "--session-state",
                "missing",
                "--skill-root",
                "missing",
                "--repository-root",
                "missing",
                "--repository-instruction",
                "missing",
            ],
            false,
        ),
        (
            &[
                "set-review-input",
                "--kind",
                "inventory",
                "--input-path",
                "missing",
                "--path",
                "output",
            ],
            false,
        ),
        (
            &[
                "create-review-work",
                "--inventory-path",
                "missing",
                "--file-groups-path",
                "missing",
                "--focus-area-directory",
                "missing",
                "--output-directory",
                "output",
                "--session-state",
                "missing",
                "--preparation-identity",
                "identity",
            ],
            false,
        ),
        (
            &[
                "initialize-review-queue",
                "--inventory-path",
                "missing",
                "--file-groups-path",
                "missing",
                "--queue-path",
                "missing",
            ],
            false,
        ),
        (&["next-review-item", "--queue-path", "missing"], false),
        (
            &[
                "complete-review-group",
                "--queue-path",
                "missing",
                "--group-id",
                "group",
            ],
            false,
        ),
        (
            &[
                "set-review-assessment",
                "--queue-path",
                "missing",
                "--inventory-path",
                "missing",
                "--file-groups-path",
                "missing",
                "--assessment-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "complete-review-batch",
                "--queue-path",
                "missing",
                "--inventory-path",
                "missing",
                "--file-groups-path",
                "missing",
                "--artifact-path",
                "missing",
                "--batch-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "fail-review-worker",
                "--queue-path",
                "missing",
                "--reason",
                "failure",
            ],
            false,
        ),
        (
            &[
                "validate-review-work",
                "--manifest-path",
                "missing",
                "--artifact-directory",
                "missing",
            ],
            false,
        ),
        (
            &[
                "worker-assignment",
                "--phase",
                "review",
                "--session-state",
                "missing",
                "--scope-path",
                "missing",
                "--output-directory",
                "output",
                "--skill-root",
                "missing",
            ],
            false,
        ),
        (
            &[
                "worker-document",
                "--assignment-path",
                "missing",
                "--document-id",
                "document",
            ],
            false,
        ),
        (&["render", "--request-path", "missing"], false),
        (
            &["render-markdown-files", "--request-path", "missing"],
            false,
        ),
        (&["resolve-anchor", "--request-path", "missing"], false),
        (
            &[
                "audit-add-decisions",
                "--path",
                "missing",
                "--plan-path",
                "missing",
                "--recorded-utc",
                "2024-05-06T07:08:09Z",
            ],
            false,
        ),
        (
            &[
                "audit-poll-classification",
                "--artifact-path",
                "missing",
                "--decisions-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "audit-add-feedback",
                "--path",
                "missing",
                "--findings-path",
                "missing",
                "--response-id",
                "response",
                "--finding-id",
                "finding",
                "--text-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "audit-get-feedback",
                "--path",
                "missing",
                "--finding-id",
                "finding",
            ],
            false,
        ),
        (
            &[
                "audit-test-merge-approval",
                "--path",
                "missing",
                "--pull-request-url",
                "https://example.com/p",
                "--source-commit",
                "abc",
            ],
            false,
        ),
        (
            &[
                "audit-set-merge-approval",
                "--path",
                "missing",
                "--context-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "audit-set-merge-approval-values",
                "--path",
                "missing",
                "--context-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "approved",
                "build",
                "--path",
                "missing",
                "--decisions-path",
                "missing",
                "--output-path",
                "output",
            ],
            false,
        ),
        (&["approved", "check", "--path", "missing"], false),
        (
            &["approved", "check-request", "--request-path", "missing"],
            false,
        ),
        (
            &[
                "plan-initialize",
                "--kind",
                "updates",
                "--path",
                "plan.json",
            ],
            true,
        ),
        (
            &[
                "plan-append",
                "--kind",
                "updates",
                "--path",
                "missing",
                "--item-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "initialize-finding",
                "--output-directory",
                ".",
                "--reviewer-id",
                "reviewer",
                "--focus-area",
                "correctness",
            ],
            true,
        ),
        (
            &[
                "add-finding",
                "--artifact-path",
                "missing",
                "--finding-path",
                "missing",
                "--origin",
                "reviewer",
            ],
            false,
        ),
        (
            &[
                "complete-finding",
                "--artifact-path",
                "missing",
                "--queue-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "validate-candidates",
                "--path",
                "missing",
                "--index-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "validate-findings",
                "--path",
                "missing",
                "--reviewer-id",
                "reviewer",
            ],
            true,
        ),
        (
            &[
                "finding-index",
                "--reviewer-directory",
                "missing",
                "--output-path",
                "output",
            ],
            false,
        ),
        (
            &[
                "consolidate-findings",
                "--reviewer-directory",
                "missing",
                "--output-path",
                "output",
            ],
            false,
        ),
        (
            &[
                "update-finding",
                "--artifact-path",
                "missing",
                "--plan-path",
                "missing",
            ],
            false,
        ),
        (
            &[
                "remove-finding",
                "--artifact-path",
                "missing",
                "--plan-path",
                "missing",
                "--now-utc",
                "2024-05-06T07:08:09Z",
            ],
            false,
        ),
    ];

    let help = workflow_command()
        .current_dir(directory.path())
        .args(["workflow", "--help"])
        .output()
        .unwrap();
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    let help = String::from_utf8(help.stdout).unwrap();
    let listed: BTreeSet<_> = help
        .lines()
        .skip_while(|line| *line != "Commands:")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
        .collect();
    let covered: BTreeSet<_> = cases
        .iter()
        .map(|(arguments, _)| *arguments.first().unwrap())
        .collect();
    assert_eq!(covered, listed);

    for (command, required) in [
        (
            "worker-document",
            &["--assignment-path", "--document-id"][..],
        ),
        ("restore-focus-area-cache", &["--snapshot-identity"][..]),
        (
            "begin-focus-area-generation",
            &["--preparation-identity"][..],
        ),
        ("publish-focus-area-generation", &["--generation-id"][..]),
        ("save-focus-area-cache", &["--preparation-identity"][..]),
        (
            "validate-focus-area-artifacts",
            &["--preparation-identity"][..],
        ),
        ("update-focus-area-expansion", &["--operation"][..]),
        ("update-focus-area-map", &["--operation", "--path"][..]),
        ("validate-focus-area-map", &["--map-path"][..]),
        ("assemble-focus-areas", &["--map-path"][..]),
        ("snapshot-instructions", &["--skill-root"][..]),
    ] {
        let invalid = workflow_command()
            .current_dir(directory.path())
            .args(["workflow", command])
            .output()
            .unwrap();
        assert_eq!(invalid.status.code(), Some(1));
        assert!(invalid.stdout.is_empty());
        let diagnostic = String::from_utf8_lossy(&invalid.stderr);
        assert!(
            diagnostic.contains("Invalid command arguments"),
            "{diagnostic}"
        );
        for argument in required {
            assert!(diagnostic.contains(argument), "{command}: {diagnostic}");
        }
    }

    for (arguments, succeeds) in cases {
        let output = workflow_command()
            .current_dir(directory.path())
            .arg("workflow")
            .args(*arguments)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(if *succeeds { 0 } else { 1 }),
            "{arguments:?}: {stderr}"
        );
        if *succeeds {
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            if arguments.first() == Some(&"validate-findings") {
                assert_eq!(result.get("isValid"), Some(&Value::Bool(false)));
            }
        } else {
            assert!(output.stdout.is_empty(), "{arguments:?}: unexpected stdout");
            assert!(
                !stderr.contains("Invalid command arguments"),
                "{arguments:?}: {stderr}"
            );
            assert!(!stderr.is_empty(), "{arguments:?}: missing diagnostic");
        }
    }
}

/// Worker commands retain their file-based handoff at the executable boundary.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
fn worker_assignment_and_document_run_with_a_bounded_stack() {
    let directory = Builder::new()
        .prefix(".workflow-worker-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let scope = directory.path().join("scope.md");
    fs::write(&scope, "Review scope\n").unwrap();
    let skill = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review");
    let output = workflow_command()
        .args(["workflow", "worker-assignment", "--phase", "fileGroups"])
        .arg("--session-state")
        .arg(directory.path())
        .arg("--scope-path")
        .arg(&scope)
        .arg("--output-directory")
        .arg(directory.path().join("assignment"))
        .arg("--skill-root")
        .arg(skill)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let assignment: Value = serde_json::from_slice(&output.stdout).unwrap();
    let path = assignment
        .get("assignmentPath")
        .and_then(Value::as_str)
        .unwrap();
    let output = workflow_command()
        .args([
            "workflow",
            "worker-document",
            "--assignment-path",
            path,
            "--document-id",
            "scope",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let page: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(page.get("content"), Some(&Value::from("Review scope\n")));
    assert_eq!(page.get("complete"), Some(&Value::Bool(true)));
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
