//! Exercises unrelated filesystem failures at the transaction boundary.

use std::fs;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use ss_ai_runtime::{Clock, FileTransaction};
use tempfile::Builder;

/// Unexpected retries indicate that an unrelated failure was treated as contention.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        panic!("file failures do not use wall-clock time")
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unrelated failures must not be retried")
    }
}

/// Failed reads and publications leave the destination and no staging files.
#[test]
fn directory_destination_fails_without_retry_or_staging_leak() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = Builder::new()
        .prefix(".file-failure-")
        .tempdir_in(root)
        .unwrap();
    let destination = workspace.path().join("directory");
    fs::create_dir_all(&destination).unwrap();
    let transaction = FileTransaction::open(&destination, &NoWait).unwrap();
    transaction.read_bytes().unwrap_err();
    transaction
        .write_bytes(b"must not be published")
        .unwrap_err();
    assert!(destination.is_dir());
    assert!(fs::read_dir(&destination).unwrap().next().is_none());
    let names = fs::read_dir(workspace.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        names
            .iter()
            .all(|name| !name.starts_with(".ss-ai-transaction-"))
    );
}
