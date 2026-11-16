use serde::{Deserialize, Serialize};

/// Identifies the provider approval and source used for the current comparison.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Anchor {
    pub(crate) review_id: String,
    pub(crate) source_commit: String,
}
