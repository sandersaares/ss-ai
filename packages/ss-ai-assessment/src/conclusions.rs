//! Concise worker inputs expand into exact native evidence and coverage records.

use std::collections::HashSet;
use std::iter::once;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::assessment::{RESPONSE_BYTES, check_response, compact_batch_page, hash};
use crate::error::{self, AssessmentError};
use crate::{Assessment, BatchJudgmentItem, BatchJudgmentRequest, Disposition, Judgment};

/// One explanation can cover changes with the same substantive conclusion.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConclusionGroup {
    /// Native short change references from this snapshot's inventory.
    pub changes: Vec<String>,
    /// Semantic outcome shared by these changes.
    pub disposition: Disposition,
    /// Concise explanation of the group's behavior and significance.
    pub rationale: String,
    /// Additional exact evidence beyond natively bound patches and context.
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// A conditional grouped transaction never partially installs its conclusions.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupedConclusions {
    /// Binds all short references and replacements to the current native state.
    pub state_token: String,
    /// Distinct changes grouped by their substantive conclusion.
    pub groups: Vec<ConclusionGroup>,
}

/// The final independent reasoning covers outcome, discussion, and open concerns.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalSynthesis {
    /// Binds the synthesis to the final set of change conclusions.
    pub state_token: String,
    /// Independently reasoned cumulative outcome.
    pub disposition: Disposition,
    /// Combined behavior and significance, not a mechanical reduction of labels.
    pub rationale: String,
    /// Material discussion and its disposition; concise when nothing is material.
    pub discussion: String,
    /// Any remaining substantive concern prevents renewal.
    #[serde(default)]
    pub unresolved_concerns: Vec<String>,
    /// Specific additional evidence supporting combined effects or discussion.
    #[serde(default)]
    pub evidence: Vec<String>,
}

impl Assessment {
    /// Short references are inventory positions within the exact snapshot and state.
    pub(crate) fn short_change(&self, id: &str) -> Result<String, AssessmentError> {
        self.inventory
            .changes
            .iter()
            .position(|change| change.id == id)
            .map(|index| format!("c{}", index.saturating_add(1)))
            .ok_or_else(error::UnknownChange::classified)
    }

    /// Resolve only native-issued references, never path guesses or abbreviated hashes.
    pub(crate) fn exact_change(&self, reference: &str) -> Result<String, AssessmentError> {
        if let Some(number) = reference
            .strip_prefix('c')
            .and_then(|value| value.parse::<usize>().ok())
        {
            if reference != format!("c{number}") {
                return Err(error::UnknownChange::classified());
            }
            return number
                .checked_sub(1)
                .and_then(|index| self.inventory.changes.get(index))
                .map(|change| change.id.clone())
                .ok_or_else(error::UnknownChange::classified);
        }
        // Exact identifiers remain usable by internal evidence readers and retained bindings.
        self.inventory
            .changes
            .iter()
            .find(|change| change.id == reference)
            .map(|change| change.id.clone())
            .ok_or_else(error::UnknownChange::classified)
    }

    /// Evidence suffixes retain their meaning after resolving the native change reference.
    pub(crate) fn exact_reference(&self, reference: &str) -> Result<String, AssessmentError> {
        if let Some((id, side)) = reference.split_once(':')
            && id.starts_with('c')
            && id
                .get(1..)
                .is_some_and(|suffix| suffix.chars().all(|c| c.is_ascii_digit()))
        {
            return Ok(format!("{}:{side}", self.exact_change(id)?));
        }
        Ok(reference.to_owned())
    }

    /// Evidence pages use the same snapshot-scoped references as inventory.
    fn short_reference(&self, reference: &str) -> Result<String, AssessmentError> {
        if let Some((id, suffix)) = reference.split_once(':')
            && self.inventory.changes.iter().any(|change| change.id == id)
        {
            return Ok(format!("{}:{suffix}", self.short_change(id)?));
        }
        Ok(reference.to_owned())
    }

    /// Expand grouped semantics into exact, individually validated coverage natively.
    pub fn conclude(
        &mut self,
        request: GroupedConclusions,
        policy: &str,
        discussion: &str,
    ) -> Result<Value, AssessmentError> {
        if request.state_token != self.state_token()? {
            return Err(error::StaleAssessment::classified());
        }
        if request.groups.is_empty() {
            return Err(error::EmptyBatch::classified());
        }
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        for group in request.groups {
            if group.changes.is_empty() {
                return Err(error::EmptyBatch::classified());
            }
            let additional = group
                .evidence
                .iter()
                .map(|reference| self.exact_reference(reference))
                .collect::<Result<Vec<_>, _>>()?;
            for reference in group.changes {
                let id = self.exact_change(&reference)?;
                if !seen.insert(id.clone()) {
                    return Err(error::DuplicateBatchItem::classified());
                }
                let mut evidence = vec![format!("{id}:patch")];
                evidence.extend(additional.iter().cloned());
                evidence.sort();
                evidence.dedup();
                items.push(BatchJudgmentItem {
                    previous_digest: self.decisions.get(&id).map(hash).transpose()?,
                    id,
                    judgment: Judgment {
                        disposition: group.disposition,
                        rationale: group.rationale.clone(),
                        evidence,
                        concerns: Vec::new(),
                        interactions: Vec::new(),
                    },
                });
            }
        }
        self.record_batch_response(
            BatchJudgmentRequest {
                state_token: request.state_token,
                items,
            },
            policy,
            discussion,
            true,
        )
    }

    /// Atomically bind final reasoning and complete only fully examined comparisons.
    pub fn synthesize(
        &mut self,
        synthesis: FinalSynthesis,
        policy: &str,
        discussion: &str,
    ) -> Result<(), AssessmentError> {
        if synthesis.state_token != self.state_token()? {
            return Err(error::StaleAssessment::classified());
        }
        if synthesis.rationale.trim().is_empty()
            || synthesis.discussion.trim().is_empty()
            || synthesis
                .unresolved_concerns
                .iter()
                .any(|text| text.trim().is_empty())
        {
            return Err(error::MissingJudgmentEvidence::classified());
        }
        let mut staged: Self = serde_json::from_slice(&serde_json::to_vec(self)?)?;
        // Missing delivery is incomplete work, not a synthesized semantic refusal.
        staged.complete_reads(policy, discussion)?;
        let mut evidence = vec!["policy".to_owned(), "discussion".to_owned()];
        evidence.extend(
            synthesis
                .evidence
                .iter()
                .map(|reference| staged.exact_reference(reference))
                .collect::<Result<Vec<_>, _>>()?,
        );
        evidence.sort();
        evidence.dedup();
        let mut rationale = format!(
            "{}\n\nMaterial discussion: {}",
            synthesis.rationale, synthesis.discussion
        );
        if !synthesis.unresolved_concerns.is_empty() {
            rationale.push_str("\n\nUnresolved concerns: ");
            rationale.push_str(&synthesis.unresolved_concerns.join("; "));
        }
        let previous = staged.cumulative.as_ref().map(hash).transpose()?;
        // A favorable label cannot erase an explicitly unresolved concern.
        let disposition = if !synthesis.unresolved_concerns.is_empty()
            && synthesis.disposition == Disposition::Insignificant
        {
            Disposition::Uncertain
        } else {
            synthesis.disposition
        };
        staged.record(
            "cumulative",
            Judgment {
                disposition,
                rationale,
                evidence,
                concerns: synthesis.unresolved_concerns,
                interactions: Vec::new(),
            },
            previous.as_deref(),
            policy,
            discussion,
        )?;
        staged.finish(policy, discussion)?;
        *self = staged;
        Ok(())
    }

    /// Deliver the next bounded evidence batch with native offsets and grouping.
    pub fn read_next(
        &mut self,
        read_token: &str,
        policy: &str,
        discussion: &str,
    ) -> Result<Value, AssessmentError> {
        self.read_next_for(read_token, &[], policy, discussion)
    }

    /// Selects relevant changes while native code owns their evidence continuations.
    pub fn read_next_for(
        &mut self,
        read_token: &str,
        changes: &[String],
        policy: &str,
        discussion: &str,
    ) -> Result<Value, AssessmentError> {
        if read_token != self.read_token()? {
            return Err(error::StaleReadToken::classified());
        }
        let mut selected = Vec::new();
        let mut seen = HashSet::new();
        for reference in changes {
            let id = self.exact_change(reference)?;
            if !seen.insert(id.clone()) {
                return Err(error::DuplicateBatchItem::classified());
            }
            selected.push(id);
        }
        // Requested context supports active reasoning; patches can then receive
        // conclusions without waiting for the final complete discussion synthesis.
        let references = once("policy".to_owned())
            .chain(
                self.contexts
                    .iter()
                    .filter(|(_, context)| {
                        selected.is_empty() || seen.contains(&context.request.change_id)
                    })
                    .map(|(reference, _)| reference.clone()),
            )
            .chain(if selected.is_empty() {
                self.inventory
                    .changes
                    .iter()
                    .map(|change| format!("{}:patch", change.id))
                    .collect::<Vec<_>>()
            } else {
                selected.iter().map(|id| format!("{id}:patch")).collect()
            })
            .chain(selected.is_empty().then(|| "discussion".to_owned()))
            .collect::<Vec<_>>();
        let mut staged: Self = serde_json::from_slice(&serde_json::to_vec(self)?)?;
        let mut response =
            json!({"snapshotDigest": self.snapshot_digest, "readToken": read_token, "items": []});
        for reference in references {
            let content = staged.content(&reference, policy, discussion)?;
            if staged.require_read(&reference, &content).is_ok() {
                continue;
            }
            let offset = staged.reads.get(&reference).copied().unwrap_or(0);
            let Some(candidate) =
                staged.append_required_page(&response, &reference, offset, policy, discussion)?
            else {
                break;
            };
            response = candidate;
        }
        check_response(&response)?;
        *self = staged;
        Ok(response)
    }

    /// Fit a trial page without copying the complete captured source per document.
    fn append_required_page(
        &mut self,
        response: &Value,
        reference: &str,
        offset: usize,
        policy: &str,
        discussion: &str,
    ) -> Result<Option<Value>, AssessmentError> {
        let previous_reads = self.reads.clone();
        let mut limit = RESPONSE_BYTES;
        loop {
            let page = self.read_page(reference, offset, limit, policy, discussion, false)?;
            let mut page = compact_batch_page(page)?;
            page.as_object_mut()
                .ok_or_else(error::InvalidAssessmentBinding::classified)?
                .insert("reference".into(), json!(self.short_reference(reference)?));
            let mut candidate = response.clone();
            let items = candidate
                .get_mut("items")
                .and_then(Value::as_array_mut)
                .ok_or_else(error::InvalidAssessmentBinding::classified)?;
            let has_prefix = !items.is_empty();
            items.push(page);
            match check_response(&candidate) {
                Ok(()) => return Ok(Some(candidate)),
                Err(failure) => {
                    // Trial receipts remain private until their entire response
                    // fits. Restore only this unpublished trial, never live state.
                    self.reads.clone_from(&previous_reads);
                    if has_prefix {
                        return Ok(None);
                    }
                    if limit == 1 {
                        return Err(failure);
                    }
                }
            }
            // Geometric shrinking reaches a fitting page or a single-character
            // failure without relying on elapsed time or a caller's byte estimate.
            let next_limit = limit.div_ceil(2);
            debug_assert!(next_limit < limit && next_limit > 0);
            limit = next_limit;
        }
    }
}
