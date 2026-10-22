use ohno::AppError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::fail;
use crate::github_approval::commit;

/// Renewals retain the original external approval rather than creating a sliding baseline.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Anchor {
    pub(crate) review_id: String,
    pub(crate) source_commit: String,
}

/// A submitted review binds its audit data to the service's author and commit.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Audit {
    pub(crate) schema_version: u32,
    pub(crate) operation_id: String,
    pub(crate) repository_id: String,
    pub(crate) account_id: String,
    pub(crate) pull_request_id: u32,
    pub(crate) anchor: Anchor,
    pub(crate) source_commit: String,
    pub(crate) policy_digest: String,
    pub(crate) assessment_digest: String,
    pub(crate) state: State,
}

/// GitHub creates the approval and body together; this does not claim postflight verification.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum State {
    Submitted,
}

impl Audit {
    pub(crate) fn parse(body: &str) -> Result<Option<Self>, AppError> {
        let marker = "<!-- ss-autoreapprove";
        let Some((_, remainder)) = body.split_once(marker) else {
            return Ok(None);
        };
        let (document, suffix) = remainder
            .split_once("-->")
            .ok_or_else(|| fail("invalidState", "Unterminated renewal audit marker"))?;
        if suffix.contains(marker) {
            return Err(fail("invalidState", "Multiple renewal audit markers"));
        }
        let audit: Self = serde_json::from_str(document.trim())?;
        audit.validate()?;
        Ok(Some(audit))
    }

    pub(crate) fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != 1
            || Uuid::parse_str(&self.operation_id).is_err()
            || self.repository_id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.pull_request_id == 0
            || self.anchor.review_id.trim().is_empty()
        {
            return Err(fail("invalidState", "Invalid renewal audit identity"));
        }
        commit(&self.anchor.source_commit)?;
        commit(&self.source_commit)?;
        for digest in [&self.policy_digest, &self.assessment_digest] {
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(fail("invalidState", "Invalid renewal audit digest"));
            }
        }
        Ok(())
    }
}
