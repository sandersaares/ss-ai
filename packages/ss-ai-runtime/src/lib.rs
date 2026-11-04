//! Shared runtime boundaries for internal skills operations.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod clock;
mod error;
mod file;
mod process;

pub use clock::{Clock, SystemClock};
pub use error::{Failure, caused_by, classification, fail};
pub use file::{FileTransaction, LockMode, ResourceLock, read_bytes};
pub use process::{NativeProcess, Process, ProcessOutput};
