//! Versioned records exchanged by the native test-audit workflow.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Identifies the immutable inventory snapshot and its repository provenance.
#[derive(Debug, Clone, Deserialize, Serialize)]
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
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Inventory {
    pub(crate) schema_version: u32,
    pub(crate) audit_id: String,
    pub(crate) repository_root: String,
    pub(crate) repository_revision: String,
    pub(crate) revision_kind: RevisionKind,
    pub(crate) scope_paths: Vec<String>,
    pub(crate) files: Vec<IndexedFile>,
    pub(crate) tests: Vec<TestCase>,
    pub(crate) assertions: Vec<AssertionSite>,
    pub(crate) diagnostics: Vec<ScanDiagnostic>,
}

/// Binds one readable file to its snapshot identity and scan outcome.
#[derive(Debug, Clone, Deserialize, Serialize)]
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
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TestCase {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) language: String,
    pub(crate) path: String,
    pub(crate) line: usize,
    pub(crate) end_line: usize,
    pub(crate) direct_assertion_ids: Vec<String>,
    pub(crate) shared_assertion_ids: Vec<String>,
}

/// Records one source predicate rather than a runtime iteration of that predicate.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AssertionSite {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) language: String,
    pub(crate) path: String,
    pub(crate) line: usize,
    pub(crate) end_line: usize,
    pub(crate) column: usize,
    pub(crate) expression: String,
    pub(crate) file_sha256: String,
    /// Transient ownership marker used before serializing the immutable inventory.
    #[serde(skip)]
    pub(crate) test_code: bool,
    pub(crate) containing_test_id: Option<String>,
    pub(crate) helper_name: Option<String>,
    pub(crate) calling_test_ids: Vec<String>,
}

/// Preserves unsupported syntax and completeness observations for the reviewer.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ScanDiagnostic {
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) line: Option<usize>,
    pub(crate) detail: String,
}

/// Carries one searchable line with immutable source provenance.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct IndexEntry {
    pub(crate) path: String,
    pub(crate) line: usize,
    pub(crate) language: String,
    pub(crate) kind: String,
    pub(crate) excerpt: String,
    pub(crate) repository_revision: String,
    pub(crate) file_sha256: String,
}

/// Holds one bounded group of assertions exported for agent judgment.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Batch {
    pub(crate) schema_version: u32,
    pub(crate) audit_id: String,
    pub(crate) snapshot_sha256: String,
    pub(crate) assertions: Vec<BatchAssertion>,
    pub(crate) completed: usize,
    pub(crate) remaining: usize,
}

/// Adds local test and source context to an assertion selected for judgment.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BatchAssertion {
    #[serde(flatten)]
    pub(crate) assertion: AssertionSite,
    pub(crate) test_name: Option<String>,
    pub(crate) test_path: Option<String>,
    pub(crate) test_line: Option<usize>,
    pub(crate) context: Vec<IndexEntry>,
}

/// Validates the caller's batch envelope before recording semantic judgments.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClassificationBatch {
    pub(crate) schema_version: u32,
    pub(crate) audit_id: String,
    pub(crate) snapshot_sha256: String,
    pub(crate) assertions: Vec<Classification>,
}

/// Distinguishes the four evidence relationships used by the audit report.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, PartialEq)]
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
#[derive(Debug, Clone, Deserialize, Serialize)]
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
}

/// Anchors agent evidence to an exact indexed source line.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Citation {
    pub(crate) path: String,
    pub(crate) line: usize,
    pub(crate) excerpt: String,
}

/// Compact agent-authored rows resolved against the currently exported batch.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CompactSubmission {
    pub(crate) schema_version: u32,
    pub(crate) rows: Vec<CompactSubmissionRow>,
}

/// Either supplies one complete judgment or explicitly reuses an accepted one.
#[derive(Debug, Clone, Deserialize)]
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
    pub(crate) citation_handles: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) incidental_reason: Option<String>,
    #[serde(default)]
    pub(crate) calling_test_ids: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) unlinked_reason: Option<String>,
    #[serde(default)]
    pub(crate) reuse_from: Option<String>,
    #[serde(default)]
    pub(crate) equivalence_confirmed: Option<bool>,
}

/// Persists accepted classifications separately from the immutable inventory.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClassificationJournal {
    pub(crate) schema_version: u32,
    pub(crate) audit_id: String,
    pub(crate) snapshot_sha256: String,
    pub(crate) classifications: Vec<Classification>,
}

/// Joins inventory and semantic judgment for one final trace row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TraceRow {
    pub(crate) assertion: AssertionSite,
    pub(crate) classification: Classification,
}

/// Summarizes one module without treating rates as a quality score.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditSummary {
    pub(crate) schema_version: u32,
    pub(crate) status: String,
    pub(crate) next_action: String,
    pub(crate) final_response_allowed: bool,
    pub(crate) audit_id: String,
    pub(crate) snapshot_sha256: String,
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
    pub(crate) modules: Vec<ModuleSummary>,
}
