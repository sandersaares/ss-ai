//! Publishes and verifies inline Azure DevOps overview comments.

use std::num::NonZeroU32;

use ohno::AppError;
use serde_json::{Value, json};
use url::Url;

use crate::error::fail;
use crate::json::{array, field, text};
use crate::target::{Platform, Target};
use crate::transport::Transport;

/// Caps untrusted routing components while allowing long organization names.
const MAX_ROUTE_LENGTH: usize = 4096;

/// Checks whether publication still targets an active pull request.
pub(crate) fn is_open(transport: &mut Transport<'_>, target: &Target) -> Result<bool, AppError> {
    let path = route(transport, target)?;
    let response = transport.request(&path, "GET", None)?;
    let status = text(field(&response, "status")?)?;
    match status {
        "active" => Ok(true),
        "abandoned" | "completed" => Ok(false),
        _ => Err(fail("service", "Unexpected Azure pull request status")),
    }
}

/// Creates one resolved, unanchored informational thread.
pub(crate) fn comment(
    transport: &mut Transport<'_>,
    target: &Target,
    payload: &Value,
) -> Result<Value, AppError> {
    let path = route(transport, target)?;
    let body = text(field(payload, "body")?)?;
    if body.is_empty() {
        return Err(fail("invalidInput", "Overview comment body is empty"));
    }
    let response = transport.ado_write_with_rate_limit_retries(
        &format!("{path}/threads"),
        "POST",
        &json!({
            "status": "fixed",
            "comments": [{ "parentCommentId": 0, "commentType": "text", "content": body }],
        }),
    )?;
    let id = field(&response, "id")
        .and_then(thread_id)
        .map_err(|error| {
            crate::error::caused_by(
                "ambiguousWrite",
                "Azure thread creation returned an invalid ID",
                error.into_std_error(),
            )
        })?;
    Ok(json!({ "id": id.get(), "url": thread_url(target, id)? }))
}

/// Confirms the complete root comment and resolved status without modifying it.
pub(crate) fn verify_comment(
    transport: &mut Transport<'_>,
    target: &Target,
    payload: &Value,
) -> Result<(), AppError> {
    let path = route(transport, target)?;
    let body = text(field(payload, "body")?)?;
    let id = thread_id(field(field(payload, "comment")?, "id")?)?;
    let response = transport.request(&format!("{path}/threads/{id}"), "GET", None)?;
    let comments = array(field(&response, "comments")?)?;
    let first = comments
        .first()
        .ok_or_else(|| fail("service", "Azure overview thread has no root comment"))?;
    if text(field(first, "content")?)? != body {
        return Err(fail(
            "service",
            "Azure overview comment body does not match",
        ));
    }
    let status = field(&response, "status")?;
    if status.as_str() != Some("fixed") && status.as_u64() != Some(2) {
        return Err(fail(
            "service",
            "Azure overview thread is not resolved; do not repost it",
        ));
    }
    Ok(())
}

/// Restricts requests and constructed links to the resolved collection and PR.
fn route(transport: &Transport<'_>, target: &Target) -> Result<String, AppError> {
    if transport.platform != Platform::AzureDevOps
        || target.platform != Platform::AzureDevOps
        || target.pull_request_id == 0
        || target
            .project
            .as_deref()
            .is_none_or(|s| s.trim().is_empty() || matches!(s, "." | ".."))
        || target.repository.trim().is_empty()
        || matches!(target.repository.as_str(), "." | "..")
        || target.pr_api().len() > MAX_ROUTE_LENGTH
    {
        return Err(fail(
            "invalidInput",
            "A resolved Azure pull request is required",
        ));
    }
    let target_collection = collection(target)?;
    if transport
        .ado_collection()
        .map(|url| url.trim_end_matches('/'))
        != Some(target_collection)
    {
        return Err(fail(
            "invalidInput",
            "Azure transport and pull request collections differ",
        ));
    }
    Ok(target.pr_api())
}

/// Restricts links to an authenticated HTTPS collection without decorations.
fn collection(target: &Target) -> Result<&str, AppError> {
    let collection = target
        .collection_url
        .as_deref()
        .ok_or_else(|| fail("invalidInput", "Missing Azure collection URL"))?;
    let url = Url::parse(collection).map_err(|error| {
        crate::error::caused_by("invalidInput", "Invalid Azure collection URL", error)
    })?;
    if collection.len() > MAX_ROUTE_LENGTH
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str().trim_end_matches('/') != collection.trim_end_matches('/')
    {
        return Err(fail("invalidInput", "Invalid Azure collection URL"));
    }
    Ok(collection.trim_end_matches('/'))
}

/// Restricts thread identifiers to the positive integer range accepted by ADO.
fn thread_id(value: &Value) -> Result<NonZeroU32, AppError> {
    let parsed = value
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .or_else(|| {
            let id = value.as_str()?;
            if id.is_empty() || id.len() > 10 || !id.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            id.parse::<u32>().ok()
        });
    parsed
        .and_then(NonZeroU32::new)
        .ok_or_else(|| fail("service", "Invalid Azure thread identifier"))
}

/// Builds the browser link from validated coordinates rather than service data.
fn thread_url(target: &Target, id: NonZeroU32) -> Result<String, AppError> {
    collection(target)?;
    let mut canonical = target.clone();
    canonical.refresh_url();
    Ok(format!("{}?discussionId={id}", canonical.web_url))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use serde_json::json;

    use super::thread_id;

    #[test]
    fn thread_ids_reject_zero_negative_and_unbounded_values() {
        assert_eq!(thread_id(&json!(u32::MAX)).unwrap().get(), u32::MAX);
        thread_id(&json!(0)).unwrap_err();
        thread_id(&json!(-1)).unwrap_err();
        thread_id(&json!("4294967296")).unwrap_err();
    }
}
