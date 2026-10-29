//! Durable audit decisions and approval gates.

use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::read_capture;
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail, read_bytes};

use crate::findings::read_artifact;
use crate::workflow_plan::{self, DecisionValue, PlanItem, PlanKind, parse_json};

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
    let mut requests = Vec::new();
    for item in plan.items() {
        let PlanItem::Decision(decision) = item else {
            return Err(invalid("Expected a decision plan"));
        };
        requests.push(serde_json::to_value(decision)?);
    }
    if let Some(feedback_path) = audit_feedback_path {
        let [request] = requests.as_mut_slice() else {
            return Err(invalid(
                "Feedback requires one individual user decision without inline feedback",
            ));
        };
        if policy_classification || request.get("feedback").is_some() {
            return Err(invalid(
                "Feedback requires one individual user decision without inline feedback",
            ));
        }
        let feedback = get_feedback(feedback_path, required_text(request, "findingId")?, clock)?;
        if !feedback.trim().is_empty() {
            request
                .as_object_mut()
                .ok_or_else(|| invalid("Invalid decision"))?
                .insert("feedback".to_owned(), json!(feedback));
        }
    }
    text(recorded_utc, "recordedUtc")?;
    validate_timestamp(clock.now(), recorded_utc)?;
    let mut ids = HashSet::new();
    for request in &requests {
        validate_decision(request, false)?;
        let id = required_text(request, "findingId")?;
        if !ids.insert(id) {
            return Err(invalid("A decision batch repeats a finding"));
        }
        if policy_classification
            && (!matches!(
                required_text(request, "decision")?,
                "auto-approved" | "needs-audit"
            ) || request
                .get("feedback")
                .and_then(Value::as_str)
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
    validate_decisions(&document, clock)?;
    let decisions = document
        .get_mut("decisions")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("Invalid journal"))?;
    let mut recorded = Vec::new();
    for request in &requests {
        let mut record = request.clone();
        let object = record
            .as_object_mut()
            .ok_or_else(|| invalid("Invalid decision"))?;
        for field in ["reason", "feedback"] {
            if object
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|s| s.trim().is_empty())
            {
                object.remove(field);
            }
        }
        object.insert("recordedUtc".to_owned(), json!(recorded_utc));
        recorded.push(required_text(&record, "findingId")?.to_owned());
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
    let decisions = read_decisions(decisions_path, clock)?;
    let mut next = Value::Null;
    let mut remaining = 0_usize;
    let mut audit_ids = Vec::new();
    for finding in array(&artifact, "findings")? {
        let id = required_text(finding, "id")?;
        match last_decision(&decisions, id)? {
            None => {
                if next.is_null() {
                    next = finding.clone();
                }
                remaining = remaining.saturating_add(1);
                audit_ids.push(id);
            }
            Some("needs-audit") => audit_ids.push(id),
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
    let message = String::from_utf8(bytes)
        .map_err(|error| caused_by("invalidInput", "Feedback text is not UTF-8", error))?;
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

/// Validates the append-only decision journal before replay.
fn validate_decisions(document: &Value, clock: &dyn Clock) -> Result<(), AppError> {
    object_fields(document, &["schemaVersion", "decisions"])?;
    version(document)?;
    for record in array(document, "decisions")? {
        validate_decision(record, true)?;
        validate_timestamp(clock.now(), required_text(record, "recordedUtc")?)?;
    }
    Ok(())
}

/// Validates request and journal entries without coercing malformed JSON.
fn validate_decision(record: &Value, journal: bool) -> Result<(), AppError> {
    object_fields(
        record,
        &["findingId", "decision", "reason", "feedback", "recordedUtc"],
    )?;
    let finding_id = required_text(record, "findingId")?;
    let decision = required_text(record, "decision")?;
    if !matches!(
        decision,
        "approved" | "auto-approved" | "needs-audit" | "rejected" | "dropped"
    ) {
        return Err(invalid(format!(
            "Unknown decision '{decision}' for finding '{finding_id}'"
        )));
    }
    for name in ["reason", "feedback"] {
        if record.get(name).is_some_and(|v| !v.is_string()) {
            return Err(invalid("Invalid decision text"));
        }
    }
    if journal {
        required_text(record, "recordedUtc")?;
    } else if record.get("recordedUtc").is_some() {
        return Err(invalid("Timestamp belongs to the journal writer"));
    }
    Ok(())
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
    let document = match read_bytes(path, clock)? {
        Some(bytes) => parse(&bytes)?,
        None => json!({"schemaVersion": 1, "decisions": []}),
    };
    validate_decisions(&document, clock)?;
    Ok(document)
}

/// Selects the last recorded decision without changing journal history.
///
/// # Errors
///
/// Rejects malformed decision entries.
pub fn last_decision<'a>(document: &'a Value, id: &str) -> Result<Option<&'a str>, AppError> {
    for record in array(document, "decisions")?.iter().rev() {
        if required_text(record, "findingId")? == id {
            return Ok(Some(required_text(record, "decision")?));
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
