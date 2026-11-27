//! Reproducible comparison of completed test-audit judgments.

use std::collections::{BTreeMap, BTreeSet};

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, fail};

use crate::model::{Classification, ClassificationKind, TestCase};
use crate::state::{LoadedState, load};

/// Compares exact semantic classifications from two complete audit snapshots.
pub fn compare(
    left_directory: &std::path::Path,
    right_directory: &std::path::Path,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let left = load(left_directory, clock)?;
    let right = load(right_directory, clock)?;
    require_complete(&left, "left")?;
    require_complete(&right, "right")?;

    let left_assertions = assertion_map(&left);
    let right_assertions = assertion_map(&right);
    let mut transition_counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut matched = 0_usize;
    let mut exact_label_agreement = 0_usize;
    let mut changed_rationale = 0_usize;
    let mut changed_evidence_status = 0_usize;
    let mut changed_review_reason = 0_usize;
    let mut caller_set_differences = 0_usize;
    let left_only_sites = left_assertions
        .keys()
        .filter(|key| !right_assertions.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    let right_only_sites = right_assertions
        .keys()
        .filter(|key| !left_assertions.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    for (key, (left_assertion, left_classification)) in &left_assertions {
        let Some((right_assertion, right_classification)) = right_assertions.get(key) else {
            continue;
        };
        matched = matched.saturating_add(1);
        let left_kind = classification_name(left_classification.kind);
        let right_kind = classification_name(right_classification.kind);
        let count = transition_counts
            .entry((left_kind.to_owned(), right_kind.to_owned()))
            .or_default();
        *count = count.saturating_add(1);
        if left_classification.kind == right_classification.kind {
            exact_label_agreement = exact_label_agreement.saturating_add(1);
        }
        if left_classification.rationale != right_classification.rationale {
            changed_rationale = changed_rationale.saturating_add(1);
        }
        if left_classification.evidence_status != right_classification.evidence_status {
            changed_evidence_status = changed_evidence_status.saturating_add(1);
        }
        if left_classification.review_reason != right_classification.review_reason {
            changed_review_reason = changed_review_reason.saturating_add(1);
        }
        if effective_callers(left_assertion, left_classification, &left)
            != effective_callers(right_assertion, right_classification, &right)
        {
            caller_set_differences = caller_set_differences.saturating_add(1);
        }
    }

    let transitions = transition_counts
        .into_iter()
        .map(|((left, right), count)| {
            json!({
                "left": left,
                "right": right,
                "count": count
            })
        })
        .collect::<Vec<_>>();
    let left_gaps = gap_keys(&left);
    let right_gaps = gap_keys(&right);
    let gap_comparison = |left: &BTreeSet<String>, right: &BTreeSet<String>| {
        json!({
            "leftOnly": left.difference(right).cloned().collect::<Vec<_>>(),
            "rightOnly": right.difference(left).cloned().collect::<Vec<_>>(),
            "both": left.intersection(right).cloned().collect::<Vec<_>>()
        })
    };
    Ok(json!({
        "schemaVersion": 1,
        "comparableContent": left.state.content_sha256 == right.state.content_sha256,
        "left": run_identity(&left),
        "right": run_identity(&right),
        "matchedAssertionCount": matched,
        "exactLabelAgreementCount": exact_label_agreement,
        "exactLabelAgreementRate": ratio(exact_label_agreement, matched),
        "changedRationaleCount": changed_rationale,
        "changedEvidenceStatusCount": changed_evidence_status,
        "changedReviewReasonCount": changed_review_reason,
        "callerSetDifferenceCount": caller_set_differences,
        "leftOnlySites": left_only_sites,
        "rightOnlySites": right_only_sites,
        "transitionMatrix": transitions,
        "noCountedAssertionTests": gap_comparison(&left_gaps.0, &right_gaps.0),
        "incidentalOnlyTests": gap_comparison(&left_gaps.1, &right_gaps.1)
    })
    .to_string())
}

/// Refuses a partial comparison that could be mistaken for an audit result.
fn require_complete(loaded: &LoadedState, side: &str) -> Result<(), AppError> {
    if loaded.journal.rows().len() != loaded.inventory.assertions.len() {
        return Err(fail(
            "incompleteState",
            format!(
                "The {side} audit is incomplete: {} of {} assertions classified",
                loaded.journal.rows().len(),
                loaded.inventory.assertions.len()
            ),
        ));
    }
    Ok(())
}

/// Maps source identities to exact assertion and semantic records.
fn assertion_map(
    loaded: &LoadedState,
) -> BTreeMap<String, (&crate::model::AssertionSite, &Classification)> {
    let classifications = loaded
        .journal
        .rows()
        .iter()
        .map(|classification| (classification.assertion_id.as_str(), classification))
        .collect::<BTreeMap<_, _>>();
    loaded
        .inventory
        .assertions
        .iter()
        .filter_map(|assertion| {
            let classification = classifications.get(assertion.id.as_str())?;
            Some((assertion_key(assertion), (assertion, *classification)))
        })
        .collect()
}

/// Uses source provenance rather than audit-local identifiers for comparison.
fn assertion_key(assertion: &crate::model::AssertionSite) -> String {
    format!(
        "{}:{}-{}:{}:{}:{}",
        assertion.path,
        assertion.line,
        assertion.end_line,
        assertion.column,
        assertion.file_sha256,
        assertion.expression
    )
}

/// Resolves audit-local test IDs to checkout-independent test identities.
fn effective_callers(
    assertion: &crate::model::AssertionSite,
    classification: &Classification,
    loaded: &LoadedState,
) -> BTreeSet<String> {
    assertion
        .calling_test_ids
        .iter()
        .chain(&classification.calling_test_ids)
        .filter_map(|id| {
            loaded
                .inventory
                .tests
                .iter()
                .find(|test| test.id == *id)
                .map(test_key)
        })
        .collect()
}

/// Finds test-level gap membership independently of audit-local IDs.
fn gap_keys(loaded: &LoadedState) -> (BTreeSet<String>, BTreeSet<String>) {
    let classifications = loaded
        .journal
        .rows()
        .iter()
        .map(|classification| (classification.assertion_id.as_str(), classification))
        .collect::<BTreeMap<_, _>>();
    let mut agent_shared: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for classification in loaded.journal.rows() {
        for test_id in &classification.calling_test_ids {
            agent_shared
                .entry(test_id.as_str())
                .or_default()
                .insert(classification.assertion_id.as_str());
        }
    }
    let mut no_counted = BTreeSet::new();
    let mut incidental_only = BTreeSet::new();
    for test in &loaded.inventory.tests {
        let ids = test
            .direct_assertion_ids
            .iter()
            .chain(test.shared_assertion_ids.iter())
            .map(String::as_str)
            .chain(
                agent_shared
                    .get(test.id.as_str())
                    .into_iter()
                    .flat_map(|ids| ids.iter().copied()),
            )
            .collect::<BTreeSet<_>>();
        if ids.is_empty() {
            no_counted.insert(test_key(test));
        } else if ids.iter().all(|id| {
            classifications
                .get(id)
                .is_some_and(|classification| classification.kind == ClassificationKind::Incidental)
        }) {
            incidental_only.insert(test_key(test));
        }
    }
    (no_counted, incidental_only)
}

/// Uses source coordinates and names rather than generated test identifiers.
fn test_key(test: &TestCase) -> String {
    format!("{}:{}:{}", test.path, test.line, test.name)
}

/// Supplies stable run metadata for reproducibility reports.
fn run_identity(loaded: &LoadedState) -> Value {
    json!({
        "auditId": loaded.state.audit_id,
        "snapshotSha256": loaded.state.snapshot_sha256,
        "contentSha256": loaded.state.content_sha256,
        "indexSha256": loaded.state.index_sha256,
        "toolVersion": loaded.state.tool_version,
        "rubricVersion": loaded.state.rubric_version,
        "repositoryRevision": loaded.state.repository_revision,
        "testCount": loaded.inventory.tests.len(),
        "assertionCount": loaded.inventory.assertions.len()
    })
}

/// Renders one canonical classification name for the transition matrix.
fn classification_name(kind: ClassificationKind) -> &'static str {
    match kind {
        ClassificationKind::Documented => "Documented",
        ClassificationKind::Implicit => "Implicit",
        ClassificationKind::Mixed => "Mixed",
        ClassificationKind::Incidental => "Incidental",
    }
}

/// Avoids NaN in an empty matched set.
fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        f64::from(u32::try_from(numerator).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(denominator).unwrap_or(u32::MAX))
    }
}
