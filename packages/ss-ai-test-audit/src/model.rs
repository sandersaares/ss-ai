//! Versioned records exchanged by the native test-audit workflow.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Identifies persisted state with explicit contract-authority policy.
pub(crate) const AUDIT_STATE_SCHEMA_VERSION: u32 = 6;
/// Identifies the semantic judgment rubric used by this utility build.
pub(crate) const RUBRIC_VERSION: &str = "2";

/// Identifies the immutable inventory snapshot and its repository provenance.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AuditState {
    pub(crate) schema_version: u32,
    pub(crate) audit_id: String,
    pub(crate) snapshot_sha256: String,
    pub(crate) index_sha256: String,
    pub(crate) repository_root: String,
    pub(crate) repository_revision: String,
    pub(crate) revision_kind: RevisionKind,
    pub(crate) scope_paths: Vec<String>,
    pub(crate) tool_version: String,
    pub(crate) rubric_version: String,
    pub(crate) contract_profile_sha256: String,
    pub(crate) content_sha256: String,
    pub(crate) scanned_at: String,
    pub(crate) test_count: usize,
    pub(crate) assertion_count: usize,
}

/// Explains whether the stored source revision is a Git object or local content.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RevisionKind {
    Git,
    Unversioned,
}

/// Contains deterministic source inventory and all observed coverage diagnostics.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Inventory {
    pub(crate) schema_version: u32,
    pub(crate) audit_id: String,
    pub(crate) repository_root: String,
    pub(crate) repository_revision: String,
    pub(crate) revision_kind: RevisionKind,
    pub(crate) scope_paths: Vec<String>,
    pub(crate) tool_version: String,
    pub(crate) rubric_version: String,
    pub(crate) content_sha256: String,
    pub(crate) contract_profile: Option<ContractProfile>,
    pub(crate) files: Vec<IndexedFile>,
    pub(crate) tests: Vec<TestCase>,
    pub(crate) assertions: Vec<AssertionSite>,
    pub(crate) diagnostics: Vec<ScanDiagnostic>,
    pub(crate) corrections: Vec<InventoryCorrection>,
}

/// Binds one readable file to its snapshot identity and scan outcome.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct IndexedFile {
    pub(crate) path: String,
    pub(crate) language: String,
    pub(crate) kind: String,
    pub(crate) sha256: String,
    pub(crate) line_count: usize,
    pub(crate) byte_count: usize,
}

/// Represents one executable test, doctest, or compile-time assertion site.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TestCase {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) language: String,
    pub(crate) path: String,
    pub(crate) line: NonZeroUsize,
    pub(crate) end_line: usize,
    pub(crate) direct_assertion_ids: Vec<String>,
    pub(crate) shared_assertion_ids: Vec<String>,
}

/// Records one source predicate rather than a runtime iteration of that predicate.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AssertionSite {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) language: String,
    pub(crate) path: String,
    pub(crate) line: NonZeroUsize,
    pub(crate) end_line: usize,
    pub(crate) column: NonZeroUsize,
    pub(crate) expression: String,
    pub(crate) file_sha256: String,
    pub(crate) owner_line: NonZeroUsize,
    pub(crate) owner_end_line: usize,
    /// Transient ownership marker used before serializing the immutable inventory.
    #[serde(skip)]
    pub(crate) test_code: bool,
    /// Retains stable source-site IDs while omitting a proven fixture check.
    #[serde(skip)]
    pub(crate) fixture_excluded: bool,
    #[serde(skip)]
    pub(crate) fixture_exclusion_reason: Option<String>,
    pub(crate) containing_test_id: Option<String>,
    pub(crate) helper_name: Option<String>,
    pub(crate) calling_test_ids: Vec<String>,
    pub(crate) caller_evidence: Vec<CallerEvidence>,
    pub(crate) unresolved_caller_candidates: Vec<CallerEvidence>,
}

/// Binds one native caller edge to the exact syntax that establishes it.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CallerEvidence {
    pub(crate) kind: String,
    pub(crate) test_id: String,
    pub(crate) path: String,
    pub(crate) line: NonZeroUsize,
    pub(crate) end_line: usize,
    pub(crate) column: NonZeroUsize,
    pub(crate) expression: String,
}

/// Preserves unsupported syntax and completeness observations for the reviewer.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ScanDiagnostic {
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) line: Option<usize>,
    pub(crate) detail: String,
}

/// Supplies one exact source-site exclusion when AST detection is ambiguous.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InventoryCorrectionRequest {
    pub(crate) schema_version: u32,
    pub(crate) repository_revision: String,
    pub(crate) corrections: Vec<InventoryCorrectionDraft>,
}

/// Names the immutable source predicate removed from one initialized audit.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InventoryCorrectionDraft {
    pub(crate) path: String,
    pub(crate) line: NonZeroUsize,
    pub(crate) end_line: usize,
    pub(crate) expression: String,
    /// Explains whether the excluded source site arranges, injects, or cleans up.
    pub(crate) candidate_role: CandidateRole,
    /// Requires the reviewer to state that the source candidate leaves the denominator.
    pub(crate) candidate_disposition: CandidateDisposition,
    pub(crate) reason: String,
}

/// Retains exact provenance for one agent-supplied inventory correction.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InventoryCorrection {
    /// Preserves the scanner's source-local ID even when the site is omitted.
    pub(crate) assertion_id: String,
    pub(crate) path: String,
    pub(crate) line: NonZeroUsize,
    pub(crate) end_line: usize,
    pub(crate) column: NonZeroUsize,
    pub(crate) expression: String,
    /// Binds the correction to the exact audited source bytes.
    pub(crate) file_sha256: String,
    /// Identifies why the candidate is a denominator correction.
    pub(crate) candidate_role: CandidateRole,
    /// Preserves the eligibility result independently from the semantic label.
    pub(crate) candidate_disposition: CandidateDisposition,
    pub(crate) reason: String,
}

/// Distinguishes a counted observation from candidate stimulus and setup sites.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CandidateRole {
    Observation,
    FixtureSetup,
    FaultInjection,
    Cleanup,
    Unresolved,
}

/// Records whether a candidate entered or left the active assertion denominator.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CandidateDisposition {
    Included,
    Excluded,
}

/// Establishes one interface's audience and admissible promise sources.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ContractProfile {
    /// Identifies the profile record grammar independently from the audit schema.
    pub(crate) schema_version: u32,
    /// Names the policy so comparison receipts remain interpretable.
    pub(crate) name: String,
    /// Identifies the product-facing interface governed by the profile.
    pub(crate) interface: String,
    /// Identifies the users to whom the interface promises behavior.
    pub(crate) audience: String,
    /// Lists supported public invocation or construction surfaces.
    pub(crate) entry_points: Vec<String>,
    /// Lists explicitly internal surfaces that must not create public promises.
    pub(crate) internal_surfaces: Vec<String>,
    /// Resolves indexed source provenance to admitted or contextual authority.
    pub(crate) authorities: Vec<ContractAuthority>,
    /// Retains unresolved source-policy questions for reviewer follow-up.
    pub(crate) unresolved: Vec<String>,
}

/// Names one admitted or rejected evidence source for a contract profile.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ContractAuthority {
    /// Binds citations to one stable authority rule.
    pub(crate) id: String,
    /// Matches repository-relative evidence paths; longer prefixes take priority.
    pub(crate) path_prefix: String,
    /// Restricts the rule to structural source roles, or all roles when empty.
    pub(crate) source_roles: Vec<String>,
    /// Distinguishes promise support from non-authoritative context.
    pub(crate) admissible: bool,
    /// Explains the rule's role in the selected public-interface policy.
    pub(crate) authority: String,
    /// Defines the behavioral scope in which the source is applicable.
    pub(crate) scope: String,
}

/// Carries one searchable line with immutable source provenance.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct IndexEntry {
    pub(crate) path: String,
    pub(crate) line: NonZeroUsize,
    pub(crate) end_line: NonZeroUsize,
    pub(crate) language: String,
    pub(crate) kind: String,
    pub(crate) owner: String,
    pub(crate) source_role: String,
    pub(crate) text: String,
    pub(crate) excerpt: String,
    pub(crate) preview_truncated: bool,
    pub(crate) repository_revision: String,
    pub(crate) file_sha256: String,
}

/// Holds one bounded group of assertions exported for agent judgment.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Batch {
    pub(crate) schema_version: u32,
    pub(crate) audit_id: String,
    pub(crate) snapshot_sha256: String,
    pub(crate) content_sha256: String,
    pub(crate) index_sha256: String,
    pub(crate) tool_version: String,
    pub(crate) rubric_version: String,
    pub(crate) contract_profile_sha256: String,
    /// Exposes the complete authority policy used to judge the exported sites.
    pub(crate) contract_profile: Option<ContractProfile>,
    pub(crate) assertions: Vec<BatchAssertion>,
    pub(crate) completed: usize,
    pub(crate) remaining: usize,
}

/// Adds local test and source context to an assertion selected for judgment.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BatchAssertion {
    #[serde(flatten)]
    pub(crate) assertion: AssertionSite,
    pub(crate) test_name: Option<String>,
    pub(crate) test_path: Option<String>,
    pub(crate) test_line: Option<NonZeroUsize>,
    pub(crate) context: Vec<BatchContextEntry>,
    pub(crate) diagnostics: Vec<ScanDiagnostic>,
    /// Prompts explicit eligibility review for ambiguous value and branch candidates.
    pub(crate) candidate_role: Option<CandidateRole>,
    /// Describes the batch hint without committing the semantic denominator decision.
    pub(crate) candidate_disposition: Option<String>,
    /// Names the deterministic evidence that leaves the candidate ambiguous.
    pub(crate) candidate_witness: Option<String>,
}

/// Bounds exported context to presentation metadata while retaining its handle-capable identity.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BatchContextEntry {
    pub(crate) path: String,
    pub(crate) line: NonZeroUsize,
    pub(crate) end_line: NonZeroUsize,
    pub(crate) language: String,
    pub(crate) kind: String,
    pub(crate) owner: String,
    pub(crate) source_role: String,
    pub(crate) excerpt: String,
    pub(crate) preview_truncated: bool,
    pub(crate) repository_revision: String,
    pub(crate) file_sha256: String,
}

/// Validates the caller's batch envelope before recording semantic judgments.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClassificationBatch {
    pub(crate) schema_version: u32,
    pub(crate) audit_id: String,
    pub(crate) snapshot_sha256: String,
    pub(crate) assertions: Vec<Classification>,
}

/// Distinguishes the four evidence relationships used by the audit report.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ClassificationKind {
    Documented,
    Implicit,
    Mixed,
    Incidental,
}

/// Distinguishes confirmed evidence from an intentionally ambiguous inference.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, PartialEq)]
pub(crate) enum EvidenceStatus {
    Confirmed,
    Ambiguous,
}

/// Records one agent-owned semantic judgment and its exact evidence.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Classification {
    pub(crate) assertion_id: String,
    #[serde(rename = "classification")]
    pub(crate) kind: ClassificationKind,
    pub(crate) evidence_status: EvidenceStatus,
    pub(crate) actual_outcome: String,
    pub(crate) rationale: String,
    pub(crate) citations: Vec<Citation>,
    #[serde(default)]
    pub(crate) incidental_reason: Option<String>,
    pub(crate) calling_test_ids: Vec<String>,
    #[serde(default)]
    pub(crate) unlinked_reason: Option<String>,
    #[serde(default)]
    pub(crate) unresolved_caller_reason: Option<String>,
    #[serde(default)]
    pub(crate) review_reason: Option<String>,
    #[serde(default)]
    /// Quotes the admissible clause that requires the observed outcome.
    pub(crate) decisive_clause: Option<String>,
    #[serde(default)]
    /// Identifies the weakest fact the literal predicate can establish.
    pub(crate) observation_strength: Option<String>,
    #[serde(default)]
    /// Names a contract-relevant alternative that still makes the predicate pass.
    pub(crate) passing_counterexample: Option<String>,
    #[serde(default)]
    /// Records why an uncertain source candidate belongs in the denominator.
    pub(crate) candidate_role: Option<CandidateRole>,
    #[serde(default)]
    /// Records the eligibility decision separately from the semantic category.
    pub(crate) candidate_disposition: Option<CandidateDisposition>,
}

/// Anchors agent evidence to an exact indexed source line.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Citation {
    pub(crate) path: String,
    #[serde(deserialize_with = "citation_line")]
    pub(crate) line: NonZeroUsize,
    pub(crate) end_line: NonZeroUsize,
    pub(crate) owner: String,
    pub(crate) source_role: String,
    pub(crate) excerpt: String,
    #[serde(default)]
    /// Binds this range to the exact contract-profile authority decision.
    pub(crate) authority_rule_id: Option<String>,
}

/// Names the citation field when a full input supplies a zero line.
fn citation_line<'de, D: Deserializer<'de>>(input: D) -> Result<NonZeroUsize, D::Error> {
    NonZeroUsize::new(usize::deserialize(input)?)
        .ok_or_else(|| D::Error::custom("Citation line must be positive"))
}

/// Compact agent-authored rows bound to the exact exported batch bytes.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CompactSubmission {
    pub(crate) schema_version: u32,
    pub(crate) batch_fingerprint: String,
    pub(crate) rows: Vec<CompactSubmissionRow>,
}

/// Either supplies one complete judgment or explicitly reuses an accepted one.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CompactSubmissionRow {
    pub(crate) assertion_id: String,
    #[serde(default)]
    pub(crate) classification: Option<ClassificationKind>,
    #[serde(default)]
    pub(crate) evidence_status: Option<EvidenceStatus>,
    #[serde(default)]
    pub(crate) actual_outcome: Option<String>,
    #[serde(default)]
    pub(crate) rationale: Option<String>,
    #[serde(default)]
    pub(crate) citation_handles: Option<Value>,
    #[serde(default)]
    pub(crate) incidental_reason: Option<String>,
    #[serde(default)]
    pub(crate) calling_test_ids: Option<Value>,
    #[serde(default)]
    pub(crate) unlinked_reason: Option<String>,
    #[serde(default)]
    pub(crate) unresolved_caller_reason: Option<String>,
    #[serde(default)]
    pub(crate) review_reason: Option<String>,
    #[serde(default)]
    /// Quotes the admissible clause that requires the observed outcome.
    pub(crate) decisive_clause: Option<String>,
    #[serde(default)]
    /// Identifies the weakest fact the literal predicate can establish.
    pub(crate) observation_strength: Option<String>,
    #[serde(default)]
    /// Names a contract-relevant alternative that still makes the predicate pass.
    pub(crate) passing_counterexample: Option<String>,
    #[serde(default)]
    /// Records why an uncertain source candidate belongs in the denominator.
    pub(crate) candidate_role: Option<CandidateRole>,
    #[serde(default)]
    /// Records the eligibility decision separately from the semantic category.
    pub(crate) candidate_disposition: Option<CandidateDisposition>,
    #[serde(default)]
    pub(crate) reuse_from: Option<String>,
    #[serde(default)]
    pub(crate) equivalence_confirmed: Option<bool>,
}

/// Persists accepted classifications separately from the immutable inventory.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClassificationJournal {
    pub(crate) schema_version: u32,
    pub(crate) audit_id: String,
    pub(crate) snapshot_sha256: String,
    pub(crate) content_sha256: String,
    pub(crate) index_sha256: String,
    pub(crate) tool_version: String,
    pub(crate) rubric_version: String,
    pub(crate) contract_profile_sha256: String,
    pub(crate) classifications: Vec<Classification>,
}

/// Joins inventory and semantic judgment for one final trace row.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TraceRow<'a> {
    pub(crate) assertion: &'a AssertionSite,
    pub(crate) classification: &'a Classification,
}

/// Summarizes one module without treating rates as a quality score.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModuleSummary {
    pub(crate) module: String,
    pub(crate) assertion_count: usize,
    pub(crate) no_explicitly_documented_promise_count: usize,
    pub(crate) no_public_contract_check_count: usize,
    pub(crate) no_explicitly_documented_promise_rate: f64,
    pub(crate) no_public_contract_check_rate: f64,
}

/// Standard machine-readable final summary for one completed audit.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditSummary {
    pub(crate) schema_version: u32,
    pub(crate) status: String,
    pub(crate) next_action: String,
    pub(crate) final_response_allowed: bool,
    pub(crate) audit_id: String,
    pub(crate) snapshot_sha256: String,
    pub(crate) content_sha256: String,
    pub(crate) index_sha256: String,
    pub(crate) tool_version: String,
    pub(crate) rubric_version: String,
    pub(crate) contract_profile_sha256: String,
    pub(crate) repository_root: String,
    pub(crate) repository_revision: String,
    pub(crate) scope_paths: Vec<String>,
    pub(crate) indexed_file_count: usize,
    pub(crate) source_languages: BTreeMap<String, usize>,
    pub(crate) test_count: usize,
    pub(crate) assertion_count: usize,
    pub(crate) module_count: usize,
    pub(crate) classification_counts: BTreeMap<String, usize>,
    pub(crate) no_counted_assertion_count: usize,
    pub(crate) incidental_only_count: usize,
    pub(crate) unlinked_assertion_count: usize,
    pub(crate) fallible_value_check_count: usize,
    pub(crate) conditional_assertion_count: usize,
    pub(crate) diagnostic_count: usize,
    pub(crate) correction_count: usize,
    pub(crate) consistency_diagnostic_count: usize,
    pub(crate) modules: Vec<ModuleSummary>,
}

/// Explains a review warning without changing accepted classifications.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConsistencyDiagnostic {
    pub(crate) priority: String,
    pub(crate) kind: String,
    pub(crate) assertion_ids: Vec<String>,
    pub(crate) detail: String,
}
