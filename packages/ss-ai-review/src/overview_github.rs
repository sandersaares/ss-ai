//! Publishes and verifies inline GitHub overview comments.

use std::num::NonZeroU64;

use ohno::AppError;
use serde_json::{Value, json};

use crate::error::fail;
use crate::json::{field, text};
use crate::target::{Platform, Target};
use crate::transport::Transport;

/// Restricts provider operations to concrete GitHub pull requests.
fn check_target(transport: &Transport<'_>, target: &Target) -> Result<(), AppError> {
    if transport.platform != Platform::GitHub
        || target.platform != Platform::GitHub
        || target.pull_request_id == 0
        || target.organization.trim().is_empty()
        || target.repository.trim().is_empty()
    {
        return Err(fail("invalidInput", "A GitHub pull request is required"));
    }
    Ok(())
}

/// Checks current PR state immediately before a publication effect.
pub(crate) fn is_open(transport: &mut Transport<'_>, target: &Target) -> Result<bool, AppError> {
    check_target(transport, target)?;
    let response = transport.request(&target.pr_api(), "GET", None)?;
    match text(field(&response, "state")?)? {
        "open" => Ok(true),
        "closed" => Ok(false),
        _ => Err(fail("service", "Unexpected GitHub pull request state")),
    }
}

/// Creates one PR conversation comment without replaying an uncertain write.
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "The provider interface consistently accepts exclusive transport access"
)]
pub(crate) fn comment(
    transport: &mut Transport<'_>,
    target: &Target,
    payload: &Value,
) -> Result<Value, AppError> {
    check_target(transport, target)?;
    let body = text(field(payload, "body")?)?;
    let response = transport.github_write_retrying_refusals(
        &format!(
            "{}/issues/{}/comments",
            target.repository_api(),
            target.pull_request_id
        ),
        &json!({"body": body}),
    )?;
    let id = comment_id(response.get("id")).ok_or_else(|| {
        fail(
            "ambiguousWrite",
            "Comment creation returned no usable identifier",
        )
    })?;
    Ok(json!({"id":id.get(), "url":bounded_url(response.get("html_url"))}))
}

/// Requires the complete posted body to match the intended comment exactly.
pub(crate) fn verify_comment(
    transport: &mut Transport<'_>,
    target: &Target,
    payload: &Value,
) -> Result<(), AppError> {
    check_target(transport, target)?;
    let body = text(field(payload, "body")?)?;
    let id = comment_id(field(payload, "comment")?.get("id"))
        .ok_or_else(|| fail("invalidState", "Invalid comment identifier"))?;
    let comment = transport.request(
        &format!("{}/issues/comments/{id}", target.repository_api()),
        "GET",
        None,
    )?;
    if comment_id(comment.get("id")) != Some(id) || text(field(&comment, "body")?)? != body {
        return Err(fail("service", "GitHub comment read-back does not match"));
    }
    Ok(())
}

/// Restricts comment database IDs to bounded numeric route segments.
fn comment_id(value: Option<&Value>) -> Option<NonZeroU64> {
    let value = value?;
    let id = value.as_u64().or_else(|| value.as_str()?.parse().ok())?;
    NonZeroU64::new(id)
}

/// Bounds ancillary response URLs before persistence.
fn bounded_url(value: Option<&Value>) -> Option<&str> {
    value?
        .as_str()
        .filter(|url| !url.is_empty() && url.len() <= 4096)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use serde_json::json;

    use super::comment_id;

    #[test]
    fn comment_ids_preserve_positive_numeric_routes() {
        for value in [json!(1), json!("1"), json!(u64::MAX)] {
            assert_ne!(comment_id(Some(&value)).unwrap().get(), 0);
        }
        for value in [
            json!(0),
            json!("0"),
            json!(-1),
            json!("18446744073709551616"),
        ] {
            assert!(comment_id(Some(&value)).is_none());
        }
    }
}
