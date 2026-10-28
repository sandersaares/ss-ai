//! Read-only Cargo manifest inventory for the dependency tidy workflow.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod discovery;
mod error;
mod inventory;
mod manifest;

pub use error::InventoryError;
pub use inventory::{Dependency, Inventory, MemberManifest, WorkspaceManifest, generate_inventory};

#[cfg(any(test, feature = "fuzzing"))]
#[cfg_attr(docsrs, doc(cfg(feature = "fuzzing")))]
pub use manifest::parse_fuzz_manifest;
