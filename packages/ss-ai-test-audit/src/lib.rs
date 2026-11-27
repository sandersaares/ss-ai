//! Deterministic inventory and resumable classification support for test audits.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod batch;
mod compare;
mod inventory;
mod model;
mod report;
mod scan;
mod state;
mod submission;

pub use batch::{advance, next_batch, record_batch, submit};
pub use compare::compare;
pub use inventory::{initialize, initialize_with_corrections};
pub use report::report;
pub use state::{inspect_assertion, inspect_evidence, search, search_many};
