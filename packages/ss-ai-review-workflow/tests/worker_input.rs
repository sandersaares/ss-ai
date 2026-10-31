//! Exercises worker assignment publication and bounded shared reads.
#![allow(
    clippy::indexing_slicing,
    clippy::assertions_on_result_states,
    reason = "The fixtures use fixed record shapes and assert failures"
)]

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::read_diagnostics;
use ss_ai_review_workflow::worker_input::{AssignmentOptions, create_assignment, read_document};
use ss_ai_runtime::{Clock, FileTransaction};

/// Keeps each test's files in an isolated checkout-local directory.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".worker-input-test-{}-{name}", std::process::id()));
        #[expect(
            clippy::create_dir,
            reason = "Each fixture must fail rather than reuse an existing directory"
        )]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn put(&self, name: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, content).unwrap();
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Allows contention retries without real-time sleeps.
struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        panic!("Worker input must not use the wall clock")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        thread::yield_now();
        Ok(())
    }
}

/// Stops each concurrent reader at its first observed lock conflict.
struct ContendedClock {
    observed: Sender<bool>,
    release: Receiver<()>,
    reported: Cell<bool>,
}

impl Clock for ContendedClock {
    fn now(&self) -> Timestamp {
        panic!("Worker input must not use the wall clock")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        if !self.reported.replace(true) {
            self.observed.send(true).unwrap();
            self.release.recv().unwrap();
        }
        thread::yield_now();
        Ok(())
    }
}

/// Uses the actual skill tree while keeping every output in the test workspace.
fn create(workspace: &Workspace, phase: &str, scope: &Path) -> Value {
    let skill = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review");
    create_assignment(
        &AssignmentOptions {
            phase,
            session_state: &workspace.0,
            scope_path: scope,
            output_directory: &workspace.0.join("attempt O'Brien"),
            skill_root: &skill,
            manifest_path: None,
            reviewer_id: None,
            context_path: None,
            resume: false,
            repository_instructions: false,
            include_pre_existing: false,
        },
        &TestClock,
    )
    .unwrap()
}

/// Exhausts pages with an operation bound and checks encoded output size.
fn collect(path: &Path, id: &str) -> String {
    let mut result = String::new();
    let mut offset = 0;
    for _ in 0..100 {
        let page = read_document(path, id, offset, &TestClock).unwrap();
        let output = format!("{}\n", serde_json::to_string(&page).unwrap());
        assert!(output.len() <= 16 * 1024);
        assert_eq!(page["contentOffset"], offset);
        result.push_str(page["content"].as_str().unwrap());
        if page["complete"] == true {
            assert!(page["nextOffset"].is_null());
            return result;
        }
        let next = usize::try_from(page["nextOffset"].as_u64().unwrap()).unwrap();
        assert!(next > offset);
        offset = next;
    }
    panic!("Document exceeded bounded fixture page count")
}

#[test]
fn exact_unicode_pages_and_native_dispatch() {
    let workspace = Workspace::new("unicode");
    let expected = format!(
        "{}{}{}",
        "quoted \"\\\n🙂".repeat(2200),
        "x".repeat(25000),
        "😀"
    );
    let scope = workspace.put("scope.md", &expected);
    let result = create(&workspace, "fileGroups", &scope);
    let assignment_path = Path::new(result["assignmentPath"].as_str().unwrap());
    let dispatch = result["dispatchPrompt"].as_str().unwrap();
    let command = format!(
        "ss-ai workflow worker-document --assignment-path '{}' --document-id '{{{{DOCUMENT_ID}}}}' --offset 0",
        assignment_path.to_str().unwrap().replace('\'', "''")
    );
    assert!(dispatch.lines().any(|line| line == command));
    assert!(command.contains("O''Brien"));
    assert!(dispatch.contains(concat!(
        "```powershell\n",
        "#Requires -Version 7.6\n",
        "if ($PSVersionTable.PSVersion -lt [version]'7.6') { throw 'PowerShell 7.6 or later is required.' }\n",
        "Set-StrictMode -Version Latest\n",
        "$ErrorActionPreference = 'Stop'\n",
        "$PSNativeCommandUseErrorActionPreference = $true\n"
    )));
    assert!(
        dispatch.find("Apply this recovery policy").unwrap()
            < dispatch.find("ss-ai workflow worker-document").unwrap()
    );
    assert_eq!(collect(assignment_path, "scope"), expected);
    let data = collect(assignment_path, "workflowData");
    let skill = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review");
    let expected_data = format!(
        "{}\n\n{}",
        fs::read_to_string(skill.join("workflow-data.md")).unwrap(),
        fs::read_to_string(skill.join("data-guides/file-groups.md")).unwrap()
    )
    .replace("\r\n", "\n");
    assert_eq!(data, expected_data);
    let prompt = collect(assignment_path, "prompt");
    assert!(prompt.contains("Primary orchestration, publication, and every human decision gate"));
    assert!(!prompt.contains("{{SKILL_ROOT}}"));
}

/// Computes the largest fitting page directly, independently of the search.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    reason = "The bounded fixture uses character boundaries and small positive counts"
)]
fn assert_largest_page(path: &Path, text: &str, offset: usize) {
    // PowerShell emits its host newline when relaying the native result.
    let newline_len = if cfg!(windows) { "\r\n" } else { "\n" }.len();
    let assignment: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let document = assignment["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|document| document["id"] == "scope")
        .unwrap();
    let length = text.encode_utf16().count();
    let start = text
        .char_indices()
        .scan(0, |units, (index, character)| {
            let current = *units;
            *units += character.len_utf16();
            Some((current, index))
        })
        .find(|(units, _)| *units == offset)
        .unwrap()
        .1;
    let mut next = offset;
    let mut expected = None;
    let mut encoded_content = 0;
    for (index, character) in text[start..].char_indices() {
        next += character.len_utf16();
        let end = start + index + character.len_utf8();
        let complete = next == length;
        encoded_content += serde_json::to_string(&character).unwrap().len() - 2;
        let envelope = json!({
            "documentId": "scope", "sha256": document["sha256"],
            "contentOffset": offset, "content": "",
            "contentLength": length,
            "nextOffset": if complete { None } else { Some(next) }, "complete": complete
        });
        if serde_json::to_vec(&envelope).unwrap().len() + encoded_content + newline_len <= 16 * 1024
        {
            expected = Some((end, next, complete));
        }
        // The raw suffix already exceeds the entire output budget.
        if end - start > 16 * 1024 {
            break;
        }
    }
    let page = read_document(path, "scope", offset, &TestClock).unwrap();
    let (end, next, complete) = expected.unwrap();
    assert_eq!(page["documentId"], "scope");
    assert_eq!(page["sha256"], document["sha256"]);
    assert_eq!(page["contentOffset"], offset);
    assert_eq!(page["content"], &text[start..end]);
    assert_eq!(page["contentLength"], length);
    assert_eq!(
        page["nextOffset"],
        json!(if complete { None } else { Some(next) })
    );
    assert_eq!(page["complete"], complete);
    assert!(serde_json::to_vec(&page).unwrap().len() + newline_len <= 16 * 1024);
}

#[test]
fn bounded_pages_match_the_largest_encoded_candidate() {
    let workspace = Workspace::new("largest-pages");
    let text = format!(
        "{}🙂{}{}",
        "a".repeat(17_000),
        "\"\\\n".repeat(10_000),
        "z".repeat(1_000_000)
    );
    let scope = workspace.put("scope.md", &text);
    let result = create(&workspace, "references", &scope);
    let assignment = Path::new(result["assignmentPath"].as_str().unwrap());
    for offset in [
        0,
        16_999,
        17_000,
        17_002,
        47_002,
        text.encode_utf16().count() - 1,
    ] {
        assert_largest_page(assignment, &text, offset);
    }
    assert!(read_document(assignment, "scope", 17_001, &TestClock).is_err());
    assert!(read_document(assignment, "scope", text.encode_utf16().count(), &TestClock).is_err());
    assert!(read_document(assignment, "scope", usize::MAX, &TestClock).is_err());
}

#[test]
#[expect(
    clippy::string_slice,
    reason = "The boundary fixture contains only ASCII characters"
)]
fn exact_output_limit_includes_the_final_newline() {
    let workspace = Workspace::new("exact-limit");
    // Match the platform newline used by the PowerShell compatibility executable.
    let newline_len = if cfg!(windows) { "\r\n" } else { "\n" }.len();
    // Find the complete ASCII page whose JSON envelope leaves exactly the budget.
    let envelope = json!({
        "documentId": "scope", "sha256": "A".repeat(64), "contentOffset": 0,
        "content": "", "contentLength": 0, "nextOffset": null, "complete": true
    });
    let base = serde_json::to_vec(&envelope).unwrap().len();
    let length = (1..16 * 1024)
        .find(|length| base + length + length.to_string().len() - 1 + newline_len == 16 * 1024)
        .unwrap();
    let text = "a".repeat(length);
    let scope = workspace.put("scope.md", &text);
    let result = create(&workspace, "references", &scope);
    let assignment = Path::new(result["assignmentPath"].as_str().unwrap());
    let page = read_document(assignment, "scope", 0, &TestClock).unwrap();
    assert_eq!(
        serde_json::to_vec(&page).unwrap().len() + newline_len,
        16 * 1024
    );
    assert_eq!(page["content"], text);
    assert_eq!(page["complete"], true);
    // The continuation metadata can make the penultimate page larger than completion.
    let mut previous = page;
    previous["content"] = json!(&text[..length - 1]);
    previous["nextOffset"] = json!(length - 1);
    previous["complete"] = json!(false);
    assert!(serde_json::to_vec(&previous).unwrap().len() + newline_len > 16 * 1024);
}

#[test]
fn invalid_document_counts_are_rejected_without_changing_the_wire_format() {
    let workspace = Workspace::new("metadata");
    let scope = workspace.put("scope.md", "one\ntwo");
    let result = create(&workspace, "references", &scope);
    let assignment = Path::new(result["assignmentPath"].as_str().unwrap());
    let record: Value = serde_json::from_slice(&fs::read(assignment).unwrap()).unwrap();
    assert_eq!(record["documents"][1]["sizeBytes"], 7);
    assert_eq!(record["documents"][1]["lineCount"], 2);
    for key in ["sizeBytes", "lineCount"] {
        for invalid in [json!(0), json!(-1), json!(null), json!("1")] {
            let mut modified = record.clone();
            modified["documents"][1][key] = invalid;
            fs::write(assignment, serde_json::to_vec(&modified).unwrap()).unwrap();
            assert!(
                read_document(assignment, "scope", 0, &TestClock).is_err(),
                "{key}"
            );
        }
    }
    fs::write(assignment, serde_json::to_vec(&record).unwrap()).unwrap();
    assert_eq!(collect(assignment, "scope"), "one\ntwo");
}

#[test]
fn invalid_offsets_changed_bytes_and_attempt_reuse_fail() {
    let workspace = Workspace::new("invalid");
    let scope = workspace.put("scope.md", "a😀z");
    let result = create(&workspace, "references", &scope);
    let assignment = Path::new(result["assignmentPath"].as_str().unwrap());
    assert!(read_document(assignment, "scope", 2, &TestClock).is_err());
    assert!(read_document(assignment, "scope", 4, &TestClock).is_err());
    fs::write(&scope, "a😀x").unwrap();
    assert!(read_document(assignment, "scope", 0, &TestClock).is_err());
    let counts = read_diagnostics(
        &workspace.0.join("attempt O'Brien/input-diagnostics.json"),
        &TestClock,
    )
    .unwrap();
    assert_eq!(counts["counts"]["documentReadCalls"], 3);
    assert_eq!(counts["counts"]["firstReadFailures"], 1);
    let old = fs::read(assignment).unwrap();
    let skill = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review");
    let options = AssignmentOptions {
        phase: "references",
        session_state: &workspace.0,
        scope_path: &scope,
        output_directory: &workspace.0.join("attempt O'Brien"),
        skill_root: &skill,
        manifest_path: None,
        reviewer_id: None,
        context_path: None,
        resume: false,
        repository_instructions: false,
        include_pre_existing: false,
    };
    assert!(create_assignment(&options, &TestClock).is_err());
    assert_eq!(old, fs::read(assignment).unwrap());
    let counts = read_diagnostics(
        &workspace.0.join("attempt O'Brien/input-diagnostics.json"),
        &TestClock,
    )
    .unwrap();
    assert_eq!(counts["counts"]["repeatedSetupCalls"], 1);
}

#[test]
fn concurrent_reads_preserve_diagnostics() {
    let workspace = Workspace::new("concurrent");
    let scope = workspace.put("scope.md", "shared scope");
    let result = create(&workspace, "fileGroups", &scope);
    let path = PathBuf::from(result["assignmentPath"].as_str().unwrap());
    let ids = ["scope", "prompt", "workflowData", "missing"];
    let diagnostics = workspace.0.join("attempt O'Brien/input-diagnostics.json");
    let lock = FileTransaction::open(&diagnostics, &TestClock).unwrap();
    let (observed, events) = mpsc::channel();
    let mut releases = Vec::new();
    thread::scope(|scope| {
        for id in ids {
            let (release, gate) = mpsc::channel();
            releases.push(release);
            let observed = observed.clone();
            let path = &path;
            scope.spawn(move || {
                let clock = ContendedClock {
                    observed: observed.clone(),
                    release: gate,
                    reported: Cell::new(false),
                };
                let result = read_document(path, id, 0, &clock);
                if !clock.reported.get() {
                    observed.send(false).unwrap();
                }
                assert_eq!(result.is_ok(), id != "missing");
            });
        }
        let all_contended = (0..ids.len()).all(|_| events.recv().unwrap());
        drop(lock);
        let mut all_released = true;
        for release in releases {
            all_released &= release.send(()).is_ok();
        }
        assert!(all_contended && all_released);
    });
    let counts = read_diagnostics(&diagnostics, &TestClock).unwrap();
    assert_eq!(counts["counts"]["documentReadCalls"], ids.len());
    assert_eq!(counts["counts"]["firstReadFailures"], 1);
}

#[test]
fn invalid_utf8_is_rejected_before_publication() {
    let workspace = Workspace::new("invalid-utf8");
    let scope = workspace.put("scope.md", [0xff]);
    let skill = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review");
    let options = AssignmentOptions {
        phase: "references",
        session_state: &workspace.0,
        scope_path: &scope,
        output_directory: &workspace.0.join("attempt"),
        skill_root: &skill,
        manifest_path: None,
        reviewer_id: None,
        context_path: None,
        resume: false,
        repository_instructions: false,
        include_pre_existing: false,
    };
    assert!(create_assignment(&options, &TestClock).is_err());
    assert!(!workspace.0.join("attempt/assignment.json").exists());
}

#[test]
fn review_resumes_prepared_work_and_rejects_excluded_dispatch() {
    let workspace = Workspace::new("review");
    let scope = workspace.put("scope.md", "scope");
    workspace.put("references.md", "references");
    workspace.put("content-overview.md", "overview");
    let focus = workspace.put("focus.md", "focus guidance");
    let queue = workspace.put("queue.json", "prepared queue");
    let artifact = workspace.put("artifact.json", "prepared findings");
    let hash = "A".repeat(64);
    let mut manifest = json!({
        "schemaVersion": 1,
        "inventoryPath": workspace.0.join("inventory.json"),
        "fileGroupsPath": workspace.0.join("groups.json"),
        "focusAreaQueuePath": workspace.0.join("focus-queue.json"),
        "repositoryRoot": workspace.0,
        "snapshot": {"inventorySha256": hash, "fileGroupsSha256": hash},
        "reviewers": [{
            "reviewerId": "focus-001", "focusArea": focus,
            "fileGroupQueuePath": queue, "dispatch": "review",
            "focusAreaSha256": hash, "artifactPath": artifact
        }]
    });
    let manifest_path = workspace.put("manifest.json", serde_json::to_vec(&manifest).unwrap());
    let skill = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ss-review");
    let attempt = workspace.0.join("review-attempt");
    let options = AssignmentOptions {
        phase: "review",
        session_state: &workspace.0,
        scope_path: &scope,
        output_directory: &attempt,
        skill_root: &skill,
        manifest_path: Some(&manifest_path),
        reviewer_id: Some("focus-001"),
        context_path: None,
        resume: false,
        repository_instructions: false,
        include_pre_existing: false,
    };
    let result = create_assignment(&options, &TestClock).unwrap();
    let assignment = Path::new(result["assignmentPath"].as_str().unwrap());
    let record: Value = serde_json::from_slice(&fs::read(assignment).unwrap()).unwrap();
    assert_eq!(record["resume"], true);
    assert!(collect(assignment, "prompt").contains("Use `-Resume` exactly"));
    assert_eq!(fs::read(&queue).unwrap(), b"prepared queue");
    assert_eq!(fs::read(&artifact).unwrap(), b"prepared findings");

    manifest["reviewers"][0]["dispatch"] = json!("excluded");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let excluded_attempt = workspace.0.join("excluded-attempt");
    let excluded = AssignmentOptions {
        output_directory: &excluded_attempt,
        ..options
    };
    assert!(create_assignment(&excluded, &TestClock).is_err());
    assert!(!excluded_attempt.join("assignment.json").exists());
    assert_eq!(fs::read(queue).unwrap(), b"prepared queue");
    assert_eq!(fs::read(artifact).unwrap(), b"prepared findings");
}

#[test]
fn malformed_assignment_roles_are_not_read() {
    let workspace = Workspace::new("roles");
    let scope = workspace.put("scope.md", "scope");
    let result = create(&workspace, "fileGroups", &scope);
    let assignment = Path::new(result["assignmentPath"].as_str().unwrap());
    let mut record: Value = serde_json::from_slice(&fs::read(assignment).unwrap()).unwrap();
    record["documents"]
        .as_array_mut()
        .unwrap()
        .retain(|document| document["id"] != "workflowData");
    fs::write(assignment, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(read_document(assignment, "scope", 0, &TestClock).is_err());
}

#[test]
fn missing_nullable_context_is_not_a_valid_assignment() {
    let workspace = Workspace::new("missing-context");
    let scope = workspace.put("scope.md", "scope");
    let result = create(&workspace, "references", &scope);
    let assignment = Path::new(result["assignmentPath"].as_str().unwrap());
    let mut record: Value = serde_json::from_slice(&fs::read(assignment).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("contextPath");
    fs::write(assignment, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(read_document(assignment, "scope", 0, &TestClock).is_err());
}

#[test]
fn malformed_diagnostics_prevent_reading() {
    let workspace = Workspace::new("bad-diagnostics");
    let scope = workspace.put("scope.md", "scope");
    let result = create(&workspace, "references", &scope);
    let assignment = Path::new(result["assignmentPath"].as_str().unwrap());
    let diagnostics = workspace.0.join("attempt O'Brien/input-diagnostics.json");
    fs::write(&diagnostics, "{}").unwrap();
    assert!(read_document(assignment, "scope", 0, &TestClock).is_err());
    assert_eq!(fs::read(&diagnostics).unwrap(), b"{}");
}

#[test]
fn skill_root_must_be_explicit_and_absolute() {
    let workspace = Workspace::new("skill-root");
    let scope = workspace.put("scope.md", "scope");
    let options = AssignmentOptions {
        phase: "references",
        session_state: &workspace.0,
        scope_path: &scope,
        output_directory: &workspace.0.join("attempt"),
        skill_root: Path::new("skills/ss-review"),
        manifest_path: None,
        reviewer_id: None,
        context_path: None,
        resume: false,
        repository_instructions: false,
        include_pre_existing: false,
    };
    assert!(create_assignment(&options, &TestClock).is_err());
    assert!(!workspace.0.join("attempt/assignment.json").exists());
}
