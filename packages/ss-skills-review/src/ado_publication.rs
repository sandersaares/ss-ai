//! ADO's mutable vote and separate audit require independently journaled effects.
//!
//! Unlike GitHub, the documented reviewer API has no commit precondition or
//! client operation ID. A unique new structured vote event, complete history
//! fences, and an authenticated audit join provide conservative reconciliation;
//! an unidentifiable effect is never replayed. See docs/reapproval.md.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::Clock;
use crate::ado_approval::{self, Evidence as AdoEvidence};
use crate::api::Api;
use crate::approval::{Decision, Evidence};
use crate::clock::timestamp;
use crate::error::{classification, fail};
use crate::journal::FileJournal;
use crate::json::{field, identifier, required_optional};
use crate::publication::{Diagnostic, Input, Outcome, Receipt, Scope, hash};
use crate::renewal::{Audit, State as AuditState};
use crate::review_notice::{self, AutomaticApproval};
use crate::target::{Platform, Target, escape};

/// Retains the same stable host lock until the enclosing run has persisted.
pub(crate) struct Store<'a> {
    journal: FileJournal<'a>,
    path: PathBuf,
    record: Record,
}

/// Provider-specific schema never reinterprets an existing GitHub host journal.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    schema_version: u32,
    scope: Scope,
    operations: Vec<Operation>,
}

/// Each effect can be reconciled independently, including a lost audit reply.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Operation {
    run_id: String,
    audit: Audit,
    attribution: String,
    explanation: String,
    captured_at: String,
    snapshot: Evidence,
    vote: i64,
    vote_state: EffectState,
    audit_state: EffectState,
    #[serde(deserialize_with = "required_optional")]
    vote_acknowledgement: Option<Value>,
    #[serde(deserialize_with = "required_optional")]
    vote_event_id: Option<String>,
    #[serde(deserialize_with = "required_optional")]
    audit_acknowledgement: Option<Value>,
    #[serde(deserialize_with = "required_optional")]
    verification: Option<Verification>,
    #[serde(deserialize_with = "required_optional")]
    result: Option<Receipt>,
}

/// Only a definitive refusal permits another attempt, never intent or uncertainty.
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum EffectState {
    Prepared,
    Intent,
    Acknowledged,
    Verified,
    Refused,
    Unknown,
}

/// Readback survives a host-save/run-save interruption without another request.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Verification {
    observed_at: String,
    evidence: Evidence,
}

impl Operation {
    fn vote_payload(&self) -> Value {
        // Preserve approval-with-suggestions rather than silently strengthening it.
        // isReapprove asks ADO to record approval on the current source iteration
        // even when the retained vote value is unchanged.
        json!({"id": self.audit.account_id, "vote": self.vote, "isReapprove": true})
    }

    fn marker(&self) -> Result<ado_approval::Renewal, AppError> {
        Ok(ado_approval::Renewal {
            audit: self.audit.clone(),
            vote_event_id: self
                .vote_event_id
                .clone()
                .ok_or_else(|| fail("invalidState", "ADO audit lacks its verified vote event"))?,
            iteration_id: ado_approval::source_iteration(&self.snapshot)?.0,
        })
    }

    fn body(&self) -> Result<String, AppError> {
        Ok(format!(
            "{}\n\nAutomatically renewed approval for source {} against original approval {} ({}).\n\n{}\n\n<!-- ss-autoreapprove-ado {} -->",
            self.attribution,
            self.audit.source_commit,
            self.audit.anchor.review_id,
            self.audit.anchor.source_commit,
            self.explanation,
            serde_json::to_string(&self.marker()?)?
        ))
    }

    fn audit_payload(&self) -> Result<Value, AppError> {
        // A standalone closed audit does not create an unresolved discussion or
        // change any existing human thread's status.
        Ok(
            json!({"comments": [{"parentCommentId": 0, "commentType": 1, "content": self.body()?}], "status": 4}),
        )
    }

    fn unresolved(&self) -> bool {
        self.vote_state != EffectState::Prepared
            && self.vote_state != EffectState::Refused
            && self.audit_state != EffectState::Verified
    }

    /// Reloads enforce the same scope, authority and lifecycle as producers.
    fn validate(&self, scope: &Scope, path: &Path) -> Result<(), AppError> {
        self.audit.validate()?;
        let at = timestamp(&self.captured_at)?;
        if Uuid::parse_str(&self.run_id).is_err()
            || self.attribution.trim().is_empty()
            || self.explanation.trim().is_empty()
            // Prose must not introduce another reserved marker after the vote
            // has already been submitted and before the audit can be recognized.
            || self.attribution.contains("<!-- ss-autoreapprove")
            || self.explanation.contains("<!-- ss-autoreapprove")
            || self.audit.repository_id != scope.repository_id
            || self.audit.account_id != scope.account_id
            || self.audit.pull_request_id != scope.pull_request_id
            || ado_approval::source_iteration(&self.snapshot)?.1 != self.audit.source_commit
            || ado_approval::current_vote(&self.snapshot, &scope.account_id)? != self.vote
            || !matches!(self.vote, 5 | 10)
            || !matches!(self.snapshot.decide(Platform::AzureDevOps, &scope.account_id,
                &scope.repository_id, scope.pull_request_id, at)?,
                Decision::AssessmentRequired { anchor, .. } if anchor == self.audit.anchor)
        {
            return Err(fail(
                "invalidState",
                "Invalid ADO publication authority or scope",
            ));
        }
        if let Some(ack) = &self.vote_acknowledgement {
            matching_vote(ack, self)?;
        }
        if matches!(self.vote_state, EffectState::Acknowledged)
            && self.vote_acknowledgement.is_none()
            || self.vote_acknowledgement.is_some()
                && !matches!(
                    self.vote_state,
                    EffectState::Acknowledged | EffectState::Verified
                )
            || (self.vote_state == EffectState::Verified) != self.vote_event_id.is_some()
            || self.audit_state != EffectState::Prepared && self.vote_state != EffectState::Verified
            || (self.audit_state == EffectState::Verified) != self.verification.is_some()
            || self.audit_state == EffectState::Verified && self.result.is_none()
            || self
                .vote_event_id
                .as_ref()
                .is_some_and(|id| id.parse::<u64>().ok().is_none_or(|id| id == 0))
            || matches!(
                self.audit_state,
                EffectState::Acknowledged | EffectState::Verified
            ) != self.audit_acknowledgement.is_some()
        {
            return Err(fail("invalidState", "Incomplete ADO effect lifecycle"));
        }
        if let Some(ack) = &self.audit_acknowledgement {
            ado_approval::matching_audit(ack, &self.body()?, &scope.account_id)?;
        }
        if let Some(receipt) = &self.result {
            receipt.validate()?;
            if receipt.scope != *scope
                || receipt.run_id != self.run_id
                || receipt.operation_id != self.audit.operation_id
                || receipt.assessment_digest != self.audit.assessment_digest
                || receipt.journal_path != path
                || receipt.review_id != self.vote_event_id
                || match receipt.outcome {
                    Outcome::Reapproved => self.audit_state != EffectState::Verified,
                    Outcome::AlreadyValid | Outcome::Ineligible => {
                        self.vote_state != EffectState::Prepared
                    }
                    Outcome::StaleDuringExecution => {
                        !matches!(self.vote_state, EffectState::Prepared)
                            && self.audit_state != EffectState::Verified
                    }
                    Outcome::Failed => {
                        self.audit_state == EffectState::Verified
                            || self.vote_state == EffectState::Prepared
                            || self.unresolved()
                                && receipt
                                    .error
                                    .as_ref()
                                    .is_none_or(|error| error.kind != "ambiguousWrite")
                    }
                }
            {
                return Err(fail(
                    "invalidState",
                    "ADO receipt belongs to another operation",
                ));
            }
            if receipt.outcome == Outcome::Reapproved {
                let verification = self
                    .verification
                    .as_ref()
                    .ok_or_else(|| fail("invalidState", "Verified ADO effect lacks readback"))?;
                if self.audit_state != EffectState::Verified
                    || receipt.review_id != self.vote_event_id
                    || !unchanged(&verification.evidence, self)?
                    || !matches!(verification.evidence.decide(Platform::AzureDevOps,
                        &scope.account_id, &scope.repository_id, scope.pull_request_id,
                        timestamp(&verification.observed_at)?)?,
                        Decision::AlreadyValid { approval_event_id } if Some(&approval_event_id) == self.vote_event_id.as_ref())
                {
                    return Err(fail(
                        "invalidState",
                        "ADO verified receipt has no matching approval",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl<'a> Store<'a> {
    pub(crate) fn open(
        directory: &Path,
        scope: &Scope,
        clock: &'a dyn Clock,
    ) -> Result<Self, AppError> {
        let key = hash(&("azureDevOps", scope))?;
        let path = directory.join("publications").join(format!("{key}.json"));
        let journal = FileJournal::open(&path, clock)?;
        let record = if let Some(bytes) = journal.read_bytes()? {
            serde_json::from_slice::<Record>(&bytes).map_err(|error| {
                crate::error::caused_by(
                    "invalidState",
                    "Malformed ADO publication journal".to_owned(),
                    error,
                )
            })?
        } else {
            Record {
                schema_version: 1,
                scope: scope.clone(),
                operations: Vec::new(),
            }
        };
        if record.schema_version != 1 || record.scope != *scope {
            return Err(fail(
                "invalidState",
                "ADO publication scope or schema changed",
            ));
        }
        let store = Self {
            journal,
            path,
            record,
        };
        store.validate()?;
        Ok(store)
    }

    fn validate(&self) -> Result<(), AppError> {
        let mut runs = HashSet::new();
        let mut operations = HashSet::new();
        for operation in &self.record.operations {
            operation.validate(&self.record.scope, &self.path)?;
            if !runs.insert(&operation.run_id) || !operations.insert(&operation.audit.operation_id)
            {
                return Err(fail("invalidState", "Duplicate ADO publication operation"));
            }
        }
        Ok(())
    }

    fn save(&mut self, operation: &Operation) -> Result<(), AppError> {
        if let Some(existing) = self
            .record
            .operations
            .iter_mut()
            .find(|item| item.run_id == operation.run_id)
        {
            *existing = operation.clone();
        } else {
            self.record.operations.push(operation.clone());
        }
        self.validate()?;
        self.journal
            .write_bytes(&serde_json::to_vec_pretty(&self.record)?)
    }

    pub(crate) fn has_effect(&self, run: &str) -> bool {
        self.record
            .operations
            .iter()
            .any(|item| item.run_id == run && item.vote_state != EffectState::Prepared)
    }

    /// Missing host history is not proof that a retained stale receipt preceded every effect.
    pub(crate) fn preflight_only(&self, run: &str) -> bool {
        self.record.operations.iter().any(|operation| {
            operation.run_id == run && operation.vote_state == EffectState::Prepared
        })
    }

    /// A vote without verified audit remains unresolved even when readback is positive.
    pub(crate) fn unresolved(&self) -> bool {
        self.record.operations.iter().any(Operation::unresolved)
    }

    /// Reports only the source-valid automatic vote retained in verified readback.
    pub(crate) fn automatic_approval(
        &self,
        run: &str,
    ) -> Result<Option<AutomaticApproval>, AppError> {
        let verification = self
            .record
            .operations
            .iter()
            .find(|operation| operation.run_id == run)
            .and_then(|operation| operation.verification.as_ref());
        verification.map_or(Ok(None), |verification| {
            review_notice::automatic_approval(
                &verification.evidence,
                &self.record.scope,
                timestamp(&verification.observed_at)?,
            )
        })
    }

    pub(crate) fn may_write(&self, run: &str) -> bool {
        self.record
            .operations
            .iter()
            .find(|item| item.run_id == run)
            .is_none_or(|item| item.vote_state == EffectState::Prepared)
    }

    fn observe(&self, api: &mut Api<'_>, target: &Target) -> Result<Evidence, AppError> {
        if api.identity()?.id.as_deref() != Some(&self.record.scope.account_id) {
            return Err(fail("authentication", "ADO publication account changed"));
        }
        let evidence = Evidence::capture(api, target)?;
        let Evidence::AzureDevOps(
            AdoEvidence::State { pull_request } | AdoEvidence::History { pull_request, .. },
        ) = &evidence
        else {
            return Err(fail("scopeChanged", "ADO publication provider changed"));
        };
        if identifier(field(field(pull_request, "repository")?, "id")?)?
            != self.record.scope.repository_id
            || identifier(field(pull_request, "pullRequestId")?)?
                != self.record.scope.pull_request_id.to_string()
            || api.identity()?.id.as_deref() != Some(&self.record.scope.account_id)
        {
            return Err(fail(
                "scopeChanged",
                "ADO publication identity changed during capture",
            ));
        }
        Ok(evidence)
    }

    pub(crate) fn publish(
        &mut self,
        api: &mut Api<'_>,
        target: &Target,
        clock: &dyn Clock,
        input: &Input<'_>,
    ) -> Result<Receipt, AppError> {
        let mut operation = if let Some(existing) = self
            .record
            .operations
            .iter()
            .find(|item| item.run_id == input.run_id)
        {
            if existing.audit.assessment_digest != input.assessment_digest
                || existing.audit.policy_digest != input.policy_digest
                || existing.audit.source_commit != input.source_commit
                || &existing.audit.anchor != input.anchor
                || &existing.snapshot != input.evidence
            {
                return Err(fail(
                    "invalidState",
                    "ADO recovery requires the original immutable assessment",
                ));
            }
            existing.clone()
        } else {
            Operation {
                run_id: input.run_id.to_owned(),
                audit: Audit {
                    schema_version: 1,
                    operation_id: Uuid::new_v4().to_string(),
                    repository_id: self.record.scope.repository_id.clone(),
                    account_id: self.record.scope.account_id.clone(),
                    pull_request_id: target.pull_request_id,
                    anchor: input.anchor.clone(),
                    source_commit: input.source_commit.to_owned(),
                    policy_digest: input.policy_digest.to_owned(),
                    assessment_digest: input.assessment_digest.to_owned(),
                    state: AuditState::Submitted,
                },
                attribution: input.attribution.to_owned(),
                explanation: input.explanation.to_owned(),
                captured_at: clock.now().to_string(),
                snapshot: input.evidence.clone(),
                vote: ado_approval::current_vote(input.evidence, &self.record.scope.account_id)?,
                vote_state: EffectState::Prepared,
                audit_state: EffectState::Prepared,
                vote_acknowledgement: None,
                vote_event_id: None,
                audit_acknowledgement: None,
                verification: None,
                result: None,
            }
        };
        if let Some(result) = &operation.result
            && !operation.unresolved()
        {
            return Ok(result.clone());
        }
        if self
            .record
            .operations
            .iter()
            .any(|item| item.run_id != input.run_id && item.unresolved())
        {
            return Err(fail(
                "ambiguousWrite",
                "Another run has an unreconciled ADO vote/audit effect",
            ));
        }
        if operation.vote_state == EffectState::Prepared {
            self.save(&operation)?;
            let fresh = self.observe(api, target)?;
            let decision = fresh.decide(
                Platform::AzureDevOps,
                &self.record.scope.account_id,
                &self.record.scope.repository_id,
                target.pull_request_id,
                clock.now(),
            )?;
            if !equivalent(&fresh, &operation.snapshot)? {
                let outcome = match decision {
                    Decision::AlreadyValid { .. } => Outcome::AlreadyValid,
                    Decision::Ineligible { .. } => Outcome::Ineligible,
                    _ => Outcome::StaleDuringExecution,
                };
                return self.finish(
                    operation,
                    outcome,
                    Some((
                        "staleEvidence",
                        "ADO publication inputs changed; no vote was written",
                    )),
                );
            }
            operation.vote_state = EffectState::Intent;
            self.save(&operation)?;
            match api.transport.ado_write_once(
                &format!(
                    "{}/reviewers/{}",
                    target.pr_api(),
                    escape(&self.record.scope.account_id)
                ),
                "PUT",
                &operation.vote_payload(),
            ) {
                Ok(ack) => {
                    if matching_vote(&ack, &operation).is_ok() {
                        operation.vote_acknowledgement = Some(ack);
                        operation.vote_state = EffectState::Acknowledged;
                    } else {
                        operation.vote_state = EffectState::Unknown;
                    }
                }
                Err(error) if classification(&error) != "ambiguousWrite" => {
                    operation.vote_state = EffectState::Refused;
                    return self.finish(
                        operation,
                        Outcome::Failed,
                        Some((classification(&error), &error.to_string())),
                    );
                }
                Err(_) => operation.vote_state = EffectState::Unknown,
            }
            self.save(&operation)?;
        }
        self.reconcile(api, target, clock, operation)
    }

    /// Vote recovery is read-only; only a never-attempted or explicitly refused audit may write.
    fn reconcile(
        &mut self,
        api: &mut Api<'_>,
        target: &Target,
        clock: &dyn Clock,
        mut operation: Operation,
    ) -> Result<Receipt, AppError> {
        let fresh = match self.observe(api, target) {
            Ok(fresh) => fresh,
            Err(error) => {
                return self.finish(
                    operation,
                    Outcome::Failed,
                    Some((
                        "ambiguousWrite",
                        &format!("ADO vote may be submitted; readback failed: {error}"),
                    )),
                );
            }
        };
        let vote_id = match ado_approval::new_vote(
            &operation.snapshot,
            &fresh,
            &self.record.scope.account_id,
            operation.vote,
        ) {
            Ok(id) => id,
            Err(error) => {
                return self.finish(
                    operation,
                    Outcome::Failed,
                    Some((
                        "ambiguousWrite",
                        &format!("No unique attributable ADO vote; not replayed: {error}"),
                    )),
                );
            }
        };
        if operation
            .vote_event_id
            .as_ref()
            .is_some_and(|previous| previous != &vote_id)
        {
            return self.finish(
                operation,
                Outcome::Failed,
                Some((
                    "ambiguousWrite",
                    "ADO vote identity changed during recovery",
                )),
            );
        }
        operation.vote_event_id = Some(vote_id);
        operation.vote_state = EffectState::Verified;
        // A recovered event supersedes the earlier unknown receipt; the run
        // receives a new receipt only after this attempt has a terminal outcome.
        operation.result = None;
        self.save(&operation)?;
        let body = operation.body()?;
        let existing = audit_threads(&fresh, &body)?;
        if !existing.is_empty()
            || !matches!(
                operation.audit_state,
                EffectState::Prepared | EffectState::Refused
            )
        {
            return self.verify_finish(operation, fresh, clock);
        }
        // A missing audit after a definitive refusal can be retried only after
        // fresh identification of the same vote. Unknown creates are never retried.
        operation.audit_state = EffectState::Intent;
        operation.result = None;
        self.save(&operation)?;
        match api.transport.ado_write_once(
            &format!("{}/threads", target.pr_api()),
            "POST",
            &operation.audit_payload()?,
        ) {
            Ok(ack) => {
                if ado_approval::matching_audit(&ack, &body, &self.record.scope.account_id).is_ok()
                {
                    operation.audit_acknowledgement = Some(ack);
                    operation.audit_state = EffectState::Acknowledged;
                } else {
                    operation.audit_state = EffectState::Unknown;
                }
            }
            Err(error) => {
                operation.audit_state = if classification(&error) == "ambiguousWrite" {
                    EffectState::Unknown
                } else {
                    EffectState::Refused
                };
                return self.finish(
                    operation,
                    Outcome::Failed,
                    Some((
                        "ambiguousWrite",
                        &format!("ADO vote submitted; audit not verified: {error}"),
                    )),
                );
            }
        }
        self.save(&operation)?;
        let fresh = match self.observe(api, target) {
            Ok(fresh) => fresh,
            Err(error) => {
                return self.finish(
                    operation,
                    Outcome::Failed,
                    Some((
                        "ambiguousWrite",
                        &format!("ADO audit may be submitted; readback failed: {error}"),
                    )),
                );
            }
        };
        self.verify_finish(operation, fresh, clock)
    }

    fn verify_finish(
        &mut self,
        mut operation: Operation,
        fresh: Evidence,
        clock: &dyn Clock,
    ) -> Result<Receipt, AppError> {
        let matching = audit_threads(&fresh, &operation.body()?)?;
        let [thread] = matching.as_slice() else {
            return self.finish(
                operation,
                Outcome::Failed,
                Some((
                    "ambiguousWrite",
                    "No unique ADO audit; effect was not replayed",
                )),
            );
        };
        if let Err(error) =
            ado_approval::matching_audit(thread, &operation.body()?, &self.record.scope.account_id)
        {
            return self.finish(
                operation,
                Outcome::Failed,
                Some(("ambiguousWrite", &error.to_string())),
            );
        }
        operation.audit_acknowledgement = Some((*thread).clone());
        operation.audit_state = EffectState::Verified;
        let valid = unchanged(&fresh, &operation)?
            && matches!(fresh.decide(Platform::AzureDevOps, &self.record.scope.account_id,
                &self.record.scope.repository_id, self.record.scope.pull_request_id, clock.now()),
                Ok(Decision::AlreadyValid { approval_event_id }) if Some(&approval_event_id) == operation.vote_event_id.as_ref());
        operation.verification = Some(Verification {
            observed_at: clock.now().to_string(),
            evidence: fresh,
        });
        if valid {
            self.finish(operation, Outcome::Reapproved, None)
        } else {
            self.finish(operation, Outcome::StaleDuringExecution, Some(("staleEvidence",
                "URGENT: ADO vote was submitted but source or decision evidence changed; no rollback was attempted")))
        }
    }

    fn finish(
        &mut self,
        mut operation: Operation,
        outcome: Outcome,
        error: Option<(&str, &str)>,
    ) -> Result<Receipt, AppError> {
        let receipt = Receipt {
            journal_path: self.path.clone(),
            operation_id: operation.audit.operation_id.clone(),
            run_id: operation.run_id.clone(),
            scope: self.record.scope.clone(),
            assessment_digest: operation.audit.assessment_digest.clone(),
            outcome,
            review_id: operation.vote_event_id.clone(),
            error: error.map(|(kind, message)| Diagnostic {
                kind: kind.to_owned(),
                message: message.to_owned(),
            }),
        };
        operation.result = Some(receipt.clone());
        self.save(&operation)?;
        Ok(receipt)
    }
}

/// ADO acknowledges only the mutable reviewer; history supplies event and source binding.
fn matching_vote(value: &Value, operation: &Operation) -> Result<(), AppError> {
    if identifier(field(value, "id")?)? != operation.audit.account_id
        || field(value, "vote")?.as_i64() != Some(operation.vote)
    {
        return Err(fail(
            "staleEvidence",
            "ADO vote acknowledgement names another account or vote",
        ));
    }
    Ok(())
}

/// Matches exact audit bytes before trusting service authorship or interpreting markers.
fn audit_threads<'a>(evidence: &'a Evidence, body: &str) -> Result<Vec<&'a Value>, AppError> {
    let Evidence::AzureDevOps(AdoEvidence::History { threads, .. }) = evidence else {
        return Err(fail("staleEvidence", "ADO complete history unavailable"));
    };
    Ok(threads
        .iter()
        .filter(|thread| {
            thread
                .get("comments")
                .and_then(Value::as_array)
                .is_some_and(|comments| {
                    comments
                        .iter()
                        .any(|comment| comment.get("content").and_then(Value::as_str) == Some(body))
                })
        })
        .collect())
}

/// Removes only uniquely owned effects; every other discussion/decision change stays visible.
fn unchanged(evidence: &Evidence, operation: &Operation) -> Result<bool, AppError> {
    let mut stripped = evidence.clone();
    let body = operation.body()?;
    let matches = audit_threads(evidence, &body)?;
    let [thread] = matches.as_slice() else {
        return Ok(false);
    };
    let audit_id = ado_approval::matching_audit(thread, &body, &operation.audit.account_id)?;
    if let Evidence::AzureDevOps(AdoEvidence::History { threads, .. }) = &mut stripped {
        threads.retain(|thread| {
            let id = thread.get("id").map(Value::to_string);
            id.as_deref() != Some(&audit_id) && id.as_ref() != operation.vote_event_id.as_ref()
        });
    }
    equivalent(&stripped, &operation.snapshot)
}

/// Approval publication shares capture and notice materiality rules.
fn equivalent(left: &Evidence, right: &Evidence) -> Result<bool, AppError> {
    left.equivalent(right)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{Audit, AuditState, Decision, EffectState, Evidence, Operation, Platform, Scope};
    use crate::clock::timestamp;

    /// A stored preparation has authority but no remote effect or terminal result.
    fn prepared() -> (Operation, Scope, PathBuf) {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/ado-no-approval-policy.json"
        ))
        .unwrap();
        let stage = fixture.get("stages").unwrap().get(2).unwrap();
        let account = fixture.get("accountId").unwrap().as_str().unwrap();
        let repository = stage
            .pointer("/pullRequest/repository/id")
            .unwrap()
            .as_str()
            .unwrap();
        let policy = json!({"id": 99, "isEnabled": true, "isBlocking": true,
                    "type": {"id": "fa4e907d-c16b-4a4c-9dfa-4906e5d171dd"},
                    "settings": {"minimumApproverCount": 1, "requireVoteOnLastIteration": true}});
        let mut policies = stage.get("policies").unwrap().as_array().unwrap().clone();
        policies.push(policy.clone());
        let evidence: Evidence = serde_json::from_value(json!({"platform": "azureDevOps", "observation": {
                    "scope": "history", "pullRequest": stage.get("pullRequest").unwrap(),
                    "sourceCommit": stage.get("sourceCommit").unwrap(), "reviewers": stage.get("reviewers").unwrap(),
                    "iterations": stage.get("iterations").unwrap(), "threads": stage.get("threads").unwrap(),
                    "policies": policies, "evaluations": [{"evaluationId": "reviewer-policy", "configuration": policy, "status": "queued"}]
                }})).unwrap();
        let captured_at = "2026-01-02T00:00:00Z".to_owned();
        let Decision::AssessmentRequired { anchor, .. } = evidence
            .decide(
                Platform::AzureDevOps,
                account,
                repository,
                1,
                timestamp(&captured_at).unwrap(),
            )
            .unwrap()
        else {
            panic!("The fixture requires an assessment");
        };
        let scope = Scope {
            repository_id: repository.to_owned(),
            account_id: account.to_owned(),
            pull_request_id: 1,
        };
        let operation = Operation {
            run_id: Uuid::new_v4().to_string(),
            audit: Audit {
                schema_version: 1,
                operation_id: Uuid::new_v4().to_string(),
                repository_id: repository.to_owned(),
                account_id: account.to_owned(),
                pull_request_id: 1,
                anchor,
                source_commit: stage
                    .get("sourceCommit")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
                // Arbitrary valid digests bind this storage-only fixture.
                policy_digest: "a".repeat(64),
                assessment_digest: "b".repeat(64),
                state: AuditState::Submitted,
            },
            attribution: "[Copilot speaking]".to_owned(),
            explanation: "Fixture assessment.".to_owned(),
            captured_at,
            snapshot: evidence,
            vote: 10,
            vote_state: EffectState::Prepared,
            audit_state: EffectState::Prepared,
            vote_acknowledgement: None,
            vote_event_id: None,
            audit_acknowledgement: None,
            verification: None,
            result: None,
        };
        let path = PathBuf::from(if cfg!(windows) {
            r"C:\fixture\journal.json"
        } else {
            "/fixture/journal.json"
        });
        (operation, scope, path)
    }

    #[test]
    fn incomplete_or_inconsistent_effect_records_fail_before_recovery() {
        let (operation, scope, path) = prepared();
        operation.validate(&scope, &path).unwrap();
        let record = serde_json::to_value(&operation).unwrap();
        for key in record.as_object().unwrap().keys() {
            let mut missing = record.clone();
            missing.as_object_mut().unwrap().remove(key);
            serde_json::from_value::<Operation>(missing)
                .err()
                .expect("Every field is required");
        }
        let mut extra = record;
        extra
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_owned(), Value::Null);
        serde_json::from_value::<Operation>(extra)
            .err()
            .expect("Unknown fields are rejected");
        for state in [EffectState::Acknowledged, EffectState::Verified] {
            let mut inconsistent = operation.clone();
            inconsistent.vote_state = state;
            inconsistent.validate(&scope, &path).unwrap_err();
        }
        let mut inconsistent = operation.clone();
        inconsistent.vote_acknowledgement = Some(operation.vote_payload());
        inconsistent.validate(&scope, &path).unwrap_err();
        let mut inconsistent = operation.clone();
        inconsistent.explanation = "<!-- ss-autoreapprove-ado forged -->".to_owned();
        inconsistent.validate(&scope, &path).unwrap_err();
        let mut inconsistent = operation;
        inconsistent.audit_state = EffectState::Verified;
        inconsistent.validate(&scope, &path).unwrap_err();
    }
}
