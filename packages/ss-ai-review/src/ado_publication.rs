//! ADO approval publication ends with one reviewer vote request.

use ohno::AppError;
use serde_json::json;

use crate::Clock;
use crate::ado_approval::{self, Evidence as AdoEvidence};
use crate::api::Api;
use crate::approval::{Decision, Evidence};
use crate::error::fail;
use crate::publication::{Diagnostic, Input, Outcome, Receipt, Scope};
use crate::target::{Platform, Target, escape};

/// Rechecks the captured authority before the single ADO vote request.
pub(crate) fn publish(
    api: &mut Api<'_>,
    target: &Target,
    clock: &dyn Clock,
    scope: &Scope,
    input: &Input<'_>,
) -> Result<Receipt, AppError> {
    if target.platform != Platform::AzureDevOps || target.pull_request_id != scope.pull_request_id {
        return Err(fail("invalidState", "ADO publication scope changed"));
    }
    if api.identity()?.id.as_deref() != Some(&scope.account_id) {
        return Err(fail("authentication", "ADO publication account changed"));
    }
    let fresh = Evidence::capture(api, target)?;
    let Evidence::AzureDevOps(evidence) = &fresh else {
        return Err(fail("scopeChanged", "ADO publication provider changed"));
    };
    evidence.validate_scope(&scope.repository_id, scope.pull_request_id)?;
    if api.identity()?.id.as_deref() != Some(&scope.account_id) {
        return Err(fail("authentication", "ADO publication account changed"));
    }
    let decision = fresh.decide(
        Platform::AzureDevOps,
        &scope.account_id,
        &scope.repository_id,
        scope.pull_request_id,
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
        let (outcome, diagnostic) = changed_evidence(&decision);
        result.outcome = outcome;
        result.error = Some(diagnostic);
        return Ok(result);
    }
    if !matches!(decision, Decision::AssessmentRequired { anchor, .. } if &anchor == input.anchor) {
        return Err(fail(
            "staleEvidence",
            "ADO publication approval baseline changed",
        ));
    }
    let AdoEvidence::History { source_commit, .. } = evidence else {
        return Err(fail(
            "invalidState",
            "ADO publication requires source history",
        ));
    };
    if source_commit != input.source_commit {
        return Err(fail("invalidState", "ADO publication source changed"));
    }
    let vote = ado_approval::current_vote(&fresh, &scope.account_id)?;
    if !matches!(vote, 5 | 10) {
        return Err(fail(
            "invalidState",
            "ADO publication requires a positive vote",
        ));
    }
    if let Some(deadline) = input.effect_deadline {
        deadline.check(clock)?;
    }
    // The successful response confirms the vote but provides no immutable event ID.
    api.transport.ado_approve_once(
        &format!(
            "{}/reviewers/{}",
            target.pr_api(),
            escape(&scope.account_id)
        ),
        &json!({"id": scope.account_id, "vote": vote, "isReapprove": true}),
    )?;
    Ok(result)
}

/// Reports an independently established current state without publishing a vote.
fn changed_evidence(decision: &Decision) -> (Outcome, Diagnostic) {
    let draft = matches!(decision, Decision::Ineligible { reason } if reason == "draft");
    let outcome = match decision {
        Decision::AlreadyValid { .. } => Outcome::AlreadyValid,
        Decision::Ineligible { .. } => Outcome::Ineligible,
        _ => Outcome::StaleDuringExecution,
    };
    (
        outcome,
        Diagnostic {
            kind: if draft { "draft" } else { "staleEvidence" }.to_owned(),
            message: "Publication inputs changed; no approval was written".to_owned(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::changed_evidence;
    use crate::approval::Decision;
    use crate::publication::Outcome;

    #[test]
    fn changed_draft_is_ineligible_with_a_draft_diagnostic() {
        let (outcome, diagnostic) = changed_evidence(&Decision::Ineligible {
            reason: "draft".to_owned(),
        });
        assert_eq!(outcome, Outcome::Ineligible);
        assert_eq!(diagnostic.kind, "draft");
    }

    #[test]
    fn changed_preflight_preserves_current_decision() {
        let cases = [
            (
                Decision::AlreadyValid {
                    approval_event_id: "vote".to_owned(),
                },
                Outcome::AlreadyValid,
            ),
            (
                Decision::Ineligible {
                    reason: "closed".to_owned(),
                },
                Outcome::Ineligible,
            ),
            (Decision::NoPriorApproval, Outcome::StaleDuringExecution),
        ];
        for (decision, expected) in cases {
            let (outcome, diagnostic) = changed_evidence(&decision);
            assert_eq!(outcome, expected);
            assert_eq!(diagnostic.kind, "staleEvidence");
        }
    }
}
