//! Provider-backed review and reapproval operations for skills.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod ado_approval;
mod ado_capture;
mod ado_publication;
mod api;
mod approval;
mod assessment {
    pub(crate) use ss_ai_assessment::{Assessment, Disposition, Judgment};
}
mod capture;
mod clock;
mod command;
mod discovery;
mod final_review;
mod final_review_ado;
mod final_review_github;
mod final_review_transaction;
mod github_approval;
mod github_capture;
mod identity;
mod journal;
mod json;
mod objects;
mod overview;
mod overview_ado;
mod overview_github;
mod overview_snapshot;
mod overview_transaction;
mod publication;
mod renewal;
mod review_notice;
mod selection;
mod target;
mod tracker_ado;
mod tracker_body;
mod tracker_github;
mod tracker_publication;
mod tracker_target;
mod tracker_transaction;
mod transport;

mod error {
    pub(crate) use ss_ai_runtime::{caused_by, classification, fail};
}

pub use capture::{capture_target, collect_capture};
pub use command::{run, run_in};
pub use final_review::{FinalReviewInput, final_review_check_prior, publish_final_review};
pub use overview::{OverviewInput, OverviewRender, publish_overview};
#[doc(hidden)]
pub use overview_snapshot::overview_text_is_publishable;
pub use ss_ai_runtime::{
    Clock, NativeProcess, Process, ProcessOutput, SystemClock, classification,
};
#[cfg(any(test, feature = "private-fuzz"))]
#[doc(hidden)]
pub use tracker_body::fuzz_tracker_body;
pub use tracker_publication::{
    TrackerAcceptance, TrackerPublicationInput, publish_tracker_items,
    tracker_publication_check_prior,
};
pub use tracker_target::{resolve_tracker_target, validate_tracker_arguments};
#[cfg(any(test, feature = "private-fuzz"))]
#[doc(hidden)]
pub use tracker_transaction::fuzz_tracker_transaction;
pub use transport::{Http, HttpResponse, NativeHttp};

#[cfg(test)]
mod tests;
