//! Durable audit decisions and approval gates.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use ohno::AppError;
use serde::Deserialize;
use serde::de::value::{Error as DeserializeError, StrDeserializer};
use serde_json::{Value, json};
use ss_ai_review_context::{decode_file_text, read_capture};
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail, read_bytes};

use crate::findings::read_artifact;
use crate::workflow_plan::{self, Decision, DecisionValue, PlanItem, PlanKind, parse_json};

/// Keeps the validated document and its last verdict for each finding together.
pub(crate) struct DecisionJournal {
    document: Value,
    last: HashMap<String, DecisionValue>,
}

impl DecisionJournal {
    /// Replays validated verdicts without revisiting the JSON representation.
    pub(crate) fn last_decision(&self, id: &str) -> Option<DecisionValue> {
        self.last.get(id).copied()
    }
}

/// Identifies a journal verdict outside the canonical decision vocabulary.
#[ohno::error]
struct UnknownJournalDecision;

/// Appends a validated batch to the decision journal under one transaction.
///
/// # Errors
///
/// Rejects invalid batches, malformed journals, and file failures.
pub fn add_decisions(
    path: &Path,
    plan_path: &Path,
    audit_feedback_path: Option<&Path>,
    policy_classification: bool,
    recorded_utc: &str,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    reject_same_file(path, plan_path)?;
    if let Some(feedback_path) = audit_feedback_path {
        reject_same_file(path, feedback_path)?;
    }
    let plan = match workflow_plan::read(PlanKind::Decisions, plan_path, clock) {
        Ok(plan) => plan,
        Err(error) => {
            if let Some(message) = decision_plan_diagnostic(plan_path, clock) {
                return Err(invalid(message));
            }
            return Err(error);
        }
    };
    if plan.items().is_empty() {
        return Err(invalid("Decision plan must not be empty"));
    }
    let mut requests = Vec::new();
    for item in plan.items() {
        let PlanItem::Decision(decision) = item else {
            return Err(invalid("Expected a decision plan"));
        };
        requests.push(decision.clone());
    }
    append_decisions(
        path,
        requests,
        audit_feedback_path,
        policy_classification,
        recorded_utc,
        clock,
    )
}

/// Records one authored decision without relaxing persisted plan schemas.
///
/// # Errors
///
/// Rejects invalid input, unauthorized policy decisions, malformed journals, and file failures.
pub fn add_decision(
    path: &Path,
    item_path: &Path,
    audit_feedback_path: Option<&Path>,
    policy_classification: bool,
    recorded_utc: &str,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    reject_same_file(path, item_path)?;
    if let Some(feedback_path) = audit_feedback_path {
        reject_same_file(path, feedback_path)?;
    }
    let bytes =
        read_bytes(item_path, clock)?.ok_or_else(|| invalid("Decision input is missing"))?;
    let mut value = parse_json(&bytes)?;
    // The authored boundary accepts case and surrounding whitespace; plan and
    // journal readers retain their strict canonical vocabulary.
    if let Some(text) = value.get("decision").and_then(Value::as_str) {
        let canonical = text.trim().to_lowercase();
        if serde_json::from_value::<DecisionValue>(json!(canonical)).is_err()
            && let Some(id) = value.get("findingId").and_then(Value::as_str)
        {
            return Err(invalid(format!(
                "Unknown decision '{text}' for finding '{id}'"
            )));
        }
        if let Some(decision) = value.get_mut("decision") {
            *decision = json!(canonical);
        }
    }
    let PlanItem::Decision(decision) = workflow_plan::parse_item(PlanKind::Decisions, value)?
    else {
        return Err(invalid("Expected a decision"));
    };
    append_decisions(
        path,
        vec![decision],
        audit_feedback_path,
        policy_classification,
        recorded_utc,
        clock,
    )
}

/// Commits a fully validated decision request through the common authority gate.
fn append_decisions(
    path: &Path,
    mut requests: Vec<Decision>,
    audit_feedback_path: Option<&Path>,
    policy_classification: bool,
    recorded_utc: &str,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    if let Some(feedback_path) = audit_feedback_path {
        let [request] = requests.as_mut_slice() else {
            return Err(invalid(
                "Feedback requires one individual user decision without inline feedback",
            ));
        };
        if policy_classification || request.feedback.is_some() {
            return Err(invalid(
                "Feedback requires one individual user decision without inline feedback",
            ));
        }
        let feedback = get_feedback(feedback_path, &request.finding_id, clock)?;
        if !feedback.trim().is_empty() {
            request.feedback = Some(feedback);
        }
    }
    text(recorded_utc, "recordedUtc")?;
    validate_timestamp(clock.now(), recorded_utc)?;
    let mut ids = HashSet::new();
    for request in &requests {
        if !ids.insert(&request.finding_id) {
            return Err(invalid("A decision batch repeats a finding"));
        }
        if policy_classification
            && (!matches!(
                request.decision,
                DecisionValue::AutoApproved | DecisionValue::NeedsAudit
            ) || request
                .feedback
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()))
        {
            return Err(invalid(
                "Policy classification cannot record user decisions or feedback",
            ));
        }
    }
    let transaction = FileTransaction::open(path, clock)?;
    let mut document = match transaction.read_bytes()? {
        Some(bytes) => parse(&bytes)?,
        None => json!({"schemaVersion": 1, "decisions": []}),
    };
    document = validate_decisions(document, clock)?.document;
    let decisions = document
        .get_mut("decisions")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("Invalid journal"))?;
    let mut recorded = Vec::new();
    for mut request in requests {
        // Check feedback presence before omitting blank optional text.
        if request
            .reason
            .as_deref()
            .is_some_and(|s| s.trim().is_empty())
        {
            request.reason = None;
        }
        if request
            .feedback
            .as_deref()
            .is_some_and(|s| s.trim().is_empty())
        {
            request.feedback = None;
        }
        recorded.push(request.finding_id.clone());
        let mut record = serde_json::to_value(request)?;
        let object = record
            .as_object_mut()
            .ok_or_else(|| invalid("Invalid decision"))?;
        object.insert("recordedUtc".to_owned(), json!(recorded_utc));
        decisions.push(record);
    }
    write(&transaction, &document)?;
    Ok(json!({"decisionsPath": path, "recorded": recorded}))
}

/// Returns the next unclassified finding without consuming it.
///
/// # Errors
///
/// Rejects invalid artifacts or decision journals.
pub fn poll_classification(
    artifact_path: &Path,
    decisions_path: &Path,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let artifact = read_artifact(artifact_path, clock)?;
    consolidated(&artifact)?;
    let decisions = read_journal(decisions_path, clock)?;
    let mut next = Value::Null;
    let mut remaining = 0_usize;
    let mut audit_ids = Vec::new();
    for finding in array(&artifact, "findings")? {
        let id = required_text(finding, "id")?;
        match decisions.last_decision(id) {
            None => {
                if next.is_null() {
                    next = finding.clone();
                }
                remaining = remaining.saturating_add(1);
                audit_ids.push(id);
            }
            Some(DecisionValue::NeedsAudit) => audit_ids.push(id),
            Some(_) => {}
        }
    }
    Ok(
        json!({"isComplete": remaining == 0, "remainingCount": remaining,
        "finding": next, "auditFindingIds": audit_ids}),
    )
}

/// Saves an immutable user response for an existing finding.
///
/// # Errors
///
/// Rejects invalid responses, missing findings, conflicts, and file failures.
pub fn add_feedback(
    path: &Path,
    findings_path: &Path,
    response_id: &str,
    finding_id: &str,
    text_path: &Path,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    reject_same_file(path, findings_path)?;
    reject_same_file(path, text_path)?;
    let bytes =
        read_bytes(text_path, clock)?.ok_or_else(|| invalid("Feedback text file is missing"))?;
    let message = decode_file_text(&bytes);
    text(response_id, "responseId")?;
    if !response_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(invalid("Invalid responseId"));
    }
    text(finding_id, "findingId")?;
    text(&message, "text")?;
    let artifact = read_artifact(findings_path, clock)?;
    if !array(&artifact, "findings")?
        .iter()
        .any(|f| f.get("id").and_then(Value::as_str) == Some(finding_id))
    {
        return Err(invalid("Feedback names an unknown finding"));
    }
    let transaction = FileTransaction::open(path, clock)?;
    let mut document = match transaction.read_bytes()? {
        Some(bytes) => parse(&bytes)?,
        None => json!({"schemaVersion": 1, "feedback": []}),
    };
    validate_feedback(&document)?;
    let item = json!({"responseId": response_id, "findingId": finding_id, "text": message});
    let entries = document
        .get_mut("feedback")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("Invalid feedback"))?;
    for entry in entries.iter() {
        if entry.get("responseId").and_then(Value::as_str) == Some(response_id)
            && entry.get("findingId").and_then(Value::as_str) == Some(finding_id)
        {
            if entry == &item {
                return Ok(());
            }
            return Err(invalid("Feedback response is immutable"));
        }
    }
    entries.push(item);
    write(&transaction, &document)
}

/// Reads a finding's user responses in their original order.
///
/// # Errors
///
/// Rejects missing or malformed feedback records.
pub fn get_feedback(path: &Path, finding_id: &str, clock: &dyn Clock) -> Result<String, AppError> {
    let document = read_required(path, clock)?;
    validate_feedback(&document)?;
    let mut messages = Vec::new();
    for entry in array(&document, "feedback")? {
        if required_text(entry, "findingId")? == finding_id {
            messages.push(required_text(entry, "text")?);
        }
    }
    Ok(messages.join("\n\n"))
}

/// Reads a commit-bound merge decision without translating it into a finding decision.
///
/// # Errors
///
/// Rejects malformed decisions or approvals for another pull request or source commit.
pub fn test_merge_approval(
    path: &Path,
    pr_url: &str,
    source_commit: &str,
    clock: &dyn Clock,
) -> Result<bool, AppError> {
    let document = read_required(path, clock)?;
    validate_merge(&document)?;
    check_merge(&document, pr_url, source_commit)
}

/// Records a merge decision or carries an existing approval to a new capture.
///
/// # Errors
///
/// Rejects invalid captures, inconsistent decisions, and self-approval.
pub fn set_merge_approval(
    path: &Path,
    context_path: &Path,
    approve: bool,
    previous_context_path: Option<&Path>,
    carry_forward: bool,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    reject_same_file(path, context_path)?;
    if let Some(previous) = previous_context_path {
        reject_same_file(path, previous)?;
    }
    let context = read_capture(context_path, clock)?;
    let previous = previous_context_path
        .map(|path| read_capture(path, clock))
        .transpose()?;
    set_merge_approval_from_context(
        path,
        &context,
        approve,
        previous.as_ref(),
        carry_forward,
        clock,
    )
}

/// Records a merge decision using JSON objects supplied by in-memory callers.
///
/// The current and previous contexts must contain the binding fields used by
/// the decision. An approval also requires explicit non-self authorship.
///
/// # Errors
///
/// Rejects invalid context data, inconsistent decisions, and self-approval.
pub fn set_merge_approval_from_files(
    path: &Path,
    context_path: &Path,
    approve: bool,
    previous_context_path: Option<&Path>,
    carry_forward: bool,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    reject_same_file(path, context_path)?;
    if let Some(previous) = previous_context_path {
        reject_same_file(path, previous)?;
    }
    let context = read_required(context_path, clock)?;
    let previous = previous_context_path
        .map(|path| read_required(path, clock))
        .transpose()?;
    set_merge_approval_from_context(
        path,
        &context,
        approve,
        previous.as_ref(),
        carry_forward,
        clock,
    )
}

/// Applies the same binding and authority checks to both context sources.
fn set_merge_approval_from_context(
    path: &Path,
    context: &Value,
    approve: bool,
    previous: Option<&Value>,
    carry_forward: bool,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let url = required_text(context, "pullRequestUrl")?;
    let pr = context
        .get("pullRequest")
        .ok_or_else(|| invalid("Missing pull request"))?;
    let commit = required_text(pr, "reviewedSourceCommit")?;
    validate_merge(&json!({"schemaVersion": 1, "pullRequestUrl": url,
        "approveMerge": false, "approvedSourceCommit": commit}))?;
    if carry_forward != previous.is_some() || (carry_forward && approve) {
        return Err(invalid("Invalid carry-forward arguments"));
    }
    if let Some(previous) = previous {
        let previous_pr = previous
            .get("pullRequest")
            .ok_or_else(|| invalid("Missing pull request"))?;
        validate_merge(&json!({"schemaVersion": 1,
            "pullRequestUrl": required_text(previous, "pullRequestUrl")?,
            "approveMerge": false,
            "approvedSourceCommit": required_text(previous_pr, "reviewedSourceCommit")?}))?;
    }
    let transaction = FileTransaction::open(path, clock)?;
    let existing = transaction
        .read_bytes()?
        .map(|bytes| parse(&bytes))
        .transpose()?;
    if let Some(record) = &existing {
        validate_merge(record)?;
        if required_text(record, "pullRequestUrl")? != url {
            return Err(invalid(
                "Merge decision belongs to a different pull request",
            ));
        }
    }
    let approve = if let Some(previous) = previous {
        if required_text(previous, "pullRequestUrl")? != url {
            return Err(invalid("Captures name different pull requests"));
        }
        let Some(record) = &existing else {
            return Ok(json!({"status": "NoApproval", "sourceCommit": commit}));
        };
        // A retry after a lost response may already have carried the approval to this commit.
        // Recognize that replay before checking the previous commit.
        if record.get("approveMerge").and_then(Value::as_bool) == Some(true)
            && record.get("approvedSourceCommit").and_then(Value::as_str) == Some(commit)
        {
            if pr.get("isAuthoredBySelf").and_then(Value::as_bool) != Some(false) {
                return Err(invalid(
                    "Cannot approve a self-authored pull request or unknown authorship",
                ));
            }
            return Ok(json!({"status": "Recorded", "sourceCommit": commit}));
        }
        let previous_pr = previous
            .get("pullRequest")
            .ok_or_else(|| invalid("Missing pull request"))?;
        let previous_commit = required_text(previous_pr, "reviewedSourceCommit")?;
        if !check_merge(record, url, previous_commit)? {
            return Ok(json!({"status": "NoApproval", "sourceCommit": commit}));
        }
        true
    } else {
        approve
    };
    if approve && pr.get("isAuthoredBySelf").and_then(Value::as_bool) != Some(false) {
        return Err(invalid(
            "Cannot approve a self-authored pull request or unknown authorship",
        ));
    }
    let record = json!({"schemaVersion": 1, "pullRequestUrl": url,
        "approveMerge": approve, "approvedSourceCommit": commit});
    validate_merge(&record)?;
    write(&transaction, &record)?;
    Ok(json!({"status": "Recorded", "sourceCommit": commit}))
}

/// Validates the journal once and builds the replay index in journal order.
fn validate_decisions(document: Value, clock: &dyn Clock) -> Result<DecisionJournal, AppError> {
    object_fields(&document, &["schemaVersion", "decisions"])?;
    version(&document)?;
    let mut last = HashMap::new();
    for record in array(&document, "decisions")? {
        let (finding_id, decision) = validate_decision(record)?;
        validate_timestamp(clock.now(), required_text(record, "recordedUtc")?)?;
        last.insert(finding_id.to_owned(), decision);
    }
    Ok(DecisionJournal { document, last })
}

/// Validates journal entries without coercing malformed JSON.
fn validate_decision(record: &Value) -> Result<(&str, DecisionValue), AppError> {
    object_fields(
        record,
        &["findingId", "decision", "reason", "feedback", "recordedUtc"],
    )?;
    let finding_id = required_text(record, "findingId")?;
    let decision = required_text(record, "decision")?;
    let decision = decision_value(decision, finding_id)?;
    for name in ["reason", "feedback"] {
        if record.get(name).is_some_and(|v| !v.is_string()) {
            return Err(invalid("Invalid decision text"));
        }
    }
    required_text(record, "recordedUtc")?;
    Ok((finding_id, decision))
}

/// Interprets a canonical journal verdict while preserving useful diagnostics.
fn decision_value(decision: &str, finding_id: &str) -> Result<DecisionValue, AppError> {
    DecisionValue::deserialize(StrDeserializer::<DeserializeError>::new(decision)).map_err(
        |_error| {
            UnknownJournalDecision::caused_by(
                invalid(format!(
                    "Unknown decision '{decision}' for finding '{finding_id}'"
                ))
                .into_std_error(),
            )
            .into()
        },
    )
}

/// Adds a useful field path to a rejected typed decision plan.
fn decision_plan_diagnostic(path: &Path, clock: &dyn Clock) -> Option<String> {
    // The canonical plan reader decides validity; this inspection only improves its error.
    let bytes = read_bytes(path, clock).ok()??;
    let value = parse_json(&bytes).ok()?;
    for (index, item) in value.get("decisions")?.as_array()?.iter().enumerate() {
        if let Some(decision) = item.get("decision")
            && serde_json::from_value::<DecisionValue>(decision.clone()).is_err()
        {
            return Some(format!("Invalid decisions/{index}/decision"));
        }
    }
    None
}

/// Enforces feedback response identity and immutability on reads and writes.
fn validate_feedback(document: &Value) -> Result<(), AppError> {
    object_fields(document, &["schemaVersion", "feedback"])?;
    version(document)?;
    let mut seen = HashSet::new();
    for entry in array(document, "feedback")? {
        object_fields(entry, &["responseId", "findingId", "text"])?;
        let response = required_text(entry, "responseId")?;
        if !response
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(invalid("Invalid responseId"));
        }
        let finding = required_text(entry, "findingId")?;
        required_text(entry, "text")?;
        if !seen.insert((response, finding)) {
            return Err(invalid("Duplicate feedback response"));
        }
    }
    Ok(())
}

/// Validates the independent merge decision schema.
fn validate_merge(document: &Value) -> Result<(), AppError> {
    object_fields(
        document,
        &[
            "schemaVersion",
            "pullRequestUrl",
            "approveMerge",
            "approvedSourceCommit",
        ],
    )?;
    version(document)?;
    required_text(document, "pullRequestUrl")?;
    document
        .get("approveMerge")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid("Invalid merge approval schema: approveMerge must be a boolean"))?;
    let commit = required_text(document, "approvedSourceCommit")?;
    if !(40..=64).contains(&commit.len()) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("Invalid approved source commit"));
    }
    Ok(())
}

/// Checks a merge record against the requested pull request and commit.
fn check_merge(document: &Value, url: &str, commit: &str) -> Result<bool, AppError> {
    if required_text(document, "pullRequestUrl")? != url {
        return Err(invalid(
            "Merge decision belongs to a different pull request",
        ));
    }
    let approved = document
        .get("approveMerge")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid("Invalid merge decision"))?;
    if approved && required_text(document, "approvedSourceCommit")? != commit {
        return Err(invalid(
            "Merge approval belongs to a different source commit",
        ));
    }
    Ok(approved)
}

/// Reads a journal, treating absence as an empty journal rather than an approval.
///
/// # Errors
///
/// Rejects malformed journals and file errors.
pub fn read_decisions(path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    Ok(read_journal(path, clock)?.document)
}

/// Reads and validates the journal for typed replay.
pub(crate) fn read_journal(path: &Path, clock: &dyn Clock) -> Result<DecisionJournal, AppError> {
    let document = match read_bytes(path, clock)? {
        Some(bytes) => parse(&bytes)?,
        None => json!({"schemaVersion": 1, "decisions": []}),
    };
    validate_decisions(document, clock)
}

/// Selects the last recorded decision without changing journal history.
///
/// # Errors
///
/// Rejects malformed decision entries.
pub fn last_decision(document: &Value, id: &str) -> Result<Option<DecisionValue>, AppError> {
    for record in array(document, "decisions")?.iter().rev() {
        if required_text(record, "findingId")? == id {
            return decision_value(required_text(record, "decision")?, id).map(Some);
        }
    }
    Ok(None)
}

/// Requires the consolidated artifact used by the audit.
fn consolidated(document: &Value) -> Result<(), AppError> {
    if document.get("kind").and_then(Value::as_str) != Some("consolidated") {
        return Err(invalid("Audit requires a consolidated artifact"));
    }
    Ok(())
}

/// Reads a required JSON file with concurrent-reader semantics.
fn read_required(path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let bytes = read_bytes(path, clock)?
        .ok_or_else(|| invalid(format!("Missing file: {}", path.display())))?;
    parse(&bytes)
}

/// Parses an entire JSON record.
fn parse(bytes: &[u8]) -> Result<Value, AppError> {
    parse_json(bytes)
}

/// Prevents a record writer from replacing a source reached through an alias.
fn reject_same_file(output: &Path, input: &Path) -> Result<(), AppError> {
    if output == input
        || matches!((fs::canonicalize(output), fs::canonicalize(input)),
            (Ok(output), Ok(input)) if output == input)
    {
        return Err(invalid("Output must not overwrite an input"));
    }
    Ok(())
}

/// Publishes a complete JSON record while the caller retains its transaction.
fn write(transaction: &FileTransaction<'_>, value: &Value) -> Result<(), AppError> {
    transaction.write_bytes(&serde_json::to_vec_pretty(value)?)
}

/// Rejects missing or unsupported schema versions.
fn version(value: &Value) -> Result<(), AppError> {
    if value.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err(invalid("Unsupported schema version"));
    }
    Ok(())
}

/// Rejects unknown properties on records with a closed schema.
fn object_fields(value: &Value, allowed: &[&str]) -> Result<(), AppError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("Expected an object"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid("Unknown record field"));
    }
    Ok(())
}

/// Requires a nonempty string without modifying its contents.
fn required_text<'a>(value: &'a Value, key: &str) -> Result<&'a str, AppError> {
    let value = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("Missing or invalid {key}")))?;
    text(value, key)?;
    Ok(value)
}

/// Checks that a string carries content while preserving verbatim whitespace.
fn text(value: &str, name: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        return Err(invalid(format!("Empty {name}")));
    }
    Ok(())
}

/// Uses the runtime clock's timestamp type to reject invalid journal timestamps.
fn validate_timestamp<T: FromStr>(_: T, value: &str) -> Result<(), AppError>
where
    T::Err: Error + Send + Sync + 'static,
{
    let _: T = value
        .parse()
        .map_err(|error| caused_by("invalidInput", "Invalid recordedUtc timestamp", error))?;
    Ok(())
}

/// Requires an array instead of treating absent data as empty.
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, AppError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("Missing or invalid {key} array")))
}

/// Classifies rejected records as invalid input at the operation boundary.
fn invalid(message: impl Into<String>) -> AppError {
    fail("invalidInput", message.into())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::json;
    use ss_ai_runtime::{Clock, classification, fail};

    use super::{UnknownJournalDecision, validate_decisions};
    use crate::workflow_plan::DecisionValue;

    /// Supplies a deterministic clock for journal validation.
    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            Timestamp::UNIX_EPOCH
        }

        fn wait(&self, _: Duration) -> Result<(), AppError> {
            Err(fail("contention", "Unexpected wait"))
        }
    }

    #[test]
    fn replay_uses_last_validated_verdict_and_preserves_history() {
        let document = json!({"schemaVersion":1,"decisions":[
            {"findingId":"F-1","decision":"approved","recordedUtc":"2026-01-01T00:00:00Z"},
            {"findingId":"F-2","decision":"needs-audit","recordedUtc":"2026-01-01T00:00:00Z"},
            {"findingId":"F-1","decision":"rejected","recordedUtc":"2025-01-01T00:00:00Z"}
        ]});
        let journal = validate_decisions(document.clone(), &FixedClock).unwrap();
        assert_eq!(journal.last_decision("F-1"), Some(DecisionValue::Rejected));
        assert_eq!(
            journal.last_decision("F-2"),
            Some(DecisionValue::NeedsAudit)
        );
        assert_eq!(journal.last_decision("missing"), None);
        assert_eq!(journal.document, document);
    }

    #[test]
    fn overwritten_unknown_verdict_keeps_its_category_and_diagnostic() {
        let document = json!({"schemaVersion":1,"decisions":[
            {"findingId":"F-1","decision":"maybe","recordedUtc":"2026-01-01T00:00:00Z"},
            {"findingId":"F-1","decision":"approved","recordedUtc":"2026-01-01T00:00:00Z"}
        ]});
        let error = validate_decisions(document, &FixedClock).err().unwrap();
        assert!(error.find_source::<UnknownJournalDecision>().is_some());
        assert_eq!(classification(&error), "invalidInput");
        assert_eq!(
            error.to_string(),
            fail("invalidInput", "Unknown decision 'maybe' for finding 'F-1'").to_string()
        );
    }
}
