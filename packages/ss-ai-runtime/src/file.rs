//! Atomic file publication protected by retained sibling locks.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ohno::AppError;
use tempfile::NamedTempFile;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(windows)]
use std::ptr::null;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ, ReplaceFileW};

use crate::{Clock, Failure, fail};

/// Bounds contention for a transaction that may wait behind provider work.
///
/// Approximately five minutes permits parallel inspections to queue without
/// treating a held file lock as a permanent conflict. Lock holders cannot extend this budget.
const CONTENTION_ATTEMPTS: u32 = 6_000;
/// Avoids busy polling while concurrent operations wait for the same lock.
const CONTENTION_INTERVAL: Duration = Duration::from_millis(50);

/// Holds a file transaction across an operation's complete read-modify-write.
///
/// Retain the transaction across its reads, effects, and writes.
/// Dropping the transaction releases exclusion without deleting the sibling lock.
#[allow(
    missing_debug_implementations,
    reason = "The injected clock need not implement Debug"
)]
pub struct FileTransaction<'a> {
    /// Publication destination; staging files stay beside it for atomic replacement.
    path: PathBuf,
    /// Retained handle makes exclusion last as long as this transaction owner.
    _lock: File,
    /// Shares the invocation's wait policy for bounded filesystem contention.
    clock: &'a dyn Clock,
}

impl<'a> FileTransaction<'a> {
    /// Acquires the transaction before testing for an existing file.
    pub fn open(path: &Path, clock: &'a dyn Clock) -> Result<Self, AppError> {
        if !path.is_absolute() {
            return Err(fail("invalidInput", "File path must be absolute"));
        }
        let parent = path
            .parent()
            .ok_or_else(|| fail("invalidInput", "File path requires a parent directory"))?;
        fs::create_dir_all(parent).map_err(|error| {
            Failure::caused_by("io", format!("Cannot create {}", parent.display()), error)
        })?;
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock = retry(clock, || {
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(windows)]
            options.share_mode(0);
            let file = options.open(&lock_path)?;
            #[cfg(not(windows))]
            file.try_lock().map_err(|error| match error {
                fs::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
                fs::TryLockError::Error(error) => error,
            })?;
            Ok(file)
        })?;
        Ok(Self {
            path: path.to_owned(),
            _lock: lock,
            clock,
        })
    }

    /// Reads one complete version while retaining this transaction's sibling lock.
    pub fn read_bytes(&self) -> Result<Option<Vec<u8>>, AppError> {
        read_bytes(&self.path, self.clock)
    }

    /// Atomically publishes bytes already validated by the owning record operation.
    pub fn write_bytes(&self, bytes: &[u8]) -> Result<(), AppError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| fail("invalidInput", "File path requires a parent"))?;
        let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
            Failure::caused_by("io", "Cannot create staging file".to_owned(), error)
        })?;
        temporary.write_all(bytes).map_err(|error| {
            Failure::caused_by("io", "Cannot write staging file".to_owned(), error)
        })?;
        temporary.as_file().sync_all().map_err(|error| {
            Failure::caused_by("io", "Cannot flush staging file".to_owned(), error)
        })?;
        // ReplaceFile opens the replacement without write sharing. Close the staging
        // handle before publication while retaining automatic cleanup of its path.
        let temporary = temporary.into_temp_path();
        retry(self.clock, || replace(&temporary, &self.path))?;
        Ok(())
    }
}

/// Reads a shared complete file while allowing concurrent readers and atomic replacement.
pub fn read_bytes(path: &Path, clock: &dyn Clock) -> Result<Option<Vec<u8>>, AppError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    options.share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
    let file = retry(clock, || match options.open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    })?;
    let Some(mut file) = file else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| {
        Failure::caused_by("io", format!("Cannot read {}", path.display()), error)
    })?;
    Ok(Some(bytes))
}

/// Retries only an uncommitted file operation, never the enclosing transaction.
fn retry<T>(
    clock: &dyn Clock,
    mut operation: impl FnMut() -> io::Result<T>,
) -> Result<T, AppError> {
    // Provider work retains the transaction lock, so retries must allow siblings to queue.
    for attempt in 0..CONTENTION_ATTEMPTS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if contention(&error) => {
                if attempt == CONTENTION_ATTEMPTS - 1 {
                    return Err(Failure::caused_by(
                        "contention",
                        "Shared file remains busy after bounded contention retries".to_owned(),
                        error,
                    )
                    .into());
                }
                clock.wait(CONTENTION_INTERVAL)?;
            }
            Err(error) => {
                return Err(
                    Failure::caused_by("io", "File operation failed".to_owned(), error).into(),
                );
            }
        }
    }
    unreachable!("the final contention attempt returns an error")
}

/// Limits retry eligibility to lock/sharing conflicts, not general I/O failures.
fn contention(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(32) || error.raw_os_error() == Some(33)
    }
    #[cfg(not(windows))]
    {
        error.kind() == io::ErrorKind::WouldBlock
    }
}

/// Publishes beside the old record so readers retain their opened complete version.
#[cfg(not(windows))]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

/// `ReplaceFile` preserves opened delete-sharing readers, unlike overwrite moves.
#[cfg(windows)]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    if !destination.try_exists()? {
        return fs::rename(source, destination);
    }
    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing filename"))?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing parent"))?;
        Ok(parent
            .canonicalize()?
            .join(name)
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect())
    };
    let source = wide(source)?;
    let destination = wide(destination)?;
    // SAFETY: both buffers are NUL-terminated native paths and live through the call.
    // No backup or reserved argument is supplied; ownership remains with this transaction.
    if unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            source.as_ptr(),
            null(),
            0,
            null(),
            null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;

    use crate::Clock;

    use super::{CONTENTION_ATTEMPTS, CONTENTION_INTERVAL, FileTransaction, retry};

    /// Counts requested waits without sleeping or accessing the filesystem.
    #[derive(Default)]
    struct RetryClock {
        waits: Cell<u32>,
    }

    impl Clock for RetryClock {
        fn now(&self) -> Timestamp {
            panic!("File-operation retries must not depend on wall-clock time")
        }

        fn wait(&self, duration: Duration) -> Result<(), AppError> {
            assert_eq!(duration, CONTENTION_INTERVAL);
            self.waits.set(self.waits.get().checked_add(1).unwrap());
            Ok(())
        }
    }

    /// Models the platform-specific conflict recognized by the file boundary.
    fn busy() -> io::Error {
        #[cfg(windows)]
        {
            io::Error::from_raw_os_error(32)
        }
        #[cfg(not(windows))]
        {
            io::Error::from(io::ErrorKind::WouldBlock)
        }
    }

    #[test]
    fn contention_can_recover_beyond_the_short_file_operation_budget() {
        let clock = RetryClock::default();
        let mut attempts = 0;
        // The earlier file-operation window exhausted after this many conflicts.
        let conflicts = 601;
        let value = retry(&clock, || {
            attempts += 1;
            if attempts <= conflicts {
                Err(busy())
            } else {
                Ok("complete")
            }
        })
        .unwrap();
        assert_eq!(value, "complete");
        assert_eq!(attempts, conflicts + 1);
        assert_eq!(clock.waits.get(), conflicts);
    }

    #[test]
    fn persistent_contention_exhausts_the_exact_bounded_budget() {
        let clock = RetryClock::default();
        let mut attempts = 0;
        let error = retry::<()>(&clock, || {
            attempts += 1;
            Err(busy())
        })
        .unwrap_err();
        assert_eq!(crate::classification(&error), "contention");
        assert_eq!(attempts, CONTENTION_ATTEMPTS);
        assert_eq!(clock.waits.get(), CONTENTION_ATTEMPTS - 1);
        assert_eq!(
            CONTENTION_INTERVAL * clock.waits.get(),
            // The budget excludes a wait after the final failed attempt.
            Duration::from_mins(5)
                .checked_sub(CONTENTION_INTERVAL)
                .unwrap()
        );
    }

    #[test]
    fn unrelated_io_failures_are_not_retried() {
        let clock = RetryClock::default();
        let mut attempts = 0;
        let error = retry::<()>(&clock, || {
            attempts += 1;
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .unwrap_err();
        assert_eq!(crate::classification(&error), "io");
        assert!(error.to_string().contains("File operation failed"));
        assert_eq!(attempts, 1);
        assert_eq!(clock.waits.get(), 0);
    }

    #[test]
    fn relative_destination_reports_file_input_error() {
        let error = FileTransaction::open(Path::new("relative"), &RetryClock::default())
            .err()
            .unwrap();
        assert_eq!(crate::classification(&error), "invalidInput");
        assert!(error.to_string().contains("File path must be absolute"));
    }
}
