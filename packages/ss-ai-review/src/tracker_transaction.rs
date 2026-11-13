//! Tracker publication with durable, per-item creation intents.
#![allow(
    clippy::result_large_err,
    reason = "The application retains classified diagnostics"
)]
#![expect(
    clippy::indexing_slicing,
    reason = "Indexes address locally built or validated journal operations"
)]
#![expect(
    clippy::self_named_module_files,
    reason = "The production transaction remains in its existing file while its offline harness has a separate module"
)]

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use ohno::AppError;
use serde::de::{Error as _, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use ss_ai_runtime::FileTransaction;
use url::Url;

use crate::classification;
use crate::error::fail;
use crate::target::decode;
use crate::tracker_body::{AcceptedRewrite, BodyVerdict, adjudication_excerpts, compare_ado_body};
use crate::tracker_target::TrackerTarget;

/// Separates native tracker journal records from legacy skill-produced records.
const JOURNAL_SCHEMA_VERSION: u64 = 2;

#[cfg(any(test, feature = "private-fuzz"))]
pub use fuzz::fuzz_tracker_transaction;

/// Provider calls supplied by the composition root, which also performs preflight.
pub(crate) trait Effects {
    fn create(&mut self, item: &Value) -> Result<Value, AppError>;
    fn read(&mut self, receipt: &Value) -> Result<Value, AppError>;
    /// Pace subsequent creates according to provider requirements.
    fn wait_between_creates(&mut self) -> Result<(), AppError>;
}

/// Journal operations remain under the caller's retained transaction lock.
trait Journal {
    fn read(&mut self) -> Result<Option<Vec<u8>>, AppError>;
    fn write(&mut self, record: &Value) -> Result<(), AppError>;
}

impl Journal for FileTransaction<'_> {
    fn read(&mut self) -> Result<Option<Vec<u8>>, AppError> {
        self.read_bytes()
    }

    fn write(&mut self, record: &Value) -> Result<(), AppError> {
        self.write_bytes(&serde_json::to_vec_pretty(record)?)
    }
}

/// A person explicitly accepts exactly the item and bodies named here.
pub(crate) struct Acceptance<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) expected_body_hash: &'a str,
    pub(crate) stored_body_hash: &'a str,
}

/// Checks a prior record before provider preflight, while retaining the lock.
pub(crate) fn check_prior(
    journal: &mut FileTransaction<'_>,
    target: &Value,
    acceptance: Option<&Acceptance<'_>>,
) -> Result<(), AppError> {
    check_prior_inner(journal, target, acceptance).map(|_| ())
}

/// Checks a prepared plan before destination preflight can contact a provider.
pub(crate) fn check_plan(
    journal: &mut FileTransaction<'_>,
    target: &Value,
    plan: &[Value],
    acceptance: Option<&Acceptance<'_>>,
) -> Result<(), AppError> {
    check_plan_inner(journal, target, plan, acceptance).map(|_| ())
}

/// Publishes prepared items with a lock acquired before provider preflight.
///
/// The caller must retain this same `FileTransaction` across `check_prior`,
/// destination preflight, and this call. Failed and paused outcomes must be
/// emitted before the command exits nonzero. No summary is filed remotely.
#[expect(
    clippy::too_many_arguments,
    reason = "Composition supplies the locked journal and prepared publication inputs"
)]
pub(crate) fn execute(
    journal: &mut FileTransaction<'_>,
    path: &Path,
    effects: &mut dyn Effects,
    target: &Value,
    state: &Value,
    plan: &[Value],
    summary: &str,
    started_utc: &str,
    acceptance: Option<&Acceptance<'_>>,
) -> Result<Value, AppError> {
    run(
        journal,
        path,
        effects,
        target,
        state,
        plan,
        summary,
        started_utc,
        acceptance,
    )
}

/// Rejects duplicate keys at every level before interpreting a persisted record.
struct Unique(Value);

impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = Unique;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate or case-ambiguous properties")
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
        Ok(Unique(json!(v)))
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
        Ok(Unique(json!(v)))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
        Ok(Unique(json!(v)))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
        Ok(Unique(json!(v)))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
        Ok(Unique(json!(v)))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Unique, E> {
        Ok(Unique(json!(v)))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
        Ok(Unique(Value::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Unique, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<Unique>()? {
            values.push(value.0);
        }
        Ok(Unique(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
        let mut values = Map::new();
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.to_ascii_lowercase()) {
                return Err(A::Error::custom("Duplicate JSON property"));
            }
            values.insert(key, map.next_value::<Unique>()?.0);
        }
        Ok(Unique(Value::Object(values)))
    }
}

/// Rejects JSON keys that `serde_json` reserves for its internal value encoding.
///
/// With arbitrary precision enabled, `serde_json` interprets an object containing
/// its private number key as a number. Check raw object keys before reading a
/// `Value` so a journal cannot use that representation to change JSON types.
fn reject_private_json_keys(bytes: &[u8]) -> Result<(), AppError> {
    let mut bytes_with_positions = bytes.iter().enumerate();
    while let Some((start, byte)) = bytes_with_positions.next() {
        if *byte != b'"' {
            continue;
        }
        let mut escaped = false;
        let mut end = None;
        for (position, byte) in bytes_with_positions.by_ref() {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                end = Some(position);
                break;
            }
        }
        let end = end.ok_or_else(|| fail("invalidState", "Malformed tracker journal string"))?;
        let next = bytes[end.saturating_add(1)..]
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace());
        if next == Some(b':') {
            let key: String = serde_json::from_slice(&bytes[start..=end]).map_err(|e| {
                fail(
                    "invalidState",
                    format!("Malformed tracker journal key: {e}"),
                )
            })?;
            if key.starts_with("$serde_json::private::") {
                return Err(fail(
                    "invalidState",
                    "Tracker journal uses a reserved JSON key",
                ));
            }
        }
    }
    Ok(())
}

/// Checks the target through its own complete identity validation.
fn validate_target(target: &Value) -> Result<TrackerTarget, AppError> {
    let target: TrackerTarget = serde_json::from_value(target.clone())
        .map_err(|e| fail("invalidInput", format!("Invalid tracker target: {e}")))?;
    target.validate()?;
    Ok(target)
}

/// Checks complete prepared input before any provider write.
fn preflight(target: &Value, state: &Value, plan: &[Value]) -> Result<(), AppError> {
    validate_target(target)?;
    if !state.is_object() || state["platform"] != target["platform"] {
        return Err(fail("invalidInput", "Invalid tracker state"));
    }
    if target["platform"] == "gitHub" {
        if !nonempty(&state["repositoryId"]) {
            return Err(fail("invalidInput", "Missing repository identity"));
        }
    } else if ["workItemType", "areaPath", "descriptionField"]
        .iter()
        .any(|k| !nonempty(&state[*k]))
        || !(state["parent"].is_null() || nonempty(&state["parent"]["itemId"]))
    {
        return Err(fail("invalidInput", "Invalid work item metadata"));
    }
    validate_plan(plan)
}

/// Checks the whole ordered plan independently of live destination metadata.
fn validate_plan(plan: &[Value]) -> Result<(), AppError> {
    let mut ids = HashSet::new();
    for item in plan {
        if !item.is_object()
            || !nonempty(&item["findingId"])
            || !nonempty(&item["title"])
            || !item["body"].is_string()
            || !ids.insert(item["findingId"].as_str())
        {
            return Err(fail("invalidInput", "Invalid tracker publication plan"));
        }
    }
    Ok(())
}

fn nonempty(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.trim().is_empty())
}

/// Matches the PowerShell plan digest, including UTF-16 field lengths.
fn plan_hash(plan: &[Value]) -> String {
    let mut digest = Sha256::new();
    for item in plan {
        for key in ["findingId", "title", "body"] {
            let field = item[key].as_str().unwrap_or_default();
            digest.update(field.encode_utf16().count().to_string().as_bytes());
            digest.update(b":");
            digest.update(field.as_bytes());
        }
    }
    format!("{:x}", digest.finalize())
}

fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn valid_hash(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

fn nullable_string(value: Option<&Value>) -> bool {
    value.is_some_and(|v| v.is_null() || v.is_string())
}

/// Rejects malformed or contradictory records before replay or continuation.
fn validate_record(record: &Value) -> Result<(), AppError> {
    let invalid = || fail("invalidState", "Invalid tracker publication journal");
    if !record.is_object()
        || record["schemaVersion"] != JOURNAL_SCHEMA_VERSION
        || validate_target(&record["target"]).is_err()
        || record["platform"] != record["target"]["platform"]
        || !nonempty(&record["startedUtc"])
        || !nonempty(&record["trackerUrl"])
        || record["trackerUrl"] != record["target"]["webUrl"]
        || !valid_hash(&record["planHash"])
        || ![
            "workItemType",
            "areaPath",
            "descriptionField",
            "parentItemId",
            "message",
        ]
        .iter()
        .all(|key| nullable_string(record.get(*key)))
        || !record
            .get("verification")
            .is_some_and(|v| v.is_null() || v.is_array())
        || !record
            .get("adjudication")
            .is_some_and(|v| v.is_null() || v.is_object())
    {
        return Err(invalid());
    }
    if record["platform"] == "azureDevOps"
        && ["workItemType", "areaPath", "descriptionField"]
            .iter()
            .any(|key| !nonempty(&record[*key]))
    {
        return Err(invalid());
    }
    let status = record["status"].as_str().ok_or_else(invalid)?;
    if !matches!(
        status,
        "inProgress" | "failed" | "succeeded" | "needsAdjudication" | "resumeReady"
    ) {
        return Err(invalid());
    }
    let ops = record["operations"].as_array().ok_or_else(invalid)?;
    let Some((verify, creates)) = ops.split_last() else {
        return Err(invalid());
    };
    if verify["operation"] != "verifyItems"
        || !["operation", "findingId", "title", "outcome"]
            .iter()
            .all(|key| verify.get(*key).is_some())
        || !verify["findingId"].is_null()
        || !verify["title"].is_null()
        || !["itemId", "itemNodeId", "url", "message"]
            .iter()
            .all(|key| nullable_string(verify.get(*key)))
        || !verify["itemId"].is_null()
        || !verify["itemNodeId"].is_null()
        || !verify["url"].is_null()
        || !matches!(
            verify["outcome"].as_str(),
            Some("pending" | "succeeded" | "failed")
        )
    {
        return Err(invalid());
    }
    let mut ids = HashSet::new();
    let mut item_ids = HashSet::new();
    let mut pending_seen = false;
    for op in creates {
        if !op.is_object()
            || op["operation"] != "createItem"
            || !nonempty(&op["findingId"])
            || !nonempty(&op["title"])
            || !ids.insert(op["findingId"].as_str())
            || !["itemId", "itemNodeId", "url", "message"]
                .iter()
                .all(|k| nullable_string(op.get(*k)))
        {
            return Err(invalid());
        }
        match op["outcome"].as_str() {
            Some("succeeded")
                if !pending_seen
                    && nonempty(&op["itemId"])
                    && nonempty(&op["itemNodeId"])
                    && nonempty(&op["url"])
                    && item_ids.insert(op["itemId"].as_str()) => {}
            Some("pending" | "failed" | "ambiguous") => {
                if pending_seen && op["outcome"] != "pending" {
                    return Err(invalid());
                }
                pending_seen = true;
                if !op["itemId"].is_null() || !op["itemNodeId"].is_null() || !op["url"].is_null() {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
        if let Some(accepted) = op.get("acceptedRewrite")
            && (!accepted.is_object()
                || record["platform"] != "azureDevOps"
                || op["outcome"] != "succeeded"
                || !valid_hash(&accepted["expectedBodyHash"])
                || !valid_hash(&accepted["storedBodyHash"]))
        {
            return Err(invalid());
        }
    }
    if status == "succeeded"
        && (pending_seen || verify["outcome"] != "succeeded" || !record["verification"].is_array())
        || status != "succeeded" && verify["outcome"] == "succeeded"
        || status != "succeeded" && !record["verification"].is_null()
        || matches!(status, "needsAdjudication" | "resumeReady")
            && (verify["outcome"] != "pending"
                || creates
                    .iter()
                    .any(|op| matches!(op["outcome"].as_str(), Some("failed" | "ambiguous"))))
    {
        return Err(invalid());
    }
    if status == "succeeded" {
        let verified = record["verification"].as_array().ok_or_else(invalid)?;
        if verified.len() != creates.len()
            || verified.iter().zip(creates).any(|(item, op)| {
                item["findingId"] != op["findingId"]
                    || item["itemId"] != op["itemId"]
                    || item["url"] != op["url"]
                    || item["title"] != op["title"]
                    || !item["descriptionFormat"].is_string()
            })
        {
            return Err(invalid());
        }
    }
    if status == "needsAdjudication" {
        let adjudication = &record["adjudication"];
        if record["platform"] != "azureDevOps"
            || !nonempty(&adjudication["itemId"])
            || !nonempty(&adjudication["findingId"])
            || !nonempty(&adjudication["title"])
            || !adjudication.get("similarity").is_some_and(|v| {
                v.is_null() || v.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n))
            })
            || !adjudication["sentExcerpt"].is_string()
            || !adjudication["storedExcerpt"].is_string()
            || !valid_hash(&adjudication["expectedBodyHash"])
            || !valid_hash(&adjudication["storedBodyHash"])
            || adjudication
                .get("firstDifference")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .is_none()
            || !creates.iter().any(|op| {
                op["outcome"] == "succeeded"
                    && op["itemId"] == adjudication["itemId"]
                    && op["findingId"] == adjudication["findingId"]
                    && op["title"] == adjudication["title"]
            })
        {
            return Err(invalid());
        }
    } else if !record["adjudication"].is_null() {
        return Err(invalid());
    }
    if status == "resumeReady"
        && (record["platform"] != "azureDevOps"
            || !creates.iter().any(|op| op.get("acceptedRewrite").is_some()))
    {
        return Err(invalid());
    }
    Ok(())
}

/// Loads a prior complete record and enforces the only allowed continuation.
fn check_prior_inner(
    journal: &mut dyn Journal,
    target: &Value,
    acceptance: Option<&Acceptance<'_>>,
) -> Result<Option<Value>, AppError> {
    let supplied_target = validate_target(target)?;
    let Some(bytes) = journal.read()? else {
        return if acceptance.is_some() {
            Err(fail("invalidState", "No paused tracker publication"))
        } else {
            Ok(None)
        };
    };
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let _: Unique = serde_json::from_slice(bytes)
        .map_err(|e| fail("invalidState", format!("Malformed tracker journal: {e}")))?;
    reject_private_json_keys(bytes)?;
    // The duplicate-key visitor checks the complete input, but under serde_json's
    // arbitrary-precision feature numeric values reach it as private map tokens.
    // Deserialize again as Value so numbers retain their actual JSON types.
    let old: Value = serde_json::from_slice(bytes)
        .map_err(|e| fail("invalidState", format!("Malformed tracker journal: {e}")))?;
    validate_record(&old)?;
    if let Some(acceptance) = acceptance {
        let recorded_target = validate_target(&old["target"])?;
        if !recorded_target.matches(&supplied_target)?
            || !matches!(
                old["status"].as_str(),
                Some("needsAdjudication" | "resumeReady")
            )
        {
            return Err(fail(
                "invalidState",
                "Tracker publication is not paused for this target",
            ));
        }
        let binding = if old["status"] == "needsAdjudication" {
            &old["adjudication"]
        } else {
            old["operations"]
                .as_array()
                .and_then(|ops| {
                    ops.iter().find(|op| {
                        op["itemId"] == acceptance.item_id && op.get("acceptedRewrite").is_some()
                    })
                })
                .map(|op| &op["acceptedRewrite"])
                .ok_or_else(|| fail("invalidState", "No matching accepted rewrite"))?
        };
        let id_matches = old["status"] == "resumeReady" || binding["itemId"] == acceptance.item_id;
        if !id_matches
            || binding["expectedBodyHash"] != acceptance.expected_body_hash
            || binding["storedBodyHash"] != acceptance.stored_body_hash
        {
            return Err(fail(
                "invalidState",
                "Acceptance names a different item or body",
            ));
        }
        return Ok(Some(old));
    }
    let ops = old["operations"]
        .as_array()
        .ok_or_else(|| fail("invalidState", "Missing operations"))?;
    let clean = old["status"] == "failed"
        && ops
            .iter()
            .filter(|op| op["operation"] == "createItem")
            .all(|op| matches!(op["outcome"].as_str(), Some("failed" | "pending")));
    if clean {
        Ok(None)
    } else {
        Err(fail(
            "ambiguousWrite",
            "Prior tracker publication requires reconciliation or explicit adjudication",
        ))
    }
}

/// Binds a continuation to its complete, ordered approved plan.
fn validate_continuation_plan(record: &Value, plan: &[Value]) -> Result<(), AppError> {
    if record["planHash"] != plan_hash(plan)
        || record["operations"].as_array().is_none_or(|ops| {
            ops.len().checked_sub(1) != Some(plan.len())
                || plan.iter().zip(ops).any(|(item, op)| {
                    item["findingId"] != op["findingId"] || item["title"] != op["title"]
                })
        })
    {
        return Err(fail("invalidState", "Continuation plan changed"));
    }
    Ok(())
}

/// Performs all journal and plan checks that do not require provider access.
fn check_plan_inner(
    journal: &mut dyn Journal,
    target: &Value,
    plan: &[Value],
    acceptance: Option<&Acceptance<'_>>,
) -> Result<Option<Value>, AppError> {
    validate_plan(plan)?;
    let prior = check_prior_inner(journal, target, acceptance)?;
    if let Some(record) = &prior {
        validate_continuation_plan(record, plan)?;
    }
    Ok(prior)
}

/// Exercises the same local guard with an in-memory journal in composition tests.
#[cfg(test)]
pub(crate) fn check_plan_bytes(
    bytes: &[u8],
    target: &Value,
    plan: &[Value],
    acceptance: Option<&Acceptance<'_>>,
) -> Result<(), AppError> {
    let mut journal = ReadOnlyJournal(bytes);
    check_plan_inner(&mut journal, target, plan, acceptance).map(|_| ())
}

/// Produces a paused record for composition's local-preflight tests.
#[cfg(test)]
pub(crate) fn paused_fixture(target: &Value, state: &Value, plan: &[Value]) -> Value {
    let mut record = new_record(target, state, plan, "2026-01-01T00:00:00Z");
    let first = &plan[0];
    let operation = &mut record["operations"][0];
    operation["outcome"] = json!("succeeded");
    operation["itemId"] = json!("1");
    operation["itemNodeId"] = json!("1");
    operation["url"] = json!("https://example.test/1");
    record["status"] = json!("needsAdjudication");
    record["adjudication"] = json!({
        "findingId":first["findingId"], "itemId":"1", "title":first["title"],
        "expectedBodyHash":hash(first["body"].as_str().unwrap()),
        "storedBodyHash":hash("stored rewrite"), "similarity":null, "firstDifference":0,
        "sentExcerpt":"approved", "storedExcerpt":"stored"
    });
    record
}

/// Keeps composition tests independent of real filesystem resources.
#[cfg(test)]
struct ReadOnlyJournal<'a>(&'a [u8]);

#[cfg(test)]
impl Journal for ReadOnlyJournal<'_> {
    fn read(&mut self) -> Result<Option<Vec<u8>>, AppError> {
        Ok(Some(self.0.to_vec()))
    }

    fn write(&mut self, _: &Value) -> Result<(), AppError> {
        Err(fail("invalidState", "Unexpected journal write"))
    }
}

fn metadata(state: &Value, key: &str) -> Value {
    if key == "parentItemId" {
        state["parent"]["itemId"].clone()
    } else {
        state[key].clone()
    }
}

/// Builds the schema shared with the tracker publication command.
fn new_record(target: &Value, state: &Value, plan: &[Value], now: &str) -> Value {
    let mut operations: Vec<Value> = plan.iter().map(|item| json!({"operation":"createItem", "findingId":item["findingId"],
        "title":item["title"], "outcome":"pending", "itemId":null, "itemNodeId":null, "url":null, "message":null})).collect();
    operations.push(
        json!({"operation":"verifyItems", "findingId":null, "title":null, "outcome":"pending",
        "itemId":null, "itemNodeId":null, "url":null, "message":null}),
    );
    json!({"schemaVersion":JOURNAL_SCHEMA_VERSION, "startedUtc":now, "platform":target["platform"], "target":target,
        "trackerUrl":target["webUrl"], "workItemType":metadata(state,"workItemType"),
        "areaPath":metadata(state,"areaPath"), "descriptionField":metadata(state,"descriptionField"),
        "parentItemId":metadata(state,"parentItemId"), "planHash":plan_hash(plan), "status":"inProgress",
        "message":null, "operations":operations, "verification":null, "adjudication":null})
}

/// Persists a complete record and stops dispatch immediately on failure.
fn save(journal: &mut dyn Journal, record: &Value) -> Result<(), AppError> {
    // Provider responses and preflight metadata also contribute to generated
    // records. Reject contradictions in every build before replacing the
    // durable recovery authority, rather than asserting on external data.
    validate_record(record).map_err(|e| {
        fail(
            "journalFailure",
            format!("Cannot persist tracker journal: {e}"),
        )
    })?;
    journal.write(record).map_err(|e| {
        fail(
            "journalFailure",
            format!("Cannot persist tracker journal: {e}"),
        )
    })
}

/// Describes created items even when a later operation failed.
fn outcome(record: &Value, path: &Path, summary: &str) -> Value {
    let ops = record["operations"]
        .as_array()
        .expect("constructed operations");
    let items: Vec<_> = ops.iter().filter(|op| op["operation"] == "createItem" && op["outcome"] == "succeeded")
        .map(|op| json!({"findingId":op["findingId"], "itemId":op["itemId"], "url":op["url"], "title":op["title"]})).collect();
    json!({"schemaVersion":1, "status":record["status"], "message":record["message"], "platform":record["platform"],
        "trackerUrl":record["trackerUrl"], "resultPath":path.to_string_lossy(), "publishedItemCount":items.len(),
        "failedOperationCount":ops.iter().filter(|op| op["outcome"] == "failed").count(),
        "ambiguousOperationCount":ops.iter().filter(|op| op["outcome"] == "ambiguous").count(),
        "summary":if record["status"] == "succeeded" { Some(summary) } else { None },
        "adjudication":record["adjudication"], "items":items})
}

/// Performs one readback, retaining exact evidence for a human decision.
fn verify(
    effects: &mut dyn Effects,
    record: &mut Value,
    plan: &[Value],
    index: usize,
) -> Result<Option<Value>, AppError> {
    let op = &record["operations"][index];
    let item = &plan[index];
    let actual = effects.read(op)?;
    if actual["itemId"] != op["itemId"]
        || actual["itemNodeId"] != op["itemNodeId"]
        || !same_item_url(&record["platform"], &actual["url"], &op["url"])
        || actual["title"] != item["title"]
        || !actual["body"].is_string()
    {
        return Err(fail(
            "service",
            "Tracker readback identity, title, or body differs",
        ));
    }
    let approved = item["body"].as_str().expect("validated body");
    let stored = actual["body"].as_str().expect("validated body");
    if record["platform"] == "gitHub" {
        if approved != stored {
            return Err(fail("service", "GitHub stored a different body"));
        }
    } else {
        if !actual["descriptionFormat"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("markdown"))
        {
            return Err(fail(
                "service",
                "Azure DevOps stored a non-Markdown description",
            ));
        }
        let accepted = op.get("acceptedRewrite");
        let accepted_pair = accepted
            .filter(|a| {
                a["expectedBodyHash"] == hash(approved) && a["storedBodyHash"] == hash(stored)
            })
            .map(|_| AcceptedRewrite { approved, stored });
        let mut verdict = compare_ado_body(approved, stored, accepted_pair.as_ref());
        // An existing acceptance must not silently become a new adjudication.
        if accepted.is_some()
            && accepted_pair.is_none()
            && matches!(verdict, BodyVerdict::Adjudicate(_))
        {
            verdict = BodyVerdict::Reject;
        }
        match verdict {
            BodyVerdict::Match => {}
            BodyVerdict::Reject => {
                return Err(fail(
                    "service",
                    "Stored description differs from approved or accepted body",
                ));
            }
            BodyVerdict::Adjudicate(deviation) => {
                let (sent_excerpt, stored_excerpt) =
                    adjudication_excerpts(approved, stored, deviation.first_difference);
                record["adjudication"] = json!({"findingId":item["findingId"], "itemId":actual["itemId"],
                    "title":item["title"], "expectedBodyHash":hash(approved), "storedBodyHash":hash(stored),
                    "similarity":deviation.similarity, "firstDifference":deviation.first_difference,
                    "sentExcerpt":sent_excerpt, "storedExcerpt":stored_excerpt});
                record["status"] = json!("needsAdjudication");
                return Ok(None);
            }
        }
    }
    Ok(Some(
        json!({"findingId":item["findingId"], "itemId":actual["itemId"], "url":op["url"],
        "title":actual["title"], "descriptionFormat":actual["descriptionFormat"]}),
    ))
}

/// Compares Azure item URLs using the case-insensitive tracker identity.
fn same_item_url(platform: &Value, actual: &Value, expected: &Value) -> bool {
    if platform == "azureDevOps" {
        actual
            .as_str()
            .and_then(azure_url_identity)
            .zip(expected.as_str().and_then(azure_url_identity))
            .is_some_and(|(actual, expected)| actual == expected)
    } else {
        actual == expected
    }
}

/// Resolves an Azure URL to the case-insensitive path identity used by the API.
fn azure_url_identity(value: &str) -> Option<(String, Vec<String>)> {
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url
        .path_segments()?
        .map(decode)
        .collect::<Result<Vec<_>, _>>()
        .ok()?
        .into_iter()
        .map(|segment| segment.to_lowercase())
        .collect();
    Some((url.origin().ascii_serialization(), path))
}

/// Records readback failure separately from the durable creation receipt.
fn checked_verify(
    effects: &mut dyn Effects,
    record: &mut Value,
    plan: &[Value],
    index: usize,
) -> Result<Option<Value>, AppError> {
    let result = verify(effects, record, plan, index);
    if let Err(error) = &result {
        record["operations"][plan.len()]["outcome"] = json!("failed");
        record["operations"][plan.len()]["message"] = json!(error.to_string());
    }
    result
}

/// Executes only after the locked journal and complete prepared plan agree.
#[expect(
    clippy::too_many_arguments,
    reason = "The injected boundary mirrors the composition inputs"
)]
fn run(
    journal: &mut dyn Journal,
    path: &Path,
    effects: &mut dyn Effects,
    target: &Value,
    state: &Value,
    plan: &[Value],
    summary: &str,
    now: &str,
    acceptance: Option<&Acceptance<'_>>,
) -> Result<Value, AppError> {
    preflight(target, state, plan)?;
    if now.trim().is_empty() {
        return Err(fail("invalidInput", "Missing start time"));
    }
    let prior = check_plan_inner(journal, target, plan, acceptance)?;
    let mut record = if let Some(mut old) = prior {
        if [
            "workItemType",
            "areaPath",
            "descriptionField",
            "parentItemId",
        ]
        .iter()
        .any(|k| old[*k] != metadata(state, k))
        {
            return Err(fail(
                "invalidState",
                "Continuation tracker metadata changed",
            ));
        }
        let acceptance = acceptance.expect("prior continuation requires acceptance");
        if old["status"] == "needsAdjudication" {
            let ops = old["operations"]
                .as_array_mut()
                .expect("validated operations");
            let op = ops
                .iter_mut()
                .find(|op| op["itemId"] == acceptance.item_id)
                .ok_or_else(|| fail("invalidState", "Accepted item is absent"))?;
            op["acceptedRewrite"] = json!({"expectedBodyHash":acceptance.expected_body_hash, "storedBodyHash":acceptance.stored_body_hash});
            old["status"] = json!("resumeReady");
            old["adjudication"] = Value::Null;
            // Keep the accepted decision durable before the next creation
            // intent; a failed intent save can resume it, while a saved
            // ambiguous intent must be reconciled. Ref:
            // skills/ss-review/docs/implementation.md#tracker-publication.
            save(journal, &old)?;
        }
        old
    } else {
        let record = new_record(target, state, plan, now);
        save(journal, &record)?;
        record
    };

    let action = (|| -> Result<(), AppError> {
        let mut created = false;
        for (index, item) in plan.iter().enumerate() {
            if record["operations"][index]["outcome"] == "succeeded" {
                if checked_verify(effects, &mut record, plan, index)?.is_none() {
                    save(journal, &record)?;
                    return Ok(());
                }
                continue;
            }
            if created {
                effects.wait_between_creates()?;
            }
            // A crash after dispatch cannot prove that the service refused the
            // creation, so the durable intent must already forbid replay.
            record["status"] = json!("inProgress");
            record["operations"][index]["outcome"] = json!("ambiguous");
            save(journal, &record)?;
            let receipt = match effects.create(item) {
                Ok(receipt) => receipt,
                Err(error) => {
                    let definite = matches!(
                        classification(&error),
                        "permissionDenied" | "authentication"
                    ) || (target["platform"] == "gitHub"
                        && classification(&error) == "explicitRateLimited")
                        || (target["platform"] == "azureDevOps"
                            && classification(&error) == "rateLimited");
                    if definite {
                        record["operations"][index]["outcome"] = json!("failed");
                    }
                    record["operations"][index]["message"] = json!(error.to_string());
                    return Err(error);
                }
            };
            if !nonempty(&receipt["itemId"])
                || !nonempty(&receipt["itemNodeId"])
                || !nonempty(&receipt["url"])
            {
                // Preserve the returned evidence even though it cannot identify
                // a readback or prove that the creation did not happen.
                record["operations"][index]["receipt"] = receipt;
                return Err(fail(
                    "ambiguousWrite",
                    "Creation returned an incomplete identity",
                ));
            }
            for key in ["itemId", "itemNodeId", "url"] {
                record["operations"][index][key] = receipt[key].clone();
            }
            record["operations"][index]["outcome"] = json!("succeeded");
            save(journal, &record)?;
            created = true;
            if checked_verify(effects, &mut record, plan, index)?.is_none() {
                save(journal, &record)?;
                return Ok(());
            }
        }
        let mut verified = Vec::new();
        for index in 0..plan.len() {
            match checked_verify(effects, &mut record, plan, index)? {
                Some(item) => verified.push(item),
                None => {
                    save(journal, &record)?;
                    return Ok(());
                }
            }
        }
        record["operations"][plan.len()]["outcome"] = json!("succeeded");
        record["verification"] = json!(verified);
        record["status"] = json!("succeeded");
        save(journal, &record)
    })();

    if let Err(error) = action {
        if classification(&error) == "journalFailure" {
            // A failed persistence attempt ends dispatch. Keep the last durable
            // record as the recovery authority; this diagnostic is not another
            // transition and must not trigger the ordinary failure save.
            record["status"] = json!("journalFailure");
            record["message"] = json!(error.to_string());
            return Ok(outcome(&record, path, summary));
        }
        record["status"] = json!("failed");
        record["adjudication"] = Value::Null;
        record["message"] = json!(error.to_string());
        if let Err(write_error) = save(journal, &record) {
            record["status"] = json!("journalFailure");
            record["message"] = json!(format!("{error}; {write_error}"));
        }
    }
    Ok(outcome(&record, path, summary))
}

#[cfg(any(test, feature = "private-fuzz"))]
mod fuzz;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Test fixtures and their operation counts are small and bounded"
)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;

    use ohno::AppError;
    use serde_json::{Value, json};

    use super::{Acceptance, Effects, Journal, check_prior_inner, plan_hash, run};
    use crate::error::fail;

    /// Simulates atomic replacement and deterministic interruption at any write.
    #[derive(Default)]
    struct Memory {
        bytes: Option<Vec<u8>>,
        writes: usize,
        fail_at: Option<usize>,
        observed: Rc<RefCell<Vec<Value>>>,
    }

    impl Journal for Memory {
        fn read(&mut self) -> Result<Option<Vec<u8>>, AppError> {
            Ok(self.bytes.clone())
        }
        fn write(&mut self, record: &Value) -> Result<(), AppError> {
            self.writes += 1;
            if self.fail_at == Some(self.writes) {
                return Err(fail("io", "write failed"));
            }
            self.bytes = Some(serde_json::to_vec(record).unwrap());
            self.observed.borrow_mut().push(record.clone());
            Ok(())
        }
    }

    /// Captures effect order and checks durable intent and receipt boundaries.
    #[derive(Default)]
    struct Provider {
        calls: Vec<String>,
        observed: Rc<RefCell<Vec<Value>>>,
        records: Vec<Value>,
        fail_create: Option<&'static str>,
        fail_after: usize,
        fail_read: bool,
        rewrite: bool,
        changed: bool,
        duplicate_id: bool,
    }

    impl Effects for Provider {
        fn create(&mut self, item: &Value) -> Result<Value, AppError> {
            let current = self.observed.borrow();
            let record = current.last().unwrap();
            let op = record["operations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|op| op["findingId"] == item["findingId"])
                .unwrap();
            assert_eq!(op["outcome"], "ambiguous");
            drop(current);
            self.calls
                .push(format!("create {}", item["findingId"].as_str().unwrap()));
            if let Some(kind) = self
                .fail_create
                .filter(|_| self.records.len() >= self.fail_after)
            {
                return Err(fail(kind, "creation failed"));
            }
            let id = if self.duplicate_id && !self.records.is_empty() {
                "1".to_owned()
            } else {
                (self.records.len() + 1).to_string()
            };
            let body = if self.rewrite && self.records.is_empty() {
                format!("{}!", item["body"].as_str().unwrap())
            } else {
                item["body"].as_str().unwrap().to_owned()
            };
            let record = json!({"itemId":id, "itemNodeId":id, "url":format!("https://example.test/{id}"),
                "title":item["title"], "body":body, "descriptionFormat":"Markdown"});
            self.records.push(record.clone());
            Ok(record)
        }
        fn read(&mut self, receipt: &Value) -> Result<Value, AppError> {
            let current = self.observed.borrow();
            assert!(
                current.last().unwrap()["operations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|op| op["itemId"] == receipt["itemId"] && op["outcome"] == "succeeded")
            );
            drop(current);
            self.calls
                .push(format!("read {}", receipt["itemId"].as_str().unwrap()));
            if self.fail_read {
                return Err(fail("service", "read failed"));
            }
            let mut item = self
                .records
                .iter()
                .find(|r| r["itemId"] == receipt["itemId"])
                .unwrap()
                .clone();
            if self.changed && item["itemId"] == "1" {
                item["body"] = json!("changed");
            }
            Ok(item)
        }
        fn wait_between_creates(&mut self) -> Result<(), AppError> {
            self.calls.push("wait".into());
            Ok(())
        }
    }

    fn target(platform: &str) -> Value {
        if platform == "gitHub" {
            json!({"platform":"gitHub","collectionUrl":null,"organization":"org","project":null,
                "repository":"repo","webUrl":"https://github.com/org/repo/issues"})
        } else {
            json!({"platform":"azureDevOps","collectionUrl":"https://dev.azure.com/org","organization":"org",
                "project":"Project","repository":null,"webUrl":"https://dev.azure.com/org/Project/_workitems"})
        }
    }
    fn state(platform: &str) -> Value {
        if platform == "gitHub" {
            json!({"platform":"gitHub","repositoryId":"repo-id"})
        } else {
            json!({"platform":"azureDevOps","workItemType":"Bug","areaPath":"Project",
            "descriptionField":"System.Description","parent":null})
        }
    }
    fn plan() -> Vec<Value> {
        vec![
            json!({"findingId":"first","title":"First","body":"A finding with enough approved text to compare accurately."}),
            json!({"findingId":"second","title":"Second","body":"Another finding."}),
        ]
    }
    fn call(
        journal: &mut Memory,
        effects: &mut Provider,
        platform: &str,
        plan: &[Value],
        acceptance: Option<&Acceptance<'_>>,
    ) -> Result<Value, AppError> {
        run(
            journal,
            Path::new("result.json"),
            effects,
            &target(platform),
            &state(platform),
            plan,
            "summary",
            "2026-01-01T00:00:00Z",
            acceptance,
        )
    }
    fn setup() -> (Memory, Provider) {
        let journal = Memory::default();
        let provider = Provider {
            observed: Rc::clone(&journal.observed),
            ..Provider::default()
        };
        (journal, provider)
    }

    #[test]
    fn publishes_one_item_at_a_time_and_returns_summary_only_after_final_readback() {
        for platform in ["gitHub", "azureDevOps"] {
            let (mut journal, mut provider) = setup();
            let result = call(&mut journal, &mut provider, platform, &plan(), None).unwrap();
            assert_eq!(result["status"], "succeeded");
            assert_eq!(result["summary"], "summary");
            assert_eq!(result["publishedItemCount"], 2);
            super::validate_record(
                &serde_json::from_slice::<Value>(journal.bytes.as_ref().unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(
                provider.calls,
                [
                    "create first",
                    "read 1",
                    "wait",
                    "create second",
                    "read 2",
                    "read 1",
                    "read 2"
                ]
            );
            _ = check_prior_inner(&mut journal, &target(platform), None).unwrap_err();
        }
    }

    #[test]
    fn each_failed_journal_write_prevents_further_effects_and_replay() {
        let (mut baseline, mut provider) = setup();
        call(&mut baseline, &mut provider, "gitHub", &plan(), None).unwrap();
        for failure in 2..=baseline.writes {
            let (mut journal, mut provider) = setup();
            journal.fail_at = Some(failure);
            let result = call(&mut journal, &mut provider, "gitHub", &plan(), None).unwrap();
            assert_eq!(result["status"], "journalFailure", "{failure}");
            _ = check_prior_inner(&mut journal, &target("gitHub"), None)
                .expect_err(&format!("write {failure} must block replay"));
        }
        let (mut journal, mut provider) = setup();
        journal.fail_at = Some(1);
        _ = call(&mut journal, &mut provider, "gitHub", &plan(), None).unwrap_err();
        assert!(provider.calls.is_empty());
    }

    #[test]
    fn contradictory_provider_receipt_preserves_the_durable_creation_intent() {
        let (mut journal, mut provider) = setup();
        provider.duplicate_id = true;
        let result = call(&mut journal, &mut provider, "gitHub", &plan(), None).unwrap();
        assert_eq!(result["status"], "journalFailure");
        let record: Value = serde_json::from_slice(journal.bytes.as_ref().unwrap()).unwrap();
        super::validate_record(&record).unwrap();
        assert_eq!(record["operations"][0]["outcome"], "succeeded");
        assert_eq!(record["operations"][1]["outcome"], "ambiguous");
        assert_eq!(
            provider.calls,
            ["create first", "read 1", "wait", "create second"]
        );
        _ = check_prior_inner(&mut journal, &target("gitHub"), None).unwrap_err();
    }

    #[test]
    fn contradictory_generated_metadata_is_not_persisted_or_dispatched() {
        let (mut journal, mut provider) = setup();
        let mut state = state("gitHub");
        state["workItemType"] = json!({"unexpected": true});
        let error = run(
            &mut journal,
            Path::new("result.json"),
            &mut provider,
            &target("gitHub"),
            &state,
            &plan(),
            "summary",
            "now",
            None,
        )
        .unwrap_err();
        assert_eq!(crate::classification(&error), "journalFailure");
        assert!(journal.bytes.is_none());
        assert!(provider.calls.is_empty());
    }

    #[test]
    fn bounded_offline_transaction_harness_covers_scripted_transitions() {
        for data in [
            vec![0, 3, 0, 0],
            vec![1, 3, 0, 0, 8, 0, 0, 8, 0, 0, 8],
            vec![0, 3, 1, 4, 0, 0, 2],
            vec![1, 2, 0, 3, 1],
        ] {
            super::fuzz_tracker_transaction(&data);
        }
    }

    #[test]
    fn ambiguous_failure_blocks_retry_but_proven_refusal_allows_it() {
        for (kind, retry) in [("ambiguousWrite", false), ("permissionDenied", true)] {
            let (mut journal, mut provider) = setup();
            provider.fail_create = Some(kind);
            let result = call(&mut journal, &mut provider, "gitHub", &plan(), None).unwrap();
            assert_eq!(result["status"], "failed");
            assert!(result["summary"].is_null());
            assert_eq!(
                check_prior_inner(&mut journal, &target("gitHub"), None).is_ok(),
                retry
            );
        }
    }

    #[test]
    fn partial_outcomes_keep_acknowledged_items_and_do_not_replay() {
        let (mut journal, mut provider) = setup();
        provider.fail_create = Some("permissionDenied");
        provider.fail_after = 1;
        let result = call(&mut journal, &mut provider, "gitHub", &plan(), None).unwrap();
        assert_eq!(result["status"], "failed");
        assert_eq!(result["publishedItemCount"], 1);
        assert_eq!(result["items"][0]["itemId"], "1");
        assert!(result["summary"].is_null());
        _ = check_prior_inner(&mut journal, &target("gitHub"), None).unwrap_err();
        let (mut journal, mut provider) = setup();
        provider.fail_read = true;
        let result = call(&mut journal, &mut provider, "gitHub", &plan(), None).unwrap();
        assert_eq!(result["publishedItemCount"], 1);
        assert_eq!(provider.calls, ["create first", "read 1"]);
        _ = check_prior_inner(&mut journal, &target("gitHub"), None).unwrap_err();
    }

    #[test]
    fn an_interrupted_accepted_decision_can_resume_but_a_new_intent_cannot() {
        let (mut journal, mut provider) = setup();
        provider.rewrite = true;
        let paused = call(&mut journal, &mut provider, "azureDevOps", &plan(), None).unwrap();
        let a = &paused["adjudication"];
        let decision = Acceptance {
            item_id: "1",
            expected_body_hash: a["expectedBodyHash"].as_str().unwrap(),
            stored_body_hash: a["storedBodyHash"].as_str().unwrap(),
        };
        // The accepted decision is durable; the next attempted intent cannot be.
        journal.fail_at = Some(journal.writes + 2);
        let failed = call(
            &mut journal,
            &mut provider,
            "azureDevOps",
            &plan(),
            Some(&decision),
        )
        .unwrap();
        assert_eq!(failed["status"], "journalFailure");
        journal.fail_at = None;
        assert_eq!(
            check_prior_inner(&mut journal, &target("azureDevOps"), Some(&decision))
                .unwrap()
                .unwrap()["status"],
            "resumeReady"
        );
        let resumed = call(
            &mut journal,
            &mut provider,
            "azureDevOps",
            &plan(),
            Some(&decision),
        )
        .unwrap();
        assert_eq!(resumed["status"], "succeeded");
        assert_eq!(
            provider
                .calls
                .iter()
                .filter(|call| *call == "create first")
                .count(),
            1
        );
    }

    #[test]
    fn continuation_rejects_changed_target_metadata_and_body_hashes() {
        let (mut journal, mut provider) = setup();
        provider.rewrite = true;
        let paused = call(&mut journal, &mut provider, "azureDevOps", &plan(), None).unwrap();
        let a = &paused["adjudication"];
        let expected = a["expectedBodyHash"].as_str().unwrap();
        let stored = a["storedBodyHash"].as_str().unwrap();
        let wrong = Acceptance {
            item_id: "1",
            expected_body_hash: stored,
            stored_body_hash: expected,
        };
        _ = call(
            &mut journal,
            &mut provider,
            "azureDevOps",
            &plan(),
            Some(&wrong),
        )
        .unwrap_err();
        let decision = Acceptance {
            item_id: "1",
            expected_body_hash: expected,
            stored_body_hash: stored,
        };
        let mut metadata = state("azureDevOps");
        metadata["areaPath"] = json!("Project\\Other");
        _ = run(
            &mut journal,
            Path::new("result.json"),
            &mut provider,
            &target("azureDevOps"),
            &metadata,
            &plan(),
            "summary",
            "now",
            Some(&decision),
        )
        .unwrap_err();
        let mut other = target("azureDevOps");
        other["organization"] = json!("different");
        other["collectionUrl"] = json!("https://dev.azure.com/different");
        other["webUrl"] = json!("https://dev.azure.com/different/Project/_workitems");
        _ = check_prior_inner(&mut journal, &other, Some(&decision)).unwrap_err();
        assert_eq!(provider.calls, ["create first", "read 1"]);
    }

    #[test]
    fn continuation_matches_case_insensitive_tracker_coordinates() {
        let (mut journal, mut provider) = setup();
        provider.rewrite = true;
        let paused = call(&mut journal, &mut provider, "azureDevOps", &plan(), None).unwrap();
        let adjudication = &paused["adjudication"];
        let acceptance = Acceptance {
            item_id: "1",
            expected_body_hash: adjudication["expectedBodyHash"].as_str().unwrap(),
            stored_body_hash: adjudication["storedBodyHash"].as_str().unwrap(),
        };
        let mut same_tracker = target("azureDevOps");
        same_tracker["organization"] = json!("ORG");
        same_tracker["project"] = json!("pROJECT");
        same_tracker["collectionUrl"] = json!("https://dev.azure.com/ORG");
        same_tracker["webUrl"] = json!("https://dev.azure.com/ORG/pROJECT/_workitems");
        let expected_url = "https://dev.azure.com/org/Project/_workitems/edit/1";
        let actual_url = "https://dev.azure.com/ORG/Project/_workitems/edit/1";
        let mut saved: Value = serde_json::from_slice(journal.bytes.as_ref().unwrap()).unwrap();
        saved["operations"][0]["url"] = json!(expected_url);
        journal.bytes = Some(serde_json::to_vec(&saved).unwrap());
        provider.records[0]["url"] = json!(actual_url);

        let mut different_collection = same_tracker.clone();
        different_collection["collectionUrl"] = json!("https://dev.azure.com/ORG/other");
        different_collection["webUrl"] =
            json!("https://dev.azure.com/ORG/other/pROJECT/_workitems");
        _ = super::validate_target(&different_collection).unwrap();
        _ = check_prior_inner(&mut journal, &different_collection, Some(&acceptance)).unwrap_err();

        let result = run(
            &mut journal,
            Path::new("result.json"),
            &mut provider,
            &same_tracker,
            &state("azureDevOps"),
            &plan(),
            "summary",
            "2026-01-01T00:00:00Z",
            Some(&acceptance),
        )
        .unwrap();
        assert_eq!(result["status"], "succeeded");
        assert_eq!(result["items"][0]["url"], expected_url);
        assert_eq!(
            &provider.calls[..4],
            ["create first", "read 1", "read 1", "create second"]
        );
    }

    #[test]
    fn azure_item_url_identity_decodes_case_insensitive_path_segments() {
        let platform = json!("azureDevOps");
        let actual = json!("https://dev.azure.com/ORG/Pr%C3%98JECT/_WORKITEMS/EDIT/1");
        let expected = json!("https://dev.azure.com/org/Pr%C3%B8ject/_workitems/edit/1");
        assert!(super::same_item_url(&platform, &actual, &expected));

        let different_project =
            json!("https://dev.azure.com/org/Another%20Project/_workitems/edit/1");
        assert!(!super::same_item_url(
            &platform,
            &actual,
            &different_project
        ));
    }

    #[test]
    fn local_plan_check_rejects_changes_before_provider_access() {
        let (mut journal, mut provider) = setup();
        provider.rewrite = true;
        let paused = call(&mut journal, &mut provider, "azureDevOps", &plan(), None).unwrap();
        let adjudication = &paused["adjudication"];
        let acceptance = Acceptance {
            item_id: "1",
            expected_body_hash: adjudication["expectedBodyHash"].as_str().unwrap(),
            stored_body_hash: adjudication["storedBodyHash"].as_str().unwrap(),
        };
        let target = target("azureDevOps");
        let mut variants = Vec::new();
        let mut body = plan();
        body[1]["body"] = json!("different");
        variants.push(body);
        let mut title = plan();
        title[1]["title"] = json!("different");
        variants.push(title);
        let mut finding = plan();
        finding[1]["findingId"] = json!("different");
        variants.push(finding);
        let mut reordered = plan();
        reordered.reverse();
        variants.push(reordered);
        let calls = provider.calls.len();
        let writes = journal.writes;
        for variant in variants {
            _ = super::check_plan_inner(&mut journal, &target, &variant, Some(&acceptance))
                .unwrap_err();
            assert_eq!(provider.calls.len(), calls);
            assert_eq!(journal.writes, writes);
        }
        _ = super::check_plan_inner(&mut journal, &target, &plan(), Some(&acceptance)).unwrap();
        let mut fixture = super::paused_fixture(&target, &state("azureDevOps"), &plan());
        fixture["adjudication"]["similarity"] = json!(0.995);
        let evidence = &fixture["adjudication"];
        let item_id = evidence["itemId"].as_str().unwrap().to_owned();
        let expected = evidence["expectedBodyHash"].as_str().unwrap().to_owned();
        let stored = evidence["storedBodyHash"].as_str().unwrap().to_owned();
        let acceptance = Acceptance {
            item_id: &item_id,
            expected_body_hash: &expected,
            stored_body_hash: &stored,
        };
        super::check_plan_bytes(
            &serde_json::to_vec(&fixture).unwrap(),
            &target,
            &plan(),
            Some(&acceptance),
        )
        .unwrap();
        fixture["adjudication"]["similarity"] = json!({"$serde_json::private::Number":"0.995"});
        _ = super::check_plan_bytes(
            &serde_json::to_vec(&fixture).unwrap(),
            &target,
            &plan(),
            Some(&acceptance),
        )
        .unwrap_err();
    }

    #[test]
    fn continuation_requires_explicit_exact_binding_and_rereads_before_creating() {
        let (mut journal, mut provider) = setup();
        provider.rewrite = true;
        let paused = call(&mut journal, &mut provider, "azureDevOps", &plan(), None).unwrap();
        assert_eq!(paused["status"], "needsAdjudication");
        super::validate_record(
            &serde_json::from_slice::<Value>(journal.bytes.as_ref().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(paused["publishedItemCount"], 1);
        assert!(paused["summary"].is_null());
        let adjudication = &paused["adjudication"];
        let expected = adjudication["expectedBodyHash"].as_str().unwrap();
        let stored = adjudication["storedBodyHash"].as_str().unwrap();
        let bad = Acceptance {
            item_id: "2",
            expected_body_hash: expected,
            stored_body_hash: stored,
        };
        _ = call(
            &mut journal,
            &mut provider,
            "azureDevOps",
            &plan(),
            Some(&bad),
        )
        .unwrap_err();
        let good = Acceptance {
            item_id: "1",
            expected_body_hash: expected,
            stored_body_hash: stored,
        };
        let mut changed_plan = plan();
        changed_plan[1]["body"] = json!("different");
        _ = call(
            &mut journal,
            &mut provider,
            "azureDevOps",
            &changed_plan,
            Some(&good),
        )
        .unwrap_err();
        provider.changed = true;
        let failed = call(
            &mut journal,
            &mut provider,
            "azureDevOps",
            &plan(),
            Some(&good),
        )
        .unwrap();
        assert_eq!(failed["status"], "failed");
        assert_eq!(
            provider
                .calls
                .iter()
                .filter(|call| *call == "create second")
                .count(),
            0
        );

        let (mut journal, mut provider) = setup();
        provider.rewrite = true;
        let paused = call(&mut journal, &mut provider, "azureDevOps", &plan(), None).unwrap();
        let a = &paused["adjudication"];
        let good = Acceptance {
            item_id: "1",
            expected_body_hash: a["expectedBodyHash"].as_str().unwrap(),
            stored_body_hash: a["storedBodyHash"].as_str().unwrap(),
        };
        let result = call(
            &mut journal,
            &mut provider,
            "azureDevOps",
            &plan(),
            Some(&good),
        )
        .unwrap();
        assert_eq!(result["status"], "succeeded");
        assert_eq!(
            &provider.calls[..4],
            ["create first", "read 1", "read 1", "create second"]
        );
    }

    #[test]
    fn malformed_or_duplicate_journal_records_never_authorize_writes() {
        let (mut journal, mut provider) = setup();
        provider.fail_create = Some("permissionDenied");
        call(&mut journal, &mut provider, "gitHub", &plan(), None).unwrap();
        let original = String::from_utf8(journal.bytes.clone().unwrap()).unwrap();
        for bytes in [
            original
                .replace(
                    "\"status\":\"failed\"",
                    "\"status\":\"inProgress\",\"status\":\"failed\"",
                )
                .into_bytes(),
            br#"{"schemaVersion":1,"status":"failed","operations":[]}"#.to_vec(),
        ] {
            journal.bytes = Some(bytes);
            _ = check_prior_inner(&mut journal, &target("gitHub"), None).unwrap_err();
        }
    }

    #[test]
    fn private_json_number_tokens_cannot_disguise_object_types() {
        for bytes in [
            br#"{"similarity":{"$serde_json::private::Number":"0.995"}}"#.as_slice(),
            br#"{"schemaVersion":{"\u0024serde_json::private::Number":"1"}}"#.as_slice(),
        ] {
            _ = super::reject_private_json_keys(bytes).unwrap_err();
        }
        super::reject_private_json_keys(
            br#"{"body":"a \"$serde_json::private::Number\" literal","similarity":0.995}"#,
        )
        .unwrap();
    }

    #[test]
    fn legacy_paused_record_is_rejected_without_changing_the_journal() {
        let (mut journal, mut provider) = setup();
        provider.rewrite = true;
        let paused = call(&mut journal, &mut provider, "azureDevOps", &plan(), None).unwrap();
        let a = &paused["adjudication"];
        let acceptance = Acceptance {
            item_id: "1",
            expected_body_hash: a["expectedBodyHash"].as_str().unwrap(),
            stored_body_hash: a["storedBodyHash"].as_str().unwrap(),
        };
        let native: Value = serde_json::from_slice(journal.bytes.as_ref().unwrap()).unwrap();
        let mut legacy = native.clone();
        legacy["schemaVersion"] = json!(1);
        legacy["adjudication"]
            .as_object_mut()
            .unwrap()
            .remove("firstDifference");
        let mut incomplete_native = native;
        incomplete_native["adjudication"]
            .as_object_mut()
            .unwrap()
            .remove("firstDifference");
        for record in [legacy, incomplete_native] {
            let bytes = serde_json::to_vec(&record).unwrap();
            journal.bytes = Some(bytes.clone());
            _ = check_prior_inner(&mut journal, &target("azureDevOps"), Some(&acceptance))
                .unwrap_err();
            assert_eq!(journal.bytes.as_deref(), Some(bytes.as_slice()));
        }
        assert_eq!(provider.calls, ["create first", "read 1"]);
    }

    #[test]
    fn plan_hash_counts_utf16_units_and_preserves_field_boundaries() {
        let first = vec![json!({"findingId":"a","title":"😀","body":"b"})];
        let second = vec![json!({"findingId":"a","title":"😀b","body":""})];
        assert_eq!(plan_hash(&first), super::hash("1:a2:😀1:b"));
        assert_ne!(plan_hash(&first), plan_hash(&second));
    }
}
