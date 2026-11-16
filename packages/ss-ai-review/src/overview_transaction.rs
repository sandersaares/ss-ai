//! Best-effort initial overview publication with durable write intents.
#![expect(
    clippy::indexing_slicing,
    reason = "Mutable JSON indexing updates locally constructed result and operation objects or a validated prior object; read indexing returns null for missing keys"
)]
#![allow(
    clippy::result_large_err,
    reason = "This best-effort boundary handles occasional failures and keeps snapshot and tool diagnostics inline rather than allocating for each error"
)]

use std::path::Path;

use serde_json::{Value, json};
use ss_ai_runtime::FileTransaction;

use crate::Clock;
use crate::overview_snapshot::PreparedSnapshot;

/// Provider and local preparation operations supplied by the composition root.
///
/// Freeze returns a provider-ready snapshot type, including ADO HTML and archive
/// preparation. It acquires the snapshot lock after the result lock and permits refreshing an unpublished snapshot:
/// the transaction calls it only after validating that no write may have occurred.
/// Write errors must mark a definite refusal only when the service proved that
/// no write occurred.
pub(crate) trait OverviewEffects {
    fn is_open(&mut self) -> Result<bool, EffectError>;
    fn freeze(&mut self, clock: &dyn Clock) -> Result<PreparedSnapshot, EffectError>;
    fn upload(&mut self, snapshot: &PreparedSnapshot) -> Result<Value, EffectError>;
    fn verify_artifact(
        &mut self,
        snapshot: &PreparedSnapshot,
        receipt: &Value,
    ) -> Result<Value, EffectError>;
    fn comment(
        &mut self,
        snapshot: &PreparedSnapshot,
        payload: &Value,
    ) -> Result<Value, EffectError>;
    fn verify_comment(
        &mut self,
        snapshot: &PreparedSnapshot,
        payload: &Value,
    ) -> Result<(), EffectError>;
}

/// Carries diagnostics and the evidence needed to decide whether to retry.
pub(crate) struct EffectError {
    pub(crate) message: String,
    pub(crate) definite_refusal: bool,
    pub(crate) missing_tools: Vec<String>,
    /// Preparation can fail after freezing Markdown but before rendering succeeds.
    pub(crate) snapshot: Option<Value>,
    pub(crate) render_failed: bool,
}

/// Holds exclusive transaction ownership over the complete publication.
trait Journal {
    fn read(&mut self) -> Result<Option<Vec<u8>>, String>;
    fn write(&mut self, record: &Value) -> Result<(), String>;
}

impl Journal for FileTransaction<'_> {
    fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.read_bytes().map_err(|error| error.to_string())
    }

    fn write(&mut self, record: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
        self.write_bytes(&bytes).map_err(|error| error.to_string())
    }
}

/// Publishes under the shared file lock, returning the interoperable result.
pub(crate) fn publish(
    path: &Path,
    clock: &dyn Clock,
    effects: &mut dyn OverviewEffects,
    pull_request_url: &str,
    comment_prefix: &str,
) -> Value {
    let mut result = new_result(pull_request_url, &path.to_string_lossy());
    match FileTransaction::open(path, clock) {
        Ok(mut journal) => run(
            &mut journal,
            effects,
            clock,
            pull_request_url,
            comment_prefix,
            result,
        ),
        Err(error) => {
            journal_failure(&mut result, &error.to_string());
            result
        }
    }
}

/// Creates the same bounded result envelope as the PowerShell publisher.
fn new_result(url: &str, path: &str) -> Value {
    json!({
        "schemaVersion": 1, "pullRequestUrl": url, "status": "preparing",
        "stage": "preflight", "resultPath": path, "snapshot": null,
        "artifact": null, "comment": null, "operations": [],
        "error": null, "missingTools": []
    })
}

/// Rejects malformed or unrelated records before allowing a replacement.
fn validate_record(record: &Value, url: &str) -> Result<(), &'static str> {
    let object = record
        .as_object()
        .ok_or("Overview record is not an object")?;
    if object.get("schemaVersion") != Some(&json!(1))
        || object.get("pullRequestUrl").and_then(Value::as_str) != Some(url)
    {
        return Err("An overview publication record for a different PR or format already exists");
    }
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .ok_or("Overview record has no status")?;
    if !matches!(
        status,
        "preparing"
            | "succeeded"
            | "failed"
            | "incomplete"
            | "skipped"
            | "needsReconciliation"
            | "journalFailure"
    ) || !matches!(
        object.get("stage").and_then(Value::as_str),
        Some(
            "preflight"
                | "freeze"
                | "renderHtml"
                | "upload"
                | "verifyArtifact"
                | "comment"
                | "verifyComment"
        )
    ) || object.get("resultPath").and_then(Value::as_str).is_none()
        || !object.get("missingTools").is_some_and(|tools| {
            tools
                .as_array()
                .is_some_and(|tools| tools.iter().all(Value::is_string))
        })
        || !object
            .get("error")
            .is_some_and(|error| error.is_null() || error.is_string())
        || ["snapshot", "artifact", "comment"].iter().any(|key| {
            !object
                .get(*key)
                .is_some_and(|value| value.is_null() || value.is_object())
        })
    {
        return Err("Malformed overview publication record");
    }
    let operations = object
        .get("operations")
        .and_then(Value::as_array)
        .ok_or("Malformed overview operations")?;
    if operations.len() > 2 {
        return Err("Malformed overview operations");
    }
    for (index, operation) in operations.iter().enumerate() {
        let name = if index == 0 { "upload" } else { "comment" };
        let state = operation.get("status").and_then(Value::as_str);
        if operation.get("name").and_then(Value::as_str) != Some(name)
            || !matches!(
                state,
                Some("pending" | "completed" | "failed" | "ambiguous")
            )
            || !operation
                .get("response")
                .is_some_and(|response| state == Some("completed") || response.is_null())
        {
            return Err("Malformed overview operation");
        }
        if index == 1
            && operations
                .first()
                .and_then(|op| op.get("status"))
                .and_then(Value::as_str)
                != Some("completed")
        {
            return Err("Comment has no completed upload");
        }
    }
    if status == "succeeded"
        && (operations.len() != 2
            || operations.iter().any(|operation| {
                operation["status"] != "completed" || !has_id(&operation["response"])
            })
            || ["snapshot", "artifact", "comment"]
                .iter()
                .any(|key| !record[*key].is_object())
            || !record["artifact"]["url"]
                .as_str()
                .is_some_and(|url| !url.is_empty())
            || !has_id(&record["artifact"])
            || !has_id(&record["comment"])
            || !record["error"].is_null())
    {
        return Err("Incomplete overview record claims success");
    }
    Ok(())
}

/// Runs the state machine while the caller retains exclusive journal ownership.
fn run(
    journal: &mut dyn Journal,
    effects: &mut dyn OverviewEffects,
    clock: &dyn Clock,
    url: &str,
    prefix: &str,
    mut result: Value,
) -> Value {
    match journal.read() {
        Err(error) => {
            journal_failure(&mut result, &error);
            return result;
        }
        Ok(Some(bytes)) => {
            let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
            let previous: Value = match serde_json::from_slice(bytes) {
                Ok(previous) => previous,
                Err(error) => {
                    result["status"] = json!("failed");
                    result["error"] =
                        json!(bounded(&format!("Malformed overview record: {error}")));
                    return result;
                }
            };
            if let Err(error) = validate_record(&previous, url) {
                result["status"] = json!("failed");
                result["error"] = json!(error);
                return result;
            }
            if previous["status"] == "succeeded" {
                return previous;
            }
            if previous["operations"].as_array().is_some_and(|operations| {
                operations
                    .iter()
                    .any(|operation| operation["status"] != "failed")
            }) {
                let mut previous = previous;
                previous["status"] = json!("needsReconciliation");
                previous["error"] = json!(
                    "A previous overview attempt wrote or may have written remote state. Investigate its recorded IDs before retrying; the review can continue."
                );
                return previous;
            }
        }
        Ok(None) => {}
    }

    if let Err(error) = execute(journal, effects, clock, prefix, &mut result) {
        if result["status"] == "journalFailure" {
            return result;
        }
        if result["stage"] == "freeze" {
            if let Some(snapshot) = error.snapshot.filter(Value::is_object) {
                result["snapshot"] = snapshot;
            }
            if error.render_failed {
                result["stage"] = json!("renderHtml");
            }
        }
        result["status"] = json!(
            if result["operations"].as_array().is_some_and(Vec::is_empty) {
                "failed"
            } else {
                "incomplete"
            }
        );
        result["error"] = json!(bounded(&error.message));
        result["missingTools"] = json!(error.missing_tools);
    }
    if let Err(error) = journal.write(&result) {
        journal_failure(&mut result, &error);
    }
    result
}

/// Sequences preparation, durable receipts, and independent readback.
fn execute(
    journal: &mut dyn Journal,
    effects: &mut dyn OverviewEffects,
    clock: &dyn Clock,
    prefix: &str,
    result: &mut Value,
) -> Result<(), EffectError> {
    if !effects.is_open()? {
        result["status"] = json!("skipped");
        result["error"] =
            json!("Automatic initial overview publication is skipped for a closed pull request.");
        return Ok(());
    }
    result["stage"] = json!("freeze");
    let snapshot = effects.freeze(clock)?;
    result["snapshot"] = snapshot.record();
    result["stage"] = json!("upload");
    let artifact = write_effect(journal, result, "upload", |_| effects.upload(&snapshot))?;
    result["stage"] = json!("verifyArtifact");
    let verified = effects.verify_artifact(&snapshot, &artifact)?;
    result["artifact"] = verified.clone();
    if !has_id(&verified) || verified["id"] != artifact["id"] {
        return Err(local_error(
            "Verified overview artifact has a different or missing identifier",
        ));
    }
    let url = verified["url"]
        .as_str()
        .filter(|url| !url.is_empty())
        .ok_or_else(|| local_error("Verified overview artifact has no URL"))?;
    let link = format!("[Pull request overview (auto-generated).]({url})");
    let body = if prefix.is_empty() {
        link
    } else {
        format!("{prefix}\n\n{link}")
    };
    result["stage"] = json!("comment");
    let payload = json!({ "body": body });
    let comment = write_effect(journal, result, "comment", |_| {
        effects.comment(&snapshot, &payload)
    })?;
    result["stage"] = json!("verifyComment");
    effects.verify_comment(&snapshot, &json!({ "body": body, "comment": comment }))?;
    result["status"] = json!("succeeded");
    Ok(())
}

/// Saves intent before a remote write and its response before readback.
fn write_effect(
    journal: &mut dyn Journal,
    result: &mut Value,
    name: &str,
    action: impl FnOnce(&Value) -> Result<Value, EffectError>,
) -> Result<Value, EffectError> {
    let operations = result["operations"]
        .as_array_mut()
        .expect("new result has operations");
    operations.push(json!({ "name": name, "status": "pending", "response": null }));
    save_or_stop(journal, result)?;
    let response = match action(result) {
        Ok(response) => response,
        Err(error) => {
            let operation = result["operations"]
                .as_array_mut()
                .expect("new result has operations")
                .last_mut()
                .expect("intent exists");
            operation["status"] = json!(if error.definite_refusal {
                "failed"
            } else {
                "ambiguous"
            });
            return Err(error);
        }
    };
    let operation = result["operations"]
        .as_array_mut()
        .expect("new result has operations")
        .last_mut()
        .expect("intent exists");
    operation["status"] = json!("completed");
    operation["response"] = response.clone();
    // Keep an unexpected receipt in the operation even when it cannot populate
    // the object-shaped convenience field; it still proves a write may exist.
    if response.is_object() {
        result[if name == "upload" {
            "artifact"
        } else {
            "comment"
        }] = response.clone();
    }
    save_or_stop(journal, result)?;
    if !has_id(&response) {
        return Err(local_error(
            "Write returned a receipt without an identifier",
        ));
    }
    Ok(response)
}

/// Accepts the string and integer identifiers used by the provider receipts.
fn has_id(value: &Value) -> bool {
    let id = &value["id"];
    id.as_str().is_some_and(|id| !id.is_empty()) || id.as_u64().is_some_and(|id| id > 0)
}

/// Stops before further effects if durable journal publication fails.
fn save_or_stop(journal: &mut dyn Journal, result: &mut Value) -> Result<(), EffectError> {
    journal.write(result).map_err(|error| {
        journal_failure(result, &error);
        local_error("Cannot save overview publication journal")
    })
}

/// Reports a journal failure without discarding remote receipts or diagnostics.
fn journal_failure(result: &mut Value, error: &str) {
    let prior = result["error"].as_str().unwrap_or("");
    result["error"] = json!(
        format!(
            "{prior} Could not save publication diagnostics: {}",
            bounded(error)
        )
        .trim()
    );
    result["status"] = json!("journalFailure");
}

/// Limits diagnostic text independently of remote identifiers and receipts.
fn bounded(text: &str) -> String {
    // Keep diagnostics suitable for a command response without changing receipt IDs.
    const LIMIT: usize = 4096;
    if text.chars().count() <= LIMIT {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(LIMIT).collect::<String>())
    }
}

/// Constructs local failures that must never prove remote refusal.
fn local_error(message: &str) -> EffectError {
    EffectError {
        message: message.to_owned(),
        definite_refusal: false,
        missing_tools: Vec::new(),
        snapshot: None,
        render_failed: false,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::{Value, json};

    use super::{Clock, EffectError, Journal, OverviewEffects, new_result, run};
    use crate::overview_snapshot::{PreparedSnapshot, frozen_snapshot_for_test};

    const URL: &str = "https://example.test/pr/1";

    struct FakeClock;

    impl Clock for FakeClock {
        fn now(&self) -> Timestamp {
            "2026-01-01T00:00:00Z".parse().unwrap()
        }

        fn wait(&self, _: Duration) -> Result<(), AppError> {
            Ok(())
        }
    }

    /// Models the retained transaction and lets tests inspect every durable transition.
    #[derive(Default)]
    struct MemoryJournal {
        bytes: Option<Vec<u8>>,
        writes: Vec<Value>,
        fail_write: Option<usize>,
        observed: Option<Rc<RefCell<Vec<Value>>>>,
    }

    impl Journal for MemoryJournal {
        fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
            Ok(self.bytes.clone())
        }

        fn write(&mut self, record: &Value) -> Result<(), String> {
            if self.fail_write == Some(self.writes.len()) {
                return Err("disk failure".into());
            }
            self.writes.push(record.clone());
            if let Some(observed) = &self.observed {
                observed.borrow_mut().push(record.clone());
            }
            self.bytes = Some(serde_json::to_vec(record).unwrap());
            Ok(())
        }
    }

    /// Simulates acquisition and release without relying on filesystem timing.
    struct LockedJournal {
        journal: MemoryJournal,
        held: Rc<Cell<bool>>,
    }

    impl Journal for LockedJournal {
        fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
            self.journal.read()
        }

        fn write(&mut self, record: &Value) -> Result<(), String> {
            self.journal.write(record)
        }
    }

    impl Drop for LockedJournal {
        fn drop(&mut self) {
            self.held.set(false);
        }
    }

    /// Controls provider outcomes and records which effects the transaction invoked.
    #[derive(Default)]
    struct Effects {
        calls: Vec<&'static str>,
        fail: Option<&'static str>,
        refused: bool,
        closed: bool,
        body: Option<String>,
        lock_held: Option<Rc<Cell<bool>>>,
        observed: Option<Rc<RefCell<Vec<Value>>>>,
        missing_id: bool,
    }

    impl Effects {
        fn step(&mut self, name: &'static str) -> Result<(), EffectError> {
            if let Some(lock_held) = &self.lock_held {
                assert!(lock_held.get(), "transaction lock must cover {name}");
            }
            if let Some(observed) = &self.observed {
                let observed = observed.borrow();
                match name {
                    "upload" => assert_eq!(
                        observed.last().unwrap()["operations"][0]["status"],
                        "pending"
                    ),
                    "verifyArtifact" => assert_eq!(
                        observed.last().unwrap()["operations"][0]["response"]["id"],
                        "artifact-id"
                    ),
                    "comment" => assert_eq!(
                        observed.last().unwrap()["operations"][1]["status"],
                        "pending"
                    ),
                    "verifyComment" => assert_eq!(
                        observed.last().unwrap()["operations"][1]["response"]["id"],
                        47
                    ),
                    _ => {}
                }
            }
            self.calls.push(name);
            if self.fail == Some(name) {
                Err(EffectError {
                    message: format!("{name} failed"),
                    definite_refusal: self.refused,
                    missing_tools: if name == "freeze" {
                        vec!["renderer".into()]
                    } else {
                        vec![]
                    },
                    snapshot: if name == "freeze" {
                        Some(json!({ "runId": "frozen" }))
                    } else {
                        None
                    },
                    render_failed: name == "freeze",
                })
            } else {
                Ok(())
            }
        }
    }

    impl OverviewEffects for Effects {
        fn is_open(&mut self) -> Result<bool, EffectError> {
            self.step("isOpen")?;
            Ok(!self.closed)
        }

        fn freeze(&mut self, _: &dyn Clock) -> Result<PreparedSnapshot, EffectError> {
            self.step("freeze")?;
            Ok(PreparedSnapshot::GitHub(frozen_snapshot_for_test(
                "0123456789abcdef0123456789abcdef",
                "report.md",
                &"0".repeat(64),
            )))
        }

        fn upload(&mut self, _: &PreparedSnapshot) -> Result<Value, EffectError> {
            self.step("upload")?;
            Ok(if self.missing_id {
                json!({ "id": null, "url": null })
            } else {
                json!({ "id": "artifact-id", "url": null })
            })
        }

        fn verify_artifact(
            &mut self,
            _: &PreparedSnapshot,
            receipt: &Value,
        ) -> Result<Value, EffectError> {
            self.step("verifyArtifact")?;
            assert_eq!(receipt["id"], "artifact-id");
            Ok(json!({ "id": "artifact-id", "url": "https://example.test/report" }))
        }

        fn comment(&mut self, _: &PreparedSnapshot, payload: &Value) -> Result<Value, EffectError> {
            self.step("comment")?;
            self.body = payload["body"].as_str().map(str::to_owned);
            Ok(json!({ "id": 47, "url": null }))
        }

        fn verify_comment(
            &mut self,
            _: &PreparedSnapshot,
            payload: &Value,
        ) -> Result<(), EffectError> {
            self.step("verifyComment")?;
            assert_eq!(payload["body"].as_str(), self.body.as_deref());
            assert_eq!(payload["comment"]["id"], 47);
            Ok(())
        }
    }

    fn publish(journal: &mut MemoryJournal, effects: &mut Effects, prefix: &str) -> Value {
        run(
            journal,
            effects,
            &FakeClock,
            URL,
            prefix,
            new_result(URL, "result.json"),
        )
    }

    #[test]
    fn writes_intents_and_receipts_before_readback_and_reuses_success_verbatim() {
        let observed = Rc::new(RefCell::new(Vec::new()));
        let mut journal = MemoryJournal {
            observed: Some(Rc::clone(&observed)),
            ..MemoryJournal::default()
        };
        let mut effects = Effects {
            observed: Some(observed),
            ..Effects::default()
        };
        let result = publish(&mut journal, &mut effects, "prefix\nexact");
        assert_eq!(result["status"], "succeeded");
        assert_eq!(result["snapshot"]["schemaVersion"], 1);
        assert_eq!(
            result["snapshot"]["runId"],
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(result["snapshot"]["markdownPath"], "report.md");
        assert_eq!(result["snapshot"]["markdownHash"], "0".repeat(64));
        assert!(result["snapshot"].get("prepared").is_none());
        assert_eq!(
            effects.body.as_deref(),
            Some(
                "prefix\nexact\n\n[Pull request overview (auto-generated).](https://example.test/report)"
            )
        );
        assert_eq!(journal.writes[0]["operations"][0]["status"], "pending");
        assert_eq!(
            journal.writes[1]["operations"][0]["response"]["id"],
            "artifact-id"
        );
        assert_eq!(journal.writes[2]["operations"][1]["status"], "pending");
        assert_eq!(journal.writes[3]["operations"][1]["response"]["id"], 47);
        assert_eq!(
            effects.calls,
            [
                "isOpen",
                "freeze",
                "upload",
                "verifyArtifact",
                "comment",
                "verifyComment"
            ]
        );
        let mut retry = Effects::default();
        assert_eq!(
            publish(&mut journal, &mut retry, "different prefix"),
            result
        );
        assert!(retry.calls.is_empty());
    }

    #[test]
    fn uncertain_and_completed_writes_never_replay() {
        for stage in ["upload", "verifyArtifact", "comment", "verifyComment"] {
            let mut journal = MemoryJournal::default();
            let mut effects = Effects {
                fail: Some(stage),
                ..Effects::default()
            };
            let result = publish(&mut journal, &mut effects, "");
            assert_eq!(result["status"], "incomplete", "{stage}");
            let mut retry = Effects::default();
            let next = publish(&mut journal, &mut retry, "");
            assert_eq!(next["status"], "needsReconciliation", "{stage}");
            assert!(retry.calls.is_empty(), "{stage}");
            if stage != "upload" {
                assert_eq!(next["operations"][0]["response"]["id"], "artifact-id");
            }
            if stage == "verifyComment" {
                assert_eq!(next["operations"][1]["response"]["id"], 47);
            }
        }
    }

    #[test]
    fn definite_refusal_and_tool_only_failure_allow_retry() {
        for stage in ["upload", "freeze"] {
            let mut journal = MemoryJournal::default();
            let mut effects = Effects {
                fail: Some(stage),
                refused: true,
                ..Effects::default()
            };
            let result = publish(&mut journal, &mut effects, "");
            if stage == "freeze" {
                assert_eq!(result["status"], "failed");
                assert_eq!(result["missingTools"], json!(["renderer"]));
                assert_eq!(result["stage"], "renderHtml");
                assert_eq!(result["snapshot"]["runId"], "frozen");
            } else {
                assert_eq!(result["operations"][0]["status"], "failed");
            }
            let mut retry = Effects::default();
            assert_eq!(publish(&mut journal, &mut retry, "")["status"], "succeeded");
        }
    }

    #[test]
    fn journal_failure_stops_effects_and_keeps_received_ids() {
        for (fail_write, expected_calls, id) in [
            (0, vec!["isOpen", "freeze"], Value::Null),
            (1, vec!["isOpen", "freeze", "upload"], json!("artifact-id")),
            (
                3,
                vec!["isOpen", "freeze", "upload", "verifyArtifact", "comment"],
                json!(47),
            ),
        ] {
            let mut journal = MemoryJournal {
                fail_write: Some(fail_write),
                ..MemoryJournal::default()
            };
            let mut effects = Effects::default();
            let result = publish(&mut journal, &mut effects, "");
            assert_eq!(result["status"], "journalFailure");
            assert!(result["error"].as_str().unwrap().contains("disk failure"));
            assert_eq!(effects.calls, expected_calls);
            if fail_write == 1 {
                assert_eq!(result["artifact"]["id"], id);
            }
            if fail_write == 3 {
                assert_eq!(result["comment"]["id"], id);
            }
        }
    }

    #[test]
    fn malformed_receipt_is_retained_and_cannot_be_replayed() {
        let mut journal = MemoryJournal::default();
        let mut effects = Effects {
            missing_id: true,
            ..Effects::default()
        };
        let result = publish(&mut journal, &mut effects, "");
        assert_eq!(result["status"], "incomplete");
        assert_eq!(result["operations"][0]["status"], "completed");
        assert!(result["operations"][0]["response"]["id"].is_null());
        let mut retry = Effects::default();
        assert_eq!(
            publish(&mut journal, &mut retry, "")["status"],
            "needsReconciliation"
        );
        assert!(retry.calls.is_empty());
    }

    #[test]
    fn effect_failure_survives_failure_to_save_diagnostics() {
        let mut journal = MemoryJournal {
            fail_write: Some(1),
            ..MemoryJournal::default()
        };
        let mut effects = Effects {
            fail: Some("upload"),
            ..Effects::default()
        };
        let result = publish(&mut journal, &mut effects, "");
        assert_eq!(result["status"], "journalFailure");
        let error = result["error"].as_str().unwrap();
        assert!(error.contains("upload failed"));
        assert!(error.contains("disk failure"));
        assert_eq!(result["operations"][0]["status"], "ambiguous");
    }

    #[test]
    fn transaction_ownership_covers_every_injected_effect() {
        // The journal represents an already acquired lock, as FileTransaction does.
        let held = Rc::new(Cell::new(true));
        let mut effects = Effects {
            lock_held: Some(Rc::clone(&held)),
            ..Effects::default()
        };
        {
            let mut journal = LockedJournal {
                journal: MemoryJournal::default(),
                held: Rc::clone(&held),
            };
            assert_eq!(
                run(
                    &mut journal,
                    &mut effects,
                    &FakeClock,
                    URL,
                    "",
                    new_result(URL, "result.json")
                )["status"],
                "succeeded"
            );
            assert!(held.get());
        }
        assert!(!held.get());
    }
    #[test]
    fn malformed_previous_records_are_never_overwritten() {
        for previous in [
            json!({"schemaVersion": 1, "pullRequestUrl": URL, "status": "succeeded"}),
            json!({"schemaVersion": 1, "pullRequestUrl": "other"}),
        ] {
            let bytes = serde_json::to_vec(&previous).unwrap();
            let mut journal = MemoryJournal {
                bytes: Some(bytes.clone()),
                ..MemoryJournal::default()
            };
            let mut effects = Effects::default();
            let result = publish(&mut journal, &mut effects, "");
            assert_eq!(result["status"], "failed");
            assert_eq!(journal.bytes, Some(bytes));
            assert!(effects.calls.is_empty());
        }
    }

    #[test]
    fn closed_pull_request_is_skipped_without_a_write() {
        let mut journal = MemoryJournal::default();
        let mut effects = Effects {
            closed: true,
            ..Effects::default()
        };
        assert_eq!(publish(&mut journal, &mut effects, "")["status"], "skipped");
        assert_eq!(effects.calls, ["isOpen"]);
    }
}
