//! Centralizes timestamp interpretation and elapsed-time review policy.
//!
//! Provider evidence and journal deadlines use the same instant representation;
//! injectable time keeps decisions independent of real waits in tests.

use std::sync::LazyLock;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use regex::Regex;
use tick::Clock as TimerClock;
use tokio::runtime::Builder;

use crate::error::{Failure, fail};

/// Supplies time and waits without coupling decisions to elapsed test time.
///
/// The command shares one boundary across selection, transport recovery, and
/// journal contention. Tests control returned instants and account for requested
/// waits instead of sleeping; the installed executable supplies `SystemClock`.
pub trait Clock {
    /// Supplies the current instant for recency, expiration, and deadline checks.
    ///
    /// Provider timestamp offsets are normalized to the same instant model.
    fn now(&self) -> Timestamp;
    /// Defers a retry or confirmation by an elapsed duration.
    ///
    /// Production waits must complete before the operation continues. Test
    /// implementations can advance controlled time instead of delaying execution.
    fn wait(&self, duration: Duration) -> Result<(), AppError>;
}

/// Uses the host clock for the command-line application.
///
/// Construct with `Default` when wiring the executable. This boundary serves
/// synchronous command execution: timestamps use Jiff, while waits use a tick
/// delay driven locally rather than requiring an application-wide async runtime.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct SystemClock;

impl Clock for SystemClock {
    /// Observes wall-clock time for comparison with service creation timestamps.
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    /// Drives the production delay while keeping the caller's workflow synchronous.
    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        // The synchronous boundary owns the runtime that drives tick's monotonic delay.
        // Keeping the runtime local also preserves SystemClock's stateless construction.
        let runtime = Builder::new_current_thread().enable_time().build()?;
        runtime.block_on(async {
            TimerClock::new_tokio().delay(duration).await;
        });
        Ok(())
    }
}

/// Shared inclusive recency window from the proactor's selection contract.
///
/// Expressed as elapsed time so calendar offset changes cannot alter eligibility.
pub(crate) const COMMIT_AGE: Duration = Duration::from_hours(14 * 24);
/// A claim's lifetime comes from its service creation time, never its edit time.
pub(crate) const CLAIM_AGE: Duration = Duration::from_hours(1);
/// Preparation time counts toward this interval before the sole final check.
///
/// Persist its deadline after creation so separate select/confirm invocations
/// share the same preparation period.
pub(crate) const PREPARATION_AGE: Duration = Duration::from_secs(60);

/// Restricts Jiff's ISO parser to the offset-bearing RFC 3339 service contract.
static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\A(?P<whole>[0-9]{4}-[0-9]{2}-[0-9]{2}[Tt ][0-9]{2}:[0-9]{2}:[0-9]{2})(?:\.(?P<fraction>[0-9]+))?(?P<offset>[Zz]|[+-](?:[01][0-9]|2[0-3]):[0-5][0-9])\z",
    )
    .expect("The fixed RFC 3339 syntax expression is valid")
});

/// Parses offset-bearing service and journal timestamps into one comparison form.
///
/// Validate the service wire shape before using Jiff's broader parser. Excess
/// fractional precision is discarded rather than rounded across a policy cutoff.
pub(crate) fn timestamp(value: &str) -> Result<Timestamp, AppError> {
    let captures = TIMESTAMP
        .captures(value)
        .ok_or_else(|| fail("service", format!("Invalid timestamp {value:?}")))?;
    // Service timestamps can carry more precision than nanoseconds. Discard only
    // the excess digits, rather than rounding across an eligibility boundary.
    let normalized = captures.name("fraction").and_then(|fraction| {
        fraction
            .as_str()
            .get(..9)
            .filter(|_| fraction.len() > 9)
            .map(|fraction| {
                format!(
                    "{}.{fraction}{}",
                    captures
                        .name("whole")
                        .expect("The whole timestamp is required")
                        .as_str(),
                    captures
                        .name("offset")
                        .expect("The offset is required")
                        .as_str()
                )
            })
    });
    normalized
        .as_deref()
        .unwrap_or(value)
        .parse()
        .map_err(|error| {
            Failure::caused_by("service", format!("Invalid timestamp {value:?}"), error).into()
        })
}

/// Reduces complete committer evidence independently of reviewer exclusions.
///
/// Inspect every supplied timestamp even after finding a recent commit. Future
/// commits do not influence ranking; absent or malformed evidence is not an
/// ordinary no-recent-commit decision.
pub(crate) fn latest_commit(
    values: &[String],
    now: Timestamp,
) -> Result<Option<Timestamp>, AppError> {
    if values.is_empty() {
        return Err(fail("service", "Missing commit timestamp evidence"));
    }
    let mut latest = None;
    for value in values {
        let instant = timestamp(value)?;
        if instant <= now && latest.is_none_or(|latest| instant > latest) {
            latest = Some(instant);
        }
    }
    Ok(latest)
}

/// Applies the single shared commit window after complete evidence reduction.
///
/// Pass the result of `latest_commit` so future timestamps have already been
/// excluded. The cutoff is inclusive and does not depend on local calendar dates.
pub(crate) fn is_recent(latest: Option<Timestamp>, now: Timestamp) -> bool {
    now.checked_sub(COMMIT_AGE)
        .is_ok_and(|cutoff| latest.is_some_and(|latest| latest >= cutoff))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{is_recent, latest_commit, timestamp};

    #[test]
    fn recency_uses_all_nonfuture_committer_times_and_inclusive_cutoff() {
        let now = timestamp("2026-09-28T10:00:00Z").unwrap();
        let values = [
            "2026-09-27T10:00:00Z",
            "2026-09-28T10:00:00+02:00",
            "2026-09-29T10:00:00Z",
            "2026-09-15T10:00:00Z",
        ]
        .map(str::to_owned);
        assert_eq!(
            latest_commit(&values, now).unwrap(),
            Some(timestamp("2026-09-28T08:00:00Z").unwrap())
        );
        let cutoff = now.checked_sub(super::COMMIT_AGE).unwrap();
        assert!(is_recent(Some(cutoff), now));
        assert!(!is_recent(
            cutoff.checked_sub(Duration::from_nanos(1)).ok(),
            now
        ));
        assert!(!is_recent(None, now));
        assert_eq!(
            latest_commit(&["2026-10-01T00:00:00Z".into()], now).unwrap(),
            None
        );
    }

    #[test]
    fn absent_or_malformed_commit_evidence_is_not_an_exclusion() {
        let now = timestamp("2026-09-28T10:00:00Z").unwrap();
        latest_commit(&[], now).unwrap_err();
        latest_commit(&[String::new()], now).unwrap_err();
        latest_commit(&["not-a-time".into()], now).unwrap_err();
    }

    #[test]
    fn timestamps_require_complete_rfc3339_with_explicit_offsets() {
        for value in [
            "",
            "2026-09-28",
            "2026-09-28T10:00:00",
            "2026-09-28T10:00Z",
            "20260928T100000Z",
            "2026-09-28T10:00:00+0200",
            "2026-09-28T10:00:00+02",
            "2026-09-28T10:00:00+02:00:00",
            "2026-09-28T10:00:00Z[UTC]",
            "2026-09-28T10:00:00Z\n",
            " 2026-09-28T10:00:00Z",
            "2026-09-28T10:00:00,1Z",
            "2026-09-28T10:00:00.Z",
            "2026-09-28T10:00:00+24:00",
            "2026-09-28T10:00:00+00:60",
            "2026-02-30T10:00:00Z",
            "2026-09-28T24:00:00Z",
            "+002026-09-28T10:00:00Z",
        ] {
            assert!(timestamp(value).is_err(), "{value}");
        }
        let expected = timestamp("2026-09-28T08:00:00Z").unwrap();
        for value in [
            "2026-09-28T10:00:00+02:00",
            "2026-09-28t08:00:00z",
            "2026-09-28 08:00:00+00:00",
            "2026-09-28T08:00:00-00:00",
        ] {
            assert_eq!(timestamp(value).unwrap(), expected, "{value}");
        }
    }

    #[test]
    fn service_fractional_precision_is_truncated_not_rounded() {
        // PowerShell emits sub-microsecond precision; services may exceed nanoseconds.
        for value in [
            "2026-09-28T10:00:00.123456700Z",
            "2026-09-28T10:00:00.123456700999+00:00",
        ] {
            assert_eq!(
                timestamp(value).unwrap(),
                timestamp("2026-09-28T10:00:00.1234567Z").unwrap()
            );
        }
    }

    #[test]
    fn recency_uses_elapsed_days_across_offset_changes() {
        // Offsets straddle the European autumn clock change within the recency window.
        let now = timestamp("2026-10-26T10:00:00+01:00").unwrap();
        assert!(is_recent(
            Some(timestamp("2026-10-12T11:00:00+02:00").unwrap()),
            now
        ));
        assert!(!is_recent(
            Some(timestamp("2026-10-12T10:59:59.999999999+02:00").unwrap()),
            now
        ));
    }
}
