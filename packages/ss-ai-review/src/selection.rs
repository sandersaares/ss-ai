//! Owns cooperative reservation decisions and their recoverable journal stages.
//!
//! The caller retains one journal transaction while this module reads evidence,
//! waits, and performs remote effects. See `docs/implementation/journals.md`.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use clap::ValueEnum;
use jiff::Timestamp;
use ohno::AppError;
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Clock;
use crate::clock::{CLAIM_AGE, PREPARATION_AGE, timestamp};
use crate::error::fail;
use crate::identity::Identity;
use crate::json::required_optional;
use crate::target::{Platform, Target};

/// Selects the review assignment policy for the whole journal transaction.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SelectionMode {
    /// Finds work that is not already assigned to the authenticated reviewer.
    #[default]
    Available,
    /// Finds assigned work without a submitted review from the authenticated reviewer.
    Assigned,
}

/// Supplies parseable ownership and review evidence from a claim marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClaimMarker<'a> {
    session: &'a str,
    token: &'a str,
    state: ClaimState,
    revision: &'a str,
    review_fingerprint: &'a str,
}

/// Names the externally observable phase of a review reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClaimState {
    Reserved,
    Active,
    Completed,
}

impl ClaimState {
    /// Supplies the stable protocol label carried in PR discussion.
    fn label(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Active => "active",
            Self::Completed => "completed",
        }
    }
}

/// Carries schema-3 evidence across command and journal boundaries.
///
/// Deserialization alone does not establish valid lifecycle state. `validate`
/// checks relationships between fields before `Workflow` attaches the required
/// evidence to a particular stage.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct State {
    /// Identifies the record contract before interpreting state-dependent fields.
    pub(crate) schema_version: u32,
    /// Primary session whose reservation this record may manage.
    pub(crate) session_id: String,
    /// Exact selection scope checked again during confirmation.
    pub(crate) repository_url: String,
    /// Assignment policy fixed for selection and every replacement in this cycle.
    pub(crate) selection_mode: SelectionMode,
    /// Authenticated account whose ownership must match the next invocation.
    pub(crate) identity: Identity,
    /// Wire-level lifecycle label determining which reservation fields are required.
    pub(crate) status: Status,
    /// Current candidate, or explicit null when no reservation is attached.
    #[serde(deserialize_with = "required_optional")]
    pub(crate) target: Option<Target>,
    /// Intent or created claim evidence, paired with the current target.
    #[serde(deserialize_with = "required_optional")]
    pub(crate) claim: Option<Claim>,
    /// Distinct PRs already tried, preventing reconsideration after a discard.
    pub(crate) attempted: Vec<NonZeroU32>,
    /// Decision evidence returned to the skill alongside the final status.
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// Absolute publication location carried in the result for the calling skill.
    #[expect(
        clippy::struct_field_names,
        reason = "The field preserves the schema-3 journal contract"
    )]
    pub(crate) state_path: String,
}

impl State {
    /// Starts an unclaimed record from authenticated, explicit invocation inputs.
    ///
    /// This is input to a fresh selection transaction, not authorization to
    /// replace an existing journal.
    pub(crate) fn new(
        repository: &Target,
        identity: &Identity,
        session: &str,
        path: &str,
        selection_mode: SelectionMode,
    ) -> Self {
        Self {
            schema_version: 3,
            session_id: session.to_owned(),
            repository_url: repository.web_url.clone(),
            selection_mode,
            identity: identity.clone(),
            status: Status::Selecting,
            target: None,
            claim: None,
            attempted: Vec::new(),
            diagnostics: Vec::new(),
            state_path: path.to_owned(),
        }
    }

    /// Rejects malformed or contradictory recovery evidence before any side effect.
    pub(crate) fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != 3
            || !valid_session(&self.session_id)
            || !Path::new(&self.state_path).is_absolute()
        {
            return Err(fail(
                "invalidState",
                "Unsupported or malformed schema-3 journal",
            ));
        }
        self.identity.validate()?;
        let repository = Target::repository(&self.repository_url)?;
        if self.identity.id.is_none() || self.identity.platform != repository.platform {
            return Err(fail(
                "invalidState",
                "Journal has no matching authenticated identity",
            ));
        }
        let mut seen = HashSet::new();
        if self.attempted.iter().any(|id| !seen.insert(id))
            || self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.reason.trim().is_empty())
        {
            return Err(fail(
                "invalidState",
                "Malformed journal attempts or diagnostics",
            ));
        }
        match (&self.target, &self.claim) {
            (None, None) if matches!(self.status, Status::Selecting | Status::NoCandidate) => {}
            (Some(target), Some(claim))
                if matches!(
                    self.status,
                    Status::Claiming
                        | Status::Claimed
                        | Status::Activating
                        | Status::Confirmed
                        | Status::Completing
                        | Status::Completed
                        | Status::Discarding
                ) =>
            {
                if !NonZeroU32::new(target.pull_request_id)
                    .is_some_and(|id| self.attempted.contains(&id))
                    || target.platform != repository.platform
                    || target.organization != repository.organization
                    || target.repository != repository.repository
                    || target.project != repository.project
                    || target.collection_url != repository.collection_url
                {
                    return Err(fail(
                        "invalidState",
                        "Journal target does not match its repository or attempts",
                    ));
                }
                let mut expected = target.clone();
                expected.refresh_url();
                if expected.web_url != target.web_url {
                    return Err(fail(
                        "invalidState",
                        "Journal target URL disagrees with its coordinates",
                    ));
                }
                let marker = marker(&claim.body)
                    .ok_or_else(|| fail("invalidState", "Journal claim has no protocol marker"))?;
                let same_session = marker.session == self.session_id;
                let same_token = marker.token == claim.token;
                if !same_session
                    || !same_token
                    || marker.state
                        != match self.status {
                            Status::Claiming | Status::Claimed | Status::Discarding => {
                                ClaimState::Reserved
                            }
                            Status::Activating | Status::Confirmed => ClaimState::Active,
                            Status::Completing | Status::Completed => ClaimState::Completed,
                            Status::Selecting | Status::NoCandidate => {
                                return Err(fail("invalidState", "Unexpected claim state"));
                            }
                        }
                    || Uuid::parse_str(&claim.token).is_err()
                {
                    return Err(fail(
                        "invalidState",
                        "Journal claim ownership tokens disagree",
                    ));
                }
                if self.status == Status::Claiming {
                    if claim.id.is_some()
                        || claim.thread_id.is_some()
                        || claim.check_after.is_some()
                    {
                        return Err(fail(
                            "invalidState",
                            "Claiming journal contains completed remote identifiers",
                        ));
                    }
                } else {
                    if claim.id.as_deref().is_none_or(str::is_empty)
                        || (target.platform == Platform::AzureDevOps
                            && claim.thread_id.as_deref().is_none_or(str::is_empty))
                        || (target.platform == Platform::GitHub && claim.thread_id.is_some())
                    {
                        return Err(fail(
                            "invalidState",
                            "Journal claim lacks valid remote identifiers",
                        ));
                    }
                    timestamp(
                        claim.check_after.as_deref().ok_or_else(|| {
                            fail("invalidState", "Journal claim lacks checkAfter")
                        })?,
                    )?;
                }
            }
            _ => {
                return Err(fail(
                    "invalidState",
                    "Journal status and reservation fields disagree",
                ));
            }
        }
        Ok(())
    }
}

/// Keeps lifecycle evidence attached to a validated stage.
///
/// Unlike the wire record, its stage owns the matching target and claim data.
/// Ref: docs/implementation/journals.md, "Wire records and workflow stages".
struct Workflow {
    /// Owner used when constructing and checking remote protocol markers.
    session_id: String,
    /// Immutable scope inherited from the validated journal.
    repository_url: String,
    /// Assignment policy inherited from the validated journal.
    selection_mode: SelectionMode,
    /// Account authorized to manage this journal's claim.
    identity: Identity,
    /// Candidates excluded from subsequent attempts within this workflow.
    attempted: Vec<NonZeroU32>,
    /// Accumulated reasons that explain selection and confirmation decisions.
    diagnostics: Vec<Diagnostic>,
    /// Wire-record publication coordinate preserved across transitions.
    state_path: String,
    /// Current operation and the evidence that operation requires.
    stage: Stage,
}

impl TryFrom<State> for Workflow {
    /// Rejects malformed journal evidence before it can drive remote effects.
    type Error = AppError;

    /// Validates a wire record and assembles its stage as one complete value.
    fn try_from(state: State) -> Result<Self, AppError> {
        state.validate()?;
        let stage = match (state.status, state.target, state.claim) {
            (Status::Selecting, None, None) => Stage::Selecting,
            (Status::NoCandidate, None, None) => Stage::NoCandidate,
            (status, Some(target), Some(claim)) => {
                let intent = Intent {
                    target,
                    token: claim.token,
                    body: claim.body,
                };
                if status == Status::Claiming {
                    Stage::Claiming(intent)
                } else {
                    let reservation = Reservation {
                        intent,
                        id: claim
                            .id
                            .ok_or_else(|| fail("invalidState", "Missing claim identifier"))?,
                        thread_id: claim.thread_id,
                        check_after: timestamp(
                            &claim
                                .check_after
                                .ok_or_else(|| fail("invalidState", "Missing checkAfter"))?,
                        )?,
                    };
                    match status {
                        Status::Claimed => Stage::Claimed(reservation),
                        Status::Activating => Stage::Activating(reservation),
                        Status::Confirmed => Stage::Confirmed(reservation),
                        Status::Completing => Stage::Completing(reservation),
                        Status::Completed => Stage::Completed(reservation),
                        Status::Discarding => Stage::Discarding(reservation),
                        _ => return Err(fail("invalidState", "Unexpected reservation stage")),
                    }
                }
            }
            _ => {
                return Err(fail(
                    "invalidState",
                    "Journal status and reservation fields disagree",
                ));
            }
        };
        Ok(Self {
            session_id: state.session_id,
            repository_url: state.repository_url,
            selection_mode: state.selection_mode,
            identity: state.identity,
            attempted: state.attempted,
            diagnostics: state.diagnostics,
            state_path: state.state_path,
            stage,
        })
    }
}

impl Workflow {
    /// Preserves the existing flat schema only at output and journal boundaries.
    fn wire(&self) -> State {
        let (status, reservation) = match &self.stage {
            Stage::Selecting => (Status::Selecting, None),
            Stage::NoCandidate => (Status::NoCandidate, None),
            Stage::Claiming(intent) => (Status::Claiming, Some((intent, None))),
            Stage::Claimed(reservation) => (
                Status::Claimed,
                Some((&reservation.intent, Some(reservation))),
            ),
            Stage::Activating(reservation) => (
                Status::Activating,
                Some((&reservation.intent, Some(reservation))),
            ),
            Stage::Confirmed(reservation) => (
                Status::Confirmed,
                Some((&reservation.intent, Some(reservation))),
            ),
            Stage::Completing(reservation) => (
                Status::Completing,
                Some((&reservation.intent, Some(reservation))),
            ),
            Stage::Completed(reservation) => (
                Status::Completed,
                Some((&reservation.intent, Some(reservation))),
            ),
            Stage::Discarding(reservation) => (
                Status::Discarding,
                Some((&reservation.intent, Some(reservation))),
            ),
        };
        State {
            schema_version: 3,
            session_id: self.session_id.clone(),
            repository_url: self.repository_url.clone(),
            selection_mode: self.selection_mode,
            identity: self.identity.clone(),
            status,
            target: reservation.map(|(intent, _)| intent.target.clone()),
            claim: reservation.map(|(intent, created)| Claim {
                token: intent.token.clone(),
                body: intent.body.clone(),
                id: created.map(|created| created.id.clone()),
                thread_id: created.and_then(|created| created.thread_id.clone()),
                check_after: created.map(|created| {
                    created
                        .check_after
                        .strftime("%Y-%m-%dT%H:%M:%S%.f%:z")
                        .to_string()
                }),
            }),
            attempted: self.attempted.clone(),
            diagnostics: self.diagnostics.clone(),
            state_path: self.state_path.clone(),
        }
    }

    /// Publishes the current complete stage through the transaction's storage.
    fn save(&self, journal: &mut dyn Journal) -> Result<(), AppError> {
        journal.save(&self.wire())
    }

    /// Attaches a decision reason without changing reservation ownership.
    fn record(&mut self, id: NonZeroU32, reason: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            pull_request_id: id,
            reason: reason.into(),
        });
    }
}

/// Keeps required reservation evidence attached to each lifecycle stage.
enum Stage {
    /// Searches without a current reservation; attempts still exclude prior work.
    Selecting,
    /// Records intent before creation; recovery must reconcile an interrupted write.
    Claiming(Intent),
    /// Holds the created reservation that a later invocation may confirm.
    Claimed(Reservation),
    /// Records activation intent before updating the observable claim.
    Activating(Reservation),
    /// Records the successful sole-live-claim check for skill handoff.
    Confirmed(Reservation),
    /// Records completion intent before updating the observable claim.
    Completing(Reservation),
    /// Records the completed review marker used by later selection.
    Completed(Reservation),
    /// Records cleanup intent before deleting an ownership-checked comment.
    Discarding(Reservation),
    /// Completes enumeration without eligible unattempted work.
    NoCandidate,
}

/// Identifies the comment to create before any remote identifiers exist.
struct Intent {
    /// Concrete PR whose discussion will carry the claim.
    target: Target,
    /// Fresh UUID distinguishing this attempt from all other claims.
    token: String,
    /// Caller announcement plus the session/token ownership marker.
    body: String,
}

/// Supplies confirmation with observed remote identifiers and a parsed deadline.
struct Reservation {
    /// Original target and marker retained for ownership verification.
    intent: Intent,
    /// Service comment identifier returned by successful creation.
    id: String,
    /// Azure comment container, absent for GitHub issue comments.
    thread_id: Option<String>,
    /// Earliest confirmation instant, shared across separate invocations.
    check_after: Timestamp,
}

/// Interrupted remote transitions are distinct from successful completion states.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Status {
    /// Search is in progress and has no attached reservation.
    Selecting,
    /// Creation intent is durable, but its remote outcome is not recorded.
    Claiming,
    /// Creation succeeded and the preparation deadline is available.
    Claimed,
    /// The created claim is being changed into a handoff reservation.
    Activating,
    /// Confirmation succeeded and the caller may perform its existing handoff.
    Confirmed,
    /// The active reservation is being changed into completion evidence.
    Completing,
    /// The review completed and later selection may observe that evidence.
    Completed,
    /// Deletion intent is durable, but cleanup may need reconciliation.
    Discarding,
    /// Search completed successfully without a claim.
    NoCandidate,
}

/// Holds ownership evidence and the post-creation preparation deadline on the wire.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Claim {
    /// UUID shared with the marker so ownership does not depend on prose.
    pub(crate) token: String,
    /// Exact announcement and marker persisted before attempting creation.
    pub(crate) body: String,
    /// Created comment identifier; explicit null while creation is only intended.
    #[serde(deserialize_with = "required_optional")]
    pub(crate) id: Option<String>,
    /// Azure thread identifier, null for GitHub or before creation.
    #[serde(deserialize_with = "required_optional")]
    pub(crate) thread_id: Option<String>,
    /// Offset-bearing preparation deadline, absent until creation succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) check_after: Option<String>,
}

/// Explains each discarded candidate and the final successful reservation check.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Diagnostic {
    /// Concrete candidate whose treatment the reason explains.
    pub(crate) pull_request_id: NonZeroU32,
    /// Workflow reason code, independent of human error diagnostics.
    pub(crate) reason: String,
}

/// Supplies only the remote operations required by the claim transaction.
///
/// Implementations provide complete evidence or an error; they do not turn
/// missing service data into successful empty work. The workflow owns ordering,
/// journal intent, claim ownership checks, and whether to try another candidate.
pub(crate) trait Provider {
    /// Enumerates mode-specific PR candidates in the explicit repository scope.
    fn targets(
        &mut self,
        repository: &Target,
        identity: &Identity,
        selection_mode: SelectionMode,
    ) -> Result<Vec<Target>, AppError>;
    /// Evaluates non-claim eligibility against the supplied account and instant.
    fn candidate(
        &mut self,
        target: &Target,
        identity: &Identity,
        now: Timestamp,
        selection_mode: SelectionMode,
    ) -> Result<Candidate, AppError>;
    /// Supplies all undeleted discussion evidence needed to discover live claims.
    fn comments(&mut self, target: &Target) -> Result<Vec<Comment>, AppError>;
    /// Performs one journaled creation and returns its remote ownership coordinates.
    fn create_claim(&mut self, target: &Target, body: &str) -> Result<RemoteClaim, AppError>;
    /// Supplies current reviewer-participation evidence independently of eligibility.
    fn review_fingerprint(
        &mut self,
        target: &Target,
        identity: &Identity,
    ) -> Result<String, AppError>;
    /// Changes a caller-verified claim marker without replacing its remote identity.
    fn update_claim(
        &mut self,
        target: &Target,
        claim: &RemoteClaim,
        body: &str,
    ) -> Result<RemoteClaim, AppError>;
    /// Removes the caller-verified owned comment without deleting its thread.
    fn remove_claim(&mut self, target: &Target, comment: &Comment) -> Result<(), AppError>;
}

/// Separates workflow persistence from the storage used by a whole transaction.
///
/// The caller must retain transaction exclusion across every load, save, wait,
/// and remote effect. Individual storage calls are not independent transactions.
pub(crate) trait Journal {
    /// Returns a complete validated record, or absence for fresh selection only.
    fn load(&mut self) -> Result<Option<State>, AppError>;
    /// Validates and publishes the entire record before the next workflow effect.
    fn save(&mut self, state: &State) -> Result<(), AppError>;
}

/// Eligibility snapshot; claims are checked separately before reservation.
///
/// Selection ranks snapshots, then asks the provider for fresh evidence before
/// claiming. An earlier eligible snapshot alone never authorizes a write.
pub(crate) struct Candidate {
    /// Concrete PR whose evidence produced this snapshot.
    pub(crate) target: Target,
    /// Exclusion reason; absence means the non-claim eligibility checks passed.
    pub(crate) reason: Option<String>,
    /// Latest non-future commit instant used for deterministic candidate ranking.
    pub(crate) latest: Option<Timestamp>,
    /// Current source revision bound into completion evidence.
    pub(crate) source_commit: Option<String>,
    /// Current reviewer participation bound into completion evidence.
    pub(crate) review_fingerprint: Option<String>,
}

/// Normalized discussion evidence includes resolved ADO threads.
#[derive(Clone, Debug)]
pub(crate) struct Comment {
    /// Service comment identifier used with the thread coordinate for ownership.
    pub(crate) id: String,
    /// Azure thread identifier; resolved threads still contribute claim evidence.
    pub(crate) thread_id: Option<String>,
    /// Discussion text inspected for a complete protocol-marker line.
    pub(crate) body: String,
    /// Service creation timestamp, not edit time, anchoring claim expiration.
    pub(crate) created_at: String,
    /// Account evidence needed before deleting the journal's recorded comment.
    pub(crate) author: Identity,
}

/// Identifiers returned by a successfully observed claim creation.
pub(crate) struct RemoteClaim {
    /// Newly observed comment identifier to persist before returning success.
    pub(crate) id: String,
    /// Azure container identifier required to locate the comment again.
    pub(crate) thread_id: Option<String>,
}

/// Fixed protocol syntax is shared by creation, discovery, and ownership checks.
static CLAIM_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"(?m)^<!-- ss-review-proactor:v3 session=([A-Za-z0-9._:-]+) claim=([0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}) state=(reserved|active|completed) revision=([A-Za-z0-9._:-]+) review=([0-9a-f]{64}) -->\r?$"
).expect("fixed claim marker pattern")
});

/// Performs selection or confirmation while the caller retains its journal lock.
///
/// Fresh selection requires an absent journal. Confirmation validates persisted
/// ownership, waits only the remaining preparation interval, and checks once.
/// If the reservation is discarded, selection resumes without revisiting
/// attempted candidates. Intent is saved before each remote write and the
/// observed outcome before returning the complete wire record.
///
/// Errors leave reconciliation evidence in the last published stage; callers
/// must not interpret interrupted stages as authorization to replay writes.
pub(crate) fn run(
    provider: &mut dyn Provider,
    journal: &mut dyn Journal,
    clock: &dyn Clock,
    initial: State,
    message: &str,
    confirm: bool,
) -> Result<State, AppError> {
    if !valid_session(&initial.session_id) || message.trim().is_empty() {
        return Err(fail(
            "invalidInput",
            "Supply a valid session identifier and nonempty claim announcement",
        ));
    }
    let mut initial = Workflow::try_from(initial)?;
    let repository = Target::repository(&initial.repository_url)?;
    let loaded = journal.load()?;
    if confirm {
        let state =
            loaded.ok_or_else(|| fail("invalidState", "No journal exists for confirmation"))?;
        let state = Workflow::try_from(state)?;
        if state.session_id != initial.session_id
            || state.repository_url != initial.repository_url
            || state.selection_mode != initial.selection_mode
            || !state.identity.same_account(&initial.identity)
        {
            return Err(fail(
                "invalidState",
                "The journal does not match this session, repository, selection mode, or authenticated identity",
            ));
        }
        initial = confirm_claim(provider, journal, clock, state)?;
        if matches!(initial.stage, Stage::Confirmed(_)) {
            return Ok(initial.wire());
        }
    } else {
        if loaded.is_some() {
            return Err(fail(
                "invalidState",
                "The journal already exists; confirm its selection or obtain human reconciliation",
            ));
        }
        initial.save(journal)?;
    }
    let mut candidates = Vec::new();
    for target in provider.targets(&repository, &initial.identity, initial.selection_mode)? {
        let id = candidate_id(&target)?;
        if initial.attempted.contains(&id) {
            continue;
        }
        let candidate = provider.candidate(
            &target,
            &initial.identity,
            clock.now(),
            initial.selection_mode,
        )?;
        if let Some(reason) = candidate.reason {
            initial.record(id, reason);
        } else {
            candidates.push(candidate);
        }
    }
    candidates.sort_by(|left, right| {
        right.latest.cmp(&left.latest).then_with(|| {
            left.target
                .pull_request_id
                .cmp(&right.target.pull_request_id)
        })
    });
    for candidate in candidates {
        let target = candidate.target;
        let id = candidate_id(&target)?;
        initial.attempted.push(id);
        let fresh = provider.candidate(
            &target,
            &initial.identity,
            clock.now(),
            initial.selection_mode,
        )?;
        if let Some(reason) = fresh.reason {
            initial.record(id, reason);
            continue;
        }
        let comments = provider.comments(&target)?;
        if comments.iter().any(|comment| {
            comment.author.same_account(&initial.identity)
                && marker(&comment.body).is_some_and(|marker| {
                    marker.state == ClaimState::Completed
                        && fresh.source_commit.as_deref() == Some(marker.revision)
                        && fresh.review_fingerprint.as_deref() == Some(marker.review_fingerprint)
                })
        }) {
            initial.record(id, "completedReview");
            continue;
        }
        let mut has_live_claim = false;
        for comment in &comments {
            // Validate every claim timestamp, even after discovering a live reservation.
            has_live_claim |= live(comment, clock.now())?;
        }
        if has_live_claim {
            initial.record(id, "liveClaim");
            continue;
        }
        let token = Uuid::new_v4().to_string();
        let source_commit = fresh
            .source_commit
            .as_deref()
            .ok_or_else(|| fail("service", "Candidate has no source revision"))?;
        let review_fingerprint = fresh.review_fingerprint.as_deref().ok_or_else(|| {
            fail(
                "service",
                "Candidate has no reviewer participation evidence",
            )
        })?;
        let body = claim_body(
            message,
            &initial.session_id,
            &token,
            ClaimState::Reserved,
            source_commit,
            review_fingerprint,
        );
        initial.stage = Stage::Claiming(Intent {
            target: target.clone(),
            token,
            body: body.clone(),
        });
        initial.save(journal)?;
        let created = provider.create_claim(&target, &body)?;
        let deadline = clock.now().checked_add(PREPARATION_AGE).map_err(|error| {
            crate::error::caused_by("ambiguousWrite", "Claim deadline overflow", error)
        })?;
        let Stage::Claiming(intent) = initial.stage else {
            return Err(fail("invalidState", "Claim intent disappeared"));
        };
        initial.stage = Stage::Claimed(Reservation {
            intent,
            id: created.id,
            thread_id: created.thread_id,
            check_after: deadline,
        });
        initial.save(journal)?;
        return Ok(initial.wire());
    }
    initial.stage = Stage::NoCandidate;
    initial.save(journal)?;
    Ok(initial.wire())
}

/// Confirms once, or journals and removes only the ownership-checked reservation.
///
/// Only a created claim can enter this phase. The result is either confirmed
/// for handoff or selecting after safe cleanup; the outer operation owns finding
/// a replacement candidate. Conflicting ownership stops before deletion.
fn confirm_claim(
    provider: &mut dyn Provider,
    journal: &mut dyn Journal,
    clock: &dyn Clock,
    mut state: Workflow,
) -> Result<Workflow, AppError> {
    let Stage::Claimed(claim) = state.stage else {
        return Err(fail(
            "invalidState",
            "Cannot confirm this journal status; human reconciliation is required",
        ));
    };
    let target = &claim.intent.target;
    let remaining = claim.check_after.duration_since(clock.now());
    if let Ok(duration) = Duration::try_from(remaining)
        && !duration.is_zero()
    {
        // Preserve the whole-second ceiling of the existing preparation interval.
        let seconds = duration
            .as_secs()
            .saturating_add(u64::from(duration.subsec_nanos() > 0));
        clock.wait(Duration::from_secs(seconds))?;
    }
    let candidate =
        provider.candidate(target, &state.identity, clock.now(), state.selection_mode)?;
    let comments = provider.comments(target)?;
    let mut own = comments
        .iter()
        .filter(|comment| comment.id == claim.id && comment.thread_id == claim.thread_id);
    let owned = own.next();
    if own.next().is_some() {
        return Err(fail(
            "service",
            "The service returned duplicate claim comment identifiers",
        ));
    }
    if let Some(comment) = owned
        && (!comment.author.same_account(&state.identity)
            || marker(&comment.body).is_none_or(|marker| {
                marker.session != state.session_id
                    || marker.token != claim.intent.token
                    || marker.state != ClaimState::Reserved
            }))
    {
        return Err(fail(
            "invalidState",
            "The recorded claim no longer matches its owner and tokens; human reconciliation is required",
        ));
    }
    let mut live_count = 0_usize;
    let mut own_live = false;
    for comment in &comments {
        if live(comment, clock.now())? {
            live_count = live_count.saturating_add(1);
            own_live |= comment.id == claim.id && comment.thread_id == claim.thread_id;
        }
    }
    let reason = candidate
        .reason
        .or_else(|| (!own_live).then(|| "claimMissingOrExpired".to_owned()))
        .or_else(|| (live_count > 1).then(|| "competingClaims".to_owned()));
    let id = candidate_id(target)?;
    if let Some(reason) = reason {
        let target = target.clone();
        state.stage = Stage::Discarding(claim);
        state.record(id, reason);
        state.save(journal)?;
        if let Some(comment) = owned {
            provider.remove_claim(&target, comment)?;
        }
        state.stage = Stage::Selecting;
    } else {
        let target = target.clone();
        let body = transition_claim(&claim.intent.body, ClaimState::Active, None, None)?;
        let remote = RemoteClaim {
            id: claim.id.clone(),
            thread_id: claim.thread_id.clone(),
        };
        let mut claim = claim;
        claim.intent.body.clone_from(&body);
        state.stage = Stage::Activating(claim);
        state.save(journal)?;
        let updated = provider.update_claim(&target, &remote, &body)?;
        if updated.id != remote.id || updated.thread_id != remote.thread_id {
            return Err(fail(
                "ambiguousWrite",
                "Claim activation returned different remote identifiers",
            ));
        }
        let Stage::Activating(claim) = state.stage else {
            return Err(fail("invalidState", "Activation intent disappeared"));
        };
        state.stage = Stage::Confirmed(claim);
        state.record(id, "soleLiveClaimAfterPreparation");
    }
    state.save(journal)?;
    Ok(state)
}

/// Verifies that a confirmed reservation still controls the pull request.
pub(crate) fn check(
    provider: &mut dyn Provider,
    journal: &mut dyn Journal,
    clock: &dyn Clock,
    initial: State,
    publish_to_closed_pull_request: bool,
) -> Result<State, AppError> {
    let mut state = owned_workflow(journal, initial)?;
    let Stage::Confirmed(reservation) = &state.stage else {
        return Err(fail(
            "invalidState",
            "Publication reservation is not confirmed",
        ));
    };
    let id = candidate_id(&reservation.intent.target)?;
    // Reviewer registration intentionally changes an available candidate's
    // assignment state after handoff; that expected transition is not stale work.
    if let Some(reason) = provider
        .candidate(
            &reservation.intent.target,
            &state.identity,
            clock.now(),
            state.selection_mode,
        )?
        .reason
        .filter(|reason| {
            !(state.selection_mode == SelectionMode::Available && reason == "alreadyReviewer")
                && !(publish_to_closed_pull_request && reason == "closed")
        })
    {
        return Err(fail(
            "invalidState",
            format!("Publication reservation is no longer eligible: {reason}"),
        ));
    }
    let comments = provider.comments(&reservation.intent.target)?;
    verify_active_claim(&state, reservation, &comments, clock.now())?;
    state.record(id, "activeClaimVerifiedBeforePublication");
    state.save(journal)?;
    Ok(state.wire())
}

/// Marks a successfully completed review as observable completion evidence.
pub(crate) fn complete(
    provider: &mut dyn Provider,
    journal: &mut dyn Journal,
    clock: &dyn Clock,
    initial: State,
    reviewed_source_commit: &str,
) -> Result<State, AppError> {
    if !valid_revision(reviewed_source_commit) {
        return Err(fail(
            "invalidInput",
            "Supply the final reviewed source commit from the review artifacts",
        ));
    }
    let mut state = owned_workflow(journal, initial)?;
    let Stage::Confirmed(reservation) = &state.stage else {
        return Err(fail(
            "invalidState",
            "Completed review reservation is not confirmed",
        ));
    };
    let target = reservation.intent.target.clone();
    let id = candidate_id(&target)?;
    let remote = RemoteClaim {
        id: reservation.id.clone(),
        thread_id: reservation.thread_id.clone(),
    };
    let comments = provider.comments(&target)?;
    verify_active_claim(&state, reservation, &comments, clock.now())?;
    let review_fingerprint =
        provider.review_fingerprint(&reservation.intent.target, &state.identity)?;
    let body = transition_claim(
        &reservation.intent.body,
        ClaimState::Completed,
        Some(reviewed_source_commit),
        Some(&review_fingerprint),
    )?;
    let Stage::Confirmed(mut reservation) = state.stage else {
        return Err(fail("invalidState", "Confirmed reservation disappeared"));
    };
    reservation.intent.body.clone_from(&body);
    state.stage = Stage::Completing(reservation);
    state.save(journal)?;
    let updated = provider.update_claim(&target, &remote, &body)?;
    if updated.id != remote.id || updated.thread_id != remote.thread_id {
        return Err(fail(
            "ambiguousWrite",
            "Claim completion returned different remote identifiers",
        ));
    }
    let Stage::Completing(reservation) = state.stage else {
        return Err(fail("invalidState", "Completion intent disappeared"));
    };
    state.stage = Stage::Completed(reservation);
    state.record(id, "reviewCompleted");
    state.save(journal)?;
    Ok(state.wire())
}

/// Loads ownership evidence without treating malformed journals as absent work.
fn owned_workflow(journal: &mut dyn Journal, initial: State) -> Result<Workflow, AppError> {
    let initial = Workflow::try_from(initial)?;
    let loaded = journal
        .load()?
        .ok_or_else(|| fail("invalidState", "No journal exists for this operation"))?;
    let state = Workflow::try_from(loaded)?;
    if state.session_id != initial.session_id
        || state.repository_url != initial.repository_url
        || state.selection_mode != initial.selection_mode
        || !state.identity.same_account(&initial.identity)
    {
        return Err(fail(
            "invalidState",
            "The journal does not match this session, repository, selection mode, or authenticated identity",
        ));
    }
    Ok(state)
}

/// Verifies the active claim and rejects concurrent observable reservations.
fn verify_active_claim(
    state: &Workflow,
    reservation: &Reservation,
    comments: &[Comment],
    now: Timestamp,
) -> Result<(), AppError> {
    let mut own = comments.iter().filter(|comment| {
        comment.id == reservation.id && comment.thread_id == reservation.thread_id
    });
    let owned = own.next();
    if own.next().is_some() {
        return Err(fail(
            "service",
            "The service returned duplicate claim comment identifiers",
        ));
    }
    let owned = owned.ok_or_else(|| {
        fail(
            "invalidState",
            "The active review reservation is missing; human reconciliation is required",
        )
    })?;
    if !owned.author.same_account(&state.identity)
        || marker(&owned.body).is_none_or(|marker| {
            marker.session != state.session_id
                || marker.token != reservation.intent.token
                || marker.state != ClaimState::Active
        })
    {
        return Err(fail(
            "invalidState",
            "The active review reservation no longer matches its owner and tokens",
        ));
    }
    let mut live_count = 0_usize;
    for comment in comments {
        if live(comment, now)? {
            live_count = live_count.saturating_add(1);
        }
    }
    if live_count != 1 {
        return Err(fail(
            "invalidState",
            "The active review reservation is contested",
        ));
    }
    Ok(())
}

/// Repository-only targets use zero, but attempts and diagnostics require a concrete PR.
fn candidate_id(target: &Target) -> Result<NonZeroU32, AppError> {
    NonZeroU32::new(target.pull_request_id)
        .ok_or_else(|| fail("service", "Missing pull request identifier"))
}

/// Restricts session tokens to the syntax shared by journal and marker validation.
pub(crate) fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

/// Restricts source-revision values to the protocol marker's safe character set.
fn valid_revision(revision: &str) -> bool {
    !revision.is_empty()
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

/// Extracts ownership tokens only from a complete protocol line, not prose fragments.
fn marker(body: &str) -> Option<ClaimMarker<'_>> {
    let captures = CLAIM_MARKER.captures(body)?;
    let state = match captures.get(3)?.as_str() {
        "reserved" => ClaimState::Reserved,
        "active" => ClaimState::Active,
        "completed" => ClaimState::Completed,
        _ => return None,
    };
    Some(ClaimMarker {
        session: captures.get(1)?.as_str(),
        token: captures.get(2)?.as_str(),
        state,
        revision: captures.get(4)?.as_str(),
        review_fingerprint: captures.get(5)?.as_str(),
    })
}

/// Constructs one complete protocol marker with caller-approved prose.
fn claim_body(
    message: &str,
    session: &str,
    token: &str,
    state: ClaimState,
    revision: &str,
    review_fingerprint: &str,
) -> String {
    format!(
        "{message}\n\n<!-- ss-review-proactor:v3 session={session} claim={token} state={} revision={revision} review={review_fingerprint} -->",
        state.label()
    )
}

/// Changes only the protocol marker while preserving the announced prose.
fn transition_claim(
    body: &str,
    state: ClaimState,
    revision: Option<&str>,
    review_fingerprint: Option<&str>,
) -> Result<String, AppError> {
    let marker =
        marker(body).ok_or_else(|| fail("invalidState", "Claim has no protocol marker"))?;
    Ok(CLAIM_MARKER
        .replace(
            body,
            format!(
                "<!-- ss-review-proactor:v3 session={} claim={} state={} revision={} review={} -->",
                marker.session,
                marker.token,
                state.label(),
                revision.unwrap_or(marker.revision),
                review_fingerprint.unwrap_or(marker.review_fingerprint),
            ),
        )
        .into_owned())
}

/// Interprets marker state as a live reservation.
///
/// Active claims remain live through the handoff. Reserved claims use service
/// creation time so future-dated evidence remains reserved under clock skew.
/// Completed and unmarked comments do not reserve work.
fn live(comment: &Comment, now: Timestamp) -> Result<bool, AppError> {
    let Some(marker) = marker(&comment.body) else {
        return Ok(false);
    };
    if marker.state == ClaimState::Active {
        return Ok(true);
    }
    if marker.state == ClaimState::Completed {
        return Ok(false);
    }
    let created = timestamp(&comment.created_at)?;
    let expires = created
        .checked_add(CLAIM_AGE)
        .map_err(|error| crate::error::caused_by("service", "Claim lifetime overflow", error))?;
    Ok(expires > now)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn checked_stages_preserve_schema_three_records() {
        for platform in [Platform::GitHub, Platform::AzureDevOps] {
            let repository = Target::repository(if platform == Platform::GitHub {
                "https://github.com/example/widgets"
            } else {
                "https://dev.azure.com/org/project/_git/widgets"
            })
            .unwrap();
            let identity =
                Identity::from_account(platform, &json!({"id":"42","login":"reviewer"})).unwrap();
            for status in [
                Status::Selecting,
                Status::Claiming,
                Status::Claimed,
                Status::Activating,
                Status::Confirmed,
                Status::Completing,
                Status::Completed,
                Status::Discarding,
                Status::NoCandidate,
            ] {
                let path = if cfg!(windows) {
                    r"C:\session\state.json"
                } else {
                    "/session/state.json"
                };
                let mut state = State::new(
                    &repository,
                    &identity,
                    "session",
                    path,
                    SelectionMode::Available,
                );
                state.status = status;
                if !matches!(status, Status::Selecting | Status::NoCandidate) {
                    let id = NonZeroU32::new(10).unwrap();
                    let created = status != Status::Claiming;
                    state.target = Some(repository.candidate(id));
                    state.attempted.push(id);
                    state.diagnostics.push(Diagnostic {
                        pull_request_id: id,
                        reason: "evidence".to_owned(),
                    });
                    let claim_state = match status {
                        Status::Claiming | Status::Claimed | Status::Discarding => "reserved",
                        Status::Activating | Status::Confirmed => "active",
                        Status::Completing | Status::Completed => "completed",
                        Status::Selecting | Status::NoCandidate => unreachable!(),
                    };
                    state.claim = Some(Claim {
                        token: "aaaaaaaa-1234-1234-1234-123456789abc".to_owned(),
                        body: format!(
                            "<!-- ss-review-proactor:v3 session=session claim=aaaaaaaa-1234-1234-1234-123456789abc state={claim_state} revision=source-10 review=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa -->"
                        ),
                        id: created.then(|| "comment".to_owned()),
                        thread_id: (created && platform == Platform::AzureDevOps)
                            .then(|| "thread".to_owned()),
                        check_after: created.then(|| "2026-09-28T10:01:00+00:00".to_owned()),
                    });
                }
                let mut contradictory = state.clone();
                contradictory.status = if matches!(status, Status::Selecting | Status::NoCandidate)
                {
                    Status::Claimed
                } else {
                    Status::Selecting
                };
                assert!(Workflow::try_from(contradictory).is_err());
                let expected = serde_json::to_value(&state).unwrap();
                let converted = Workflow::try_from(state).unwrap().wire();
                converted.validate().unwrap();
                assert_eq!(serde_json::to_value(converted).unwrap(), expected);
                for property in ["attempted", "diagnostics"] {
                    let mut invalid = expected.clone();
                    *invalid.get_mut(property).unwrap() = if property == "attempted" {
                        json!([0])
                    } else {
                        json!([{"pullRequestId":0,"reason":"invalid"}])
                    };
                    serde_json::from_value::<State>(invalid).unwrap_err();
                }
                let mut incompatible = expected.clone();
                *incompatible.get_mut("schemaVersion").unwrap() = json!(2);
                let incompatible = serde_json::from_value::<State>(incompatible).unwrap();
                assert!(incompatible.validate().is_err());
            }
        }
    }
}
