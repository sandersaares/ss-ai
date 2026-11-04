//! Preserves assessment-batch and path-scope contracts during native migration.
//!
//! Batch tests assert published records and failure preservation. Shared-file
//! tests cover the underlying replacement operation; internal call counts are
//! not part of the batch contract.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixtures use fixed record shapes and assert operation failures"
)]

use std::cell::Cell;
use std::fs;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai_review_workflow::{findings, review_work};
use ss_ai_runtime::{Clock, FileTransaction, classification, fail};

/// Supplies a reproducible removal timestamp and refuses unexpected contention.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2024-05-06T07:08:09Z".parse().unwrap()
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected lock contention"))
    }
}

/// Keeps each scenario's mutable state inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".migration-coverage-{}-{name}", std::process::id()));
        #[expect(clippy::create_dir, reason = "A collision must not reuse test state")]
        fs::create_dir(&path).unwrap();
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

/// Groups a deleted path and a new path together, plus an independent group.
struct BatchFixture {
    workspace: Workspace,
    inventory: PathBuf,
    groups: PathBuf,
    queue: PathBuf,
    artifact: PathBuf,
    files: Value,
    snapshot: Value,
}

impl BatchFixture {
    fn new(name: &str) -> Self {
        let workspace = Workspace::new(name);
        let files = json!([
            {"path":workspace.path("old.rs"),"sizeBytes":1,"change":"deleted"},
            {"path":workspace.path("new.ps1"),"sizeBytes":1,"change":"new"},
            {"path":workspace.path("lib.rs"),"sizeBytes":1,"change":"modified"}
        ]);
        let groups = json!([
            {"groupId":"rename","files":[files[0],files[1]]},
            {"groupId":"other","files":[files[2]]}
        ]);
        let inventory = workspace.put("inventory.json", &files);
        let groups = workspace.put("groups.json", &groups);
        let queue = workspace.path("queue.json");
        let artifact = workspace.path("artifact.json");
        let clock = FixedClock;
        findings::initialize(&artifact, "focus-001", "rules", false, &clock).unwrap();
        let state =
            review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
        let snapshot = state["snapshot"].clone();
        Self {
            workspace,
            inventory,
            groups,
            queue,
            artifact,
            files,
            snapshot,
        }
    }

    fn batch(&self, group: &str, indices: &[usize]) -> Value {
        let assessments: Vec<_> = indices
            .iter()
            .map(|&index| {
                json!({"groupId":group,"path":self.files[index]["path"],
                    "disposition":"reviewed","scope":"","reason":""})
            })
            .collect();
        json!({"schemaVersion":1,"snapshot":self.snapshot,"groupId":group,
            "assessments":assessments,"findingIds":[]})
    }

    fn complete(&self, batch: &Value) -> Result<Value, AppError> {
        review_work::complete_batch(
            &self.queue,
            &self.inventory,
            &self.groups,
            &self.artifact,
            batch,
            &FixedClock,
        )
    }

    fn journal(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.queue).unwrap()).unwrap()
    }
}

#[test]
fn assessment_shape_is_checked_at_draft_batch_and_journal_boundaries() {
    let fixture = BatchFixture::new("assessment-boundaries");
    review_work::next(&fixture.queue, &FixedClock).unwrap();
    let initial = fs::read(&fixture.queue).unwrap();
    let original: Value = serde_json::from_slice(&initial).unwrap();
    let good = fixture.batch("rename", &[0])["assessments"][0].clone();
    let invalid = [
        (json!(null), "assessment object"),
        (
            json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":"reviewed","scope":"","reason":"","extra":true}),
            "extra",
        ),
        (
            json!({"path":fixture.files[0]["path"],"disposition":"reviewed","scope":"","reason":""}),
            "groupId",
        ),
        (
            json!({"groupId":"bad id","path":fixture.files[0]["path"],"disposition":"reviewed","scope":"","reason":""}),
            "group identifier",
        ),
        (
            json!({"groupId":"réname","path":fixture.files[0]["path"],"disposition":"reviewed","scope":"","reason":""}),
            "group identifier",
        ),
        (
            json!({"groupId":"rename","path":"relative","disposition":"reviewed","scope":"","reason":""}),
            "absolute path",
        ),
        (
            json!({"groupId":"rename","path":null,"disposition":"reviewed","scope":"","reason":""}),
            "path",
        ),
        (
            json!({"groupId":"rename","path":"  ","disposition":"reviewed","scope":"","reason":""}),
            "non-empty string 'path'",
        ),
        (
            json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":" ","scope":"","reason":""}),
            "non-empty string 'disposition'",
        ),
        (
            json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":"reviewed","scope":null,"reason":""}),
            "scope",
        ),
        (
            json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":"reviewed","scope":"","reason":null}),
            "reason",
        ),
        (
            json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":"other","scope":"","reason":""}),
            "disposition",
        ),
        (
            json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":"excluded","scope":" ","reason":"reason"}),
            "scope and reason",
        ),
        (
            json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":"excluded","scope":"scope","reason":" "}),
            "scope and reason",
        ),
    ];

    for (assessment, detail) in invalid {
        let error = review_work::set_assessment(
            &fixture.queue,
            &fixture.inventory,
            &fixture.groups,
            &assessment,
            &FixedClock,
        )
        .unwrap_err();
        assert_eq!(classification(&error), "invalidInput");
        assert!(error.to_string().contains(detail), "{error}");
        assert_eq!(fs::read(&fixture.queue).unwrap(), initial);

        let mut batch = fixture.batch("rename", &[0, 1]);
        batch["assessments"][0] = assessment.clone();
        let error = fixture.complete(&batch).unwrap_err();
        assert_eq!(classification(&error), "invalidInput");
        assert!(error.to_string().contains(detail), "{error}");
        assert_eq!(fs::read(&fixture.queue).unwrap(), initial);

        let mut journal = original.clone();
        journal["assessments"] = json!([assessment]);
        let bytes = serde_json::to_vec(&journal).unwrap();
        fs::write(&fixture.queue, &bytes).unwrap();
        let error = review_work::next(&fixture.queue, &FixedClock).unwrap_err();
        assert_eq!(classification(&error), "invalidInput");
        assert!(error.to_string().contains(detail), "{error}");
        assert_eq!(fs::read(&fixture.queue).unwrap(), bytes);
        fs::write(&fixture.queue, &initial).unwrap();
    }

    // Reviewed records may have blank evidence, while exclusions require both fields.
    for assessment in [
        good.clone(),
        json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":"excluded","scope":"scope","reason":"reason"}),
    ] {
        let mut journal = original.clone();
        journal["assessments"] = json!([assessment.clone()]);
        fs::write(&fixture.queue, serde_json::to_vec(&journal).unwrap()).unwrap();
        review_work::next(&fixture.queue, &FixedClock).unwrap();
        fs::write(&fixture.queue, &initial).unwrap();
        review_work::set_assessment(
            &fixture.queue,
            &fixture.inventory,
            &fixture.groups,
            &assessment,
            &FixedClock,
        )
        .unwrap();
        fs::write(&fixture.queue, &initial).unwrap();
    }

    // The journal schema accepts the identifier vocabulary and preserves absolute
    // path spelling without requiring that the path exist.
    for assessment in [
        json!({"groupId":"0_-Z","path":fixture.workspace.path("folder").join("..").join("é script.rs"),"disposition":"reviewed","scope":" ","reason":" "}),
        json!({"groupId":"_","path":fixture.files[0]["path"],"disposition":"excluded","scope":" scope ","reason":" reason "}),
    ] {
        let mut journal = original.clone();
        journal["assessments"] = json!([assessment]);
        fs::write(&fixture.queue, serde_json::to_vec(&journal).unwrap()).unwrap();
        review_work::next(&fixture.queue, &FixedClock).unwrap();
    }

    let mut journal = original;
    journal["assessments"] = json!([good, good]);
    fs::write(&fixture.queue, serde_json::to_vec(&journal).unwrap()).unwrap();
    let error = review_work::next(&fixture.queue, &FixedClock).unwrap_err();
    assert_eq!(classification(&error), "invalidInput");
    assert!(error.to_string().contains("Duplicate assessment path"));
}

#[test]
fn malformed_journal_reports_earliest_assessment_field() {
    let fixture = BatchFixture::new("assessment-error-order");
    let mut journal = fixture.journal();
    // Each record has multiple faults so the diagnostic identifies the first
    // actionable field in the persisted assessment schema.
    for (record, detail) in [
        (
            json!({"groupId":"bad id","path":"relative","disposition":null,"scope":null}),
            "group identifier",
        ),
        (
            json!({"groupId":"rename","path":"relative","disposition":null,"scope":null}),
            "absolute path",
        ),
        (
            json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":null,"scope":null}),
            "non-empty string 'disposition'",
        ),
        (
            json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":"other","scope":null,"reason":null}),
            "assessment scope",
        ),
        (
            json!({"groupId":"rename","path":fixture.files[0]["path"],"disposition":"other","scope":"","reason":null}),
            "assessment reason",
        ),
    ] {
        journal["assessments"] = json!([record]);
        let bytes = serde_json::to_vec(&journal).unwrap();
        fs::write(&fixture.queue, &bytes).unwrap();
        let error = review_work::next(&fixture.queue, &FixedClock).unwrap_err();
        assert_eq!(classification(&error), "invalidInput");
        assert!(error.to_string().contains(detail), "{error}");
        assert_eq!(fs::read(&fixture.queue).unwrap(), bytes);
    }
}

#[test]
fn batch_completion_publishes_all_assessments_and_receipt_without_touching_findings() {
    let fixture = BatchFixture::new("batch-publication");
    let clock = FixedClock;
    review_work::next(&fixture.queue, &clock).unwrap();
    let batch = fixture.batch("rename", &[0, 1]);
    let artifact = fs::read(&fixture.artifact).unwrap();
    let result = fixture.complete(&batch).unwrap();
    assert_eq!(result["groupId"], "rename");
    assert_eq!(result["alreadyComplete"], false);
    assert_eq!(result["assessmentCount"], 2);
    let journal = fixture.journal();
    assert_eq!(journal["completedGroupIds"], json!(["rename"]));
    assert_eq!(journal["inProgress"], Value::Null);
    assert_eq!(journal["assessments"], batch["assessments"]);
    assert_eq!(journal["batchReceipts"].as_array().unwrap().len(), 1);
    assert_eq!(journal["batchReceipts"][0]["groupId"], "rename");
    assert_eq!(journal["batchReceipts"][0]["findingIds"], json!([]));
    assert_eq!(
        journal["batchReceipts"][0]["artifactPath"],
        fixture.artifact.to_str().unwrap()
    );
    assert_eq!(fs::read(&fixture.artifact).unwrap(), artifact);
}

#[test]
fn recorded_assessments_reject_initial_replays_and_conflicting_repairs() {
    for changed in [false, true] {
        let fixture = BatchFixture::new(if changed { "changed" } else { "identical" });
        let clock = FixedClock;
        review_work::next(&fixture.queue, &clock).unwrap();
        let mut batch = fixture.batch("rename", &[0, 1]);
        review_work::set_assessment(
            &fixture.queue,
            &fixture.inventory,
            &fixture.groups,
            &batch["assessments"][0],
            &clock,
        )
        .unwrap();
        review_work::initialize_queue(
            &fixture.inventory,
            &fixture.groups,
            &fixture.queue,
            true,
            &clock,
        )
        .unwrap();
        if changed {
            batch["assessments"][0]["reason"] = json!("A different judgment");
        }
        let before = fs::read(&fixture.queue).unwrap();
        let artifact = fs::read(&fixture.artifact).unwrap();
        assert!(fixture.complete(&batch).is_err());
        assert_eq!(fs::read(&fixture.queue).unwrap(), before);
        assert_eq!(fs::read(&fixture.artifact).unwrap(), artifact);
        assert_eq!(
            review_work::next(&fixture.queue, &clock).unwrap()["item"]["groupId"],
            "rename"
        );
        batch["assessments"].as_array_mut().unwrap().remove(0);
        fixture.complete(&batch).unwrap();
        let mut repaired = fixture.batch("rename", &[0, 1]);
        repaired["assessments"][0]["reason"] = json!("Repaired judgment");
        review_work::set_assessment(
            &fixture.queue,
            &fixture.inventory,
            &fixture.groups,
            &repaired["assessments"][0],
            &clock,
        )
        .unwrap();
        let before = fs::read(&fixture.queue).unwrap();
        assert!(fixture.complete(&batch).is_err());
        assert_eq!(fs::read(&fixture.queue).unwrap(), before);
    }
}

#[test]
fn changed_inventory_and_groups_do_not_authorize_resume_or_completion() {
    let fixture = BatchFixture::new("changed-inputs");
    let clock = FixedClock;
    review_work::next(&fixture.queue, &clock).unwrap();
    let batch = fixture.batch("rename", &[0, 1]);
    let before = fs::read(&fixture.queue).unwrap();
    let mut files = fixture.files.clone();
    files[0]["sizeBytes"] = json!(2);
    fs::write(&fixture.inventory, serde_json::to_vec(&files).unwrap()).unwrap();
    fs::write(
        &fixture.groups,
        serde_json::to_vec(&json!([
            {"groupId":"rename","files":[files[0],files[1]]},
            {"groupId":"other","files":[files[2]]}
        ]))
        .unwrap(),
    )
    .unwrap();
    assert!(fixture.complete(&batch).is_err());
    assert!(
        review_work::initialize_queue(
            &fixture.inventory,
            &fixture.groups,
            &fixture.queue,
            true,
            &clock
        )
        .is_err()
    );
    assert_eq!(fs::read(&fixture.queue).unwrap(), before);
}

#[test]
fn worker_failure_requires_validated_resume_and_preserves_prior_work() {
    let fixture = BatchFixture::new("failure-authority");
    let clock = FixedClock;
    let claim = review_work::next(&fixture.queue, &clock).unwrap();
    let mut batch = fixture.batch("rename", &[0, 1]);
    review_work::set_assessment(
        &fixture.queue,
        &fixture.inventory,
        &fixture.groups,
        &batch["assessments"][0],
        &clock,
    )
    .unwrap();
    review_work::set_failure(&fixture.queue, "worker stopped", &clock).unwrap();
    let failed = fs::read(&fixture.queue).unwrap();
    review_work::set_failure(&fixture.queue, "worker stopped", &clock).unwrap();
    assert_eq!(fs::read(&fixture.queue).unwrap(), failed);
    assert!(review_work::next(&fixture.queue, &clock).is_err());
    assert!(fixture.complete(&batch).is_err());
    assert!(review_work::complete_group(&fixture.queue, "rename", &clock).is_err());
    assert!(findings::complete(&fixture.artifact, &fixture.queue, &clock).is_err());
    assert_eq!(fs::read(&fixture.queue).unwrap(), failed);
    review_work::initialize_queue(
        &fixture.inventory,
        &fixture.groups,
        &fixture.queue,
        true,
        &clock,
    )
    .unwrap();
    assert_eq!(fixture.journal()["failure"], Value::Null);
    assert_eq!(
        fixture.journal()["assessments"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        review_work::next(&fixture.queue, &clock).unwrap()["item"],
        claim["item"]
    );
    batch["assessments"].as_array_mut().unwrap().remove(0);
    fixture.complete(&batch).unwrap();
}

#[test]
fn withdrawal_receipts_survive_before_and_after_completion() {
    for (name, before_batch, keep_active) in [
        ("withdraw-before", true, false),
        ("withdraw-after", false, false),
        ("withdraw-mixed", false, true),
    ] {
        let fixture = BatchFixture::new(name);
        let clock = FixedClock;
        let finding = fixture.workspace.put(
            "finding.json",
            &json!({
                "title":"Problem","description":"Description","reasoning":"Evidence",
                "consequence":"Impact","recommendedAction":"Fix",
                "anchor":{"kind":"file","path":"old.rs"}
            }),
        );
        let id = findings::add(&fixture.artifact, &finding, "reviewer", &[], &clock).unwrap();
        let mut batch = fixture.batch("rename", &[0, 1]);
        batch["findingIds"] = json!([id]);
        if keep_active {
            let active =
                findings::add(&fixture.artifact, &finding, "reviewer", &[], &clock).unwrap();
            batch["findingIds"]
                .as_array_mut()
                .unwrap()
                .push(json!(active));
        }
        review_work::next(&fixture.queue, &clock).unwrap();
        if !before_batch {
            fixture.complete(&batch).unwrap();
        }
        findings::remove(
            &fixture.artifact,
            &[(id.clone(), "Evidence does not support it".into())],
            &clock,
        )
        .unwrap();
        let removed =
            findings::read_artifact(&fixture.artifact, &clock).unwrap()["removed"].clone();
        if before_batch {
            fixture.complete(&batch).unwrap();
        }
        assert_eq!(
            review_work::next(&fixture.queue, &clock).unwrap()["item"]["groupId"],
            "other"
        );
        fixture.complete(&fixture.batch("other", &[2])).unwrap();
        let journal = fs::read(&fixture.queue).unwrap();
        let artifact = fs::read(&fixture.artifact).unwrap();
        assert_eq!(fixture.complete(&batch).unwrap()["alreadyComplete"], true);
        assert_eq!(fs::read(&fixture.queue).unwrap(), journal);
        assert_eq!(fs::read(&fixture.artifact).unwrap(), artifact);
        findings::complete(&fixture.artifact, &fixture.queue, &clock).unwrap();
        let completed = findings::read_artifact(&fixture.artifact, &clock).unwrap();
        assert_eq!(completed["removed"], removed);
        assert_eq!(
            completed["findings"].as_array().unwrap().len(),
            usize::from(keep_active)
        );
        assert_eq!(fs::read(&fixture.queue).unwrap(), journal);
        let mut malformed = completed;
        malformed["removed"][0]["finding"] = json!({"id":id});
        fs::write(&fixture.artifact, serde_json::to_vec(&malformed).unwrap()).unwrap();
        let artifact = fs::read(&fixture.artifact).unwrap();
        assert!(findings::complete(&fixture.artifact, &fixture.queue, &clock).is_err());
        assert_eq!(fs::read(&fixture.artifact).unwrap(), artifact);
        assert_eq!(fs::read(&fixture.queue).unwrap(), journal);
        malformed["removed"] = json!([]);
        fs::write(&fixture.artifact, serde_json::to_vec(&malformed).unwrap()).unwrap();
        assert!(findings::complete(&fixture.artifact, &fixture.queue, &clock).is_err());
        assert_eq!(fs::read(&fixture.queue).unwrap(), journal);
    }
}

/// Produces the same content-hash-bound metadata as generated focus areas.
fn area(workspace: &Workspace, contexts: &[Value]) -> PathBuf {
    let focus = workspace.path("focus");
    fs::create_dir_all(&focus).unwrap();
    let span = json!({"root":"instruction-set-raw","path":"rules.md","startLine":1,"endLine":1});
    let chapters: Vec<_> = contexts
        .iter()
        .enumerate()
        .map(|(index, context)| {
            json!({"id":format!("rule-{index}"),"categoryId":"all","content":span,
            "sources":[span],"applicability":context})
        })
        .collect();
    let content = "Rules\n";
    let digest = format!("{:X}", Sha256::digest(content.as_bytes()));
    let metadata = json!({"schemaVersion":1,"contentSha256":digest,"map":{
        "schemaVersion":1,"categories":[{"id":"all","title":"All"}],"chapters":chapters}});
    let encoded = STANDARD.encode(serde_json::to_vec(&metadata).unwrap());
    fs::write(
        focus.join("area.md"),
        format!("<!-- ss-review applicability {encoded} -->\n{content}"),
    )
    .unwrap();
    focus
}

/// Declared path alternatives are evidence only when no applicability is uncertain.
fn context(scope: &Value) -> Value {
    json!({"effectiveScope":"Declared paths","pathScope":scope,
        "declarations":[{"root":"instruction-set-raw","path":"rules.md","startLine":1,"endLine":1}],
        "origins":[],"uncertainties":[]})
}

fn prepare(fixture: &BatchFixture, contexts: &[Value], root: &Path) -> Value {
    let focus = area(&fixture.workspace, contexts);
    review_work::create(
        &fixture.inventory,
        &fixture.groups,
        &focus,
        &fixture.workspace.path("work"),
        Some(root),
        Some(&fixture.workspace.path("artifacts")),
        &FixedClock,
    )
    .unwrap()
}

#[test]
fn partial_exclusions_survive_resume_and_batch_with_exact_provenance() {
    let fixture = BatchFixture::new("partial-scope");
    let clock = FixedClock;
    let work = prepare(
        &fixture,
        &[json!([context(
            &json!({"base":".","include":["**/*.ps1"]})
        )])],
        &fixture.workspace.0,
    );
    assert_eq!(work["dispatchCount"], 1);
    let assignment = review_work::next(
        Path::new(work["focusAreaQueuePath"].as_str().unwrap()),
        &clock,
    )
    .unwrap()["item"]
        .clone();
    let queue = Path::new(assignment["fileGroupQueuePath"].as_str().unwrap());
    let artifact = Path::new(assignment["artifactPath"].as_str().unwrap());
    let inventory = Path::new(work["inventoryPath"].as_str().unwrap());
    let groups = Path::new(work["fileGroupsPath"].as_str().unwrap());
    let initial: Value = serde_json::from_slice(&fs::read(queue).unwrap()).unwrap();
    assert_eq!(initial["assessments"].as_array().unwrap().len(), 2);
    assert_eq!(initial["completedGroupIds"], json!(["other"]));
    let state = review_work::initialize_queue(inventory, groups, queue, true, &clock).unwrap();
    review_work::next(queue, &clock).unwrap();
    let mut batch = fixture.batch("rename", &[0, 1]);
    batch["snapshot"] = state["snapshot"].clone();
    let before = fs::read(queue).unwrap();
    assert!(
        review_work::complete_batch(queue, inventory, groups, artifact, &batch, &clock).is_err()
    );
    assert_eq!(fs::read(queue).unwrap(), before);
    batch["assessments"].as_array_mut().unwrap().remove(0);
    review_work::complete_batch(queue, inventory, groups, artifact, &batch, &clock).unwrap();
    let completed = fs::read(queue).unwrap();
    assert_eq!(
        review_work::complete_batch(queue, inventory, groups, artifact, &batch, &clock).unwrap()["alreadyComplete"],
        true
    );
    assert_eq!(fs::read(queue).unwrap(), completed);
    let journal: Value = serde_json::from_slice(&completed).unwrap();
    for excluded in initial["assessments"].as_array().unwrap() {
        assert!(
            journal["assessments"]
                .as_array()
                .unwrap()
                .contains(excluded)
        );
    }
    findings::complete(artifact, queue, &clock).unwrap();
    assert_eq!(
        review_work::validate(
            Path::new(work["manifestPath"].as_str().unwrap()),
            &fixture.workspace.path("artifacts"),
            &clock
        )
        .unwrap()["isValid"],
        true
    );
}

#[test]
fn conservative_scope_retains_alternatives_uncertainty_and_outside_paths() {
    let rust = context(&json!({"base":".","include":["**/*.rs"]}));
    let scripts = context(&json!({"base":".","include":["**/*.ps1"]}));
    let mut absent = context(&Value::Null);
    absent.as_object_mut().unwrap().remove("pathScope");
    let mut uncertain = context(&json!({"base":".","include":["**/Cargo.toml"]}));
    uncertain["uncertainties"] = json!([{"reason":"Needs content inspection","evidence":[
        {"root":"instruction-set-raw","path":"rules.md","startLine":1,"endLine":1}]}]);
    for (name, contexts, outside) in [
        (
            "scope-sections",
            vec![json!([rust.clone()]), json!([scripts.clone()])],
            false,
        ),
        ("scope-alternatives", vec![json!([rust, scripts])], false),
        (
            "scope-semantic",
            vec![json!([context(&Value::Null)])],
            false,
        ),
        ("scope-absent", vec![json!([absent])], false),
        ("scope-uncertain", vec![json!([uncertain])], false),
        (
            "scope-outside",
            vec![json!([context(
                &json!({"base":".","include":["**/Cargo.toml"]})
            )])],
            true,
        ),
    ] {
        let fixture = BatchFixture::new(name);
        let root = if outside {
            fixture.workspace.path("nested")
        } else {
            fixture.workspace.0.clone()
        };
        fs::create_dir_all(&root).unwrap();
        let work = prepare(&fixture, &contexts, &root);
        let assignment = review_work::next(
            Path::new(work["focusAreaQueuePath"].as_str().unwrap()),
            &FixedClock,
        )
        .unwrap()["item"]
            .clone();
        let journal: Value = serde_json::from_slice(
            &fs::read(assignment["fileGroupQueuePath"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert!(
            journal["assessments"].as_array().unwrap().is_empty(),
            "{name}"
        );
    }
}

#[test]
fn scope_globs_match_root_nested_and_case_without_guessing_filesystem_rules() {
    let fixture = BatchFixture::new("scope-globs");
    let files = json!([
        {"path":fixture.workspace.path("CODE.PS1"),"sizeBytes":1,"change":"modified"},
        {"path":fixture.workspace.path("src/code.ps1"),"sizeBytes":1,"change":"modified"},
        {"path":fixture.workspace.path("src/deep/code.ps1"),"sizeBytes":1,"change":"modified"},
        {"path":fixture.workspace.path("other.rs"),"sizeBytes":1,"change":"modified"}
    ]);
    fs::write(&fixture.inventory, serde_json::to_vec(&files).unwrap()).unwrap();
    fs::write(
        &fixture.groups,
        serde_json::to_vec(&json!([{"groupId":"group","files":files}])).unwrap(),
    )
    .unwrap();
    let work = prepare(
        &fixture,
        &[json!([context(
            &json!({"base":".","include":["**/*.ps1"]})
        )])],
        &fixture.workspace.0,
    );
    let assignment = review_work::next(
        Path::new(work["focusAreaQueuePath"].as_str().unwrap()),
        &FixedClock,
    )
    .unwrap()["item"]
        .clone();
    let journal: Value = serde_json::from_slice(
        &fs::read(assignment["fileGroupQueuePath"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(journal["assessments"].as_array().unwrap().len(), 1);
    assert_eq!(journal["assessments"][0]["path"], files[3]["path"]);

    let fixture = BatchFixture::new("scope-base");
    let files = json!([
        {"path":fixture.workspace.path("scripts/a.ps1"),"sizeBytes":1,"change":"modified"},
        {"path":fixture.workspace.path("scripts/deep/a.ps1"),"sizeBytes":1,"change":"modified"}
    ]);
    fs::write(&fixture.inventory, serde_json::to_vec(&files).unwrap()).unwrap();
    fs::write(
        &fixture.groups,
        serde_json::to_vec(&json!([{"groupId":"group","files":files}])).unwrap(),
    )
    .unwrap();
    let work = prepare(
        &fixture,
        &[json!([context(
            &json!({"base":"scripts","include":["*.ps1"]})
        )])],
        &fixture.workspace.0,
    );
    let assignment = review_work::next(
        Path::new(work["focusAreaQueuePath"].as_str().unwrap()),
        &FixedClock,
    )
    .unwrap()["item"]
        .clone();
    let journal: Value = serde_json::from_slice(
        &fs::read(assignment["fileGroupQueuePath"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(journal["assessments"].as_array().unwrap().len(), 1);
    assert_eq!(journal["assessments"][0]["path"], files[1]["path"]);
}

#[test]
fn damaged_metadata_and_unsupported_globs_do_not_authorize_exclusions() {
    for (name, pattern, damage) in [
        ("scope-damaged", "**/*.ps1", true),
        ("scope-unsupported", "a**b", false),
    ] {
        let fixture = BatchFixture::new(name);
        let focus = area(
            &fixture.workspace,
            &[json!([context(&json!({"base":".","include":[pattern]}))])],
        );
        if damage {
            // Metadata with a changed content body must fail, rather than disappear.
            let original = area_text_for_stale_metadata();
            fs::write(focus.join("area.md"), original).unwrap();
        }
        assert!(
            review_work::create(
                &fixture.inventory,
                &fixture.groups,
                &focus,
                &fixture.workspace.path("work"),
                Some(&fixture.workspace.0),
                Some(&fixture.workspace.path("artifacts")),
                &FixedClock
            )
            .is_err()
        );
    }
}

/// A stale body retains its metadata header so the reader must reject it.
fn area_text_for_stale_metadata() -> String {
    let content = "Rules\n";
    let span = json!({"root":"instruction-set-raw","path":"rules.md","startLine":1,"endLine":1});
    let metadata = json!({"schemaVersion":1,"contentSha256":format!("{:X}", Sha256::digest(content.as_bytes())),
        "map":{"schemaVersion":1,"categories":[{"id":"all","title":"All"}],
        "chapters":[{"id":"rule","categoryId":"all","content":span,"sources":[span],
            "applicability":[context(&json!({"base":".","include":["**/*.ps1"]}))]}]}});
    format!(
        "<!-- ss-review applicability {} -->\nChanged\n",
        STANDARD.encode(serde_json::to_vec(&metadata).unwrap())
    )
}

#[test]
fn incomplete_failed_and_missing_reviewers_are_distinct_from_excluded_work() {
    let fixture = BatchFixture::new("reviewer-states");
    let clock = FixedClock;
    let work = prepare(
        &fixture,
        &[json!([context(&Value::Null)])],
        &fixture.workspace.0,
    );
    let dispatch = Path::new(work["focusAreaQueuePath"].as_str().unwrap());
    let assignment = review_work::next(dispatch, &clock).unwrap()["item"].clone();
    let queue = Path::new(assignment["fileGroupQueuePath"].as_str().unwrap());
    let artifact = Path::new(assignment["artifactPath"].as_str().unwrap());
    let manifest = Path::new(work["manifestPath"].as_str().unwrap());
    let artifacts = fixture.workspace.path("artifacts");
    review_work::next(queue, &clock).unwrap();
    let result = review_work::validate(manifest, &artifacts, &clock).unwrap();
    assert_eq!(result["isValid"], false);
    assert_eq!(result["incompleteReviewers"], json!(["focus-001"]));
    assert_eq!(result["failedReviewers"], json!([]));
    review_work::set_failure(queue, "worker stopped", &clock).unwrap();
    let result = review_work::validate(manifest, &artifacts, &clock).unwrap();
    assert_eq!(result["isValid"], false);
    assert_eq!(result["failedReviewers"], json!(["focus-001"]));
    fs::remove_file(artifact).unwrap();
    let result = review_work::validate(manifest, &artifacts, &clock).unwrap();
    assert_eq!(result["isValid"], false);
    assert_eq!(result["missingReviewers"], json!(["focus-001"]));
    assert!(findings::complete(artifact, queue, &clock).is_err());
}

/// Signals actual contention and requires explicit permission for its sole retry.
struct GateClock {
    event: Sender<()>,
    permit: Receiver<()>,
    waited: Cell<bool>,
}

impl Clock for GateClock {
    fn now(&self) -> Timestamp {
        panic!("Batch completion does not read the clock")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        if self.waited.replace(true) {
            return Err(fail("contention", "Unexpected second wait"));
        }
        self.event
            .send(())
            .map_err(|error| fail("io", error.to_string()))?;
        self.permit
            .recv()
            .map_err(|error| fail("io", error.to_string()))
    }
}

#[test]
#[cfg_attr(
    mutants,
    ignore = "A mutation may keep the worker alive without signaling contention"
)]
fn batch_waits_for_existing_journal_transaction() {
    let fixture = BatchFixture::new("batch-lock");
    let clock = FixedClock;
    review_work::next(&fixture.queue, &clock).unwrap();
    let batch = fixture.batch("rename", &[0, 1]);
    let held = FileTransaction::open(&fixture.queue, &clock).unwrap();
    let (event_tx, event_rx) = mpsc::channel();
    let (permit_tx, permit_rx) = mpsc::channel();
    std::thread::scope(move |scope| {
        let worker = scope.spawn(move || {
            let clock = GateClock {
                event: event_tx,
                permit: permit_rx,
                waited: Cell::new(false),
            };
            review_work::complete_batch(
                &fixture.queue,
                &fixture.inventory,
                &fixture.groups,
                &fixture.artifact,
                &batch,
                &clock,
            )
        });
        event_rx.recv().unwrap();
        drop(held);
        permit_tx.send(()).unwrap();
        drop(permit_tx);
        assert_eq!(worker.join().unwrap().unwrap()["alreadyComplete"], false);
    });
}

#[test]
fn completed_exclusions_are_revalidated_against_provenance() {
    let fixture = BatchFixture::new("excluded-provenance");
    let clock = FixedClock;
    let work = prepare(
        &fixture,
        &[json!([context(
            &json!({"base":".","include":["**/Cargo.toml"]})
        )])],
        &fixture.workspace.0,
    );
    assert_eq!(work["dispatchCount"], 0);
    assert_eq!(work["excludedReviewerCount"], 1);
    let manifest = Path::new(work["manifestPath"].as_str().unwrap());
    let artifacts = fixture.workspace.path("artifacts");
    assert_eq!(
        review_work::validate(manifest, &artifacts, &clock).unwrap()["isValid"],
        true
    );
    let manifest_value: Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    let journal_path = Path::new(
        manifest_value["reviewers"][0]["fileGroupQueuePath"]
            .as_str()
            .unwrap(),
    );
    let mut journal: Value = serde_json::from_slice(&fs::read(journal_path).unwrap()).unwrap();
    journal["assessments"][0]["reason"] = json!("Invented reason");
    fs::write(journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
    assert_eq!(
        review_work::validate(manifest, &artifacts, &clock).unwrap()["isValid"],
        false
    );
}

#[test]
fn bounded_sequences_preserve_complete_coverage_across_retries_and_resume() {
    let fixture = BatchFixture::new("bounded-sequences");
    let clock = FixedClock;
    let mut state = 0x7294_1185_u32;
    let mut committed = Vec::new();
    // Marsaglia's xorshift32 transition uses the published (13, 17, 5) shifts:
    // https://www.jstatsoft.org/article/view/v008i14
    // The fixed seed selects reproducible operations; the finite tail completes work.
    for _ in 0..32 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        match state % 3 {
            0 => {
                review_work::initialize_queue(
                    &fixture.inventory,
                    &fixture.groups,
                    &fixture.queue,
                    true,
                    &clock,
                )
                .unwrap();
            }
            1 => {
                let claim = review_work::next(&fixture.queue, &clock).unwrap();
                if claim["exhausted"] == false {
                    let group = claim["item"]["groupId"].as_str().unwrap();
                    let batch = if group == "rename" {
                        fixture.batch(group, &[0, 1])
                    } else {
                        fixture.batch(group, &[2])
                    };
                    fixture.complete(&batch).unwrap();
                    committed.push(batch);
                }
            }
            _ => {
                if let Some(batch) = committed.last() {
                    let before = fs::read(&fixture.queue).unwrap();
                    assert_eq!(fixture.complete(batch).unwrap()["alreadyComplete"], true);
                    assert_eq!(fs::read(&fixture.queue).unwrap(), before);
                }
            }
        }
        let journal = fixture.journal();
        let paths: Vec<_> = journal["assessments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["path"].clone())
            .collect();
        for path in &paths {
            assert_eq!(
                paths.iter().filter(|candidate| *candidate == path).count(),
                1
            );
        }
        assert!(paths.iter().all(|path| {
            fixture
                .files
                .as_array()
                .unwrap()
                .iter()
                .any(|file| file["path"] == *path)
        }));
    }
    for _ in 0..2 {
        let claim = review_work::next(&fixture.queue, &clock).unwrap();
        if claim["exhausted"] == false {
            let group = claim["item"]["groupId"].as_str().unwrap();
            let batch = if group == "rename" {
                fixture.batch(group, &[0, 1])
            } else {
                fixture.batch(group, &[2])
            };
            fixture.complete(&batch).unwrap();
        }
    }
    assert_eq!(
        fixture.journal()["assessments"].as_array().unwrap().len(),
        3
    );
    findings::complete(&fixture.artifact, &fixture.queue, &clock).unwrap();
}

/// Stops the replacement retry only after confirming the complete staged journal.
#[cfg(windows)]
struct ReplacementFailureClock {
    directory: PathBuf,
    queue: PathBuf,
    before: Vec<u8>,
    waited: Cell<bool>,
}

#[cfg(windows)]
impl Clock for ReplacementFailureClock {
    fn now(&self) -> Timestamp {
        panic!("Batch completion does not read the clock")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        assert!(!self.waited.replace(true));
        let staging = fs::read_dir(&self.directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".ss-ai-transaction-")
            })
            .collect::<Vec<_>>();
        let [staged] = staging.as_slice() else {
            panic!("Expected one staged journal");
        };
        let journal: Value = serde_json::from_slice(&fs::read(staged).unwrap()).unwrap();
        assert_eq!(journal["completedGroupIds"], json!(["rename"]));
        assert_eq!(journal["assessments"].as_array().unwrap().len(), 2);
        assert_eq!(fs::read(&self.queue).unwrap(), self.before);
        Err(fail("contention", "Replacement remains blocked"))
    }
}

#[cfg(windows)]
#[test]
#[cfg_attr(miri, ignore = "requires Windows file sharing and replacement")]
fn failed_replacement_preserves_journal_and_allows_retry() {
    let fixture = BatchFixture::new("failed-replacement");
    let clock = FixedClock;
    review_work::next(&fixture.queue, &clock).unwrap();
    let batch = fixture.batch("rename", &[0, 1]);
    let before = fs::read(&fixture.queue).unwrap();
    let artifact = fs::read(&fixture.artifact).unwrap();
    // Sharing reads and writes but not deletion blocks ReplaceFile without blocking reads.
    let blocker = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001 | 0x0000_0002)
        .open(&fixture.queue)
        .unwrap();
    let failure_clock = ReplacementFailureClock {
        directory: fixture.workspace.0.clone(),
        queue: fixture.queue.clone(),
        before: before.clone(),
        waited: Cell::new(false),
    };
    assert!(
        review_work::complete_batch(
            &fixture.queue,
            &fixture.inventory,
            &fixture.groups,
            &fixture.artifact,
            &batch,
            &failure_clock,
        )
        .is_err()
    );
    assert!(failure_clock.waited.get());
    assert_eq!(fs::read(&fixture.queue).unwrap(), before);
    assert_eq!(fs::read(&fixture.artifact).unwrap(), artifact);
    assert!(fs::read_dir(&fixture.workspace.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".ss-ai-transaction-")
    }));
    drop(blocker);
    review_work::initialize_queue(
        &fixture.inventory,
        &fixture.groups,
        &fixture.queue,
        true,
        &clock,
    )
    .unwrap();
    assert_eq!(
        review_work::next(&fixture.queue, &clock).unwrap()["item"]["groupId"],
        "rename"
    );
    assert_eq!(fixture.complete(&batch).unwrap()["alreadyComplete"], false);
}
