//! Validated source inventory for approval assessment.

use crate::required_optional;
use ohno::AppError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ss_skills_runtime::fail;
use std::collections::HashSet;

/// Complete exact source-tree changes, including path, object, and mode differences.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Inventory {
    /// Original approved commit.
    pub old_commit: String,
    /// Current source commit.
    pub new_commit: String,
    /// Original tree identifier.
    pub old_tree: String,
    /// Current tree identifier.
    pub new_tree: String,
    /// Complete changed-path evidence.
    pub changes: Vec<Change>,
    /// Whether all changed paths were captured.
    pub complete: bool,
}

/// Each changed path pair has one stable ID and explicit absent or unreadable sides.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Change {
    /// Stable identifier derived from both source sides.
    pub id: String,
    /// Git change classification.
    pub kind: String,
    #[serde(deserialize_with = "required_optional")]
    /// Original side, absent for additions.
    pub old: Option<Side>,
    #[serde(deserialize_with = "required_optional")]
    /// Current side, absent for deletions.
    pub new: Option<Side>,
}

/// Git paths remain data; no path is checked out or used to open a working-tree file.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Side {
    /// Repository-relative path.
    pub path: String,
    /// Immutable Git object identifier.
    pub object: String,
    /// Git file mode.
    pub mode: String,
    /// Captured text or explicit unavailable-content classification.
    pub content: Content,
}

/// Unknown content cannot masquerade as an empty file.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Content {
    /// Text available for assessment.
    Text {
        /// Complete text.
        text: String,
    },
    /// Binary content cannot be assessed as text.
    Binary {
        /// Object size in bytes.
        size: u64,
    },
    /// Content exceeds the capture budget.
    TooLarge {
        /// Object size in bytes.
        size: u64,
    },
    /// A submodule refers to another repository.
    Submodule,
}

impl Inventory {
    /// Checks the completeness and identity of source evidence.
    pub fn validate(&self) -> Result<(), AppError> {
        for id in [
            &self.old_commit,
            &self.new_commit,
            &self.old_tree,
            &self.new_tree,
        ] {
            commit(id)?;
        }
        if !self.complete || (self.old_tree == self.new_tree) != self.changes.is_empty() {
            return Err(fail(
                "invalidState",
                "Incomplete or contradictory source inventory",
            ));
        }
        let mut ids = HashSet::new();
        let mut old_paths = HashSet::new();
        let mut new_paths = HashSet::new();
        for change in &self.changes {
            if change.id != change_id(&change.kind, change.old.as_ref(), change.new.as_ref())?
                || !ids.insert(&change.id)
            {
                return Err(fail("invalidState", "Invalid or repeated change ID"));
            }
            let valid = match change.kind.as_str() {
                "A" => change.old.is_none() && change.new.is_some(),
                "D" => change.old.is_some() && change.new.is_none(),
                "M" | "T" | "R" => change.old.is_some() && change.new.is_some(),
                _ => false,
            };
            if !valid {
                return Err(fail("invalidState", "Invalid changed-path shape"));
            }
            if let (Some(old), Some(new)) = (&change.old, &change.new)
                && ((change.kind == "R") == (old.path == new.path)
                    || (old.path == new.path && old.object == new.object && old.mode == new.mode))
            {
                return Err(fail("invalidState", "Contradictory source change"));
            }
            for (side, paths) in [(&change.old, &mut old_paths), (&change.new, &mut new_paths)] {
                if let Some(side) = side {
                    commit(&side.object)?;
                    if side
                        .path
                        .split('/')
                        .any(|part| matches!(part, "" | "." | ".."))
                        || side.path.contains('\0')
                        || !paths.insert(&side.path)
                        || !matches!(
                            side.mode.as_str(),
                            "100644" | "100755" | "120000" | "160000"
                        )
                        || (side.mode == "160000") != matches!(side.content, Content::Submodule)
                    {
                        return Err(fail("invalidState", "Invalid inventory side"));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Change {
    /// Absent sides describe additions/deletions; opaque present sides lack assessment evidence.
    #[must_use]
    pub fn content_available(&self) -> bool {
        self.old
            .iter()
            .chain(self.new.iter())
            .all(|side| matches!(side.content, Content::Text { .. }))
    }
}

/// Computes the stable identity of an assessed source change.
pub fn change_id(kind: &str, old: Option<&Side>, new: Option<&Side>) -> Result<String, AppError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(kind, old, new))?)
    ))
}

/// Validates the full source identifier used by approval evidence.
pub fn commit(value: &str) -> Result<(), AppError> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(fail("service", "Invalid GitHub commit identifier"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Inventory, commit};

    #[test]
    fn empty_inventory_requires_identical_trees_and_complete_evidence() {
        let mut inventory = Inventory {
            old_commit: "a".repeat(40),
            new_commit: "b".repeat(40),
            old_tree: "c".repeat(40),
            new_tree: "c".repeat(40),
            changes: Vec::new(),
            complete: true,
        };
        inventory.validate().unwrap();
        inventory.complete = false;
        assert!(inventory.validate().is_err());
        inventory.complete = true;
        inventory.new_tree = "d".repeat(40);
        assert!(inventory.validate().is_err());
    }

    #[test]
    fn source_identifiers_require_full_lowercase_sha1() {
        commit(&"a".repeat(40)).unwrap();
        for invalid in ["A".repeat(40), "a".repeat(39), "g".repeat(40)] {
            assert!(commit(&invalid).is_err());
        }
    }
}
