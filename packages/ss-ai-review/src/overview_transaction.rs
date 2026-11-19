//! Best-effort initial overview publication with durable write intents.
#![expect(
    clippy::indexing_slicing,
    reason = "Mutable JSON indexing updates locally constructed result and operation objects or a validated prior object; read indexing returns null for missing keys"
)]
#![allow(
    clippy::result_large_err,
    reason = "This best-effort boundary handles occasional failures and keeps snapshot diagnostics inline rather than allocating for each error"
)]

use std::path::Path;

use serde_json::{Value, json};
use ss_ai_runtime::FileTransaction;

use crate::Clock;
use crate::overview_snapshot::FrozenSnapshot;

/// Provider and local preparation operations supplied by the composition root.
///
/// Freeze acquires the snapshot lock after the result lock and permits refreshing an unpublished snapshot:
/// the transaction calls it only after validating that no write may have occurred.
/// Write errors must mark a definite refusal only when the service proved that
/// no write occurred.
pub(crate) trait OverviewEffects {
    fn is_open(&mut self) -> Result<bool, EffectError>;
    fn freeze(&mut self, clock: &dyn Clock) -> Result<FrozenSnapshot, EffectError>;
    fn comment_bodies(
        &mut self,
        snapshot: &FrozenSnapshot,
        prefix: &str,
        clock: &dyn Clock,
    ) -> Result<Vec<String>, EffectError>;
    fn comment(&mut self, snapshot: &FrozenSnapshot, payload: &Value)
    -> Result<Value, EffectError>;
    fn verify_comment(
        &mut self,
        snapshot: &FrozenSnapshot,
        payload: &Value,
    ) -> Result<(), EffectError>;
}

/// Carries diagnostics and the evidence needed to decide whether to retry.
pub(crate) struct EffectError {
    pub(crate) message: String,
    pub(crate) definite_refusal: bool,
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

/// Creates the bounded result envelope for the native publication command.
fn new_result(url: &str, path: &str) -> Value {
    json!({
        "schemaVersion": 2, "pullRequestUrl": url, "status": "preparing",
        "stage": "preflight", "resultPath": path, "snapshot": null,
        "comments": [], "operations": [], "error": null
    })
}

/// Rejects malformed or unrelated records before allowing a replacement.
fn validate_record(record: &Value, url: &str) -> Result<(), &'static str> {
    let object = record
        .as_object()
        .ok_or("Overview record is not an object")?;
    if object.get("schemaVersion") != Some(&json!(2))
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
        Some("preflight" | "freeze" | "prepareComments" | "comment" | "verifyComment")
    ) || object.get("resultPath").and_then(Value::as_str).is_none()
        || !object
            .get("error")
            .is_some_and(|error| error.is_null() || error.is_string())
        || !object
            .get("snapshot")
            .is_some_and(|value| value.is_null() || value.is_object())
        || !object.get("comments").is_some_and(|comments| {
            comments
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_object))
        })
    {
        return Err("Malformed overview publication record");
    }
    let operations = object
        .get("operations")
        .and_then(Value::as_array)
        .ok_or("Malformed overview operations")?;
    let mut expected_part = 1_u64;
    let mut expected_count = None;
    for (index, operation) in operations.iter().enumerate() {
        let part = operation
            .get("part")
            .and_then(Value::as_u64)
            .ok_or("Malformed overview comment part")?;
        let part_count = operation
            .get("partCount")
            .and_then(Value::as_u64)
            .ok_or("Malformed overview comment count")?;
        if part != expected_part
            || part > part_count
            || expected_count.is_some_and(|count| count != part_count)
        {
            return Err("Malformed overview operations");
        }
        expected_part = expected_part
            .checked_add(1)
            .ok_or("Malformed overview operations")?;
        expected_count = Some(part_count);
        let state = operation.get("status").and_then(Value::as_str);
        if operation.get("name").and_then(Value::as_str) != Some("comment")
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
        if index > 0
            && index
                .checked_sub(1)
                .and_then(|previous| operations.get(previous))
                .and_then(|operation| operation.get("status"))
                .and_then(Value::as_str)
                != Some("completed")
        {
            return Err("Overview comment part has no completed predecessor");
        }
    }
    if expected_count.is_some_and(|count| usize::try_from(count).ok() != Some(operations.len()))
        && status == "succeeded"
    {
        return Err("Incomplete overview record claims success");
    }
    if status == "succeeded"
        && (operations.is_empty()
            || operations.iter().any(|operation| {
                operation["status"] != "completed" || !has_id(&operation["response"])
            })
            || !record["snapshot"].is_object()
            || !record["comments"].is_array()
            || record["comments"].as_array().is_none_or(|comments| {
                comments.len() != operations.len()
                    || comments.iter().any(|comment| !has_id(comment))
            })
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
        result["status"] = json!(
            if result["operations"].as_array().is_some_and(Vec::is_empty) {
                "failed"
            } else {
                "incomplete"
            }
        );
        result["error"] = json!(bounded(&error.message));
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
    result["stage"] = json!("prepareComments");
    let bodies = effects.comment_bodies(&snapshot, prefix, clock)?;
    if bodies.is_empty() {
        return Err(local_error("Frozen overview produced no comments"));
    }
    let count = bodies.len();
    for (index, body) in bodies.into_iter().enumerate() {
        result["stage"] = json!("comment");
        let part = index
            .checked_add(1)
            .ok_or_else(|| local_error("Overview has too many comment parts"))?;
        let payload = json!({ "body": body });
        let comment = write_effect(journal, result, part, count, |_| {
            effects.comment(&snapshot, &payload)
        })?;
        result["stage"] = json!("verifyComment");
        effects.verify_comment(&snapshot, &json!({ "body": body, "comment": comment }))?;
    }
    result["status"] = json!("succeeded");
    Ok(())
}

/// Saves intent before a remote write and its response before readback.
fn write_effect(
    journal: &mut dyn Journal,
    result: &mut Value,
    part: usize,
    part_count: usize,
    action: impl FnOnce(&Value) -> Result<Value, EffectError>,
) -> Result<Value, EffectError> {
    let operations = result["operations"]
        .as_array_mut()
        .expect("new result has operations");
    operations.push(json!({
        "name": "comment",
        "part": part,
        "partCount": part_count,
        "status": "pending",
        "response": null
    }));
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
    // Keep an unexpected receipt even when it cannot populate the receipt list;
    // the completed operation still proves a write may exist.
    if response.is_object() {
        let comments = result["comments"]
            .as_array_mut()
            .expect("new result has comments");
        comments.push(response.clone());
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
    use crate::overview_snapshot::{FrozenSnapshot, frozen_snapshot_for_test};

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
        bodies: Option<Vec<String>>,
    }

    impl Effects {
        fn step(&mut self, name: &'static str) -> Result<(), EffectError> {
            if let Some(lock_held) = &self.lock_held {
                assert!(lock_held.get(), "transaction lock must cover {name}");
            }
            if let Some(observed) = &self.observed {
                let observed = observed.borrow();
                match name {
                    "comment" => assert_eq!(
                        observed.last().unwrap()["operations"]
                            .as_array()
                            .unwrap()
                            .last()
                            .unwrap()["status"],
                        "pending"
                    ),
                    "verifyComment" => assert_eq!(
                        observed.last().unwrap()["operations"]
                            .as_array()
                            .unwrap()
                            .last()
                            .unwrap()["response"]["id"],
                        u64::try_from(
                            observed.last().unwrap()["operations"]
                                .as_array()
                                .unwrap()
                                .len()
                        )
                        .unwrap()
                        .checked_add(46)
                        .unwrap()
                    ),
                    _ => {}
                }
            }
            self.calls.push(name);
            if self.fail == Some(name) {
                Err(EffectError {
                    message: format!("{name} failed"),
                    definite_refusal: self.refused,
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

        fn freeze(&mut self, _: &dyn Clock) -> Result<FrozenSnapshot, EffectError> {
            self.step("freeze")?;
            Ok(frozen_snapshot_for_test(
                "0123456789abcdef0123456789abcdef",
                "report.md",
                &"0".repeat(64),
            ))
        }

        fn comment_bodies(
            &mut self,
            _: &FrozenSnapshot,
            prefix: &str,
            _: &dyn Clock,
        ) -> Result<Vec<String>, EffectError> {
            self.step("prepareComments")?;
            Ok(self
                .bodies
                .clone()
                .unwrap_or_else(|| vec![format!("[Copilot speaking]\n\n{prefix}\n\nreport")]))
        }

        fn comment(&mut self, _: &FrozenSnapshot, payload: &Value) -> Result<Value, EffectError> {
            self.step("comment")?;
            self.body = payload["body"].as_str().map(str::to_owned);
            let id = u64::try_from(self.calls.iter().filter(|name| **name == "comment").count())
                .unwrap()
                .checked_add(46)
                .unwrap();
            Ok(if self.missing_id {
                json!({ "id": null, "url": null })
            } else {
                json!({ "id": id, "url": null })
            })
        }

        fn verify_comment(
            &mut self,
            _: &FrozenSnapshot,
            payload: &Value,
        ) -> Result<(), EffectError> {
            self.step("verifyComment")?;
            assert_eq!(payload["body"].as_str(), self.body.as_deref());
            let id = u64::try_from(self.calls.iter().filter(|name| **name == "comment").count())
                .unwrap()
                .checked_add(46)
                .unwrap();
            assert_eq!(payload["comment"]["id"], id);
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
            Some("[Copilot speaking]\n\nprefix\nexact\n\nreport")
        );
        assert_eq!(journal.writes[0]["operations"][0]["status"], "pending");
        assert_eq!(journal.writes[1]["operations"][0]["response"]["id"], 47);
        assert_eq!(result["comments"], json!([{"id": 47, "url": null}]));
        assert_eq!(
            effects.calls,
            [
                "isOpen",
                "freeze",
                "prepareComments",
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
        for stage in ["comment", "verifyComment"] {
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
            if stage == "verifyComment" {
                assert_eq!(next["comments"][0]["id"], 47);
            }
        }
    }

    #[test]
    fn definite_refusal_and_preparation_failure_allow_retry() {
        for stage in ["comment", "freeze"] {
            let mut journal = MemoryJournal::default();
            let mut effects = Effects {
                fail: Some(stage),
                refused: true,
                ..Effects::default()
            };
            let result = publish(&mut journal, &mut effects, "");
            if stage == "freeze" {
                assert_eq!(result["status"], "failed");
                assert_eq!(result["stage"], "freeze");
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
            (0, vec!["isOpen", "freeze", "prepareComments"], Value::Null),
            (
                1,
                vec!["isOpen", "freeze", "prepareComments", "comment"],
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
            if fail_write != 0 {
                assert_eq!(result["comments"][0]["id"], id);
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
            fail: Some("comment"),
            ..Effects::default()
        };
        let result = publish(&mut journal, &mut effects, "");
        assert_eq!(result["status"], "journalFailure");
        let error = result["error"].as_str().unwrap();
        assert!(error.contains("comment failed"));
        assert!(error.contains("disk failure"));
        assert_eq!(result["operations"][0]["status"], "ambiguous");
    }

    #[test]
    fn multipart_comments_keep_part_identity_in_the_journal() {
        let mut journal = MemoryJournal::default();
        let mut effects = Effects {
            bodies: Some(vec!["part one".into(), "part two".into()]),
            ..Effects::default()
        };
        let result = publish(&mut journal, &mut effects, "");
        assert_eq!(result["status"], "succeeded");
        assert_eq!(result["comments"].as_array().unwrap().len(), 2);
        assert_eq!(result["operations"][0]["part"], 1);
        assert_eq!(result["operations"][0]["partCount"], 2);
        assert_eq!(result["operations"][1]["part"], 2);
        assert_eq!(result["operations"][1]["partCount"], 2);
        assert_eq!(
            effects.calls,
            [
                "isOpen",
                "freeze",
                "prepareComments",
                "comment",
                "verifyComment",
                "comment",
                "verifyComment"
            ]
        );
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
