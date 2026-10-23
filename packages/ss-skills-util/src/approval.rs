use jiff::Timestamp;
use ohno::AppError;
use serde::{Deserialize, Serialize};

use crate::ado_approval;
use crate::api::Api;
use crate::error::fail;
use crate::github_approval;
use crate::renewal::Anchor;
use crate::review_notice::{self, Notice};
use crate::target::{Platform, Target};

/// Provider-tagged observations cannot be interpreted under another service's rules.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "platform", content = "observation", deny_unknown_fields)]
pub(crate) enum Evidence {
    #[serde(rename = "azureDevOps")]
    AzureDevOps(ado_approval::Evidence),
    #[serde(rename = "gitHub")]
    GitHub(github_approval::Evidence),
}

impl Evidence {
    pub(crate) fn capture(api: &mut Api<'_>, target: &Target) -> Result<Self, AppError> {
        match target.platform {
            Platform::AzureDevOps => ado_approval::capture(api, target).map(Self::AzureDevOps),
            Platform::GitHub => github_approval::capture(api, target).map(Self::GitHub),
        }
    }

    /// Recomputes the decision using the identities bound to the enclosing run.
    pub(crate) fn decide(
        &self,
        platform: Platform,
        account: &str,
        repository: &str,
        number: u32,
        observed_at: Timestamp,
    ) -> Result<Decision, AppError> {
        let decision = match (self, platform) {
            (Self::AzureDevOps(evidence), Platform::AzureDevOps) => {
                evidence.decide(account, repository, number, observed_at)
            }
            (Self::GitHub(evidence), Platform::GitHub) => {
                evidence.decide(account, repository, number, observed_at)
            }
            _ => Err(fail(
                "invalidState",
                "Approval evidence has another platform",
            )),
        }?;
        review_notice::apply(self, decision, account, repository, number, observed_at)
    }
}

/// Eligibility alone never authorizes publication without a complete assessment.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum Decision {
    AssessmentRequired {
        anchor: Anchor,
        #[serde(rename = "approvalEventId")]
        approval_event_id: String,
    },
    AlreadyValid {
        #[serde(rename = "approvalEventId")]
        approval_event_id: String,
    },
    NoPriorApproval,
    NeedsReview {
        notice: Notice,
    },
    Ineligible {
        reason: String,
    },
}

impl Decision {
    pub(crate) fn status(&self) -> &'static str {
        match self {
            Self::AssessmentRequired { .. } => "assessmentRequired",
            Self::AlreadyValid { .. } => "alreadyValid",
            Self::NoPriorApproval => "noPriorApproval",
            Self::NeedsReview { .. } => "needsReview",
            Self::Ineligible { .. } => "ineligible",
        }
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        match self {
            Self::Ineligible { reason } => Some(reason),
            Self::NeedsReview { notice } => Some(&notice.rationale),
            Self::AssessmentRequired { .. } | Self::AlreadyValid { .. } | Self::NoPriorApproval => {
                None
            }
        }
    }

    pub(crate) fn approval_event_id(&self) -> Option<&str> {
        match self {
            Self::AssessmentRequired {
                approval_event_id, ..
            }
            | Self::AlreadyValid { approval_event_id } => Some(approval_event_id),
            _ => None,
        }
    }
}
