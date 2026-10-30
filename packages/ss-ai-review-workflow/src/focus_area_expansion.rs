//! Reference-expansion records and inherited applicability chains.

use std::collections::HashSet;
use std::path::Path;

use ohno::AppError;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use ss_ai_review_context::powershell_json;
use ss_ai_runtime::{Clock, FileTransaction, LockMode, ResourceLock, caused_by, fail, read_bytes};

use crate::focus_area_source::{SourceSnapshot, Span, decode_text, source_path};
use crate::workflow_plan::parse_json;

/// A reference source and the scope inherited from that source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "The wire schema defines these exact fields"
)]
pub struct Referrer {
    /// Parent reference, or null for a materialized direct source.
    #[serde(deserialize_with = "required_option")]
    pub parent_id: Option<String>,
    /// Exact source span containing the reference.
    pub source: Span,
    /// Agent-authored scope judgment.
    pub scope: String,
    /// Exact source spans supporting the scope judgment.
    pub declarations: Vec<Span>,
}

/// An agent-authored registration for a canonical reference.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "The wire schema defines these exact fields"
)]
pub struct ReferenceInput {
    /// Stable identifier for this target.
    pub id: String,
    /// Canonical target URI.
    pub uri: String,
    /// Independent contexts that refer to this target.
    pub referrers: Vec<Referrer>,
}

/// An agent-authored result for a previously registered target.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "The wire schema defines these exact fields"
)]
pub struct ReferenceResult {
    /// Registered target identifier.
    pub reference_id: String,
    /// Expanded paths relative to the raw source root.
    pub outputs: Vec<String>,
    /// Failure explanation, or null for success.
    #[serde(deserialize_with = "required_option")]
    pub error: Option<String>,
}

/// One reference and its recorded expansion outcome.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    id: String,
    uri: String,
    referrers: Vec<Referrer>,
    state: ReferenceState,
    outputs: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    error: Option<String>,
}

/// One reference edge in a resolved inheritance chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReferenceContextLink {
    reference_id: String,
    pub(crate) uri: String,
    source: Span,
    scope: String,
    pub(crate) declarations: Vec<Span>,
}

/// The schema's reference outcome vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum ReferenceState {
    Pending,
    Complete,
    Failed,
}

/// The schema's aggregate expansion state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum ExpansionState {
    Unknown,
    Complete,
    Degraded,
}

/// Identifies unfinished discovery when finalization is requested.
#[ohno::error]
#[display("References are still pending")]
struct PendingReferences;

/// The full versioned expansion manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Expansion {
    schema_version: u32,
    state: ExpansionState,
    references: Vec<Reference>,
}

/// A single transaction on the expansion manifest.
#[derive(Debug)]
#[expect(
    clippy::exhaustive_enums,
    reason = "The variants define the supported operations"
)]
pub enum Operation {
    /// Create an empty, unfinished manifest.
    Initialize,
    /// Add a target or another distinct referring context.
    Register(ReferenceInput),
    /// Record a successful worker result.
    Complete(ReferenceResult),
    /// Record a failed worker result.
    Fail(ReferenceResult),
    /// Derive the aggregate state after discovery finishes.
    Finalize,
    /// Read the manifest and optionally expand one target's chains.
    Read(Option<String>),
}

/// Requires explicit null for nullable schema fields instead of accepting omissions.
fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

impl Expansion {
    /// Reports whether finalization completed without failed references.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.state == ExpansionState::Complete
    }

    /// Returns every currently registered chain for one reference.
    ///
    /// # Errors
    ///
    /// Returns an error if the reference is absent or its chain is cyclic.
    pub fn reference_contexts(&self, id: &str) -> Result<Vec<Value>, AppError> {
        self.contexts(id)
    }

    /// Returns reference chains for a raw output when a target claims it.
    ///
    /// The preparation agent uses these chains to verify authored alternatives.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid path or an inconsistent reference chain.
    pub fn output_contexts(&self, path: &str) -> Result<Option<Vec<Value>>, AppError> {
        self.output_chains(path)?
            .map(Self::chains_to_values)
            .transpose()
    }

    /// Resolves claimed output paths without losing the structure of each edge.
    pub(crate) fn output_chains(
        &self,
        path: &str,
    ) -> Result<Option<Vec<Vec<ReferenceContextLink>>>, AppError> {
        let path = source_path(path)?;
        for reference in &self.references {
            for output in &reference.outputs {
                if source_path(output)? == path {
                    return self.chains(&reference.id, &mut Vec::new()).map(Some);
                }
            }
        }
        Ok(None)
    }

    /// Parses and validates a stored manifest against a captured source generation.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed records, broken references, or invalid spans.
    pub fn parse(
        bytes: &[u8],
        sources: &SourceSnapshot,
        require_final: bool,
    ) -> Result<Self, AppError> {
        let document = decode_text(bytes)?;
        let document = parse_json(document.as_bytes())?;
        let document: Self = serde_json::from_value(document)
            .map_err(|error| caused_by("invalidInput", "Invalid expansion manifest", error))?;
        document.validate(sources, require_final)?;
        Ok(document)
    }

    /// Validates source references, output ownership, cycles, and finality.
    ///
    /// Scope interpretation and discovering references remain agent judgments.
    ///
    /// # Errors
    ///
    /// Returns an error when the document cannot safely represent its sources.
    pub fn validate(&self, sources: &SourceSnapshot, require_final: bool) -> Result<(), AppError> {
        if self.schema_version != 1 {
            return Err(fail("invalidInput", "Invalid expansion schema version"));
        }
        let mut ids = HashSet::new();
        let mut uris = HashSet::new();
        let mut outputs = HashSet::new();
        let mut pending = false;
        let mut failed = false;
        for reference in &self.references {
            if !valid_id(&reference.id)
                || reference.uri.trim().is_empty()
                || !ids.insert(&reference.id)
                || !uris.insert(&reference.uri)
                || reference.referrers.is_empty()
            {
                return Err(fail("invalidInput", "Invalid or duplicate reference"));
            }
            match reference.state {
                ReferenceState::Pending
                    if reference.outputs.is_empty() && reference.error.is_none() =>
                {
                    pending = true;
                }
                ReferenceState::Complete
                    if !reference.outputs.is_empty() && reference.error.is_none() => {}
                ReferenceState::Failed
                    if reference.outputs.is_empty()
                        && reference
                            .error
                            .as_ref()
                            .is_some_and(|error| !error.trim().is_empty()) =>
                {
                    failed = true;
                }
                _ => {
                    return Err(fail(
                        "invalidInput",
                        "Reference outcome disagrees with its state",
                    ));
                }
            }
            for output in &reference.outputs {
                let path = source_path(output)?;
                sources.raw_output(output)?;
                if !outputs.insert(path) {
                    return Err(fail(
                        "invalidInput",
                        "Multiple references claim the same output",
                    ));
                }
            }
            for referrer in &reference.referrers {
                if referrer.scope.trim().is_empty() {
                    return Err(fail("invalidInput", "Referrer scope is blank"));
                }
                sources.span(&referrer.source)?;
                for declaration in &referrer.declarations {
                    sources.span(declaration)?;
                }
                if let Some(parent_id) = &referrer.parent_id {
                    if !valid_id(parent_id) || referrer.source.root != "instruction-set-raw" {
                        return Err(fail("invalidInput", "Invalid nested referrer"));
                    }
                    let parent = self.reference(parent_id)?;
                    if parent.state != ReferenceState::Complete
                        || !parent.outputs.iter().any(|output| {
                            source_path(output).ok() == source_path(&referrer.source.path).ok()
                        })
                    {
                        return Err(fail(
                            "invalidInput",
                            "Nested referrer must name a completed parent output",
                        ));
                    }
                } else if referrer.source.root != "instruction-set-source" {
                    return Err(fail(
                        "invalidInput",
                        "Direct referrer must name a materialized source",
                    ));
                }
            }
            self.chains(&reference.id, &mut Vec::new())?;
        }
        let expected = if failed {
            ExpansionState::Degraded
        } else {
            ExpansionState::Complete
        };
        if self.state != ExpansionState::Unknown && (pending || self.state != expected) {
            return Err(fail(
                "invalidInput",
                "Expansion state disagrees with reference outcomes",
            ));
        }
        if require_final && (pending || self.state == ExpansionState::Unknown) {
            return Err(fail("invalidInput", "Expansion must be finalized"));
        }
        Ok(())
    }

    /// Resolves every distinct referring chain, including late parent additions.
    fn contexts(&self, id: &str) -> Result<Vec<Value>, AppError> {
        Self::chains_to_values(self.chains(id, &mut Vec::new())?)
    }

    /// Preserves the established JSON chain representation for command callers.
    fn chains_to_values(chains: Vec<Vec<ReferenceContextLink>>) -> Result<Vec<Value>, AppError> {
        chains
            .into_iter()
            .map(|chain| {
                let chain = serde_json::to_value(chain).map_err(|error| {
                    caused_by("invalidInput", "Cannot serialize reference chain", error)
                })?;
                Ok(json!({"chain": chain}))
            })
            .collect()
    }

    /// Follows each referrer independently and rejects path-local cycles.
    fn chains(
        &self,
        id: &str,
        ancestors: &mut Vec<String>,
    ) -> Result<Vec<Vec<ReferenceContextLink>>, AppError> {
        if ancestors.iter().any(|ancestor| ancestor == id) {
            return Err(fail("invalidInput", "Reference cycle detected"));
        }
        ancestors.push(id.to_owned());
        let reference = self.reference(id)?;
        let mut chains = Vec::new();
        for referrer in &reference.referrers {
            let local = ReferenceContextLink {
                reference_id: reference.id.clone(),
                uri: reference.uri.clone(),
                source: referrer.source.clone(),
                scope: referrer.scope.clone(),
                declarations: referrer.declarations.clone(),
            };
            if let Some(parent) = &referrer.parent_id {
                for mut chain in self.chains(parent, ancestors)? {
                    chain.push(local.clone());
                    chains.push(chain);
                }
            } else {
                chains.push(vec![local]);
            }
        }
        ancestors.pop();
        Ok(chains)
    }

    /// Resolves a unique reference by its case-sensitive identifier.
    fn reference(&self, id: &str) -> Result<&Reference, AppError> {
        let mut matches = self
            .references
            .iter()
            .filter(|reference| reference.id == id);
        let reference = matches
            .next()
            .ok_or_else(|| fail("invalidInput", "Unknown reference"))?;
        if matches.next().is_some() {
            return Err(fail("invalidInput", "Repeated reference identifier"));
        }
        Ok(reference)
    }

    /// Applies an operation to a private copy before any publication occurs.
    fn apply(&mut self, operation: &Operation, sources: &SourceSnapshot) -> Result<(), AppError> {
        if !matches!(operation, Operation::Read(_) | Operation::Finalize)
            && self.state != ExpansionState::Unknown
        {
            return Err(fail(
                "invalidInput",
                "Finalized expansion inputs are immutable",
            ));
        }
        match operation {
            Operation::Register(input) => {
                if !valid_id(&input.id) || input.uri.trim().is_empty() || input.referrers.is_empty()
                {
                    return Err(fail("invalidInput", "Invalid reference registration"));
                }
                if let Some(reference) = self
                    .references
                    .iter_mut()
                    .find(|reference| reference.id == input.id)
                {
                    if reference.uri != input.uri {
                        return Err(fail("invalidInput", "Reference ID cannot change URI"));
                    }
                    for referrer in &input.referrers {
                        if !reference.referrers.contains(referrer) {
                            reference.referrers.push(referrer.clone());
                        }
                    }
                } else {
                    self.references.push(Reference {
                        id: input.id.clone(),
                        uri: input.uri.clone(),
                        referrers: input.referrers.clone(),
                        state: ReferenceState::Pending,
                        outputs: Vec::new(),
                        error: None,
                    });
                }
            }
            Operation::Complete(result) | Operation::Fail(result) => {
                let reference = self
                    .references
                    .iter_mut()
                    .find(|reference| reference.id == result.reference_id)
                    .ok_or_else(|| fail("invalidInput", "Unknown reference"))?;
                if matches!(operation, Operation::Complete(_)) {
                    if result.error.is_some() {
                        return Err(fail(
                            "invalidInput",
                            "Successful result cannot carry an error",
                        ));
                    }
                    reference.state = ReferenceState::Complete;
                    reference.outputs.clone_from(&result.outputs);
                    reference.error = None;
                } else {
                    if !result.outputs.is_empty() {
                        return Err(fail("invalidInput", "Failed result cannot claim outputs"));
                    }
                    reference.state = ReferenceState::Failed;
                    reference.outputs.clear();
                    reference.error.clone_from(&result.error);
                }
            }
            Operation::Finalize => {
                if self
                    .references
                    .iter()
                    .any(|reference| reference.state == ReferenceState::Pending)
                {
                    return Err(caused_by(
                        "invalidInput",
                        "Cannot finalize pending references",
                        PendingReferences::new(),
                    )
                    .into());
                }
                self.state = if self
                    .references
                    .iter()
                    .any(|reference| reference.state == ReferenceState::Failed)
                {
                    ExpansionState::Degraded
                } else {
                    ExpansionState::Complete
                };
            }
            Operation::Read(_) => {}
            Operation::Initialize => return Err(fail("invalidInput", "Manifest already exists")),
        }
        self.validate(sources, false)
    }

    /// Produces the established operation summary without losing alternatives.
    fn summary(&self, reference: Option<&str>) -> Result<Value, AppError> {
        let pending: Vec<_> = self
            .references
            .iter()
            .filter(|reference| reference.state == ReferenceState::Pending)
            .map(|reference| &reference.id)
            .collect();
        let failed: Vec<_> = self.references.iter().filter(|reference| reference.state == ReferenceState::Failed)
            .map(|reference| json!({"referenceId": reference.id, "uri": reference.uri, "error": reference.error})).collect();
        let contexts = reference
            .map(|id| self.contexts(id))
            .transpose()?
            .unwrap_or_default();
        Ok(
            json!({"state": self.state, "referenceCount": self.references.len(),
            "pendingReferenceIds": pending, "failedReferences": failed, "contexts": contexts}),
        )
    }
}

/// Checks the workflow schema's reference identifier vocabulary.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Reads the manifest while the caller owns the session operation lock.
///
/// This function does not acquire locks and can be used inside a cache transaction.
///
/// # Errors
///
/// Returns an error for missing or invalid sources or manifest.
pub fn read_locked(
    session: &Path,
    clock: &dyn Clock,
    require_final: bool,
) -> Result<Expansion, AppError> {
    let sources = SourceSnapshot::read(session, clock)?;
    let path = session.join("focus-area-expansion-status.json");
    let bytes = read_bytes(&path, clock)?
        .ok_or_else(|| fail("invalidInput", "Expansion manifest is missing"))?;
    Expansion::parse(&bytes, &sources, require_final)
}

/// Updates the manifest under the coordinated source and record locks.
///
/// The source generation is captured before reading or publishing the record.
/// The returned JSON has the same summary shape as the PowerShell command.
///
/// # Errors
///
/// Returns an error for invalid state, input, source data, or I/O failure.
pub fn update(session: &Path, operation: Operation, clock: &dyn Clock) -> Result<Value, AppError> {
    update_with_reference(session, operation, None, clock)
}

/// Updates the manifest and optionally returns another reference's chains.
///
/// The explicit reference takes precedence over the result's identifier.
///
/// # Errors
///
/// Returns an error for invalid state, input, source data, or I/O failure.
#[expect(
    clippy::needless_pass_by_value,
    reason = "The transaction API consumes the caller's authored operation"
)]
pub fn update_with_reference(
    session: &Path,
    operation: Operation,
    reference_id: Option<&str>,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    if !session.is_absolute() {
        return Err(fail("invalidInput", "Session path must be absolute"));
    }
    let _source_lock = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Shared,
        clock,
    )?;
    let path = session.join("focus-area-expansion-status.json");
    let transaction = FileTransaction::open(&path, clock)?;
    let sources = SourceSnapshot::read(session, clock)?;
    let existing = transaction.read_bytes()?;
    let mut document = if matches!(operation, Operation::Initialize) {
        if existing.is_some() {
            return Err(fail("invalidInput", "Expansion manifest already exists"));
        }
        Expansion {
            schema_version: 1,
            state: ExpansionState::Unknown,
            references: Vec::new(),
        }
    } else {
        let bytes =
            existing.ok_or_else(|| fail("invalidInput", "Expansion manifest is missing"))?;
        let mut document = Expansion::parse(&bytes, &sources, false)?;
        document.apply(&operation, &sources)?;
        document
    };
    document.validate(&sources, false)?;
    let reference = reference_id.or(match &operation {
        Operation::Read(id) => id.as_deref(),
        Operation::Complete(result) | Operation::Fail(result) => Some(result.reference_id.as_str()),
        _ => None,
    });
    let summary = document.summary(reference)?;
    if !matches!(operation, Operation::Read(_)) {
        let value = serde_json::to_value(&mut document).map_err(|error| {
            caused_by("invalidInput", "Cannot serialize expansion manifest", error)
        })?;
        transaction.write_bytes(powershell_json(&value)?.as_bytes())?;
    }
    Ok(summary)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::assertions_on_result_states,
    reason = "Boolean result checks make invalid-record cases concise"
)]
#[allow(
    clippy::indexing_slicing,
    reason = "Tests index known JSON fixture and summary shapes"
)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{Expansion, Operation, PendingReferences, ReferenceInput, ReferenceResult};
    use crate::focus_area_source::SourceSnapshot;

    fn sources() -> SourceSnapshot {
        let files = [
            (("instruction-set-source", "source.md"), "Direct reference."),
            (("instruction-set-raw", "parent.md"), "Nested reference."),
            (("instruction-set-raw", "child.md"), "Child guidance."),
        ];
        SourceSnapshot::from_files(
            files
                .into_iter()
                .map(|((root, path), text)| {
                    ((root.to_owned(), path.to_owned()), vec![text.to_owned()])
                })
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn input(id: &str, parent: Option<&str>, path: &str, scope: &str) -> ReferenceInput {
        serde_json::from_value(json!({"id": id, "uri": format!("https://example.invalid/{id}"),
            "referrers": [{"parentId": parent, "source": {
                "root": if parent.is_some() { "instruction-set-raw" } else { "instruction-set-source" },
                "path": path, "startLine": 1, "endLine": 1
            }, "scope": scope, "declarations": []}]})).unwrap()
    }

    fn complete(id: &str, path: &str) -> Operation {
        Operation::Complete(ReferenceResult {
            reference_id: id.to_owned(),
            outputs: vec![path.to_owned()],
            error: None,
        })
    }

    #[test]
    fn late_referrers_extend_child_chains_and_finality() {
        let sources = sources();
        let mut expansion = Expansion::parse(
            br#"{"schemaVersion":1,"state":"Unknown","references":[]}"#,
            &sources,
            false,
        )
        .unwrap();
        expansion
            .apply(
                &Operation::Register(input("parent", None, "source.md", "First")),
                &sources,
            )
            .unwrap();
        let error = expansion.apply(&Operation::Finalize, &sources).unwrap_err();
        assert!(error.find_source::<PendingReferences>().is_some());
        assert_eq!(ss_ai_runtime::classification(&error), "invalidInput");
        expansion
            .apply(&complete("parent", "parent.md"), &sources)
            .unwrap();
        expansion
            .apply(
                &Operation::Register(input("child", Some("parent"), "parent.md", "Nested")),
                &sources,
            )
            .unwrap();
        expansion
            .apply(&complete("child", "child.md"), &sources)
            .unwrap();
        expansion
            .apply(
                &Operation::Register(input("parent", None, "source.md", "Second")),
                &sources,
            )
            .unwrap();
        let summary = expansion.summary(Some("child")).unwrap();
        assert_eq!(summary["contexts"].as_array().unwrap().len(), 2);
        assert_eq!(summary["contexts"][1]["chain"][0]["scope"], "Second");
        expansion.apply(&Operation::Finalize, &sources).unwrap();
        assert!(expansion.is_complete());
        assert!(
            expansion
                .apply(
                    &Operation::Register(input("parent", None, "source.md", "Third")),
                    &sources
                )
                .is_err()
        );
    }

    #[test]
    fn rejects_broken_schema_cycles_and_wrong_provenance() {
        let sources = sources();
        let missing = br#"{"schemaVersion":1,"state":"Unknown","references":[{"id":"a","uri":"x","referrers":[{"source":{"root":"instruction-set-source","path":"source.md","startLine":1,"endLine":1},"scope":"x","declarations":[]}],"state":"Pending","outputs":[],"error":null}]}"#;
        assert!(Expansion::parse(missing, &sources, false).is_err());
        let mut expansion = Expansion::parse(
            br#"{"schemaVersion":1,"state":"Unknown","references":[]}"#,
            &sources,
            false,
        )
        .unwrap();
        expansion
            .apply(
                &Operation::Register(input("parent", None, "source.md", "All")),
                &sources,
            )
            .unwrap();
        expansion
            .apply(&complete("parent", "parent.md"), &sources)
            .unwrap();
        assert!(
            expansion
                .apply(
                    &Operation::Register(input("parent", Some("parent"), "parent.md", "All")),
                    &sources
                )
                .is_err()
        );
    }

    #[test]
    fn failure_is_retained_and_inconsistent_final_state_is_rejected() {
        let sources = sources();
        let mut expansion = Expansion::parse(
            br#"{"schemaVersion":1,"state":"Unknown","references":[]}"#,
            &sources,
            false,
        )
        .unwrap();
        expansion
            .apply(
                &Operation::Register(input("parent", None, "source.md", "All")),
                &sources,
            )
            .unwrap();
        expansion
            .apply(
                &Operation::Fail(ReferenceResult {
                    reference_id: "parent".into(),
                    outputs: vec![],
                    error: Some("Unavailable".into()),
                }),
                &sources,
            )
            .unwrap();
        expansion.apply(&Operation::Finalize, &sources).unwrap();
        let summary = expansion.summary(None).unwrap();
        assert_eq!(summary["state"], "Degraded");
        assert_eq!(summary["failedReferences"][0]["error"], "Unavailable");
        let mut invalid = serde_json::to_value(expansion).unwrap();
        invalid["state"] = json!("Complete");
        assert!(Expansion::parse(&serde_json::to_vec(&invalid).unwrap(), &sources, true).is_err());
    }
}
