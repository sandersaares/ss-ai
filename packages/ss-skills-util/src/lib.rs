//! Native operations shared by the repository's skills.
//!
//! The library wires this package's executable and tests; it is not a Rust SDK.
//! [`run`] dispatches complete commands. The binary supplies [`Process`], [`Http`],
//! and [`Clock`] implementations; tests replace those dependencies rather than
//! changing workflow decisions. [`classification`] exposes recovery categories
//! independently of human-readable diagnostics.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod ado_approval;
mod ado_publication;
mod api;
mod approval;
mod assessment;
mod clock;
mod command;
mod discovery;
mod error;
mod git;
mod git_diff;
mod github_approval;
mod identity;
mod journal;
mod json;
mod objects;
mod process;
mod publication;
mod renewal;
mod review_notice;
mod selection;
mod target;
mod transport;

pub use clock::{Clock, SystemClock};
pub use command::{run, run_in};
pub use error::classification;
pub use process::{NativeProcess, Process, ProcessOutput};
pub use transport::{Http, HttpResponse, NativeHttp};

#[cfg(test)]
mod tests;
