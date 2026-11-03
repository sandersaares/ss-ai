//! Persistent assessment limits exclude completed-worker observation delays.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Clock, error::fail};
use ohno::AppError;

/// The per-PR target includes the separately bounded provider freshness phase.
const TOTAL_MILLISECONDS: i64 = 9 * 60 * 1_000;
/// Bound freshness and publication without charging completed-worker idle time.
const PUBLICATION_RESERVE_MILLISECONDS: i64 = 60 * 1_000;
/// Batching permits the incident-shaped comparison with substantial retry headroom.
const WORK_COMMAND_LIMIT: usize = 256;
/// Limit all worker reservations, including stale recovery and replacement.
const WORKER_LIMIT: usize = 3;

/// A durable limit applies across interruption and stale-evidence recovery.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AssessmentBudget {
    started_at_milliseconds: i64,
    last_observed_milliseconds: i64,
    #[serde(deserialize_with = "crate::json::required_optional")]
    completed_at_milliseconds: Option<i64>,
    excluded_idle_milliseconds: i64,
    work_commands: usize,
    #[serde(default)]
    worker_reservations: usize,
    #[serde(default)]
    evidence_pages: usize,
    #[serde(default)]
    delivered_text_bytes: usize,
    #[serde(default)]
    read_batches: usize,
    #[serde(default)]
    judgment_batches: usize,
    exhaustion: Option<ExhaustionReason>,
}

/// Persist the cause so an interrupted run retains an actionable diagnostic.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum ExhaustionReason {
    TimeLimit,
    CommandLimit,
    WorkerLimit,
    ClockMovedBackwards,
}

/// Distinguishes an exhausted budget from a clock that cannot establish elapsed time.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BudgetStatus {
    Available,
    Exhausted,
    ClockMovedBackwards,
}

/// Snapshot of the deadline used immediately before a new publication intent.
#[derive(Clone, Copy)]
pub(crate) struct EffectDeadline {
    started_at_milliseconds: i64,
}

impl EffectDeadline {
    /// Preflight may consume the reserved time, so recheck before saving intent.
    pub(crate) fn check(self, clock: &dyn Clock) -> Result<(), AppError> {
        let now = clock.now().as_millisecond();
        if now
            .checked_sub(self.started_at_milliseconds)
            .is_none_or(|elapsed| !(0..PUBLICATION_RESERVE_MILLISECONDS).contains(&elapsed))
        {
            return Err(fail(
                "publicationTimeout",
                "Publication freshness window expired or its clock moved backwards; no new effect was attempted",
            ));
        }
        Ok(())
    }
}

impl AssessmentBudget {
    /// Starts before inspection so source capture is part of the end-to-end budget.
    pub(crate) fn new(now_milliseconds: i64) -> Self {
        Self {
            started_at_milliseconds: now_milliseconds,
            last_observed_milliseconds: now_milliseconds,
            completed_at_milliseconds: None,
            excluded_idle_milliseconds: 0,
            work_commands: 0,
            worker_reservations: 0,
            evidence_pages: 0,
            delivered_text_bytes: 0,
            read_batches: 0,
            judgment_batches: 0,
            exhaustion: None,
        }
    }

    /// Rejects records that cannot represent a real sequence of budget observations.
    pub(crate) fn valid(&self, assessment_complete: bool) -> bool {
        self.started_at_milliseconds <= self.last_observed_milliseconds
            && self
                .last_observed_milliseconds
                .checked_sub(self.started_at_milliseconds)
                .is_some_and(|elapsed| {
                    self.excluded_idle_milliseconds >= 0
                        && self.excluded_idle_milliseconds <= elapsed
                })
            && self.completed_at_milliseconds.is_none_or(|completed| {
                assessment_complete
                    && completed == self.last_observed_milliseconds
                    && self.elapsed(completed).is_some_and(|elapsed| {
                        elapsed < TOTAL_MILLISECONDS - PUBLICATION_RESERVE_MILLISECONDS
                    })
            })
            && self.work_commands <= WORK_COMMAND_LIMIT
            && self.worker_reservations <= WORKER_LIMIT
    }

    /// Exhaustion remains incomplete even if an earlier judgment exists.
    pub(crate) fn exhausted(&self) -> bool {
        self.exhaustion.is_some()
    }

    /// Reports only measurements obtained by the native command boundary.
    pub(crate) fn diagnostics(&self) -> Value {
        json!({
            "startedAtMilliseconds": self.started_at_milliseconds,
            "lastObservedMilliseconds": self.last_observed_milliseconds,
            "observedElapsedMilliseconds": self.elapsed(self.last_observed_milliseconds),
            "completedAtMilliseconds": self.completed_at_milliseconds,
            "excludedIdleMilliseconds": self.excluded_idle_milliseconds,
            "workCommands": self.work_commands,
            "workCommandLimit": WORK_COMMAND_LIMIT,
            "totalMilliseconds": TOTAL_MILLISECONDS,
            "publicationReserveMilliseconds": PUBLICATION_RESERVE_MILLISECONDS,
            "exhausted": self.exhausted(),
            "diagnostic": self.exhaustion,
            "modelRequests": null,
            "workerReservations": self.worker_reservations,
            "workerLimit": WORKER_LIMIT,
            "evidencePagesProduced": self.evidence_pages,
            "deliveredTextBytesProduced": self.delivered_text_bytes,
            "readBatches": self.read_batches,
            "judgmentBatches": self.judgment_batches,
            "uniqueEvidenceBytes": null,
            "repeatedEvidenceBytes": null,
            "workerCount": null
        })
    }

    /// Consumes a native assessment command without relying on model-call estimates.
    pub(crate) fn begin_work(&mut self, now_milliseconds: i64) -> BudgetStatus {
        let status = self.observe(now_milliseconds);
        if status != BudgetStatus::Available {
            return status;
        }
        if self.work_commands >= WORK_COMMAND_LIMIT {
            self.exhaustion = Some(ExhaustionReason::CommandLimit);
            return BudgetStatus::Exhausted;
        }
        self.work_commands = self.work_commands.saturating_add(1);
        BudgetStatus::Available
    }

    /// Accounts for time spent inside a command before its result becomes usable.
    pub(crate) fn finish_work(&mut self, now_milliseconds: i64) -> BudgetStatus {
        self.observe(now_milliseconds)
    }

    /// Freeze only after native semantic completion and its final time check succeed.
    pub(crate) fn complete(&mut self) {
        if !self.exhausted() {
            self.completed_at_milliseconds = Some(self.last_observed_milliseconds);
        }
    }

    /// Stale recovery resumes the retained budget without charging completed idle time.
    pub(crate) fn resume(&mut self, now_milliseconds: i64) -> BudgetStatus {
        if self.exhausted() {
            return BudgetStatus::Exhausted;
        }
        if let Some(completed) = self.completed_at_milliseconds {
            let Some(idle) = now_milliseconds
                .checked_sub(completed)
                .filter(|idle| *idle >= 0)
            else {
                self.exhaustion = Some(ExhaustionReason::ClockMovedBackwards);
                return BudgetStatus::ClockMovedBackwards;
            };
            let Some(excluded) = self.excluded_idle_milliseconds.checked_add(idle) else {
                self.exhaustion = Some(ExhaustionReason::ClockMovedBackwards);
                return BudgetStatus::ClockMovedBackwards;
            };
            self.excluded_idle_milliseconds = excluded;
            self.completed_at_milliseconds = None;
        }
        self.begin_work(now_milliseconds)
    }

    /// Reserve before dispatch so a crash cannot reset the worker limit.
    pub(crate) fn reserve_worker(&mut self) -> BudgetStatus {
        if self.exhausted() {
            return BudgetStatus::Exhausted;
        }
        if self.worker_reservations >= WORKER_LIMIT {
            self.exhaustion = Some(ExhaustionReason::WorkerLimit);
            return BudgetStatus::Exhausted;
        }
        self.worker_reservations = self.worker_reservations.saturating_add(1);
        BudgetStatus::Available
    }

    /// A finalizer cannot turn a timely completed assessment into a slow worker.
    pub(crate) fn before_publication(&mut self, now_milliseconds: i64) -> BudgetStatus {
        self.observe(now_milliseconds)
    }

    /// Bound fresh provider checks independently of time spent awaiting the parent.
    pub(crate) fn effect_deadline(&self, now_milliseconds: i64) -> EffectDeadline {
        EffectDeadline {
            // A finalizer's clock must not precede the worker's retained observation.
            started_at_milliseconds: now_milliseconds.max(self.last_observed_milliseconds),
        }
    }

    /// Count native-produced text pages, including replays, rather than model comprehension.
    pub(crate) fn record_evidence(
        &mut self,
        response: &Value,
        batch: bool,
    ) -> Result<(), AppError> {
        let pages = if batch {
            self.read_batches = self.read_batches.saturating_add(1);
            response
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| fail("invalidState", "Evidence batch omitted its pages"))?
                .iter()
                .collect::<Vec<_>>()
        } else {
            vec![response]
        };
        for page in pages {
            let text = page
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| fail("invalidState", "Evidence page omitted its text"))?;
            self.evidence_pages = self.evidence_pages.saturating_add(1);
            self.delivered_text_bytes = self.delivered_text_bytes.saturating_add(text.len());
        }
        Ok(())
    }

    /// Count successful atomic judgment batches, not individual model turns.
    pub(crate) fn record_judgment_batch(&mut self) {
        self.judgment_batches = self.judgment_batches.saturating_add(1);
    }

    fn observe(&mut self, now_milliseconds: i64) -> BudgetStatus {
        if self.exhausted() {
            return BudgetStatus::Exhausted;
        }
        // Completion is persisted by the worker, not inferred when the parent returns.
        if self.completed_at_milliseconds.is_some() {
            return BudgetStatus::Available;
        }
        if now_milliseconds < self.last_observed_milliseconds {
            self.exhaustion = Some(ExhaustionReason::ClockMovedBackwards);
            return BudgetStatus::ClockMovedBackwards;
        }
        self.last_observed_milliseconds = now_milliseconds;
        let Some(elapsed) = self.elapsed(now_milliseconds) else {
            self.exhaustion = Some(ExhaustionReason::ClockMovedBackwards);
            return BudgetStatus::ClockMovedBackwards;
        };
        let limit = TOTAL_MILLISECONDS - PUBLICATION_RESERVE_MILLISECONDS;
        if elapsed >= limit {
            self.exhaustion = Some(ExhaustionReason::TimeLimit);
            BudgetStatus::Exhausted
        } else {
            BudgetStatus::Available
        }
    }

    /// Measure active assessment spans across completed snapshots and stale recovery.
    fn elapsed(&self, now_milliseconds: i64) -> Option<i64> {
        now_milliseconds
            .checked_sub(self.started_at_milliseconds)?
            .checked_sub(self.excluded_idle_milliseconds)
    }
}

#[cfg(test)]
mod tests {
    use crate::assessment_budget::{
        AssessmentBudget, BudgetStatus, PUBLICATION_RESERVE_MILLISECONDS, TOTAL_MILLISECONDS,
        WORK_COMMAND_LIMIT, WORKER_LIMIT,
    };
    use crate::{Clock, classification};
    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::json;
    use std::time::Duration;

    struct TestClock(i64);

    impl Clock for TestClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_millisecond(self.0).unwrap()
        }

        fn wait(&self, _: Duration) -> Result<(), AppError> {
            panic!("This test does not wait");
        }
    }

    #[test]
    fn work_reserves_publication_time_and_exhaustion_is_durable() {
        let mut budget = AssessmentBudget::new(0);
        assert_eq!(
            budget.begin_work(TOTAL_MILLISECONDS - PUBLICATION_RESERVE_MILLISECONDS - 1),
            BudgetStatus::Available
        );
        assert_eq!(
            budget.begin_work(TOTAL_MILLISECONDS - PUBLICATION_RESERVE_MILLISECONDS),
            BudgetStatus::Exhausted
        );
        let mut restored: AssessmentBudget =
            serde_json::from_slice(&serde_json::to_vec(&budget).unwrap()).unwrap();
        restored.complete();
        assert_eq!(restored.begin_work(0), BudgetStatus::Exhausted);
        assert_eq!(restored.resume(TOTAL_MILLISECONDS), BudgetStatus::Exhausted);
        assert_eq!(restored.before_publication(0), BudgetStatus::Exhausted);
    }

    #[test]
    fn command_limit_and_clock_regression_cannot_reset_the_budget() {
        let mut budget = AssessmentBudget::new(10);
        for _ in 0..WORK_COMMAND_LIMIT {
            assert_eq!(budget.begin_work(10), BudgetStatus::Available);
        }
        assert_eq!(budget.begin_work(10), BudgetStatus::Exhausted);

        let mut budget = AssessmentBudget::new(10);
        assert_eq!(budget.begin_work(11), BudgetStatus::Available);
        assert_eq!(budget.begin_work(10), BudgetStatus::ClockMovedBackwards);
        assert_eq!(budget.begin_work(11), BudgetStatus::Exhausted);
    }

    #[test]
    fn completed_work_excludes_delayed_observation_and_resumes_only_active_time() {
        let mut budget = AssessmentBudget::new(0);
        assert_eq!(budget.finish_work(100), BudgetStatus::Available);
        budget.complete();
        let mut budget: AssessmentBudget =
            serde_json::from_slice(&serde_json::to_vec(&budget).unwrap()).unwrap();
        let before = budget.diagnostics();
        assert_eq!(
            budget.before_publication(TOTAL_MILLISECONDS),
            BudgetStatus::Available
        );
        assert_eq!(budget.diagnostics(), before);
        assert!(budget.valid(true));
        assert!(!budget.valid(false));
        assert_eq!(budget.resume(TOTAL_MILLISECONDS), BudgetStatus::Available);
        assert_eq!(
            budget.diagnostics().get("observedElapsedMilliseconds"),
            Some(&json!(100))
        );
        assert_eq!(
            budget.finish_work(
                TOTAL_MILLISECONDS + TOTAL_MILLISECONDS - PUBLICATION_RESERVE_MILLISECONDS - 100
            ),
            BudgetStatus::Exhausted
        );
    }

    #[test]
    fn worker_reservations_survive_resume_and_stop_new_work() {
        let mut budget = AssessmentBudget::new(0);
        for _ in 0..WORKER_LIMIT {
            assert_eq!(budget.reserve_worker(), BudgetStatus::Available);
        }
        let mut restored: AssessmentBudget =
            serde_json::from_slice(&serde_json::to_vec(&budget).unwrap()).unwrap();
        assert_eq!(restored.reserve_worker(), BudgetStatus::Exhausted);
        assert_eq!(restored.before_publication(0), BudgetStatus::Exhausted);
    }

    #[test]
    fn freshness_checks_cannot_consume_the_deadline_and_start_an_effect() {
        let mut budget = AssessmentBudget::new(0);
        assert_eq!(budget.finish_work(100), BudgetStatus::Available);
        budget.complete();
        // A delayed parent receives a fresh bounded finalization window, not a new assessment.
        let started = TOTAL_MILLISECONDS;
        let deadline = budget.effect_deadline(started);
        deadline
            .check(&TestClock(started + PUBLICATION_RESERVE_MILLISECONDS - 1))
            .unwrap();
        assert_eq!(
            classification(
                &deadline
                    .check(&TestClock(started + PUBLICATION_RESERVE_MILLISECONDS))
                    .unwrap_err()
            ),
            "publicationTimeout"
        );
        assert!(!budget.exhausted());
        assert!(deadline.check(&TestClock(started - 1)).is_err());
    }

    #[test]
    fn a_command_that_finishes_late_cannot_freeze_a_timely_completion() {
        let mut budget = AssessmentBudget::new(0);
        assert_eq!(budget.begin_work(0), BudgetStatus::Available);
        assert_eq!(
            budget.finish_work(TOTAL_MILLISECONDS - PUBLICATION_RESERVE_MILLISECONDS),
            BudgetStatus::Exhausted
        );
        budget.complete();
        assert!(
            budget
                .diagnostics()
                .get("completedAtMilliseconds")
                .unwrap()
                .is_null()
        );
        assert_eq!(
            budget.before_publication(TOTAL_MILLISECONDS),
            BudgetStatus::Exhausted
        );
    }
}
