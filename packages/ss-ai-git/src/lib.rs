//! Immutable Git comparison and diff interpretation for skills.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod git;
mod git_diff;

pub use git::{Comparison, compare};
pub use git_diff::{FileDiff, normalize_path, parse};
use ss_ai_runtime::Process;
#[cfg(test)]
use ss_ai_runtime::ProcessOutput;
