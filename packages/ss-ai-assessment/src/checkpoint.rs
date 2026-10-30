//! Typed semantic handoffs cannot substitute for authority or complete exact evidence.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::assessment::{check_response, document_digest, hash};
use crate::error::{self, AssessmentError};
use crate::units::{EvidenceUnit, partition};
use crate::{Assessment, Judgment, required_optional};

/// Observations carry explicit obligations, not an approval disposition.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Note {
    /// Behavioral effects observed in the exact evidence.
    pub observations: String,
    /// Relevant contracts or invariants, including why none apply when appropriate.
    pub interfaces: String,
    /// Cross-unit effects requiring explicit cumulative examination.
    pub interactions: Vec<String>,
    /// Potential objections requiring explicit evidence-backed disposition.
    pub concerns: Vec<String>,
    /// Exact native references supporting these observations and obligations.
    pub evidence: Vec<String>,
}

/// Digests bind every semantic input so replacement cannot retain stale conclusions.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Bound<T> {
    pub(crate) value: T,
    pub(crate) dependencies: BTreeMap<String, String>,
}

/// A concern or interaction stays attached to its original note or change judgment.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Check {
    pub(crate) id: String,
    pub(crate) origin: String,
    pub(crate) kind: &'static str,
    pub(crate) observation: String,
    pub(crate) evidence: Vec<String>,
}

/// Immutable work bounds planned handoffs independently of replacement and stale recovery.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Checkpoints {
    pub(crate) units: Vec<EvidenceUnit>,
    pub(crate) manifest_digest: String,
    pub(crate) notes: BTreeMap<String, Bound<Note>>,
    #[serde(deserialize_with = "required_optional")]
    pub(crate) discussion: Option<Bound<Note>>,
    pub(crate) aggregates: BTreeMap<String, BTreeMap<String, String>>,
    pub(crate) resolutions: BTreeMap<String, Bound<Judgment>>,
    pub(crate) completed_work: BTreeSet<String>,
    pub(crate) handoffs: usize,
    pub(crate) checkpoint_progress: usize,
}

impl Checkpoints {
    pub(crate) fn new(
        snapshot: &str,
        sources: &BTreeMap<String, String>,
        discussion: &str,
    ) -> Result<Self, AssessmentError> {
        let mut units = Vec::new();
        for (id, text) in sources {
            units.extend(partition(snapshot, id, &format!("{id}:patch"), text)?);
        }
        units.extend(partition(snapshot, "discussion", "discussion", discussion)?);
        Ok(Self {
            manifest_digest: hash(&units)?,
            units,
            notes: BTreeMap::new(),
            discussion: None,
            aggregates: BTreeMap::new(),
            resolutions: BTreeMap::new(),
            completed_work: BTreeSet::new(),
            handoffs: 0,
            checkpoint_progress: 0,
        })
    }
}

impl Assessment {
    /// Pages immutable unit metadata without loading source or semantic notes.
    pub fn units(&self, after: usize, limit: usize) -> Result<Value, AssessmentError> {
        self.units_for(None, after, limit)
    }

    /// Selects one original document without paging unrelated changed-path units.
    pub fn units_for(
        &self,
        reference: Option<&str>,
        after: usize,
        limit: usize,
    ) -> Result<Value, AssessmentError> {
        if reference.is_some_and(|reference| {
            reference != "discussion"
                && !self
                    .source_evidence
                    .keys()
                    .any(|id| reference == format!("{id}:patch"))
        }) {
            return Err(error::InvalidReference::classified());
        }
        let records = self
            .checkpoints
            .units
            .iter()
            .filter(|unit| reference.is_none_or(|reference| reference == unit.reference))
            .map(|unit| {
                Ok(json!({
                    "unit": unit.metadata(), "reference": unit.reference(),
                    "bindingReference": unit.binding_reference(),
                    "noteReference": format!("note:{}", unit.id),
                    "noteDigest": self.checkpoints.notes.get(&unit.id).map(hash).transpose()?,
                    "readOffset": self.reads.get(&unit.reference()),
                    "completed": self.checkpoints.notes.contains_key(&unit.id)
                }))
            });
        page(records, after, limit, &self.snapshot_digest)
    }

    /// Records a completed unit or the single discussion aggregate.
    pub fn note(
        &mut self,
        id: &str,
        note: Note,
        previous_digest: Option<&str>,
        policy: &str,
        discussion: &str,
    ) -> Result<String, AssessmentError> {
        if self.complete {
            return Err(error::ImmutableAssessment::classified());
        }
        let dependencies = self.note_dependencies(id, &note, policy, discussion)?;
        let source = self
            .checkpoints
            .units
            .iter()
            .find(|unit| unit.id == id)
            .map(|unit| unit.reference.as_str());
        self.required_contexts(source, policy, discussion)?;
        let bound = Bound {
            value: note,
            dependencies,
        };
        let previous = if id == "discussion" {
            self.checkpoints.discussion.as_ref()
        } else {
            self.checkpoints.notes.get(id)
        };
        let digest = hash(&bound)?;
        if previous == Some(&bound) {
            return Ok(digest);
        }
        if previous.map(hash).transpose()?.as_deref() != previous_digest {
            return Err(error::StaleJudgment::classified());
        }
        let replacing = previous.is_some();
        let reference = if id == "discussion" {
            "discussion-note".to_owned()
        } else {
            format!("note:{id}")
        };
        if replacing {
            self.invalidate(&reference, policy, discussion)?;
        }
        if id == "discussion" {
            self.checkpoints.discussion = Some(bound);
        } else {
            self.checkpoints.notes.insert(id.to_owned(), bound);
            self.checkpoints.completed_work.insert(reference);
        }
        if replacing {
            self.remove_obsolete_reads(policy, discussion);
        }
        Ok(digest)
    }

    /// Lists every recorded obligation, including already resolved checks.
    pub fn checks(&self, after: usize, limit: usize) -> Result<Value, AssessmentError> {
        let checks = self.check_records()?;
        let records = checks.iter().map(|check| Ok(json!({
            "id": check.id, "origin": check.origin, "kind": check.kind,
            "reference": format!("check:{}", check.id),
            "resolutionReference": format!("resolution:{}", check.id),
            "resolutionDigest": self.checkpoints.resolutions.get(&check.id).map(hash).transpose()?
        })));
        page(records, after, limit, &self.snapshot_digest)
    }

    /// Each recorded concern needs an independent evidence-backed cumulative disposition.
    pub fn resolve(
        &mut self,
        id: &str,
        judgment: Judgment,
        previous_digest: Option<&str>,
        policy: &str,
        discussion: &str,
    ) -> Result<String, AssessmentError> {
        if self.complete {
            return Err(error::ImmutableAssessment::classified());
        }
        if judgment.rationale.trim().is_empty()
            || !judgment.concerns.is_empty()
            || !judgment.interactions.is_empty()
            || judgment
                .evidence
                .iter()
                .any(|reference| reference.starts_with("resolution:"))
        {
            return Err(error::InvalidCheckpointNote::classified());
        }
        self.require_read("policy", policy)?;
        let check = self
            .check_records()?
            .into_iter()
            .find(|check| check.id == id)
            .ok_or_else(error::UnknownCheck::classified)?;
        if !judgment
            .evidence
            .iter()
            .any(|reference| self.raw_reference(reference))
        {
            return Err(error::MissingExactEvidence::classified());
        }
        let mut dependencies = self.dependencies(&judgment.evidence, policy, discussion)?;
        self.bind_read(
            &format!("check:{id}"),
            &mut dependencies,
            policy,
            discussion,
        )?;
        self.bind_read(&check.origin, &mut dependencies, policy, discussion)?;
        self.required_contexts(None, policy, discussion)?;
        let bound = Bound {
            value: judgment,
            dependencies,
        };
        let previous = self.checkpoints.resolutions.get(id);
        let digest = hash(&bound)?;
        if previous == Some(&bound) {
            return Ok(digest);
        }
        if previous.map(hash).transpose()?.as_deref() != previous_digest {
            return Err(error::StaleJudgment::classified());
        }
        let replacing = previous.is_some();
        if replacing {
            self.invalidate(&format!("resolution:{id}"), policy, discussion)?;
        }
        self.checkpoints.resolutions.insert(id.to_owned(), bound);
        if replacing {
            self.remove_obsolete_reads(policy, discussion);
        }
        Ok(digest)
    }

    /// Replayed reads and replacement notes never earn another planned handoff.
    pub fn checkpoint(&mut self) -> Result<Value, AssessmentError> {
        if self.complete
            || self.checkpoints.completed_work.len() <= self.checkpoints.checkpoint_progress
            || self.checkpoints.handoffs
                >= self
                    .checkpoints
                    .units
                    .len()
                    .saturating_add(self.inventory.changes.len())
        {
            return Err(error::NoCheckpointProgress::classified());
        }
        self.checkpoints.handoffs = self.checkpoints.handoffs.saturating_add(1);
        self.checkpoints.checkpoint_progress = self.checkpoints.completed_work.len();
        Ok(self.progress())
    }

    /// Aggregates consume every relevant note and bind all exact citations.
    pub(crate) fn aggregate_dependencies(
        &self,
        id: &str,
        judgment: &Judgment,
        policy: &str,
        discussion: &str,
    ) -> Result<BTreeMap<String, String>, AssessmentError> {
        let mut dependencies = self.dependencies(&judgment.evidence, policy, discussion)?;
        if id == "cumulative" {
            for change in &self.inventory.changes {
                self.bind_read(
                    &format!("judgment:{}", change.id),
                    &mut dependencies,
                    policy,
                    discussion,
                )?;
            }
            self.bind_read("discussion-note", &mut dependencies, policy, discussion)?;
            for check in self.check_records()? {
                self.bind_read(
                    &format!("check:{}", check.id),
                    &mut dependencies,
                    policy,
                    discussion,
                )?;
                self.bind_read(
                    &format!("resolution:{}", check.id),
                    &mut dependencies,
                    policy,
                    discussion,
                )?;
            }
        } else {
            for unit in self
                .checkpoints
                .units
                .iter()
                .filter(|unit| unit.reference == format!("{id}:patch"))
            {
                self.bind_read(
                    &format!("note:{}", unit.id),
                    &mut dependencies,
                    policy,
                    discussion,
                )?;
            }
        }
        Ok(dependencies)
    }

    /// Checks the complete manifest and every persisted dependency at the reader boundary.
    pub(crate) fn validate_checkpoints(
        &self,
        policy: &str,
        discussion: &str,
    ) -> Result<(), AssessmentError> {
        let sources = self
            .source_evidence
            .iter()
            .map(|(id, source)| Ok((id.clone(), source.document()?)))
            .collect::<Result<BTreeMap<_, _>, AssessmentError>>()?;
        let expected = Checkpoints::new(&self.snapshot_digest, &sources, discussion)?;
        if self.checkpoints.units != expected.units
            || self.checkpoints.manifest_digest != expected.manifest_digest
            || self.checkpoints.handoffs > self.checkpoints.checkpoint_progress
            || self.checkpoints.checkpoint_progress > self.checkpoints.completed_work.len()
        {
            return Err(error::InvalidUnitManifest::classified());
        }
        for work in &self.checkpoints.completed_work {
            if let Some(id) = work.strip_prefix("note:") {
                if !self.checkpoints.units.iter().any(|unit| unit.id == id) {
                    return Err(error::InvalidUnitManifest::classified());
                }
            } else if let Some(id) = work.strip_prefix("judgment:") {
                if !self.inventory.changes.iter().any(|change| change.id == id) {
                    return Err(error::InvalidUnitManifest::classified());
                }
            } else {
                return Err(error::InvalidUnitManifest::classified());
            }
        }
        for (id, bound) in &self.checkpoints.notes {
            if bound.dependencies != self.note_dependencies(id, &bound.value, policy, discussion)? {
                return Err(error::InvalidCheckpointNote::classified());
            }
            self.validate_dependencies(&bound.dependencies, policy, discussion)?;
        }
        if let Some(bound) = &self.checkpoints.discussion {
            if bound.dependencies
                != self.note_dependencies("discussion", &bound.value, policy, discussion)?
            {
                return Err(error::InvalidCheckpointNote::classified());
            }
            self.validate_dependencies(&bound.dependencies, policy, discussion)?;
        }
        if self.checkpoints.aggregates.len()
            != self
                .decisions
                .len()
                .saturating_add(usize::from(self.cumulative.is_some()))
        {
            return Err(error::StaleAggregate::classified());
        }
        for (id, dependencies) in &self.checkpoints.aggregates {
            let judgment = if id == "cumulative" {
                self.cumulative.as_ref()
            } else {
                self.decisions.get(id)
            }
            .ok_or_else(error::StaleAggregate::classified)?;
            if *dependencies != self.aggregate_dependencies(id, judgment, policy, discussion)? {
                return Err(error::StaleAggregate::classified());
            }
        }
        let checks = self.check_records()?;
        for (id, bound) in &self.checkpoints.resolutions {
            let check = checks
                .iter()
                .find(|check| check.id == *id)
                .ok_or_else(error::UnknownCheck::classified)?;
            if bound.value.rationale.trim().is_empty()
                || !bound.value.concerns.is_empty()
                || !bound.value.interactions.is_empty()
                || bound
                    .value
                    .evidence
                    .iter()
                    .any(|reference| reference.starts_with("resolution:"))
                || !bound
                    .value
                    .evidence
                    .iter()
                    .any(|reference| self.raw_reference(reference))
            {
                return Err(error::InvalidCheckpointNote::classified());
            }
            let mut expected = self.dependencies(&bound.value.evidence, policy, discussion)?;
            self.bind_read(&format!("check:{id}"), &mut expected, policy, discussion)?;
            self.bind_read(&check.origin, &mut expected, policy, discussion)?;
            if expected != bound.dependencies {
                return Err(error::StaleAggregate::classified());
            }
            self.validate_dependencies(&bound.dependencies, policy, discussion)?;
        }
        Ok(())
    }

    /// Producer and consumer enforce the same fixed-level note contract.
    fn note_dependencies(
        &self,
        id: &str,
        note: &Note,
        policy: &str,
        discussion: &str,
    ) -> Result<BTreeMap<String, String>, AssessmentError> {
        validate_note(note)?;
        if note.evidence.iter().any(|reference| {
            reference != "policy"
                && !self.raw_reference(reference)
                && !(id == "discussion" && reference.starts_with("note:"))
        }) {
            return Err(error::MissingExactEvidence::classified());
        }
        self.require_read("policy", policy)?;
        let mut dependencies = self.dependencies(&note.evidence, policy, discussion)?;
        if id == "discussion" {
            self.require_read("discussion", discussion)?;
            for unit in self
                .checkpoints
                .units
                .iter()
                .filter(|unit| unit.reference == "discussion")
            {
                self.bind_read(
                    &format!("note:{}", unit.id),
                    &mut dependencies,
                    policy,
                    discussion,
                )?;
            }
            if !note
                .evidence
                .iter()
                .any(|reference| self.raw_reference(reference))
            {
                return Err(error::MissingExactEvidence::classified());
            }
        } else {
            let unit = self
                .checkpoints
                .units
                .iter()
                .find(|unit| unit.id == id)
                .ok_or_else(error::UnknownUnit::classified)?;
            if !note.evidence.contains(&unit.reference()) {
                return Err(error::MissingExactEvidence::classified());
            }
            self.bind_read(&unit.reference(), &mut dependencies, policy, discussion)?;
            self.bind_read(
                &unit.binding_reference(),
                &mut dependencies,
                policy,
                discussion,
            )?;
        }
        Ok(dependencies)
    }

    /// Derived references expose typed notes and checks through the existing byte-bounded reader.
    pub(crate) fn checkpoint_content(
        &self,
        reference: &str,
        policy: &str,
        discussion: &str,
    ) -> Result<Option<String>, AssessmentError> {
        if let Some(id) = reference.strip_prefix("unit:") {
            let unit = self
                .checkpoints
                .units
                .iter()
                .find(|unit| unit.id == id)
                .ok_or_else(error::UnknownUnit::classified)?;
            return Ok(Some(unit.content(&self.content(
                &unit.reference,
                policy,
                discussion,
            )?)));
        }
        if let Some(id) = reference.strip_prefix("binding:") {
            let unit = self
                .checkpoints
                .units
                .iter()
                .find(|unit| unit.id == id)
                .ok_or_else(error::UnknownUnit::classified)?;
            return Ok(Some(serde_json::to_string(&unit.bindings)?));
        }
        if let Some(id) = reference.strip_prefix("note:") {
            return Ok(Some(serde_json::to_string(
                self.checkpoints
                    .notes
                    .get(id)
                    .ok_or_else(error::IncompleteUnits::classified)?,
            )?));
        }
        if reference == "discussion-note" {
            return Ok(Some(serde_json::to_string(
                self.checkpoints
                    .discussion
                    .as_ref()
                    .ok_or_else(error::IncompleteUnits::classified)?,
            )?));
        }
        if let Some(id) = reference.strip_prefix("judgment:") {
            return Ok(Some(serde_json::to_string(
                self.decisions
                    .get(id)
                    .ok_or_else(error::IncompleteCoverage::classified)?,
            )?));
        }
        if let Some(id) = reference.strip_prefix("check:") {
            let check = self
                .check_records()?
                .into_iter()
                .find(|check| check.id == id)
                .ok_or_else(error::UnknownCheck::classified)?;
            return Ok(Some(serde_json::to_string(&check)?));
        }
        if let Some(id) = reference.strip_prefix("resolution:") {
            return Ok(Some(serde_json::to_string(
                self.checkpoints
                    .resolutions
                    .get(id)
                    .ok_or_else(error::UnresolvedCheck::classified)?,
            )?));
        }
        Ok(None)
    }

    pub(crate) fn unit_coverage(&self, reference: &str) -> bool {
        let units = self
            .checkpoints
            .units
            .iter()
            .filter(|unit| unit.reference == reference)
            .collect::<Vec<_>>();
        !units.is_empty()
            && units.iter().all(|unit| {
                self.reads.get(&unit.reference()).copied() == Some(unit.total_characters)
            })
    }

    /// Replaced semantic records have no valid delivery receipt under their former identity.
    pub(crate) fn remove_obsolete_reads(&mut self, policy: &str, discussion: &str) {
        let obsolete = self
            .reads
            .keys()
            .filter(|key| self.content(key, policy, discussion).is_err())
            .cloned()
            .collect::<Vec<_>>();
        for key in obsolete {
            self.reads.remove(&key);
        }
    }

    pub(crate) fn bind_read(
        &self,
        reference: &str,
        dependencies: &mut BTreeMap<String, String>,
        policy: &str,
        discussion: &str,
    ) -> Result<(), AssessmentError> {
        let content = self.content(reference, policy, discussion)?;
        self.require_read(reference, &content)?;
        dependencies.insert(reference.to_owned(), document_digest(&content));
        Ok(())
    }

    fn dependencies(
        &self,
        references: &[String],
        policy: &str,
        discussion: &str,
    ) -> Result<BTreeMap<String, String>, AssessmentError> {
        let mut dependencies = BTreeMap::new();
        for reference in references {
            if dependencies.contains_key(reference) {
                return Err(error::RepeatedJudgmentEvidence::classified());
            }
            self.bind_read(reference, &mut dependencies, policy, discussion)?;
        }
        Ok(dependencies)
    }

    fn validate_dependencies(
        &self,
        dependencies: &BTreeMap<String, String>,
        policy: &str,
        discussion: &str,
    ) -> Result<(), AssessmentError> {
        for (reference, digest) in dependencies {
            let content = self.content(reference, policy, discussion)?;
            self.require_read(reference, &content)?;
            if *digest != document_digest(&content) {
                return Err(error::StaleAggregate::classified());
            }
        }
        Ok(())
    }

    fn raw_reference(&self, reference: &str) -> bool {
        reference == "discussion"
            || reference.starts_with("unit:")
            || self.contexts.contains_key(reference)
            || self
                .inventory
                .changes
                .iter()
                .any(|change| reference.starts_with(&format!("{}:", change.id)))
    }

    pub(crate) fn required_contexts(
        &self,
        source: Option<&str>,
        policy: &str,
        discussion: &str,
    ) -> Result<(), AssessmentError> {
        for (reference, context) in &self.contexts {
            if source.is_none_or(|source| source == format!("{}:patch", context.request.change_id))
            {
                self.require_read(reference, &self.content(reference, policy, discussion)?)?;
            }
        }
        Ok(())
    }

    pub(crate) fn check_records(&self) -> Result<Vec<Check>, AssessmentError> {
        let mut checks = Vec::new();
        for (id, bound) in &self.checkpoints.notes {
            append_checks(
                &mut checks,
                &format!("note:{id}"),
                &bound.value.concerns,
                &bound.value.interactions,
                &bound.value.evidence,
                &hash(bound)?,
            )?;
        }
        if let Some(bound) = &self.checkpoints.discussion {
            append_checks(
                &mut checks,
                "discussion-note",
                &bound.value.concerns,
                &bound.value.interactions,
                &bound.value.evidence,
                &hash(bound)?,
            )?;
        }
        for (id, judgment) in &self.decisions {
            append_checks(
                &mut checks,
                &format!("judgment:{id}"),
                &judgment.concerns,
                &judgment.interactions,
                &judgment.evidence,
                &hash(judgment)?,
            )?;
        }
        Ok(checks)
    }

    /// Replacement removes every transitive dependent; unrelated notes remain reusable.
    pub(crate) fn invalidate(
        &mut self,
        reference: &str,
        policy: &str,
        discussion: &str,
    ) -> Result<(), AssessmentError> {
        self.reads.remove(reference);
        self.cumulative = None;
        self.checkpoints.aggregates.remove("cumulative");
        let mut removed = BTreeSet::from([reference.to_owned()]);
        let original_checks = self.check_records()?;
        // Each iteration removes a persisted node. No recursive summarization or retry is involved.
        loop {
            let before = removed.len();
            let notes = self
                .checkpoints
                .notes
                .iter()
                .filter(|(_, bound)| bound.dependencies.keys().any(|key| removed.contains(key)))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in notes {
                self.checkpoints.notes.remove(&id);
                removed.insert(format!("note:{id}"));
            }
            if self
                .checkpoints
                .discussion
                .as_ref()
                .is_some_and(|bound| bound.dependencies.keys().any(|key| removed.contains(key)))
            {
                self.checkpoints.discussion = None;
                removed.insert("discussion-note".to_owned());
            }
            let aggregates = self
                .checkpoints
                .aggregates
                .iter()
                .filter(|(_, deps)| deps.keys().any(|key| removed.contains(key)))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in aggregates {
                self.decisions.remove(&id);
                self.checkpoints.aggregates.remove(&id);
                removed.insert(format!("judgment:{id}"));
            }
            if before == removed.len() {
                break;
            }
        }
        let checks = self
            .check_records()?
            .into_iter()
            .map(|check| check.id)
            .collect::<BTreeSet<_>>();
        for check in original_checks {
            if removed.contains(&check.origin) || !checks.contains(&check.id) {
                removed.insert(format!("check:{}", check.id));
            }
        }
        self.checkpoints.resolutions.retain(|id, bound| {
            checks.contains(id) && !bound.dependencies.keys().any(|key| removed.contains(key))
        });
        let obsolete = self
            .reads
            .keys()
            .filter(|key| removed.contains(*key) || self.content(key, policy, discussion).is_err())
            .cloned()
            .collect::<Vec<_>>();
        for key in obsolete {
            self.reads.remove(&key);
        }
        Ok(())
    }
}

/// Notes stay small enough to read as semantic inputs rather than recursively summarize.
fn validate_note(note: &Note) -> Result<(), AssessmentError> {
    if note.observations.trim().is_empty() || note.interfaces.trim().is_empty()
        || note.evidence.is_empty()
        || note.concerns.iter().chain(&note.interactions).any(|text| text.trim().is_empty())
        // Half a response budget leaves space for exact dependency bindings.
        || serde_json::to_vec(note)?.len() > 8192
    {
        return Err(error::InvalidCheckpointNote::classified());
    }
    Ok(())
}

/// Stable obligations distinguish identical excerpts used in different threads.
fn append_checks(
    checks: &mut Vec<Check>,
    origin: &str,
    concerns: &[String],
    interactions: &[String],
    evidence: &[String],
    digest: &str,
) -> Result<(), AssessmentError> {
    for (kind, observations) in [("concern", concerns), ("interaction", interactions)] {
        for (index, observation) in observations.iter().enumerate() {
            checks.push(Check {
                id: hash(&(origin, digest, kind, index, observation))?,
                origin: origin.to_owned(),
                kind,
                observation: observation.clone(),
                evidence: evidence.to_vec(),
            });
        }
    }
    Ok(())
}

/// Metadata stops at whole item boundaries; text uses the regular read command.
fn page(
    records: impl Iterator<Item = Result<Value, AssessmentError>>,
    after: usize,
    limit: usize,
    snapshot: &str,
) -> Result<Value, AssessmentError> {
    let mut records = records.skip(after).peekable();
    let mut items = Vec::new();
    let mut next = after;
    let mut overflow = false;
    for record in records.by_ref().take(limit) {
        items.push(record?);
        next = next.saturating_add(1);
        if check_response(&json!({"snapshotDigest":snapshot, "items":items, "nextOffset":next}))
            .is_err()
        {
            items.pop();
            next = next.saturating_sub(1);
            if items.is_empty() {
                return Err(error::OversizedResponse::classified());
            }
            overflow = true;
            break;
        }
    }
    let result = json!({"snapshotDigest":snapshot, "items":items, "nextOffset":(overflow || records.peek().is_some()).then_some(next)});
    check_response(&result)?;
    Ok(result)
}
