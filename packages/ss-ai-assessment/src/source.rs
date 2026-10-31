//! Exact source documents and captured-side context, independent of provider authority.

use std::fmt::{self, Debug, Formatter};
use std::num::NonZeroUsize;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use ss_ai_git::{Hunk, blob_patch, validate_patch};

use crate::assessment::{document_digest, hash};
use crate::error;
use crate::{AssessmentError, Change, Content, Inventory, Side, required_optional};

/// Required operation evidence binds exact hunks to the authoritative inventory.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SourceEvidence {
    snapshot_digest: String,
    change_id: String,
    kind: String,
    #[serde(deserialize_with = "required_optional")]
    old: Option<SourceBinding>,
    #[serde(deserialize_with = "required_optional")]
    new: Option<SourceBinding>,
    pub(crate) patch: String,
    pub(crate) hunks: Vec<Hunk>,
}

impl SourceEvidence {
    /// Fixed Git output is validated before it becomes required model evidence.
    pub(crate) fn new(
        change: &Change,
        snapshot: &str,
        diff: &str,
    ) -> Result<Self, AssessmentError> {
        let patch = blob_patch(diff).map_err(error::InvalidSourcePatch::from_error)?;
        let hunks = validate_patch(
            patch,
            readable_text(change.old.as_ref()),
            readable_text(change.new.as_ref()),
        )
        .map_err(error::InvalidSourcePatch::from_error)?;
        Ok(Self {
            snapshot_digest: snapshot.to_owned(),
            change_id: change.id.clone(),
            kind: change.kind.clone(),
            old: change.old.as_ref().map(SourceBinding::new),
            new: change.new.as_ref().map(SourceBinding::new),
            patch: patch.to_owned(),
            hunks,
        })
    }

    /// Reconstructs from retained sides again at the consumer boundary.
    pub(crate) fn validate(&self, change: &Change, snapshot: &str) -> Result<(), AssessmentError> {
        let expected = Self::new(change, snapshot, &self.patch)?;
        if hash(self)? != hash(&expected)? {
            return Err(error::InvalidSourceEvidence::classified());
        }
        Ok(())
    }

    /// Operation metadata remains mandatory even when there are no textual hunks.
    pub(crate) fn document(&self) -> Result<String, AssessmentError> {
        Ok(serde_json::to_string(self)?)
    }
}

impl Debug for SourceEvidence {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct(std::any::type_name::<Self>())
            .field("snapshot_digest", &self.snapshot_digest)
            .field("change_id", &self.change_id)
            .field("patch_bytes", &self.patch.len())
            .field("hunk_count", &self.hunks.len())
            .finish_non_exhaustive()
    }
}

/// Exact range identity survives paging and binds completion without duplicating text.
#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SourceContext {
    pub(crate) request: ContextRequest,
    snapshot_digest: String,
    source: SourceBinding,
    start_byte: usize,
    end_byte: usize,
    start_character: usize,
    end_character: usize,
    content_digest: String,
}

impl SourceContext {
    /// Line boundaries preserve LF, CRLF, and an unterminated final line exactly.
    pub(crate) fn new(
        inventory: &Inventory,
        snapshot: &str,
        request: ContextRequest,
    ) -> Result<Self, AssessmentError> {
        let side = captured_side(inventory, &request)?;
        let Content::Text { text } = &side.content else {
            return Err(error::UnreadableEvidence::classified());
        };
        let first = request.start_line.get().saturating_sub(1);
        let last = first
            .checked_add(request.line_count.get())
            .ok_or_else(error::InvalidContextRange::classified)?;
        let mut start = None;
        let mut end = None;
        let mut offset = 0_usize;
        for (index, line) in text.split_inclusive('\n').enumerate() {
            if index == first {
                start = Some(offset);
            }
            offset = offset.saturating_add(line.len());
            if index.saturating_add(1) == last {
                end = Some(offset);
                break;
            }
        }
        let (start_byte, end_byte) = start
            .zip(end)
            .ok_or_else(error::InvalidContextRange::classified)?;
        let before = text
            .get(..start_byte)
            .ok_or_else(error::InvalidContextRange::classified)?;
        let content = text
            .get(start_byte..end_byte)
            .ok_or_else(error::InvalidContextRange::classified)?;
        Ok(Self {
            source: SourceBinding::new(side),
            start_byte,
            end_byte,
            start_character: before.chars().count(),
            end_character: before
                .chars()
                .count()
                .saturating_add(content.chars().count()),
            content_digest: document_digest(content),
            snapshot_digest: snapshot.to_owned(),
            request,
        })
    }

    /// Repeated requests name the same exact captured range.
    pub(crate) fn reference(&self) -> Result<String, AssessmentError> {
        Ok(format!(
            "{}:context:{}",
            self.request.change_id,
            hash(self)?
        ))
    }

    /// Recomputes every bound from retained source rather than trusting persisted offsets.
    pub(crate) fn validate(
        &self,
        inventory: &Inventory,
        snapshot: &str,
    ) -> Result<(), AssessmentError> {
        if *self != Self::new(inventory, snapshot, self.request.clone())? {
            return Err(error::InvalidContextBinding::classified());
        }
        Ok(())
    }

    /// Serves captured bytes, never a pathname opened in a checkout.
    pub(crate) fn content<'a>(&self, inventory: &'a Inventory) -> Result<&'a str, AssessmentError> {
        let side = captured_side(inventory, &self.request)?;
        let Content::Text { text } = &side.content else {
            return Err(error::UnreadableEvidence::classified());
        };
        text.get(self.start_byte..self.end_byte)
            .ok_or_else(error::InvalidContextRange::classified)
    }
}

/// A named line range registers additional mandatory evidence.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextRequest {
    /// Stable inventory identity.
    pub change_id: String,
    /// Captured endpoint.
    pub side: SourceSide,
    /// One-based first line.
    pub start_line: NonZeroUsize,
    /// Positive number of complete lines.
    pub line_count: NonZeroUsize,
}

/// Selects an exact captured endpoint rather than a working-tree file.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "camelCase")]
pub enum SourceSide {
    /// Original external-approved source.
    Old,
    /// Captured current source.
    New,
}

/// Source identity is metadata, never a second copy of the captured body.
#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceBinding {
    path: String,
    object: String,
    mode: String,
    content_kind: String,
}

impl SourceBinding {
    fn new(side: &Side) -> Self {
        Self {
            path: side.path.clone(),
            object: side.object.clone(),
            mode: side.mode.clone(),
            content_kind: match side.content {
                Content::Text { .. } => "text",
                Content::Binary { .. } => "binary",
                Content::TooLarge { .. } => "tooLarge",
                Content::Submodule => "submodule",
            }
            .to_owned(),
        }
    }
}

/// An opaque side contributes no invented text; its classification remains in the document.
fn readable_text(side: Option<&Side>) -> &str {
    match side.map(|side| &side.content) {
        Some(Content::Text { text }) => text,
        _ => "",
    }
}

/// Range requests are limited to existing sides of the authoritative changed-path inventory.
fn captured_side<'a>(
    inventory: &'a Inventory,
    request: &ContextRequest,
) -> Result<&'a Side, AssessmentError> {
    let change = inventory
        .changes
        .iter()
        .find(|change| change.id == request.change_id)
        .ok_or_else(error::UnknownEvidenceChange::classified)?;
    match request.side {
        SourceSide::Old => change.old.as_ref(),
        SourceSide::New => change.new.as_ref(),
    }
    .ok_or_else(error::UnknownEvidenceSide::classified)
}
