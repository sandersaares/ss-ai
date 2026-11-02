use std::env;
use std::path::PathBuf;

use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::Clock;
use crate::ado_publication;
use crate::api::Api;
use crate::approval::{Decision, Evidence};
use crate::error::fail;
use crate::github_approval::Evidence as GitHubEvidence;
use crate::json::{field, identifier};
use crate::renewal::Anchor;
use crate::target::{Platform, Target};

/// Stable scope binds an assessment to its provider account and pull request.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(
    clippy::struct_field_names,
    reason = "Persisted scope distinguishes stable IDs from names and URLs"
)]
pub(crate) struct Scope {
    pub(crate) repository_id: String,
    pub(crate) account_id: String,
    pub(crate) pull_request_id: u32,
}

/// A completed assessment supplies the evidence and source to check before publication.
pub(crate) struct Input<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) assessment_digest: &'a str,
    pub(crate) policy_digest: &'a str,
    pub(crate) evidence: &'a Evidence,
    pub(crate) anchor: &'a Anchor,
    pub(crate) source_commit: &'a str,
    pub(crate) attribution: &'a str,
    pub(crate) explanation: &'a str,
    pub(crate) effect_deadline: Option<crate::assessment_budget::EffectDeadline>,
}

/// The run records its outcome without maintaining a separate approval ledger.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Receipt {
    pub(crate) run_id: String,
    pub(crate) scope: Scope,
    pub(crate) assessment_digest: String,
    pub(crate) outcome: Outcome,
    pub(crate) error: Option<Diagnostic>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Outcome {
    Reapproved,
    AlreadyValid,
    Ineligible,
    StaleDuringExecution,
    Failed,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Reapproved => "reapproved",
            Self::AlreadyValid => "alreadyValid",
            Self::Ineligible => "ineligible",
            Self::StaleDuringExecution => "staleDuringExecution",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Diagnostic {
    pub(crate) kind: String,
    pub(crate) message: String,
}

impl Receipt {
    pub(crate) fn validate(&self, _platform: Platform) -> Result<(), AppError> {
        if Uuid::parse_str(&self.run_id).is_err()
            || self.scope.repository_id.is_empty()
            || self.scope.account_id.is_empty()
            || self.scope.pull_request_id == 0
            || self.assessment_digest.len() != 64
            || !self
                .assessment_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.outcome == Outcome::Reapproved && self.error.is_some()
            || matches!(
                self.outcome,
                Outcome::Failed | Outcome::StaleDuringExecution
            ) && self.error.is_none()
            || self.error.as_ref().is_some_and(|error| {
                error.kind.trim().is_empty() || error.message.trim().is_empty()
            })
        {
            return Err(fail("invalidState", "Invalid publication result"));
        }
        Ok(())
    }
}

/// Dispatches one provider publication after the assessment has completed.
pub(crate) fn publish(
    api: &mut Api<'_>,
    target: &Target,
    clock: &dyn Clock,
    scope: &Scope,
    input: &Input<'_>,
) -> Result<Receipt, AppError> {
    match target.platform {
        Platform::GitHub => publish_github(api, target, clock, scope, input),
        Platform::AzureDevOps => ado_publication::publish(api, target, clock, scope, input),
    }
}

/// Checks current provider evidence before issuing the single approval request.
fn publish_github(
    api: &mut Api<'_>,
    target: &Target,
    clock: &dyn Clock,
    scope: &Scope,
    input: &Input<'_>,
) -> Result<Receipt, AppError> {
    if target.platform != Platform::GitHub || target.pull_request_id != scope.pull_request_id {
        return Err(fail("invalidState", "GitHub publication scope changed"));
    }
    if api.identity()?.id.as_deref() != Some(&scope.account_id) {
        return Err(fail("authentication", "Publication account changed"));
    }
    let fresh = Evidence::capture(api, target)?;
    if let Evidence::GitHub(
        GitHubEvidence::State { pull_request } | GitHubEvidence::History { pull_request, .. },
    ) = &fresh
        && identifier(field(field(field(pull_request, "base")?, "repo")?, "id")?)?
            != scope.repository_id
    {
        return Err(fail(
            "scopeChanged",
            "Publication repository identity changed",
        ));
    }
    if api.identity()?.id.as_deref() != Some(&scope.account_id) {
        return Err(fail(
            "authentication",
            "Publication account changed during evidence capture",
        ));
    }
    let decision = fresh.decide(
        Platform::GitHub,
        &scope.account_id,
        &scope.repository_id,
        target.pull_request_id,
        clock.now(),
    )?;
    let mut result = Receipt {
        run_id: input.run_id.to_owned(),
        scope: scope.clone(),
        assessment_digest: input.assessment_digest.to_owned(),
        outcome: Outcome::Reapproved,
        error: None,
    };
    if !fresh.equivalent(input.evidence)? {
        let draft = matches!(&decision, Decision::Ineligible { reason } if reason == "draft");
        result.outcome = match decision {
            Decision::AlreadyValid { .. } => Outcome::AlreadyValid,
            Decision::Ineligible { .. } => Outcome::Ineligible,
            _ => Outcome::StaleDuringExecution,
        };
        result.error = Some(Diagnostic {
            kind: if draft { "draft" } else { "staleEvidence" }.to_owned(),
            message: "Publication inputs changed; no approval was written".to_owned(),
        });
        return Ok(result);
    }
    if !matches!(decision, Decision::AssessmentRequired { anchor, .. } if &anchor == input.anchor) {
        return Err(fail(
            "staleEvidence",
            "Publication approval baseline changed",
        ));
    }
    if let Some(deadline) = input.effect_deadline {
        deadline.check(clock)?;
    }
    let body = format!("{}\n\n{}", input.attribution, input.explanation);
    api.transport.github_approve_once(
        &format!("{}/reviews", target.pr_api()),
        &json!({"event": "APPROVE", "commit_id": input.source_commit, "body": body}),
    )?;
    Ok(result)
}

/// Resolves the host directory used for review notices.
pub(crate) fn data_directory() -> Result<PathBuf, AppError> {
    let name = if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "HOME"
    };
    let root = PathBuf::from(env::var_os(name).ok_or_else(|| {
        fail(
            "io",
            format!("{name} is required for review notice records"),
        )
    })?);
    Ok(if cfg!(windows) {
        root.join("ss-ai")
    } else {
        root.join(".local").join("share").join("ss-ai")
    })
}

pub(crate) fn hash(value: &impl Serialize) -> Result<String, AppError> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
