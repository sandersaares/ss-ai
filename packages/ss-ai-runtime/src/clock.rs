//! Injectable clock and waiting boundary.

use jiff::Timestamp;
use ohno::AppError;
use std::time::Duration;
use tick::Clock as TimerClock;
use tokio::runtime::Builder;

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
