//! Reproducible comparison of completed test-audit judgments.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_runtime::{Clock, fail};

use crate::model::{
    AssertionSite, CandidateDisposition, Classification, ClassificationKind, InventoryCorrection,
    TestCase,
};
use crate::state::{LoadedState, load};

/// Compares completed audits and groups drift by policy and membership.
pub fn compare(
    left_directory: &Path,
    right_directory: &Path,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let left = load(left_directory, clock)?;
    let right = load(right_directory, clock)?;
    require_complete(&left, "left")?;
    require_complete(&right, "right")?;

    let left_assertions = assertion_map(&left);
    let right_assertions = assertion_map(&right);
    let left_test_keys = test_key_map(&left);
    let right_test_keys = test_key_map(&right);
    let mut transition_counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut policy_groups: BTreeMap<(String, String, String, String), BTreeSet<String>> =
        BTreeMap::new();
    let mut matched = 0_usize;
    let mut exact_label_agreement = 0_usize;
    let mut changed_rationale = 0_usize;
    let mut changed_evidence_status = 0_usize;
    let mut changed_review_reason = 0_usize;
    let mut decisive_clause_differences = BTreeSet::new();
    let mut observation_strength_differences = BTreeSet::new();
    let mut passing_counterexample_differences = BTreeSet::new();
    let mut candidate_role_differences = BTreeSet::new();
    let mut caller_set_differences = BTreeSet::new();
    let mut agreement_sample = BTreeSet::new();
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
        let left_authority = authority_signature(left_classification);
        let right_authority = authority_signature(right_classification);
        if left_classification.kind == right_classification.kind {
            exact_label_agreement = exact_label_agreement.saturating_add(1);
            if agreement_sample.len() < 5 {
                agreement_sample.insert(key.clone());
            }
        }
        if left_classification.kind != right_classification.kind
            || left_authority != right_authority
        {
            policy_groups
                .entry((
                    left_kind.to_owned(),
                    right_kind.to_owned(),
                    left_authority,
                    right_authority,
                ))
                .or_default()
                .insert(key.clone());
        }
        changed_rationale = changed_rationale.saturating_add(usize::from(
            left_classification.rationale != right_classification.rationale,
        ));
        changed_evidence_status = changed_evidence_status.saturating_add(usize::from(
            left_classification.evidence_status != right_classification.evidence_status,
        ));
        changed_review_reason = changed_review_reason.saturating_add(usize::from(
            left_classification.review_reason != right_classification.review_reason,
        ));
        if left_classification.decisive_clause != right_classification.decisive_clause {
            decisive_clause_differences.insert(key.clone());
        }
        if left_classification.observation_strength != right_classification.observation_strength {
            observation_strength_differences.insert(key.clone());
        }
        if left_classification.passing_counterexample != right_classification.passing_counterexample
        {
            passing_counterexample_differences.insert(key.clone());
        }
        if left_classification.candidate_role != right_classification.candidate_role {
            candidate_role_differences.insert(key.clone());
        }
        if effective_callers(left_assertion, left_classification, &left_test_keys)
            != effective_callers(right_assertion, right_classification, &right_test_keys)
        {
            caller_set_differences.insert(key.clone());
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
    let authority_transitions = policy_groups
        .iter()
        .map(
            |((left, right, left_authority, right_authority), assertion_ids)| {
                json!({
                    "left": left,
                    "right": right,
                    "leftAuthorityRules": rule_list(left_authority),
                    "rightAuthorityRules": rule_list(right_authority),
                    "assertionKeys": assertion_ids,
                    "count": assertion_ids.len()
                })
            },
        )
        .collect::<Vec<_>>();
    let left_gaps = gap_keys(&left);
    let right_gaps = gap_keys(&right);
    let denominator_differences = denominator_differences(&left, &right);
    candidate_role_differences.extend(correction_role_differences(&left, &right));
    let same_contract_profile =
        left.state.contract_profile_sha256 == right.state.contract_profile_sha256;
    let mut review_queue = Vec::new();
    if !same_contract_profile {
        review_queue.push(review_item(
            "high",
            "contractProfileMismatch",
            "The runs used different contract-authority profiles; resolve that policy mismatch before interpreting row-level agreement",
            &[],
        ));
    }
    if !denominator_differences.is_empty() {
        let keys = denominator_differences
            .iter()
            .filter_map(|value| value["assertionKey"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        review_queue.push(review_item(
            "high",
            "denominatorDifference",
            "Active assertion and correction membership differs; compare denominator membership separately from semantic labels",
            &keys,
        ));
    }
    for ((left_kind, right_kind, left_rules, right_rules), assertion_keys) in &policy_groups {
        review_queue.push(review_item(
            "high",
            "authorityPolicyTransition",
            format!(
                "Transition {left_kind} to {right_kind} uses authority rules [{}] to [{}]",
                rule_list(left_rules).join(", "),
                rule_list(right_rules).join(", ")
            ),
            &assertion_keys.iter().cloned().collect::<Vec<_>>(),
        ));
    }
    add_group_review(
        &mut review_queue,
        "medium",
        "decisiveClauseDifference",
        "The runs use different decisive wording for the same active assertion",
        &decisive_clause_differences,
    );
    add_group_review(
        &mut review_queue,
        "medium",
        "observationStrengthDifference",
        "The runs claim different predicate strength for the same active assertion",
        &observation_strength_differences,
    );
    add_group_review(
        &mut review_queue,
        "medium",
        "passingCounterexampleDifference",
        "The runs describe different alternatives that still satisfy the predicate",
        &passing_counterexample_differences,
    );
    add_group_review(
        &mut review_queue,
        "medium",
        "candidateRoleDifference",
        "The runs disagree about whether a source candidate observes, arranges, injects, or cleans up",
        &candidate_role_differences,
    );
    add_group_review(
        &mut review_queue,
        "medium",
        "callerSetDifference",
        "Effective caller identity differs; reconcile native and agent-confirmed links before comparing labels",
        &caller_set_differences,
    );
    let changing_gaps = left_gaps
        .0
        .symmetric_difference(&right_gaps.0)
        .chain(left_gaps.1.symmetric_difference(&right_gaps.1))
        .cloned()
        .collect::<BTreeSet<_>>();
    add_group_review(
        &mut review_queue,
        "medium",
        "testGapDifference",
        "No-counted-assertion or incidental-only test membership differs between runs",
        &changing_gaps,
    );

    Ok(json!({
        "schemaVersion": 2,
        "comparableContent": left.state.index_sha256 == right.state.index_sha256
            && left.state.repository_revision == right.state.repository_revision
            && left.state.scope_paths == right.state.scope_paths,
        "sameContractProfile": same_contract_profile,
        "left": run_identity(&left),
        "right": run_identity(&right),
        "commonActiveAssertionCount": matched,
        "matchedAssertionCount": matched,
        "exactLabelAgreementCount": exact_label_agreement,
        "exactLabelAgreementRate": ratio(exact_label_agreement, matched),
        "changedRationaleCount": changed_rationale,
        "changedEvidenceStatusCount": changed_evidence_status,
        "changedReviewReasonCount": changed_review_reason,
        "decisiveClauseDifferenceCount": decisive_clause_differences.len(),
        "observationStrengthDifferenceCount": observation_strength_differences.len(),
        "passingCounterexampleDifferenceCount": passing_counterexample_differences.len(),
        "candidateRoleDifferenceCount": candidate_role_differences.len(),
        "callerSetDifferenceCount": caller_set_differences.len(),
        "leftOnlySites": left_only_sites,
        "rightOnlySites": right_only_sites,
        "denominatorDifferenceCount": denominator_differences.len(),
        "denominatorDifferences": denominator_differences,
        "transitionMatrix": transitions,
        "authorityRuleUsage": {
            "left": authority_rule_usage(&left),
            "right": authority_rule_usage(&right)
        },
        "authorityRuleTransitions": authority_transitions,
        "noCountedAssertionTests": gap_comparison(&left_gaps.0, &right_gaps.0),
        "incidentalOnlyTests": gap_comparison(&left_gaps.1, &right_gaps.1),
        "agreementSampleAssertionKeys": agreement_sample.into_iter().take(5).collect::<Vec<_>>(),
        "reviewQueue": review_queue
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
fn assertion_map(loaded: &LoadedState) -> BTreeMap<String, (&AssertionSite, &Classification)> {
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
fn assertion_key(assertion: &AssertionSite) -> String {
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

/// Uses the same source identity for excluded inventory corrections.
fn correction_key(correction: &InventoryCorrection) -> String {
    format!(
        "{}:{}-{}:{}:{}:{}",
        correction.path,
        correction.line,
        correction.end_line,
        correction.column,
        correction.file_sha256,
        correction.expression
    )
}

/// Resolves audit-local test IDs to checkout-independent test identities.
fn effective_callers(
    assertion: &AssertionSite,
    classification: &Classification,
    test_keys: &BTreeMap<&str, String>,
) -> BTreeSet<String> {
    effective_caller_ids(assertion, classification)
        .filter_map(|id| test_keys.get(id).cloned())
        .collect()
}

/// Indexes audit-local test IDs once for linear-edge caller normalization.
fn test_key_map(loaded: &LoadedState) -> BTreeMap<&str, String> {
    loaded
        .inventory
        .tests
        .iter()
        .map(|test| (test.id.as_str(), test_key(test)))
        .collect()
}

/// Combines direct ownership with native and agent-confirmed shared callers.
fn effective_caller_ids<'a>(
    assertion: &'a AssertionSite,
    classification: &'a Classification,
) -> impl Iterator<Item = &'a str> {
    assertion
        .containing_test_id
        .iter()
        .chain(assertion.calling_test_ids.iter())
        .chain(classification.calling_test_ids.iter())
        .map(String::as_str)
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

/// Supplies active/excluded membership for every source candidate in both runs.
fn denominator_differences(left: &LoadedState, right: &LoadedState) -> Vec<Value> {
    let memberships = |loaded: &LoadedState| {
        let mut memberships = BTreeMap::new();
        for assertion in &loaded.inventory.assertions {
            memberships.insert(
                assertion_key(assertion),
                json!({
                    "membership": "active",
                    "candidateRole": "observation",
                    "reason": null
                }),
            );
        }
        for correction in &loaded.inventory.corrections {
            memberships.insert(
                correction_key(correction),
                json!({
                    "membership": "excluded",
                    "candidateRole": correction.candidate_role,
                    "candidateDisposition": CandidateDisposition::Excluded,
                    "reason": correction.reason
                }),
            );
        }
        memberships
    };
    let left = memberships(left);
    let right = memberships(right);
    left.keys()
        .chain(right.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|key| {
            let left_membership = left.get(&key);
            let right_membership = right.get(&key);
            let same_membership = left_membership.and_then(|value| value["membership"].as_str())
                == right_membership.and_then(|value| value["membership"].as_str());
            if same_membership {
                return None;
            }
            Some(json!({
                "assertionKey": key,
                "left": left_membership.cloned().unwrap_or_else(|| json!({"membership":"missing"})),
                "right": right_membership.cloned().unwrap_or_else(|| json!({"membership":"missing"}))
            }))
        })
        .collect()
}

/// Finds correction-role drift even when both runs exclude the same source site.
fn correction_role_differences(left: &LoadedState, right: &LoadedState) -> BTreeSet<String> {
    let roles = |loaded: &LoadedState| {
        loaded
            .inventory
            .corrections
            .iter()
            .map(|correction| (correction_key(correction), correction.candidate_role))
            .collect::<BTreeMap<_, _>>()
    };
    let left = roles(left);
    let right = roles(right);
    left.keys()
        .chain(right.keys())
        .filter(|key| left.get(*key) != right.get(*key))
        .cloned()
        .collect()
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
        "contractProfileSha256": loaded.state.contract_profile_sha256,
        "contractProfileName": loaded.inventory.contract_profile.as_ref().map(|profile| profile.name.clone()),
        "contractProfileInterface": loaded.inventory.contract_profile.as_ref().map(|profile| profile.interface.clone()),
        "repositoryRevision": loaded.state.repository_revision,
        "testCount": loaded.inventory.tests.len(),
        "assertionCount": loaded.inventory.assertions.len(),
        "correctionCount": loaded.inventory.corrections.len(),
        "candidateCount": loaded.inventory.assertions.len()
            .saturating_add(loaded.inventory.corrections.len())
    })
}

/// Canonicalizes the contract rules cited by one judgment.
fn authority_signature(classification: &Classification) -> String {
    let mut rules = classification
        .citations
        .iter()
        .map(|citation| {
            citation
                .authority_rule_id
                .clone()
                .unwrap_or_else(|| "unresolved".to_owned())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if rules.is_empty() {
        rules.push("none".to_owned());
    }
    rules.join("|")
}

/// Splits the canonical rule signature for the machine review queue.
fn rule_list(signature: &str) -> Vec<&str> {
    signature.split('|').collect()
}

/// Counts exact authority-rule use without converting it into a correctness score.
fn authority_rule_usage(loaded: &LoadedState) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for classification in loaded.journal.rows() {
        if classification.citations.is_empty() {
            let count = counts.entry("none".to_owned()).or_default();
            *count = count.saturating_add(1);
            continue;
        }
        for citation in &classification.citations {
            let rule = citation
                .authority_rule_id
                .clone()
                .unwrap_or_else(|| "unresolved".to_owned());
            let count = counts.entry(rule).or_default();
            *count = count.saturating_add(1);
        }
    }
    counts
}

/// Adds one nonempty group to the prioritized review queue.
fn add_group_review(
    review_queue: &mut Vec<Value>,
    priority: &str,
    kind: &str,
    detail: &str,
    keys: &BTreeSet<String>,
) {
    if !keys.is_empty() {
        review_queue.push(review_item(
            priority,
            kind,
            detail,
            &keys.iter().cloned().collect::<Vec<_>>(),
        ));
    }
}

/// Builds one stable queue record without selecting a winning run.
fn review_item(priority: &str, kind: &str, detail: impl Into<String>, keys: &[String]) -> Value {
    json!({
        "priority": priority,
        "kind": kind,
        "detail": detail.into(),
        "assertionKeys": keys,
        "count": keys.len()
    })
}

/// Returns comparable test-gap membership without run-local test IDs.
fn gap_comparison(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Value {
    json!({
        "leftOnly": left.difference(right).cloned().collect::<Vec<_>>(),
        "rightOnly": right.difference(left).cloned().collect::<Vec<_>>(),
        "both": left.intersection(right).cloned().collect::<Vec<_>>()
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::{effective_caller_ids, effective_callers};
    use crate::model::{
        AssertionSite, CandidateDisposition, CandidateRole, Classification, ClassificationKind,
        EvidenceStatus,
    };
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn comparison_caller_ids_include_direct_ownership() {
        let assertion = AssertionSite {
            id: "A000001".to_owned(),
            kind: "assertion".to_owned(),
            language: "rust".to_owned(),
            path: "src/lib.rs".to_owned(),
            line: NonZeroUsize::MIN,
            end_line: 1,
            column: NonZeroUsize::MIN,
            expression: "assert!(ready())".to_owned(),
            file_sha256: "source".to_owned(),
            owner_line: NonZeroUsize::MIN,
            owner_end_line: 1,
            test_code: true,
            fixture_excluded: false,
            fixture_exclusion_reason: None,
            containing_test_id: Some("T000001".to_owned()),
            helper_name: None,
            calling_test_ids: Vec::new(),
            caller_evidence: Vec::new(),
            unresolved_caller_candidates: Vec::new(),
        };
        let classification = Classification {
            assertion_id: assertion.id.clone(),
            kind: ClassificationKind::Incidental,
            evidence_status: EvidenceStatus::Confirmed,
            actual_outcome: "ready".to_owned(),
            rationale: "The test owns this predicate".to_owned(),
            citations: Vec::new(),
            incidental_reason: Some("test-local".to_owned()),
            calling_test_ids: Vec::new(),
            unlinked_reason: None,
            unresolved_caller_reason: None,
            review_reason: None,
            decisive_clause: None,
            observation_strength: None,
            passing_counterexample: None,
            candidate_role: Some(CandidateRole::Observation),
            candidate_disposition: Some(CandidateDisposition::Included),
        };

        assert_eq!(
            effective_caller_ids(&assertion, &classification).collect::<Vec<_>>(),
            ["T000001"]
        );
        let keys = BTreeMap::from([("T000001", "src/lib.rs:1:owner".to_owned())]);
        assert_eq!(
            effective_callers(&assertion, &classification, &keys),
            BTreeSet::from(["src/lib.rs:1:owner".to_owned()])
        );
    }
}
