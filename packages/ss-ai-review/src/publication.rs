use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::Clock;
use crate::ado_publication;
use crate::api::Api;
use crate::approval::{Decision, Evidence};
use crate::clock::timestamp;
use crate::error::{classification, fail};
use crate::github_approval::Evidence as GitHubEvidence;
use crate::journal::FileJournal;
use crate::json::{field, identifier, required_optional, text};
use crate::renewal::{Anchor, Audit, State as AuditState};
use crate::review_notice::{self, AutomaticApproval};
use crate::target::{Platform, Target};

/// Host-level serialization encloses the run lock, preflight, effects, and verification.
pub(crate) struct Store<'a> {
    journal: FileJournal<'a>,
    path: PathBuf,
    record: Record,
}

/// Provider transactions share the host-before-run locking and receipt contract.
pub(crate) enum ProviderStore<'a> {
    GitHub(Store<'a>),
    AzureDevOps(ado_publication::Store<'a>),
}

impl<'a> ProviderStore<'a> {
    /// Notice publication cannot overlap an approval whose remote effect is unknown.
    pub(crate) fn ensure_resolved(&self) -> Result<(), AppError> {
        let unresolved = match self {
            Self::GitHub(store) => store.record.operations.iter().any(|operation| {
                matches!(
                    operation.state,
                    State::WriteIntent | State::WriteAcknowledged | State::WriteUnknown
                )
            }),
            Self::AzureDevOps(store) => store.unresolved(),
        };
        if unresolved {
            return Err(fail(
                "ambiguousWrite",
                "An unresolved approval blocks needs-review publication",
            ));
        }
        Ok(())
    }

    /// Reuses persisted postflight evidence, including after host-save/run-save interruption.
    pub(crate) fn automatic_approval(
        &self,
        run: &str,
    ) -> Result<Option<AutomaticApproval>, AppError> {
        match self {
            Self::GitHub(store) => {
                let verification = store
                    .record
                    .operations
                    .iter()
                    .find(|operation| operation.run_id == run)
                    .and_then(|operation| operation.verification.as_ref());
                verification.map_or(Ok(None), |verification| {
                    review_notice::automatic_approval(
                        &verification.evidence,
                        &store.record.scope,
                        timestamp(&verification.observed_at)?,
                    )
                })
            }
            Self::AzureDevOps(store) => store.automatic_approval(run),
        }
    }

    pub(crate) fn open(
        platform: Platform,
        directory: &Path,
        scope: &Scope,
        clock: &'a dyn Clock,
    ) -> Result<Self, AppError> {
        match platform {
            Platform::GitHub => Store::open(directory, scope, clock).map(Self::GitHub),
            Platform::AzureDevOps => {
                ado_publication::Store::open(directory, scope, clock).map(Self::AzureDevOps)
            }
        }
    }

    pub(crate) fn has_effect(&self, run_id: &str) -> bool {
        match self {
            Self::GitHub(store) => store.has_effect(run_id),
            Self::AzureDevOps(store) => store.has_effect(run_id),
        }
    }

    /// A retained receipt is refreshable only with positive host evidence of no write intent.
    pub(crate) fn preflight_only(&self, run_id: &str) -> bool {
        match self {
            Self::GitHub(store) => store
                .record
                .operations
                .iter()
                .any(|operation| operation.run_id == run_id && operation.effect.is_none()),
            Self::AzureDevOps(store) => store.preflight_only(run_id),
        }
    }

    pub(crate) fn may_write(&self, run_id: &str) -> bool {
        match self {
            Self::GitHub(store) => store.may_write(run_id),
            Self::AzureDevOps(store) => store.may_write(run_id),
        }
    }

    pub(crate) fn publish(
        &mut self,
        api: &mut Api<'_>,
        target: &Target,
        clock: &dyn Clock,
        input: &Input<'_>,
    ) -> Result<Receipt, AppError> {
        match self {
            Self::GitHub(store) => store.publish(api, target, clock, input),
            Self::AzureDevOps(store) => store.publish(api, target, clock, input),
        }
    }
}

/// Stable scope is independent of repository URL spelling and session directories.
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

/// Inputs are produced only from a validated complete insignificant assessment.
pub(crate) struct Input<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) assessment_digest: &'a str,
    pub(crate) policy_digest: &'a str,
    pub(crate) evidence: &'a Evidence,
    pub(crate) anchor: &'a Anchor,
    pub(crate) source_commit: &'a str,
    pub(crate) attribution: &'a str,
    pub(crate) explanation: &'a str,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    schema_version: u32,
    scope: Scope,
    operations: Vec<Operation>,
}

/// One review-create effect has its own intent, acknowledgement, and verification.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Operation {
    run_id: String,
    audit: Audit,
    body: String,
    captured_at: String,
    snapshot: Evidence,
    state: State,
    #[serde(deserialize_with = "required_optional")]
    effect: Option<Effect>,
    #[serde(deserialize_with = "required_optional")]
    verification: Option<Verification>,
    #[serde(deserialize_with = "required_optional")]
    result: Option<Receipt>,
}

/// A terminal result describes the effect, not a promise that the PR remains mergeable.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Receipt {
    pub(crate) journal_path: PathBuf,
    pub(crate) operation_id: String,
    pub(crate) run_id: String,
    pub(crate) scope: Scope,
    pub(crate) assessment_digest: String,
    pub(crate) outcome: Outcome,
    #[serde(deserialize_with = "required_optional")]
    pub(crate) review_id: Option<String>,
    #[serde(deserialize_with = "required_optional")]
    pub(crate) error: Option<Diagnostic>,
}

/// Readback has its own observation time for strict service-history validation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Verification {
    observed_at: String,
    evidence: Evidence,
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum State {
    Prepared,
    WriteIntent,
    WriteAcknowledged,
    Verified,
    Cancelled,
    Stale,
    Failed,
    WriteUnknown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Effect {
    kind: EffectKind,
    operation_id: String,
    payload_digest: String,
    state: EffectState,
    #[serde(deserialize_with = "required_optional")]
    acknowledgement: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum EffectKind {
    ReviewCreate,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum EffectState {
    Intent,
    Acknowledged,
    Verified,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Diagnostic {
    pub(crate) kind: String,
    pub(crate) message: String,
}

impl Receipt {
    pub(crate) fn validate(&self) -> Result<(), AppError> {
        if Uuid::parse_str(&self.operation_id).is_err()
            || !self.journal_path.is_absolute()
            || Uuid::parse_str(&self.run_id).is_err()
            || self.scope.repository_id.is_empty()
            || self.scope.account_id.is_empty()
            || self.scope.pull_request_id == 0
            || self.assessment_digest.len() != 64
            || !self
                .assessment_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self
                .review_id
                .as_ref()
                .is_some_and(|id| id.trim().is_empty())
            || self.outcome == Outcome::Reapproved
                && (self.review_id.is_none() || self.error.is_some())
            || matches!(
                self.outcome,
                Outcome::Failed | Outcome::StaleDuringExecution
            ) && self.error.is_none()
            || self.error.as_ref().is_some_and(|error| {
                error.kind.trim().is_empty() || error.message.trim().is_empty()
            })
        {
            return Err(fail("invalidState", "Invalid publication receipt"));
        }
        Ok(())
    }
}

impl Record {
    fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != 1
            || self.scope.repository_id.is_empty()
            || self.scope.account_id.is_empty()
            || self.scope.pull_request_id == 0
        {
            return Err(fail("invalidState", "Invalid publication scope"));
        }
        let mut ids = HashSet::new();
        let mut runs = HashSet::new();
        for operation in &self.operations {
            operation.validate(&self.scope)?;
            if !ids.insert(&operation.audit.operation_id) || !runs.insert(&operation.run_id) {
                return Err(fail(
                    "invalidState",
                    "Repeated publication operation or run",
                ));
            }
        }
        Ok(())
    }
}

impl Operation {
    fn payload(&self) -> Value {
        json!({"event": "APPROVE", "commit_id": self.audit.source_commit, "body": self.body})
    }

    fn validate(&self, scope: &Scope) -> Result<(), AppError> {
        self.audit.validate()?;
        if Uuid::parse_str(&self.run_id).is_err()
            || self.audit.repository_id != scope.repository_id
            || self.audit.account_id != scope.account_id
            || self.audit.pull_request_id != scope.pull_request_id
            || Audit::parse(&self.body)?.as_ref() != Some(&self.audit)
        {
            return Err(fail(
                "invalidState",
                "Publication identity or audit binding changed",
            ));
        }
        let Evidence::GitHub(GitHubEvidence::History { pull_request, .. }) = &self.snapshot else {
            return Err(fail(
                "invalidState",
                "Publication requires complete GitHub source evidence",
            ));
        };
        if text(field(field(pull_request, "head")?, "sha")?)? != self.audit.source_commit
            || !matches!(self.snapshot.decide(Platform::GitHub, &scope.account_id, &scope.repository_id,
            scope.pull_request_id, timestamp(&self.captured_at)?)?,
            Decision::AssessmentRequired { anchor, .. } if anchor == self.audit.anchor)
        {
            return Err(fail(
                "invalidState",
                "Publication has no matching external approval authority",
            ));
        }
        let terminal = matches!(
            self.state,
            State::Verified | State::Cancelled | State::Stale | State::Failed | State::WriteUnknown
        );
        if terminal != self.result.is_some() {
            return Err(fail("invalidState", "Publication lifecycle is incomplete"));
        }
        if let Some(result) = &self.result {
            result.validate()?;
            if result.operation_id != self.audit.operation_id
                || &result.scope != scope
                || result.run_id != self.run_id
                || result.assessment_digest != self.audit.assessment_digest
            {
                return Err(fail("invalidState", "Receipt names another effect"));
            }
        }
        if let Some(effect) = &self.effect {
            if effect.operation_id != self.audit.operation_id
                || effect.payload_digest != hash(&self.payload())?
                || matches!(
                    effect.state,
                    EffectState::Acknowledged | EffectState::Verified
                ) != effect.acknowledgement.is_some()
            {
                return Err(fail("invalidState", "Publication effect evidence changed"));
            }
            if let Some(acknowledgement) = &effect.acknowledgement {
                matching_review(acknowledgement, self)?;
            }
        } else if !matches!(
            self.state,
            State::Prepared | State::Cancelled | State::Failed
        ) {
            return Err(fail(
                "invalidState",
                "Publication is missing its effect intent",
            ));
        }
        let effect_state = self.effect.as_ref().map(|effect| effect.state);
        let valid_effect = match self.state {
            State::Prepared | State::Cancelled => effect_state.is_none(),
            State::WriteIntent => effect_state == Some(EffectState::Intent),
            State::WriteAcknowledged => effect_state == Some(EffectState::Acknowledged),
            State::Verified => effect_state == Some(EffectState::Verified),
            State::Failed => effect_state == Some(EffectState::Failed),
            State::Stale | State::WriteUnknown => matches!(
                effect_state,
                Some(EffectState::Intent | EffectState::Acknowledged | EffectState::Unknown)
            ),
        };
        if !valid_effect
            || self.state == State::WriteUnknown
                && self
                    .result
                    .as_ref()
                    .and_then(|result| result.error.as_ref())
                    .is_none_or(|error| error.kind != "ambiguousWrite")
            || self.result.as_ref().is_some_and(|result| match self.state {
                State::Verified => result.outcome != Outcome::Reapproved,
                State::Stale => result.outcome != Outcome::StaleDuringExecution,
                State::Failed | State::WriteUnknown => result.outcome != Outcome::Failed,
                State::Cancelled => matches!(result.outcome, Outcome::Reapproved | Outcome::Failed),
                _ => true,
            })
        {
            return Err(fail(
                "invalidState",
                "Publication lifecycle and effect disagree",
            ));
        }
        if self.state == State::Verified
            && (self.verification.is_none()
                || self
                    .effect
                    .as_ref()
                    .is_none_or(|effect| effect.state != EffectState::Verified)
                || self
                    .result
                    .as_ref()
                    .is_none_or(|result| result.outcome != Outcome::Reapproved))
        {
            return Err(fail("invalidState", "Verified publication lacks readback"));
        }
        if self.state == State::Verified {
            let verification = self
                .verification
                .as_ref()
                .ok_or_else(|| fail("invalidState", "Missing verification"))?;
            let id = verify(
                &verification.evidence,
                self,
                scope,
                timestamp(&verification.observed_at)?,
            )?
            .0;
            if self
                .result
                .as_ref()
                .and_then(|result| result.review_id.as_ref())
                != Some(&id)
            {
                return Err(fail("invalidState", "Verification and receipt disagree"));
            }
        }
        Ok(())
    }
}

impl<'a> Store<'a> {
    /// An owned effect permits reconciliation or copying its terminal receipt to the run.
    ///
    /// Host persistence precedes run persistence, so either record may still need recovery.
    pub(crate) fn has_effect(&self, run_id: &str) -> bool {
        self.record
            .operations
            .iter()
            .any(|operation| operation.run_id == run_id && operation.effect.is_some())
    }

    /// Policy upgrades block new effects, not read-only reconciliation of an owned old effect.
    pub(crate) fn may_write(&self, run_id: &str) -> bool {
        self.record
            .operations
            .iter()
            .find(|operation| operation.run_id == run_id)
            .is_none_or(|operation| operation.state == State::Prepared)
    }

    pub(crate) fn open(
        directory: &Path,
        scope: &Scope,
        clock: &'a dyn Clock,
    ) -> Result<Self, AppError> {
        let key = hash(&("gitHub", scope))?;
        let path = directory.join("publications").join(format!("{key}.json"));
        let journal = FileJournal::open(&path, clock)?;
        let record = if let Some(bytes) = journal.read_bytes()? {
            serde_json::from_slice::<Record>(&bytes).map_err(|error| {
                crate::error::caused_by(
                    "invalidState",
                    "Malformed publication store".to_owned(),
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
        record.validate()?;
        if &record.scope != scope {
            return Err(fail("invalidState", "Publication scope changed"));
        }
        if record.operations.iter().any(|operation| {
            operation
                .result
                .as_ref()
                .is_some_and(|receipt| receipt.journal_path != path)
        }) {
            return Err(fail(
                "invalidState",
                "Publication receipt names another journal",
            ));
        }
        Ok(Self {
            journal,
            path,
            record,
        })
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
        self.record.validate()?;
        self.journal
            .write_bytes(&serde_json::to_vec_pretty(&self.record)?)
    }

    pub(crate) fn publish(
        &mut self,
        api: &mut Api<'_>,
        target: &Target,
        clock: &dyn Clock,
        input: &Input<'_>,
    ) -> Result<Receipt, AppError> {
        let existing = self
            .record
            .operations
            .iter()
            .find(|item| item.run_id == input.run_id);
        let mut operation = if let Some(existing) = existing {
            if existing.audit.assessment_digest != input.assessment_digest
                || existing.audit.policy_digest != input.policy_digest
                || existing.audit.source_commit != input.source_commit
                || &existing.audit.anchor != input.anchor
                || &existing.snapshot != input.evidence
            {
                return Err(fail(
                    "invalidState",
                    "Recovery requires the original immutable assessment",
                ));
            }
            existing.clone()
        } else {
            let audit = Audit {
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
            };
            let body = format!(
                "{}\n\nAutomatically renewed approval for source {} against original approval {} ({}).\n\n{}\n\n<!-- ss-autoreapprove {} -->",
                input.attribution,
                input.source_commit,
                input.anchor.review_id,
                input.anchor.source_commit,
                input.explanation,
                serde_json::to_string(&audit)?
            );
            Operation {
                run_id: input.run_id.to_owned(),
                audit,
                body,
                captured_at: clock.now().to_string(),
                snapshot: input.evidence.clone(),
                state: State::Prepared,
                effect: None,
                verification: None,
                result: None,
            }
        };
        if !matches!(
            operation.state,
            State::Prepared | State::WriteIntent | State::WriteAcknowledged | State::WriteUnknown
        ) {
            return operation
                .result
                .ok_or_else(|| fail("invalidState", "Terminal publication has no result"));
        }
        if operation.state != State::Prepared {
            return self.reconcile(api, target, clock, operation);
        }
        // Preparation can outlive another run's uncertain write. Recheck under
        // the host lock for every entry to the write path, including resumed work.
        if self.record.operations.iter().any(|item| {
            item.run_id != input.run_id
                && matches!(
                    item.state,
                    State::WriteIntent | State::WriteAcknowledged | State::WriteUnknown
                )
        }) {
            return Err(fail(
                "ambiguousWrite",
                "Another run has an unreconciled approval effect; no new publication is allowed",
            ));
        }
        self.save(&operation)?;
        let fresh = self.observe(api, target)?;
        let decision = fresh.decide(
            Platform::GitHub,
            &self.record.scope.account_id,
            &self.record.scope.repository_id,
            target.pull_request_id,
            clock.now(),
        )?;
        if !fresh.equivalent(&operation.snapshot)? {
            let outcome = match decision {
                Decision::AlreadyValid { .. } => Outcome::AlreadyValid,
                Decision::Ineligible { .. } => Outcome::Ineligible,
                _ => Outcome::StaleDuringExecution,
            };
            return self.finish(
                operation,
                State::Cancelled,
                outcome,
                None,
                Some((
                    "staleEvidence",
                    "Publication inputs changed; no approval was written",
                )),
            );
        }
        operation.state = State::WriteIntent;
        operation.effect = Some(Effect {
            kind: EffectKind::ReviewCreate,
            operation_id: operation.audit.operation_id.clone(),
            payload_digest: hash(&operation.payload())?,
            state: EffectState::Intent,
            acknowledgement: None,
        });
        self.save(&operation)?;
        let response = api.transport.github_write_once(
            &format!("{}/reviews", target.pr_api()),
            &operation.payload(),
        );
        match response {
            Ok(response) => {
                if let Err(error) = matching_review(&response, &operation) {
                    operation.state = State::WriteUnknown;
                    operation
                        .effect
                        .as_mut()
                        .ok_or_else(|| fail("invalidState", "Missing effect"))?
                        .state = EffectState::Unknown;
                    return self.finish(
                        operation,
                        State::WriteUnknown,
                        Outcome::Failed,
                        None,
                        Some(("ambiguousWrite", &error.to_string())),
                    );
                }
                operation.state = State::WriteAcknowledged;
                let effect = operation
                    .effect
                    .as_mut()
                    .ok_or_else(|| fail("invalidState", "Missing effect"))?;
                effect.state = EffectState::Acknowledged;
                effect.acknowledgement = Some(response);
                self.save(&operation)?;
                self.reconcile(api, target, clock, operation)
            }
            Err(error) if classification(&error) == "ambiguousWrite" => {
                operation.state = State::WriteUnknown;
                operation
                    .effect
                    .as_mut()
                    .ok_or_else(|| fail("invalidState", "Missing effect"))?
                    .state = EffectState::Unknown;
                // Persist uncertainty before read-only reconciliation; never repeat the POST.
                let receipt = self.finish(
                    operation.clone(),
                    State::WriteUnknown,
                    Outcome::Failed,
                    None,
                    Some(("ambiguousWrite", &error.to_string())),
                )?;
                operation.result = Some(receipt);
                self.reconcile(api, target, clock, operation)
            }
            Err(error) => {
                operation
                    .effect
                    .as_mut()
                    .ok_or_else(|| fail("invalidState", "Missing effect"))?
                    .state = EffectState::Failed;
                self.finish(
                    operation,
                    State::Failed,
                    Outcome::Failed,
                    None,
                    Some((classification(&error), &error.to_string())),
                )
            }
        }
    }

    fn observe(&self, api: &mut Api<'_>, target: &Target) -> Result<Evidence, AppError> {
        if api.identity()?.id.as_deref() != Some(&self.record.scope.account_id) {
            return Err(fail("authentication", "Publication account changed"));
        }
        let evidence = Evidence::capture(api, target)?;
        if let Evidence::GitHub(
            GitHubEvidence::State { pull_request } | GitHubEvidence::History { pull_request, .. },
        ) = &evidence
            && identifier(field(field(field(pull_request, "base")?, "repo")?, "id")?)?
                != self.record.scope.repository_id
        {
            return Err(fail(
                "scopeChanged",
                "Publication repository identity changed",
            ));
        }
        // Preflight interprets authority before writing. Reconciliation must first
        // identify an owned effect even if a concurrent withdrawal invalidates it.
        if api.identity()?.id.as_deref() != Some(&self.record.scope.account_id) {
            return Err(fail(
                "authentication",
                "Publication account changed during evidence capture",
            ));
        }
        Ok(evidence)
    }

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
                    State::WriteUnknown,
                    Outcome::Failed,
                    None,
                    Some((
                        "ambiguousWrite",
                        &format!("Approval may be published; verification failed: {error}"),
                    )),
                );
            }
        };
        let Evidence::GitHub(GitHubEvidence::History { reviews, .. }) = &fresh else {
            return self.finish(
                operation,
                State::WriteUnknown,
                Outcome::Failed,
                None,
                Some((
                    "ambiguousWrite",
                    "Approval may be published; current history is unavailable",
                )),
            );
        };
        let matching = reviews
            .iter()
            .filter(|review| review.get("body").and_then(Value::as_str) == Some(&operation.body))
            .collect::<Vec<_>>();
        let [review] = matching.as_slice() else {
            return self.finish(
                operation,
                State::WriteUnknown,
                Outcome::Failed,
                None,
                Some((
                    "ambiguousWrite",
                    "No unique matching remote approval; publication was not replayed",
                )),
            );
        };
        let review_id =
            match matching_review(review, &operation) {
                Ok(id) => id,
                Err(error) => return self.finish(
                    operation,
                    State::Stale,
                    Outcome::StaleDuringExecution,
                    None,
                    Some((
                        "staleEvidence",
                        &format!(
                            "Approval effect exists but is not the expected valid approval: {error}"
                        ),
                    )),
                ),
            };
        let verification = verify(&fresh, &operation, &self.record.scope, clock.now());
        operation.verification = Some(Verification {
            observed_at: clock.now().to_string(),
            evidence: fresh,
        });
        let Ok((_, acknowledgement)) = verification else {
            return self.finish(operation, State::Stale, Outcome::StaleDuringExecution, Some(review_id),
                Some(("staleEvidence", "URGENT: approval was submitted but source or decision evidence changed; no rollback was attempted")));
        };
        let effect = operation
            .effect
            .as_mut()
            .ok_or_else(|| fail("invalidState", "Missing approval intent"))?;
        effect.state = EffectState::Verified;
        effect.acknowledgement = Some(acknowledgement);
        self.finish(
            operation,
            State::Verified,
            Outcome::Reapproved,
            Some(review_id),
            None,
        )
    }

    fn finish(
        &mut self,
        mut operation: Operation,
        state: State,
        outcome: Outcome,
        review_id: Option<String>,
        error: Option<(&str, &str)>,
    ) -> Result<Receipt, AppError> {
        operation.state = state;
        let result = Receipt {
            journal_path: self.path.clone(),
            operation_id: operation.audit.operation_id.clone(),
            run_id: operation.run_id.clone(),
            scope: self.record.scope.clone(),
            assessment_digest: operation.audit.assessment_digest.clone(),
            outcome,
            review_id,
            error: error.map(|(kind, message)| Diagnostic {
                kind: kind.to_owned(),
                message: message.to_owned(),
            }),
        };
        operation.result = Some(result.clone());
        self.save(&operation)?;
        Ok(result)
    }
}

/// The same exact readback gate runs both before saving and when consuming a verified record.
fn verify(
    evidence: &Evidence,
    operation: &Operation,
    scope: &Scope,
    now: jiff::Timestamp,
) -> Result<(String, Value), AppError> {
    let Evidence::GitHub(GitHubEvidence::History { reviews, .. }) = evidence else {
        return Err(fail("staleEvidence", "Missing complete postflight history"));
    };
    let matches = reviews
        .iter()
        .filter(|review| review.get("body").and_then(Value::as_str) == Some(&operation.body))
        .collect::<Vec<_>>();
    let [review] = matches.as_slice() else {
        return Err(fail("staleEvidence", "Postflight approval is not unique"));
    };
    let id = matching_review(review, operation)?;
    let mut stripped = evidence.clone();
    if let Evidence::GitHub(GitHubEvidence::History {
        reviews, timeline, ..
    }) = &mut stripped
    {
        reviews.retain(|review| review.get("node_id").and_then(Value::as_str) != Some(&id));
        timeline.retain(|event| {
            event.get("__typename").and_then(Value::as_str) != Some("PullRequestReview")
                || event.get("id").and_then(Value::as_str) != Some(&id)
        });
    }
    let decision = evidence.decide(
        Platform::GitHub,
        &scope.account_id,
        &scope.repository_id,
        scope.pull_request_id,
        now,
    )?;
    if !stripped.equivalent(&operation.snapshot)?
        || !matches!(decision, Decision::AlreadyValid { approval_event_id } if approval_event_id == id)
    {
        return Err(fail(
            "staleEvidence",
            "Postflight source or decision evidence changed",
        ));
    }
    Ok((id, (*review).clone()))
}

/// The installed command has one host store; embedded callers can inject an isolated location.
pub(crate) fn data_directory() -> Result<PathBuf, AppError> {
    let name = if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "HOME"
    };
    let root = PathBuf::from(env::var_os(name).ok_or_else(|| {
        fail(
            "io",
            format!("{name} is required for shared publication records"),
        )
    })?);
    Ok(publication_data_directory(&root, cfg!(windows)))
}

/// Keeps publication recovery records in the same store across executable upgrades.
fn publication_data_directory(root: &Path, windows: bool) -> PathBuf {
    // Pending publication intents must remain visible and cannot be replayed
    // under a new executable name.
    if windows {
        root.join("ss-skills-util")
    } else {
        root.join(".local").join("share").join("ss-skills-util")
    }
}

fn matching_review(review: &Value, operation: &Operation) -> Result<String, AppError> {
    if identifier(field(field(review, "user")?, "id")?)? != operation.audit.account_id
        || text(field(review, "commit_id")?)? != operation.audit.source_commit
        || text(field(review, "body")?)? != operation.body
        || text(field(review, "state")?)? != "APPROVED"
    {
        return Err(fail(
            "staleEvidence",
            "Remote approval does not match the intended account, source, body, and state",
        ));
    }
    Ok(text(field(review, "node_id")?)?.to_owned())
}

pub(crate) fn hash(value: &impl Serialize) -> Result<String, AppError> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::thread;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::Value;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::{
        Audit, AuditState, Decision, Evidence, Operation, Platform, Scope, State, Store,
        publication_data_directory,
    };
    use crate::Clock;
    use crate::clock::timestamp;

    #[test]
    fn publication_store_preserves_existing_directory() {
        let root = Path::new("profile");
        assert_eq!(
            publication_data_directory(root, true),
            root.join("ss-skills-util")
        );
        assert_eq!(
            publication_data_directory(root, false),
            root.join(".local").join("share").join("ss-skills-util")
        );
    }

    struct UncontendedClock;

    impl Clock for UncontendedClock {
        fn now(&self) -> Timestamp {
            timestamp("2026-09-28T10:00:00Z").unwrap()
        }
        fn wait(&self, _: Duration) -> Result<(), AppError> {
            panic!("The owning transaction cannot contend");
        }
    }

    /// Signals actual lock contention; release is controlled by the owner, never wall time.
    struct GateClock {
        entered: Sender<()>,
        release: Receiver<()>,
    }

    impl Clock for GateClock {
        fn now(&self) -> Timestamp {
            timestamp("2026-09-28T10:00:00Z").unwrap()
        }
        fn wait(&self, _: Duration) -> Result<(), AppError> {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            Ok(())
        }
    }

    fn prepared(scope: &Scope, clock: &dyn Clock) -> Operation {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/github-approval.json")).unwrap();
        let evidence = Evidence::GitHub(
            serde_json::from_value(fixture.get("evidence").unwrap().clone()).unwrap(),
        );
        let Decision::AssessmentRequired { anchor, .. } = evidence
            .decide(
                Platform::GitHub,
                &scope.account_id,
                &scope.repository_id,
                scope.pull_request_id,
                clock.now(),
            )
            .unwrap()
        else {
            panic!("Fixture must authorize source assessment")
        };
        let source = fixture
            .pointer("/evidence/pullRequest/head/sha")
            .unwrap()
            .as_str()
            .unwrap();
        let audit = Audit {
            schema_version: 1,
            operation_id: Uuid::new_v4().to_string(),
            repository_id: scope.repository_id.clone(),
            account_id: scope.account_id.clone(),
            pull_request_id: scope.pull_request_id,
            anchor,
            source_commit: source.to_owned(),
            // Arbitrary valid digests bind this storage-only fixture, not a semantic approval.
            policy_digest: "a".repeat(64),
            assessment_digest: "b".repeat(64),
            state: AuditState::Submitted,
        };
        Operation {
            run_id: Uuid::new_v4().to_string(),
            body: format!(
                "<!-- ss-autoreapprove {} -->",
                serde_json::to_string(&audit).unwrap()
            ),
            audit,
            captured_at: clock.now().to_string(),
            snapshot: evidence,
            state: State::Prepared,
            effect: None,
            verification: None,
            result: None,
        }
    }

    #[test]
    fn overlapping_host_transactions_preserve_every_operation() {
        let directory = TempDir::new().unwrap();
        let scope = Scope {
            repository_id: "123".to_owned(),
            account_id: "42".to_owned(),
            pull_request_id: 1,
        };
        let (entered_tx, entered_rx) = mpsc::channel();
        let clock = UncontendedClock;
        let mut owner = Store::open(directory.path(), &scope, &clock).unwrap();
        let first = prepared(&scope, &clock);
        let first_id = first.audit.operation_id.clone();
        let second = prepared(&scope, &clock);
        let second_id = second.audit.operation_id.clone();
        thread::scope(|threads| {
            let (contender_release_tx, contender_release_rx) = mpsc::channel();
            let directory = directory.path();
            let scope = &scope;
            let contender = threads.spawn(move || {
                let clock = GateClock {
                    entered: entered_tx,
                    release: contender_release_rx,
                };
                let mut store = Store::open(directory, scope, &clock).unwrap();
                assert_eq!(store.record.operations.len(), 1);
                store.save(&second).unwrap();
            });
            entered_rx.recv().unwrap();
            owner.save(&first).unwrap();
            drop(owner);
            contender_release_tx.send(()).unwrap();
            contender.join().unwrap();
        });
        let store = Store::open(directory.path(), &scope, &clock).unwrap();
        let actual = store
            .record
            .operations
            .iter()
            .map(|op| &op.audit.operation_id)
            .collect::<Vec<_>>();
        assert_eq!(actual, [&first_id, &second_id]);
        store.record.validate().unwrap();
    }
}
