//! Composes the final review transaction with the provider-specific operations.

use std::any::type_name;
use std::fmt;
use std::path::Path;

use ohno::AppError;
use serde_json::{Value, from_value, json};
use ss_ai_runtime::FileTransaction;

use crate::error::fail;
use crate::final_review_transaction::{self, Effects};
use crate::identity::Identity;
use crate::target::{Platform, Target};
use crate::transport::Transport;
use crate::{
    Clock, Http, Process, ado_capture, final_review_ado, final_review_github, github_capture,
};

/// Validated local inputs for a final review publication.
#[expect(
    clippy::exhaustive_structs,
    reason = "This is an internal command composition interface"
)]
pub struct FinalReviewInput<'a> {
    /// Validated pull request coordinates.
    pub target: &'a Value,
    /// Ordered, rendered finding operations.
    pub plan: &'a [Value],
    /// Summary rendered from the approved findings.
    pub summary_body: &'a str,
    /// Hash of the audited capture.
    pub expected_evidence_hash: &'a str,
    /// Account whose authorship was established in the audited capture.
    pub expected_identity: &'a Value,
    /// Reviewed source object.
    pub source_commit: &'a str,
    /// Reviewed merge-base object.
    pub merge_base_commit: &'a str,
    /// Explicit authorization to remove an existing GitHub draft.
    pub discard_pending_review: bool,
    /// Explicit authorization to publish to a closed pull request.
    pub publish_closed: bool,
    /// Merge decision derived from a validated, commit-bound record.
    pub approve_merge: bool,
    /// Optional Azure OAuth tenant.
    pub tenant: Option<&'a str>,
}

impl fmt::Debug for FinalReviewInput<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(type_name::<Self>())
            .field("finding_count", &self.plan.len())
            .field("approve_merge", &self.approve_merge)
            .field("publish_closed", &self.publish_closed)
            .finish_non_exhaustive()
    }
}

/// Checks a prior result while retaining ownership of the publication lock.
///
/// # Errors
///
/// Rejects a prior result that requires reconciliation.
pub fn final_review_check_prior(
    journal: &mut FileTransaction<'_>,
    target: &Value,
) -> Result<(), AppError> {
    final_review_transaction::check_prior(journal, target)
}

/// Runs provider preflight and the final review transaction under the retained lock.
///
/// The caller supplies already validated and rendered local inputs. The hash
/// callback applies the same canonical capture rules as the audited capture.
///
/// # Errors
///
/// Rejects stale evidence, provider refusals, and durable journal failures.
pub fn publish_final_review(
    journal: &mut FileTransaction<'_>,
    result_path: &Path,
    input: &FinalReviewInput<'_>,
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
    capture_hash: impl Fn(&Value) -> Result<String, AppError>,
) -> Result<Value, AppError> {
    final_review_transaction::check_prior(journal, input.target)?;
    let target: Target = from_value(input.target.clone())?;
    let expected_identity: Identity = from_value(input.expected_identity.clone())?;
    expected_identity.validate()?;
    if expected_identity.platform != target.platform
        || (expected_identity.id.is_none()
            && expected_identity.descriptor.is_none()
            && expected_identity.unique_name.is_none())
    {
        return Err(fail(
            "invalidInput",
            "Audited identity cannot identify this provider account",
        ));
    }
    let mut transport = Transport::new(&target, input.tenant, process, http, clock);
    let state = match target.platform {
        Platform::GitHub => final_review_github::state(&mut transport, &target),
        Platform::AzureDevOps => final_review_ado::state(&mut transport, &target),
    }?;
    let active = state
        .get("isActive")
        .and_then(Value::as_bool)
        .ok_or_else(|| fail("service", "Publication state has no active status"))?;
    if !active && !input.publish_closed {
        return Err(fail(
            "invalidState",
            "Pull request is closed; explicit authorization is required",
        ));
    }
    if !state
        .get("sourceCommit")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(input.source_commit))
        || active
            && !state
                .get("mergeBaseCommit")
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case(input.merge_base_commit))
    {
        return Err(fail(
            "invalidState",
            "Pull request revisions changed before publication",
        ));
    }

    let paths = input
        .plan
        .iter()
        .filter_map(|operation| {
            operation
                .get("anchor")
                .and_then(|anchor| anchor.get("path"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let identities = match target.platform {
        Platform::GitHub => final_review_github::file_identity(&transport, &target, &state, &paths),
        Platform::AzureDevOps => {
            final_review_ado::file_identity(&mut transport, &target, &state, &paths)
        }
    }?;
    let mut plan = input.plan.to_vec();
    for operation in &mut plan {
        if let Some(path) = operation
            .get("anchor")
            .and_then(|anchor| anchor.get("path"))
            .and_then(Value::as_str)
            && let Some(identity) = identities.get(path)
        {
            operation
                .as_object_mut()
                .ok_or_else(|| fail("invalidInput", "Invalid publication operation"))?
                .insert("changeTrackingId".to_owned(), identity.clone());
        }
    }
    final_review_transaction::preflight(input.target, &state, &plan)?;

    let live = match target.platform {
        Platform::GitHub => github_capture::collect(&mut transport, &target),
        Platform::AzureDevOps => ado_capture::collect(&mut transport, &target),
    }?;
    if capture_hash(&live)? != input.expected_evidence_hash {
        return Err(fail(
            "invalidState",
            "Pull request evidence changed before publication",
        ));
    }
    let actual_identity: Identity = from_value(
        live.get("authenticatedIdentity")
            .cloned()
            .ok_or_else(|| fail("service", "Capture identity is missing"))?,
    )?;
    if !expected_identity.same_account(&actual_identity) {
        return Err(fail(
            "authentication",
            "Publication account differs from the audited capture",
        ));
    }
    let state_identity = state
        .get("identity")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if let Some(state_identity) = state_identity {
        if actual_identity
            .unique_name
            .as_deref()
            .is_none_or(|value| !state_identity.eq_ignore_ascii_case(value))
        {
            return Err(fail(
                "authentication",
                "Publication identity changed during capture",
            ));
        }
    } else if target.platform == Platform::GitHub {
        return Err(fail("service", "GitHub publication identity is missing"));
    }

    if let Some(draft) = state
        .get("pendingReviewId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        if !input.discard_pending_review {
            return Err(fail(
                "invalidState",
                "An existing draft requires explicit discard authorization",
            ));
        }
        final_review_github::discard(&transport, &target, &json!({"reviewId": draft}))?;
    }

    let mut effects = ProviderEffects {
        transport,
        target,
        state: state.clone(),
        review: None,
        approval: None,
    };
    final_review_transaction::execute(
        journal,
        result_path,
        &mut effects,
        input.target,
        &state,
        &plan,
        input.summary_body,
        input.approve_merge,
        clock,
    )
}

/// Holds provider state for the lifetime of one journaled review.
struct ProviderEffects<'a> {
    transport: Transport<'a>,
    target: Target,
    state: Value,
    review: Option<Value>,
    approval: Option<Value>,
}

impl ProviderEffects<'_> {
    /// Requires a review receipt before subsequent provider operations.
    fn review(&self) -> Result<&Value, AppError> {
        self.review
            .as_ref()
            .ok_or_else(|| fail("invalidState", "Review has not started"))
    }
}

impl Effects for ProviderEffects<'_> {
    fn start(&mut self) -> Result<Value, AppError> {
        let review = match self.target.platform {
            Platform::GitHub => {
                final_review_github::start(&self.transport, &self.target, &self.state)
            }
            Platform::AzureDevOps => {
                final_review_ado::start(&mut self.transport, &self.target, &self.state)
            }
        }?;
        self.review = Some(review.clone());
        Ok(review)
    }

    fn add(&mut self, operation: &Value) -> Result<Value, AppError> {
        let review = self
            .review
            .as_mut()
            .ok_or_else(|| fail("invalidState", "Review has not started"))?;
        match self.target.platform {
            Platform::GitHub => {
                final_review_github::add(&self.transport, &self.target, review, operation)
            }
            Platform::AzureDevOps => {
                final_review_ado::add(&mut self.transport, &self.target, review, operation)
            }
        }
    }

    fn verify_findings(&mut self) -> Result<Value, AppError> {
        let review = self.review()?.clone();
        match self.target.platform {
            Platform::GitHub => {
                final_review_github::verify_findings(&self.transport, &self.target, &review)
            }
            Platform::AzureDevOps => {
                final_review_ado::verify_findings(&mut self.transport, &self.target, &review)
            }
        }
    }

    fn approve(&mut self) -> Result<Value, AppError> {
        match self.target.platform {
            Platform::AzureDevOps => {
                let receipt = final_review_ado::approve(&mut self.transport, &self.target)?;
                self.approval = Some(receipt.clone());
                Ok(receipt)
            }
            Platform::GitHub => Err(fail(
                "invalidState",
                "GitHub approval is part of review submission",
            )),
        }
    }

    fn complete(&mut self, summary: &str, approve: bool) -> Result<Value, AppError> {
        let review = self
            .review
            .as_mut()
            .ok_or_else(|| fail("invalidState", "Review has not started"))?;
        match self.target.platform {
            Platform::GitHub => final_review_github::complete(
                &self.transport,
                &self.target,
                review,
                summary,
                approve,
            ),
            Platform::AzureDevOps => final_review_ado::complete(
                &mut self.transport,
                &self.target,
                review,
                summary,
                approve,
            ),
        }
    }

    fn verify(&mut self) -> Result<Value, AppError> {
        let review = self.review()?.clone();
        match self.target.platform {
            Platform::GitHub => final_review_github::verify(&self.transport, &self.target, &review),
            Platform::AzureDevOps => {
                let mut verification =
                    final_review_ado::verify(&mut self.transport, &self.target, &review)?;
                if let Some(receipt) = &self.approval {
                    let approval = final_review_ado::verify_approval(
                        &mut self.transport,
                        &self.target,
                        receipt,
                    )?;
                    verification
                        .as_object_mut()
                        .ok_or_else(|| fail("service", "Invalid review verification"))?
                        .insert("approval".to_owned(), approval);
                }
                Ok(verification)
            }
        }
    }

    fn reopen(&mut self, thread_id: &str) -> Result<(), AppError> {
        match self.target.platform {
            Platform::GitHub => {
                final_review_github::reopen(&self.transport, &self.target, thread_id)
            }
            Platform::AzureDevOps => {
                final_review_ado::reopen(&mut self.transport, &self.target, thread_id)
            }
        }
    }

    fn discard(&mut self) -> Result<(), AppError> {
        let review = self.review()?.clone();
        match self.target.platform {
            Platform::GitHub => {
                final_review_github::discard(&self.transport, &self.target, &review)
            }
            Platform::AzureDevOps => {
                final_review_ado::discard(&mut self.transport, &self.target, &review)
            }
        }
    }
}
