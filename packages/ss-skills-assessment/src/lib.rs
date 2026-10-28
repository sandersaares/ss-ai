//! Source change coverage and judgments for approval assessment.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(
    clippy::exhaustive_enums,
    clippy::exhaustive_structs,
    clippy::partial_pub_fields,
    reason = "Internal record types define closed, validated workflow schemas"
)]

pub use assessment::{Assessment, Disposition, Judgment};
pub use error::AssessmentError;
pub use objects::{Change, Content, Inventory, Side, change_id, commit};

use serde::{Deserialize, Deserializer};

/// Requires nullable record fields to be present in serialized evidence.
fn required_optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

mod assessment;
mod error;
mod objects;
