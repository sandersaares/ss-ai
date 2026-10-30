//! Exercises review coverage and journal transactions through native APIs.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixtures use fixed record shapes and assert failures"
)]

use std::cell::Cell;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::{
    findings, focus_area_expansion, focus_area_generation, focus_area_map, focus_cache,
    review_work, worker_input,
};
use ss_ai_runtime::{Clock, FileTransaction, LockMode, ResourceLock, SystemClock, caused_by, fail};

/// Excludes process creation while a test requires releasing the only lock handle.
///
/// A Unix child inherits other threads' lock descriptors until exec, even when
/// those descriptors are close-on-exec.
static PROCESS_CREATION_AND_CONTROLLED_LOCK: Mutex<()> = Mutex::new(());

/// Keeps each test's journal and staging files inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".review-work-test-{}-{name}", std::process::id()));
        #[expect(clippy::create_dir, reason = "A collision must not reuse test state")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn put(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Creates deleted and rename-side records without requiring the files to exist.
fn inputs(workspace: &Workspace) -> (PathBuf, PathBuf, Value) {
    let files = json!([
        {"path":workspace.path("old").to_str().unwrap(),"sizeBytes":0,"change":"deleted"},
        {"path":workspace.path("new").to_str().unwrap(),"sizeBytes":17,"change":"new"}
    ]);
    let groups = json!([{"groupId":"group-1","files":files}]);
    (
        workspace.put("inventory.json", &files),
        workspace.put("groups.json", &groups),
        files,
    )
}

/// Produces a published preparation using the same identity chain as the workflow.
fn published_preparation(workspace: &Workspace, clock: &dyn Clock) -> String {
    let session = &workspace.0;
    fs::create_dir_all(session.join("instruction-set-source")).unwrap();
    fs::write(
        session.join("instruction-set-source/guide.md"),
        "# Rule\nKeep it.\n",
    )
    .unwrap();
    let identity = restore_identity(workspace, clock);
    publish_identity(session, &identity, clock);
    identity
}

/// Publishes complete artifacts for the caller's restored preparation.
fn publish_identity(session: &Path, identity: &str, clock: &dyn Clock) {
    let generation = focus_area_generation::begin(session, identity, clock).unwrap();
    let preparation = Path::new(&generation.session_state);
    focus_area_expansion::update(
        preparation,
        focus_area_expansion::Operation::Initialize,
        clock,
    )
    .unwrap();
    focus_area_expansion::update(
        preparation,
        focus_area_expansion::Operation::Finalize,
        clock,
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
        clock,
    )
    .unwrap();
    focus_area_map::assemble(preparation, &map_path, clock).unwrap();
    focus_area_generation::publish(session, &generation.generation_id, clock).unwrap();
}

/// Starts a new source and restore epoch without authorizing old artifacts.
fn restore_identity(workspace: &Workspace, clock: &dyn Clock) -> String {
    let session = &workspace.0;
    let _operation = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        clock,
    )
    .unwrap();
    focus_area_generation::invalidate_locked(session, clock).unwrap();
    focus_area_generation::invalidate_snapshot_locked(session, clock).unwrap();
    focus_area_generation::bind_snapshot_locked(session, clock).unwrap();
    focus_area_generation::bind_restore_locked(
        session,
        &session.join("instruction-set-source"),
        clock,
    )
    .unwrap()
}

#[test]
fn bound_work_rejects_stale_unready_changed_and_unbound_artifacts() {
    let workspace = Workspace::new("bound-fencing");
    let clock = SystemClock::default();
    let (inventory, groups, _) = inputs(&workspace);
    let first = published_preparation(&workspace, &clock);
    let focus = workspace.path("focus-areas");
    let output = workspace.path("work");
    let make = |identity: &str, focus_path: &Path| {
        review_work::create_with_preparation(
            &inventory,
            &groups,
            focus_path,
            &output,
            None,
            None,
            &review_work::PreparationBinding {
                session: &workspace.0,
                identity,
            },
            &clock,
        )
    };

    assert!(review_work::create(&inventory, &groups, &focus, &output, None, None, &clock).is_err());
    let other = workspace.path("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("area.md"), "Legacy guidance").unwrap();
    assert!(make(&first, &other).is_err());
    assert!(!output.exists());
    assert!(
        review_work::create(
            &inventory,
            &groups,
            &other,
            &workspace.path("legacy-work"),
            None,
            None,
            &clock
        )
        .is_ok()
    );

    let second = restore_identity(&workspace, &clock);
    assert_ne!(first, second);
    assert!(make(&first, &focus).is_err());
    assert!(make(&second, &focus).is_err());
    assert!(!output.exists());

    // A published receipt must not authorize later modified artifact bytes.
    publish_identity(&workspace.0, &second, &clock);
    fs::write(focus.join("focus-rules.md"), "changed").unwrap();
    assert!(make(&second, &focus).is_err());
    assert!(!output.exists());
}

/// Keeps cache freshness stable and treats unexpected contention as a failure.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected fixture lock contention"))
    }
}

#[test]
fn bound_work_accepts_verified_cache_hits() {
    let workspace = Workspace::new("bound-cache-hit");
    let clock = FixedClock;
    let (inventory, groups, _) = inputs(&workspace);
    let source = workspace.path("instruction-set-source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("guide.md"), "# Rule\nKeep it.\n").unwrap();
    let skill =
        fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review"))
            .unwrap();
    let recipe = skill.join("prepare-focus-areas.md");
    let cache = workspace.path("cache");
    let options = focus_cache::CacheOptions {
        session: &workspace.0,
        input_directory: &source,
        skill_root: &skill,
        recipe: &recipe,
        cache_directory: &cache,
        generator_profile: "review-test",
        // Match the wrapper default; the fake clock keeps the bucket stable.
        freshness_days: 7,
    };
    let snapshot = {
        let _operation = ResourceLock::open(
            &workspace.path("focus-area-cache-operation"),
            LockMode::Exclusive,
            &clock,
        )
        .unwrap();
        focus_area_generation::invalidate_locked(&workspace.0, &clock).unwrap();
        focus_area_generation::invalidate_snapshot_locked(&workspace.0, &clock).unwrap();
        focus_area_generation::bind_snapshot_locked(&workspace.0, &clock).unwrap()
    };
    let miss = focus_cache::restore(&options, false, &snapshot, &clock).unwrap();
    assert_eq!(miss.status, "Miss");
    let identity = miss.preparation_identity.unwrap();
    publish_identity(&workspace.0, &identity, &clock);
    assert_eq!(
        focus_cache::store(&options, &identity, &clock)
            .unwrap()
            .status,
        "Stored"
    );
    let snapshot = {
        let _operation = ResourceLock::open(
            &workspace.path("focus-area-cache-operation"),
            LockMode::Exclusive,
            &clock,
        )
        .unwrap();
        focus_area_generation::invalidate_locked(&workspace.0, &clock).unwrap();
        focus_area_generation::invalidate_snapshot_locked(&workspace.0, &clock).unwrap();
        focus_area_generation::bind_snapshot_locked(&workspace.0, &clock).unwrap()
    };
    let hit = focus_cache::restore(&options, false, &snapshot, &clock).unwrap();
    assert_eq!(hit.status, "Hit");
    let identity = hit.preparation_identity.unwrap();
    let result = review_work::create_with_preparation(
        &inventory,
        &groups,
        &workspace.path("focus-areas"),
        &workspace.path("work"),
        None,
        None,
        &review_work::PreparationBinding {
            session: &workspace.0,
            identity: &identity,
        },
        &clock,
    )
    .unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(result["manifestPath"].as_str().unwrap()).unwrap())
            .unwrap();
    assert!(
        Path::new(manifest["reviewers"][0]["focusArea"].as_str().unwrap())
            .starts_with(workspace.path("work"))
    );
}

/// Builds an assessment for each assigned file.
fn assessments(files: &Value) -> Value {
    Value::Array(
        files.as_array().unwrap().iter().map(|file| {
            json!({"groupId":"group-1","path":file["path"],"disposition":"reviewed","scope":"","reason":""})
        }).collect(),
    )
}

#[test]
fn coverage_requires_exact_paths_and_metadata() {
    let workspace = Workspace::new("coverage");
    let clock = SystemClock::default();
    let (inventory, groups, files) = inputs(&workspace);
    let published = workspace.path("published.json");
    assert!(review_work::set_input_groups(&inventory, &groups, &published, false, &clock).is_ok());
    assert!(review_work::set_input_groups(&inventory, &groups, &published, false, &clock).is_err());
    let omitted = workspace.put("omitted.json", &json!([{"groupId":"g","files":[files[0]]}]));
    assert!(review_work::set_input_groups(&inventory, &omitted, &published, true, &clock).is_err());
    let altered = workspace.put("altered.json", &json!([{"groupId":"g","files":[files[0],{"path":files[1]["path"],"sizeBytes":18,"change":"new"}]}]));
    assert!(review_work::set_input_groups(&inventory, &altered, &published, true, &clock).is_err());
    assert_eq!(fs::read(&published).unwrap(), fs::read(&groups).unwrap());
}

#[test]
fn group_limits_allow_only_oversized_singletons() {
    let workspace = Workspace::new("limits");
    let clock = SystemClock::default();
    let files: Vec<_> = (0..21)
        .map(|index| json!({"path":workspace.path(&format!("f{index}")).to_str().unwrap(),"sizeBytes":1,"change":"modified"}))
        .collect();
    let inventory = workspace.put("inventory.json", &json!(files));
    let groups = workspace.put("groups.json", &json!([{"groupId":"g","files":files}]));
    let output = workspace.path("output.json");
    assert!(review_work::set_input_groups(&inventory, &groups, &output, false, &clock).is_err());
    let large = json!({"path":workspace.path("large").to_str().unwrap(),"sizeBytes":100001,"change":"modified"});
    let small =
        json!({"path":workspace.path("small").to_str().unwrap(),"sizeBytes":1,"change":"modified"});
    workspace.put("inventory.json", &json!([large, small]));
    workspace.put(
        "groups.json",
        &json!([{"groupId":"g","files":[large,small]}]),
    );
    assert!(review_work::set_input_groups(&inventory, &groups, &output, false, &clock).is_err());
    workspace.put(
        "groups.json",
        &json!([{"groupId":"big","files":[large]},{"groupId":"small","files":[small]}]),
    );
    assert!(review_work::set_input_groups(&inventory, &groups, &output, false, &clock).is_ok());
}

#[test]
fn resume_rejects_reformatted_snapshot_without_resetting_work() {
    let workspace = Workspace::new("stale");
    let clock = SystemClock::default();
    let (inventory, groups, _) = inputs(&workspace);
    let queue = workspace.path("queue.json");
    review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    review_work::next(&queue, &clock).unwrap();
    review_work::set_failure(&queue, "stopped", &clock).unwrap();
    let before = fs::read(&queue).unwrap();
    let mut group_text = fs::read_to_string(&groups).unwrap();
    group_text.push('\n');
    fs::write(&groups, group_text).unwrap();
    assert!(review_work::initialize_queue(&inventory, &groups, &queue, true, &clock).is_err());
    assert_eq!(fs::read(&queue).unwrap(), before);
}

#[test]
fn malformed_persisted_unicode_cannot_replace_input() {
    let workspace = Workspace::new("malformed-unicode");
    let clock = SystemClock::default();
    let (inventory, _, _) = inputs(&workspace);
    let valid = fs::read_to_string(&inventory).unwrap();
    let output = workspace.put("output.json", &json!({"preserved":true}));
    let before = fs::read(&output).unwrap();
    let byte_index = valid.find("\"deleted\"").unwrap() + 1;
    let prefix = valid.get(..byte_index).unwrap();
    let utf16_index = prefix.encode_utf16().count();
    let utf32_index = prefix.chars().count();
    let mut invalid_utf8 = valid.as_bytes().to_vec();
    invalid_utf8[byte_index] = 0xff;
    let mut invalid_utf16 = vec![0xff, 0xfe];
    for (index, word) in valid.encode_utf16().enumerate() {
        let word = if index == utf16_index { 0xd800 } else { word };
        invalid_utf16.extend_from_slice(&u16::to_le_bytes(word));
    }
    let mut invalid_utf32 = vec![0xff, 0xfe, 0x00, 0x00];
    for (index, character) in valid.chars().enumerate() {
        let word = if index == utf32_index {
            0x110000
        } else {
            u32::from(character)
        };
        invalid_utf32.extend_from_slice(&u32::to_le_bytes(word));
    }
    for bytes in [invalid_utf8, invalid_utf16, invalid_utf32] {
        fs::write(&inventory, bytes).unwrap();
        assert!(review_work::set_input_inventory(&inventory, &output, true, &clock).is_err());
        assert_eq!(fs::read(&output).unwrap(), before);
    }
}

#[test]
fn utf16_inventory_snapshot_hashes_decoded_text() {
    let workspace = Workspace::new("utf16-snapshot");
    let clock = SystemClock::default();
    let (inventory, groups, _) = inputs(&workspace);
    let text = fs::read_to_string(&inventory).unwrap();
    let mut encoded = vec![0xff, 0xfe];
    for word in text.encode_utf16() {
        encoded.extend_from_slice(&word.to_le_bytes());
    }
    fs::write(&inventory, encoded).unwrap();
    let result = review_work::initialize_queue(
        &inventory,
        &groups,
        &workspace.path("queue.json"),
        false,
        &clock,
    )
    .unwrap();
    assert_eq!(
        result["snapshot"]["inventorySha256"],
        ss_ai_review_context::bytes_hash(text.as_bytes())
    );
}

#[test]
fn batch_retry_failure_and_resume_preserve_claims() {
    let workspace = Workspace::new("batch");
    let clock = SystemClock::default();
    let (inventory, groups, files) = inputs(&workspace);
    let queue = workspace.path("queue.json");
    let artifact = workspace.path("artifact.json");
    findings::initialize(&artifact, "focus-001", "/focus", false, &clock).unwrap();
    let state = review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    let claim = review_work::next(&queue, &clock).unwrap();
    assert_eq!(claim["item"]["groupId"], "group-1");
    assert_eq!(
        review_work::next(&queue, &clock).unwrap()["item"],
        claim["item"]
    );
    let batch = json!({"schemaVersion":1,"snapshot":state["snapshot"],"groupId":"group-1","assessments":assessments(&files),"findingIds":[]});
    let mut partial = batch.clone();
    partial["assessments"].as_array_mut().unwrap().pop();
    assert!(
        review_work::complete_batch(&queue, &inventory, &groups, &artifact, &partial, &clock)
            .is_err()
    );
    review_work::set_failure(&queue, "worker stopped", &clock).unwrap();
    assert!(review_work::next(&queue, &clock).is_err());
    assert!(
        review_work::complete_batch(&queue, &inventory, &groups, &artifact, &batch, &clock)
            .is_err()
    );
    review_work::initialize_queue(&inventory, &groups, &queue, true, &clock).unwrap();
    assert_eq!(
        review_work::next(&queue, &clock).unwrap()["item"],
        claim["item"]
    );
    assert_eq!(
        review_work::complete_batch(&queue, &inventory, &groups, &artifact, &batch, &clock)
            .unwrap()["alreadyComplete"],
        false
    );
    let before = fs::read(&queue).unwrap();
    assert_eq!(
        review_work::complete_batch(&queue, &inventory, &groups, &artifact, &batch, &clock)
            .unwrap()["alreadyComplete"],
        true
    );
    assert_eq!(fs::read(&queue).unwrap(), before);
    let mut conflict = batch;
    conflict["assessments"][0]["reason"] = json!("changed");
    assert!(
        review_work::complete_batch(&queue, &inventory, &groups, &artifact, &conflict, &clock)
            .is_err()
    );
    assert_eq!(fs::read(&queue).unwrap(), before);
    assert_eq!(
        review_work::next(&queue, &clock).unwrap()["exhausted"],
        true
    );
}

#[test]
fn individual_assessments_remain_compatible_with_group_completion() {
    let workspace = Workspace::new("individual");
    let clock = SystemClock::default();
    let (inventory, groups, files) = inputs(&workspace);
    let queue = workspace.path("queue.json");
    review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    review_work::next(&queue, &clock).unwrap();
    let records = assessments(&files);
    review_work::set_assessment(&queue, &inventory, &groups, &records[0], &clock).unwrap();
    assert!(review_work::complete_group(&queue, "group-1", &clock).is_err());
    review_work::set_assessment(&queue, &inventory, &groups, &records[1], &clock).unwrap();
    assert_eq!(
        review_work::complete_group(&queue, "group-1", &clock).unwrap()["alreadyComplete"],
        false
    );
    assert_eq!(
        review_work::complete_group(&queue, "group-1", &clock).unwrap()["alreadyComplete"],
        true
    );
}

#[test]
fn file_drafts_reject_duplicate_properties_and_accept_relative_paths() {
    let workspace = Workspace::new("file-drafts");
    let clock = SystemClock::default();
    let (inventory, groups, files) = inputs(&workspace);
    let queue = workspace.path("queue.json");
    let artifact = workspace.path("artifact.json");
    findings::initialize(&artifact, "focus-001", "/focus", false, &clock).unwrap();
    let state = review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    review_work::next(&queue, &clock).unwrap();
    let records = assessments(&files);
    let assessment_path = workspace.path("assessment.json");
    let batch_path = workspace.path("batch.json");
    let assessment = serde_json::to_string(&records[0]).unwrap();
    let batch = json!({"schemaVersion":1,"snapshot":state["snapshot"],"groupId":"group-1","assessments":[records[1]],"findingIds":[]});
    let batch = serde_json::to_string(&batch).unwrap();
    let before = fs::read(&queue).unwrap();
    for duplicate in ["groupId", "GroupId"] {
        let invalid = assessment.replacen(
            "\"groupId\":",
            &format!("\"{duplicate}\":\"group-1\",\"groupId\":"),
            1,
        );
        fs::write(&assessment_path, invalid).unwrap();
        assert!(
            review_work::set_assessment_from_file(
                &queue,
                &inventory,
                &groups,
                &assessment_path,
                &clock
            )
            .is_err()
        );
        let invalid = batch.replacen(
            "\"disposition\":",
            &format!(
                "\"{}\":\"reviewed\",\"disposition\":",
                if duplicate == "groupId" {
                    "disposition"
                } else {
                    "Disposition"
                }
            ),
            1,
        );
        fs::write(&batch_path, invalid).unwrap();
        assert!(
            review_work::complete_batch_from_file(
                &queue,
                &inventory,
                &groups,
                &artifact,
                &batch_path,
                &clock
            )
            .is_err()
        );
        assert_eq!(fs::read(&queue).unwrap(), before);
    }

    fs::write(&assessment_path, format!("\u{feff}{assessment}")).unwrap();
    fs::write(&batch_path, format!("\u{feff}{batch}")).unwrap();
    let cwd = env::current_dir().unwrap();
    let relative = |path: &Path| path.strip_prefix(&cwd).unwrap().to_owned();
    let result = review_work::set_assessment_from_file(
        &relative(&queue),
        &relative(&inventory),
        &relative(&groups),
        &relative(&assessment_path),
        &clock,
    )
    .unwrap();
    assert_eq!(
        result,
        json!({"status":"Recorded","path":records[0]["path"],"disposition":"reviewed"})
    );
    let result = review_work::complete_batch_from_file(
        &relative(&queue),
        &relative(&inventory),
        &relative(&groups),
        &relative(&artifact),
        &relative(&batch_path),
        &clock,
    )
    .unwrap();
    assert_eq!(result["alreadyComplete"], false);
}

#[test]
fn manifest_gate_rejects_missing_or_incomplete_coverage() {
    let workspace = Workspace::new("manifest");
    let clock = SystemClock::default();
    let (inventory, groups, files) = inputs(&workspace);
    let focus = workspace.path("focus");
    fs::create_dir_all(&focus).unwrap();
    fs::write(focus.join("area.md"), "Focus").unwrap();
    let work = review_work::create(
        &inventory,
        &groups,
        &focus,
        &workspace.path("work"),
        None,
        None,
        &clock,
    )
    .unwrap();
    let manifest = Path::new(work["manifestPath"].as_str().unwrap());
    let inventory = Path::new(work["inventoryPath"].as_str().unwrap());
    let groups = Path::new(work["fileGroupsPath"].as_str().unwrap());
    let dispatch = Path::new(work["focusAreaQueuePath"].as_str().unwrap());
    let artifacts = workspace.path("artifacts");
    fs::create_dir_all(&artifacts).unwrap();
    let assignment = review_work::next(dispatch, &clock).unwrap()["item"].clone();
    let queue = Path::new(assignment["fileGroupQueuePath"].as_str().unwrap());
    let artifact = artifacts.join("focus-001.json");
    findings::initialize(
        &artifact,
        "focus-001",
        assignment["focusArea"].as_str().unwrap(),
        false,
        &clock,
    )
    .unwrap();
    assert_eq!(
        review_work::validate(manifest, &artifacts, &clock).unwrap()["isValid"],
        false
    );
    let state = review_work::initialize_queue(inventory, groups, queue, false, &clock).unwrap();
    review_work::next(queue, &clock).unwrap();
    let batch = json!({"schemaVersion":1,"snapshot":state["snapshot"],"groupId":"group-1","assessments":assessments(&files),"findingIds":[]});
    review_work::complete_batch(queue, inventory, groups, &artifact, &batch, &clock).unwrap();
    findings::complete(&artifact, queue, &clock).unwrap();
    assert_eq!(
        review_work::validate(manifest, &artifacts, &clock).unwrap()["isValid"],
        true
    );
    fs::remove_file(queue).unwrap();
    assert_eq!(
        review_work::validate(manifest, &artifacts, &clock).unwrap()["isValid"],
        false
    );
}

#[test]
fn complete_scope_exclusion_needs_no_worker_and_checks_snapshot() {
    let workspace = Workspace::new("scope");
    let clock = SystemClock::default();
    let (inventory, groups, _) = inputs(&workspace);
    let focus = workspace.path("focus");
    fs::create_dir_all(&focus).unwrap();
    // This metadata is produced by PowerShell for a complete Rust-only scope.
    let metadata = "eyJjb250ZW50U2hhMjU2IjoiREE5QUE2RkVEOUZEMjgwQzEzNDdCQ0MwMzU1MTExQzg5NjkxODgwNEQ5RTczODYyRDk5NzI3QjI2OTVGRDBENiIsInNjaGVtYVZlcnNpb24iOjEsIm1hcCI6eyJjYXRlZ29yaWVzIjpbeyJ0aXRsZSI6IkFsbCIsImlkIjoiYWxsIn1dLCJzY2hlbWFWZXJzaW9uIjoxLCJjaGFwdGVycyI6W3sic291cmNlcyI6W3sicm9vdCI6Imluc3RydWN0aW9uLXNldC1yYXciLCJzdGFydExpbmUiOjEsImVuZExpbmUiOjEsInBhdGgiOiJydWxlcy5tZCJ9XSwiY2F0ZWdvcnlJZCI6ImFsbCIsImNvbnRlbnQiOnsicm9vdCI6Imluc3RydWN0aW9uLXNldC1yYXciLCJzdGFydExpbmUiOjEsImVuZExpbmUiOjEsInBhdGgiOiJydWxlcy5tZCJ9LCJpZCI6InJ1c3QiLCJhcHBsaWNhYmlsaXR5IjpbeyJkZWNsYXJhdGlvbnMiOlt7InJvb3QiOiJpbnN0cnVjdGlvbi1zZXQtcmF3Iiwic3RhcnRMaW5lIjoxLCJlbmRMaW5lIjoxLCJwYXRoIjoicnVsZXMubWQifV0sImVmZmVjdGl2ZVNjb3BlIjoiUnVzdCBmaWxlcyIsInVuY2VydGFpbnRpZXMiOltdLCJwYXRoU2NvcGUiOnsiaW5jbHVkZSI6WyIqKi8qLnJzIl0sImJhc2UiOiIuIn0sIm9yaWdpbnMiOltdfV19XX19";
    let area = focus.join("area.md");
    fs::write(
        &area,
        format!("<!-- ss-review applicability {metadata} -->\nRules\n"),
    )
    .unwrap();
    let artifact_directory = workspace.path("artifacts");
    let result = review_work::create(
        &inventory,
        &groups,
        &focus,
        &workspace.path("work"),
        Some(&workspace.0),
        Some(&artifact_directory),
        &clock,
    )
    .unwrap();
    assert_eq!(result["excludedReviewerCount"], 1);
    assert_eq!(result["dispatchCount"], 0);
    let manifest = Path::new(result["manifestPath"].as_str().unwrap());
    assert_eq!(
        review_work::validate(manifest, &artifact_directory, &clock).unwrap()["isValid"],
        true
    );
    fs::write(
        &area,
        format!("<!-- ss-review applicability {metadata} -->\nChanged\n"),
    )
    .unwrap();
    assert_eq!(
        review_work::validate(manifest, &artifact_directory, &clock).unwrap()["isValid"],
        false
    );
}

#[test]
fn receipt_checks_active_and_removed_findings() {
    let workspace = Workspace::new("history");
    let clock = SystemClock::default();
    let (inventory, groups, files) = inputs(&workspace);
    let queue = workspace.path("queue.json");
    let artifact = workspace.path("artifact.json");
    let finding = |id| json!({"schemaVersion":1,"id":id,"sourceFindingIds":[id],"origin":"reviewer","title":"Finding","description":"Problem","reasoning":"Reason","consequence":"Impact","recommendedAction":"Fix","anchor":{"kind":"file","path":"src/lib.rs"}});
    workspace.put("artifact.json", &json!({"schemaVersion":1,"kind":"reviewer","reviewerId":"focus-001","focusArea":"/focus","status":"open","findings":[finding("active")],"removed":[{"finding":finding("removed"),"reason":"withdrawn","removedUtc":"2026-01-01T00:00:00Z"}]}));
    let state = review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    review_work::next(&queue, &clock).unwrap();
    let mut batch = json!({"schemaVersion":1,"snapshot":state["snapshot"],"groupId":"group-1","assessments":assessments(&files),"findingIds":["active","missing"]});
    assert!(
        review_work::complete_batch(&queue, &inventory, &groups, &artifact, &batch, &clock)
            .is_err()
    );
    batch["findingIds"] = json!(["removed", "active"]);
    assert!(
        review_work::complete_batch(&queue, &inventory, &groups, &artifact, &batch, &clock).is_ok()
    );
}

/// Synchronizes a blocked lock attempt without reading a clock or sleeping.
struct GateClock {
    contended: Sender<()>,
    release: Receiver<()>,
    waited: Cell<bool>,
}

impl Clock for GateClock {
    fn now(&self) -> Timestamp {
        panic!("Journal locking does not consult the wall clock")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        if self.waited.replace(true) {
            return Err(fail("contention", "Unexpected repeated lock contention"));
        }
        self.contended.send(()).unwrap();
        self.release.recv().unwrap();
        Ok(())
    }
}

/// Reports contention or premature acquisition through the same ordered channel.
struct EventGateClock {
    events: Sender<&'static str>,
    release: Receiver<()>,
    waits: Cell<u8>,
}

impl Clock for EventGateClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        let waits = self.waits.get();
        // Each participant may be blocked once at each controlled checkpoint.
        if waits >= 2 {
            return Err(fail("contention", "Unexpected repeated lock contention"));
        }
        self.waits.set(waits.saturating_add(1));
        self.events.send("contended").unwrap();
        self.release
            .recv()
            .map_err(|error| caused_by("io", "Retry permission was not received", error))?;
        Ok(())
    }
}

#[test]
fn bound_work_holds_operation_lock_and_snapshots_before_replacement() {
    let _process_exclusion = PROCESS_CREATION_AND_CONTROLLED_LOCK
        .lock()
        .expect("Another test panicked while excluding process creation");
    let workspace = Workspace::new("bound-overlap");
    let clock = SystemClock::default();
    let (inventory, groups, _) = inputs(&workspace);
    let identity = published_preparation(&workspace, &clock);
    let scope_path = workspace.path("scope.md");
    fs::write(&scope_path, "Review scope").unwrap();
    fs::write(workspace.path("references.md"), "References").unwrap();
    fs::write(workspace.path("content-overview.md"), "Overview").unwrap();
    let skill =
        fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review"))
            .unwrap();
    let focus = workspace.path("focus-areas");
    let original = fs::read_to_string(focus.join("focus-rules.md")).unwrap();
    let held_artifacts = FileTransaction::open(&focus, &clock).unwrap();
    let findings = workspace.path("artifacts");
    fs::create_dir_all(&findings).unwrap();
    let held_finding = FileTransaction::open(&findings.join("focus-001.json"), &clock).unwrap();
    let (worker_events_tx, worker_events_rx) = mpsc::channel();
    let (worker_retry_tx, worker_retry_rx) = mpsc::channel();
    let (writer_events_tx, writer_events_rx) = mpsc::channel();
    let (writer_retry_tx, writer_retry_rx) = mpsc::channel();
    let (writer_resume_tx, writer_resume_rx) = mpsc::channel();
    let session = workspace.0.clone();
    let output = workspace.path("work");
    let writer_session = session.clone();
    let assignment_session = session.clone();
    let assignment_output = workspace.path("attempt");
    let writer_focus = focus.clone();
    let live_focus = focus.clone();
    std::thread::scope(move |scope| {
        let worker = scope.spawn(move || {
            let clock = EventGateClock {
                events: worker_events_tx.clone(),
                release: worker_retry_rx,
                waits: Cell::new(0),
            };
            let result = review_work::create_with_preparation(
                &inventory,
                &groups,
                &focus,
                &output,
                Some(&session),
                Some(&findings),
                &review_work::PreparationBinding {
                    session: &session,
                    identity: &identity,
                },
                &clock,
            );
            worker_events_tx.send("finished").unwrap();
            result
        });
        assert_eq!(worker_events_rx.recv().unwrap(), "contended");
        let writer = scope.spawn(move || -> Result<(), AppError> {
            let clock = EventGateClock {
                events: writer_events_tx.clone(),
                release: writer_retry_rx,
                waits: Cell::new(0),
            };
            let _operation = ResourceLock::open(
                &writer_session.join("focus-area-cache-operation"),
                LockMode::Exclusive,
                &clock,
            )?;
            writer_events_tx.send("acquired").unwrap();
            writer_resume_rx
                .recv()
                .map_err(|error| caused_by("io", "Writer permission was not received", error))?;
            let _artifacts = FileTransaction::open(&writer_focus, &clock)?;
            focus_area_generation::invalidate_locked(&writer_session, &clock)?;
            fs::write(writer_focus.join("focus-rules.md"), "replacement")?;
            Ok(())
        });
        assert_eq!(writer_events_rx.recv().unwrap(), "contended");
        drop(held_artifacts);
        worker_retry_tx.send(()).unwrap();
        assert_eq!(worker_events_rx.recv().unwrap(), "contended");
        writer_retry_tx.send(()).unwrap();
        assert_eq!(writer_events_rx.recv().unwrap(), "contended");
        drop(held_finding);
        worker_retry_tx.send(()).unwrap();
        let result = worker.join().unwrap().unwrap();
        writer_retry_tx.send(()).unwrap();
        assert_eq!(writer_events_rx.recv().unwrap(), "acquired");
        writer_resume_tx.send(()).unwrap();
        writer.join().unwrap().unwrap();

        let manifest: Value =
            serde_json::from_slice(&fs::read(result["manifestPath"].as_str().unwrap()).unwrap())
                .unwrap();
        let snapshot = Path::new(manifest["reviewers"][0]["focusArea"].as_str().unwrap());
        assert_eq!(fs::read_to_string(snapshot).unwrap(), original);
        assert_eq!(
            fs::read_to_string(live_focus.join("focus-rules.md")).unwrap(),
            "replacement"
        );
        let assignment = worker_input::create_assignment(
            &worker_input::AssignmentOptions {
                phase: "review",
                session_state: &assignment_session,
                scope_path: &scope_path,
                output_directory: &assignment_output,
                skill_root: &skill,
                manifest_path: Some(Path::new(result["manifestPath"].as_str().unwrap())),
                reviewer_id: Some("focus-001"),
                context_path: None,
                resume: false,
                repository_instructions: false,
                include_pre_existing: false,
            },
            &SystemClock::default(),
        )
        .unwrap();
        let page = worker_input::read_document(
            Path::new(assignment["assignmentPath"].as_str().unwrap()),
            "focusArea",
            0,
            &SystemClock::default(),
        )
        .unwrap();
        assert_eq!(page["content"], original);
        assert_eq!(page["complete"], true);
        let preparation_assignment = worker_input::create_assignment(
            &worker_input::AssignmentOptions {
                phase: "focusAreas",
                session_state: &assignment_session,
                scope_path: &scope_path,
                output_directory: &assignment_session.join("preparation-attempt"),
                skill_root: &skill,
                manifest_path: None,
                reviewer_id: None,
                context_path: None,
                resume: false,
                repository_instructions: false,
                include_pre_existing: false,
            },
            &SystemClock::default(),
        )
        .unwrap();
        assert!(preparation_assignment["assignmentPath"].is_string());
    });
}

/// Exchanges explicit retry permission with a PowerShell lock owner.
struct ProcessInteropClock {
    waited: Cell<bool>,
}

impl Clock for ProcessInteropClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        if self.waited.replace(true) {
            return Err(fail("contention", "Unexpected repeated lock contention"));
        }
        writeln!(io::stderr(), "lock-contended")
            .map_err(|error| caused_by("io", "Cannot signal contention", error))?;
        let mut reply = String::new();
        io::stdin()
            .read_line(&mut reply)
            .map_err(|error| caused_by("io", "Cannot receive retry", error))?;
        if reply.trim_end() != "retry" {
            return Err(fail("invalidInput", "Expected retry permission"));
        }
        Ok(())
    }
}

/// Exposes a native claim to an external process that controls lock release.
#[test]
fn native_review_work_lock_interop_helper() {
    let Some(queue) = env::var_os("SS_AI_REVIEW_WORK_INTEROP_QUEUE") else {
        return;
    };
    let clock = ProcessInteropClock {
        waited: Cell::new(false),
    };
    let result = review_work::next(Path::new(&queue), &clock).unwrap();
    writeln!(io::stderr(), "result-json:{result}").unwrap();
    writeln!(io::stdout(), "{result}").unwrap();
    io::stdout().flush().unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "requires subprocesses and file locks")]
fn native_review_work_helper_reports_real_contention() {
    let _process_exclusion = PROCESS_CREATION_AND_CONTROLLED_LOCK
        .lock()
        .expect("Another test panicked while excluding process creation");
    let workspace = Workspace::new("interop");
    let clock = SystemClock::default();
    let (inventory, groups, _) = inputs(&workspace);
    let queue = workspace.path("queue.json");
    review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    let held = FileTransaction::open(&queue, &clock).unwrap();
    let mut child = Command::new(env::current_exe().unwrap())
        .args([
            "native_review_work_lock_interop_helper",
            "--exact",
            "--nocapture",
        ])
        .env("SS_AI_REVIEW_WORK_INTEROP_QUEUE", &queue)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut signal = String::new();
    stderr.read_line(&mut signal).unwrap();
    assert_eq!(signal.trim_end(), "lock-contended");
    drop(held);
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "retry").unwrap();
    input.flush().unwrap();
    drop(input);
    let mut output = String::new();
    stderr.read_to_string(&mut output).unwrap();
    assert!(child.wait().unwrap().success());
    let result = output
        .lines()
        .find_map(|line| line.strip_prefix("result-json:"))
        .unwrap();
    let result: Value = serde_json::from_str(result).unwrap();
    assert_eq!(result["item"]["groupId"], "group-1");
}

#[test]
fn claim_waits_for_an_existing_transaction() {
    let _process_exclusion = PROCESS_CREATION_AND_CONTROLLED_LOCK
        .lock()
        .expect("Another test panicked while excluding process creation");
    let workspace = Workspace::new("contention");
    let clock = SystemClock::default();
    let (inventory, groups, _) = inputs(&workspace);
    let queue = workspace.path("queue.json");
    review_work::initialize_queue(&inventory, &groups, &queue, false, &clock).unwrap();
    let held = FileTransaction::open(&queue, &clock).unwrap();
    let (contended_tx, contended_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    // Own the endpoints inside the scope so failures disconnect the worker before
    // scope cleanup joins it. One retry permission must not leave a second wait blocked.
    std::thread::scope(move |scope| {
        let worker = scope.spawn(move || {
            let clock = GateClock {
                contended: contended_tx,
                release: release_rx,
                waited: Cell::new(false),
            };
            review_work::next(&queue, &clock).unwrap()
        });
        contended_rx.recv().unwrap();
        drop(held);
        release_tx.send(()).unwrap();
        drop(release_tx);
        assert_eq!(worker.join().unwrap()["item"]["groupId"], "group-1");
    });
}
