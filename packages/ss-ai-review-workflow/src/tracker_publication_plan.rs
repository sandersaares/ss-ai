//! Prepares tracker items without contacting a publication provider.

use std::path::Path;

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_context::read_capture;
use ss_ai_runtime::{Clock, fail, read_bytes};

use crate::approved::{check_approved_value, identifier_matches};
use crate::markdown::render;
use crate::review_publication_plan::read_approved;
use crate::workflow_plan::parse_json;

/// Reads and prepares a complete tracker publication from local files.
///
/// `platform` names the tracker provider, independently of the capture's
/// provider. The result contains ordered `items` and `summaryBody`; no
/// provider effects or output files are produced.
///
/// # Errors
///
/// Rejects missing or malformed inputs, stale evidence, and findings that
/// cannot be rendered or linked to their captured discussion.
pub fn prepare(
    approved_path: &Path,
    diagnostics_path: &Path,
    context_path: Option<&Path>,
    platform: &str,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let approved = read_approved(approved_path, clock)?;
    let context = context_path
        .map(|path| read_capture(path, clock))
        .transpose()?;
    let follow_up_count = validate_evidence(&approved, context.as_ref())?;
    let diagnostics = read_bytes(diagnostics_path, clock)?
        .ok_or_else(|| invalid("Publication diagnostics are missing"))?;
    let diagnostics = parse_json(&diagnostics)?;
    render_plan(
        &approved,
        context.as_ref(),
        &diagnostics,
        platform,
        follow_up_count,
    )
}

/// Checks evidence and follow-ups before reading unrelated diagnostics.
fn validate_evidence(approved: &Value, context: Option<&Value>) -> Result<usize, AppError> {
    check_approved_value(approved, context)?;
    let findings = approved
        .get("findings")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Missing findings"))?;
    if findings.is_empty() {
        return Err(invalid("An empty review has no tracker items"));
    }

    let mut follow_up_count = 0_usize;
    for finding in findings {
        if let Some(follow_up) = finding.get("followUp").filter(|value| !value.is_null()) {
            check_follow_up_target(context, follow_up)?;
            follow_up_count = follow_up_count.saturating_add(1);
        }
    }
    Ok(follow_up_count)
}

/// Renders the checked snapshots before returning any publishable content.
fn render_plan(
    approved: &Value,
    context: Option<&Value>,
    diagnostics: &Value,
    platform: &str,
    follow_up_count: usize,
) -> Result<Value, AppError> {
    if !diagnostics.is_object() {
        return Err(invalid("Publication diagnostics must be an object"));
    }
    let findings = approved
        .get("findings")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Missing findings"))?;
    // Tracker creation follows the same stable presentation order as the audit.
    let ordered =
        findings
            .iter()
            .filter(|finding| finding.get("origin").and_then(Value::as_str) == Some("synthesis"))
            .chain(findings.iter().filter(|finding| {
                finding.get("origin").and_then(Value::as_str) != Some("synthesis")
            }));
    let mut items = Vec::with_capacity(findings.len());
    // The capture is identical for every item, so copy it only once.
    let mut request = json!({"operation": "trackerItem", "finding": null, "context": context});
    for finding in ordered {
        request
            .get_mut("finding")
            .ok_or_else(|| invalid("Missing finding in tracker item request"))?
            .clone_from(finding);
        let item = render(&request).map_err(invalid)?;
        for field in ["findingId", "title", "body"] {
            if item
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(|text| text.trim().is_empty())
            {
                return Err(invalid(format!("Rendered tracker item has no {field}")));
            }
        }
        items.push(item);
    }
    let summary = render(&json!({
        "operation": "summary", "findings": findings, "platform": platform,
        "followUpCount": follow_up_count, "diagnostics": diagnostics
    }))
    .map_err(invalid)?;
    Ok(json!({"items": items, "summaryBody": summary}))
}

/// Tracker follow-ups only require a captured comment, not a reply-capable thread.
fn check_follow_up_target(context: Option<&Value>, follow_up: &Value) -> Result<(), AppError> {
    let thread_id = text(follow_up, "threadId")?;
    let comment_id = text(follow_up, "commentId")?;
    let context = context.ok_or_else(|| invalid("Follow-up requires a captured discussion"))?;
    let thread = captured_list(context.get("threads"))
        .iter()
        .find(|thread| identifier_matches(thread.get("id"), thread_id))
        .ok_or_else(|| invalid(format!("Follow-up thread '{thread_id}' is absent")))?;
    if !captured_list(thread.get("comments"))
        .iter()
        .any(|comment| identifier_matches(comment.get("id"), comment_id))
    {
        return Err(invalid(format!(
            "Follow-up comment '{comment_id}' is absent from thread '{thread_id}'"
        )));
    }
    Ok(())
}

/// Capture arrays can contain one object in the PowerShell representation.
fn captured_list(value: Option<&Value>) -> &[Value] {
    match value {
        Some(Value::Array(values)) => values,
        Some(value @ Value::Object(_)) => std::slice::from_ref(value),
        _ => &[],
    }
}

/// Reads a nonblank follow-up identifier.
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, AppError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| invalid(format!("Missing or invalid {field}")))
}

/// Classifies invalid local publication inputs.
fn invalid(message: impl Into<String>) -> AppError {
    fail("invalidInput", message.into())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::indexing_slicing,
    reason = "The fixtures use fixed record shapes"
)]
mod tests {
    use serde_json::{Value, json};

    use super::{render, render_plan};

    #[test]
    fn reused_request_preserves_rendered_items_and_summary() {
        let findings = json!([
            {"id":"F1", "origin":"reviewer", "title":"First", "description":"Problem",
             "reasoning":"Reason", "consequence":"Impact", "recommendedAction":"Fix",
             "anchor":{"kind":"file","path":"src/a.rs"}},
            {"id":"F2", "origin":"synthesis", "title":"Second", "description":"Problem",
             "reasoning":"Reason", "consequence":"Impact", "recommendedAction":"Fix",
             "anchor":{"kind":"file","path":"src/b.rs"}}
        ]);
        let approved = json!({"findings": findings});
        let context = json!({"target":{"platform":"gitHub"}, "threads":[]});
        let diagnostics = json!({});
        for context in [None, Some(&context)] {
            let result = render_plan(&approved, context, &diagnostics, "gitHub", 0).unwrap();
            let findings = approved["findings"].as_array().unwrap();
            let expected: Vec<Value> = [1, 0]
                .into_iter()
                .map(|index| {
                    render(
                        &json!({"operation":"trackerItem", "finding":findings[index],
                        "context":context}),
                    )
                    .unwrap()
                })
                .collect();
            assert_eq!(result["items"], json!(expected));
            assert_eq!(
                result["summaryBody"],
                render(&json!({"operation":"summary", "findings":findings,
                    "platform":"gitHub", "followUpCount":0, "diagnostics":diagnostics}))
                .unwrap()
            );
        }
    }
}
