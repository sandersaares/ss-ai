//! Command composition for the internal skills utility; not a supported SDK.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod cargo;
mod command;
mod git;

pub use cargo::run as run_cargo;
pub use command::{run, run_in};
pub use git::run as run_git;
pub use ss_ai_review::{
    Clock, Http, HttpResponse, NativeHttp, NativeProcess, Process, ProcessOutput, SystemClock,
    classification,
};
