//! Read-only Cargo manifest inventory for the dependency tidy workflow.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod discovery;
mod inventory;
mod manifest;

pub use inventory::{
    Dependency, Inventory, InventoryError, MemberManifest, WorkspaceManifest, generate_inventory,
};

#[cfg(feature = "fuzzing")]
#[cfg_attr(docsrs, doc(cfg(feature = "fuzzing")))]
pub use manifest::parse_fuzz_manifest;
