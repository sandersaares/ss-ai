//! Provider-backed review and reapproval operations for skills.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod ado_approval;
mod ado_publication;
mod api;
mod approval;
mod assessment {
    pub(crate) use ss_skills_assessment::{Assessment, Disposition, Judgment};
}
mod clock;
mod command;
mod discovery;
mod github_approval;
mod identity;
mod journal;
mod json;
mod objects;
mod publication;
mod renewal;
mod review_notice;
mod selection;
mod target;
mod transport;

mod error {
    pub(crate) use ss_skills_runtime::{caused_by, classification, fail};
}

pub use command::{run, run_in};
pub use ss_skills_runtime::{
    Clock, NativeProcess, Process, ProcessOutput, SystemClock, classification,
};
pub use transport::{Http, HttpResponse, NativeHttp};

#[cfg(test)]
mod tests;
