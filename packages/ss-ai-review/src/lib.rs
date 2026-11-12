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
mod assessment_budget;
mod capture;
mod clock;
mod command;
mod discovery;
mod discussion;
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
pub use transport::{Http, HttpResponse, NativeHttp};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
