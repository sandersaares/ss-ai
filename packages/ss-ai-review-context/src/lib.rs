//! Local capture and fingerprint operations for review workflows.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod capture;
mod diagnostics;
mod json;
mod query;
mod request;

pub use capture::{evidence_hash, read_capture, snapshot_id};
pub use diagnostics::{increment_diagnostic, read_diagnostics};
pub use json::{bytes_hash, document_hash, powershell_json};
pub use query::{normalize_options, query};
pub use request::{prepare, prepare_failure, read_saved};
