//! Bounded model checks for acceptance and replay without filesystem effects.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;

use serde_json::{from_value, json};

use super::{MAX_BATCH_SIZE, ValidatedJournal, accept_batch, build_batch, next_action};
use crate::model::{
    AssertionSite, AuditState, Classification, ClassificationBatch, ClassificationJournal,
    ClassificationKind, EvidenceStatus, Inventory, RevisionKind,
};
use crate::state::LoadedState;
use crate::submission::resolve_submission;

/// Small enough for a simple model, but large enough to expose partial updates.
const SITES: usize = 5;

/// Provides source ownership independently of a scanner or external data.
fn snapshot() -> LoadedState {
    let id = "audit";
    let digest = "snapshot";
    let state: AuditState = from_value(json!({
        "schemaVersion": 7, "auditId": id, "snapshotSha256": digest, "indexSha256": "index",
        "repositoryRoot": "repo", "repositoryRevision": "unversioned", "revisionKind": RevisionKind::Unversioned,
        "scopePaths": [], "toolVersion": "test", "rubricVersion": "test",
        "contentSha256": "content", "contractProfileSha256": "profile",
        "scannedAt": "1970-01-01T00:00:00Z", "testCount": 1, "assertionCount": SITES
    })).unwrap();
    let assertions = (1..=SITES)
        .map(|site| AssertionSite {
            id: format!("A{site:06}"),
            kind: "assertion".to_owned(),
            language: "rust".to_owned(),
            path: "lib.rs".to_owned(),
            line: NonZeroUsize::new(site).unwrap(),
            end_line: site,
            column: NonZeroUsize::MIN,
            expression: format!("assert!(check_{site}())"),
            file_sha256: "source".to_owned(),
            owner_line: NonZeroUsize::MIN,
            owner_end_line: SITES,
            test_code: true,
            fixture_excluded: false,
            fixture_exclusion_reason: None,
            containing_test_id: Some("T000001".to_owned()),
            helper_name: None,
            calling_test_ids: Vec::new(),
            caller_evidence: Vec::new(),
            unresolved_caller_candidates: Vec::new(),
        })
        .collect();
    let inventory = Inventory {
        schema_version: 7,
        audit_id: id.to_owned(),
        repository_root: state.repository_root.clone(),
        repository_revision: state.repository_revision.clone(),
        revision_kind: state.revision_kind,
        scope_paths: Vec::new(),
        tool_version: state.tool_version.clone(),
        rubric_version: state.rubric_version.clone(),
        content_sha256: state.content_sha256.clone(),
        contract_profile: None,
        files: Vec::new(),
        tests: vec![from_value(json!({
            "id": "T000001", "name": "checks", "kind": "unit", "language": "rust", "path": "lib.rs",
            "line": 1, "endLine": SITES, "directAssertionIds": [], "sharedAssertionIds": []
        })).unwrap()],
        assertions,
        diagnostics: Vec::new(),
        corrections: Vec::new(),
    };
    let journal = ValidatedJournal::from_persisted(
        ClassificationJournal {
            schema_version: 7,
            audit_id: id.to_owned(),
            snapshot_sha256: digest.to_owned(),
            content_sha256: state.content_sha256.clone(),
            index_sha256: state.index_sha256.clone(),
            tool_version: state.tool_version.clone(),
            rubric_version: state.rubric_version.clone(),
            contract_profile_sha256: state.contract_profile_sha256.clone(),
            classifications: Vec::new(),
        },
        &state,
        &inventory,
        &[],
    )
    .unwrap();
    LoadedState {
        state,
        inventory,
        index: Vec::new(),
        journal,
    }
}

/// Uses the outcome as the only varying semantic field for replay conflicts.
fn judgment(site: usize, outcome: &str) -> Classification {
    Classification {
        assertion_id: format!("A{site:06}"),
        kind: ClassificationKind::Implicit,
        evidence_status: EvidenceStatus::Ambiguous,
        actual_outcome: outcome.to_owned(),
        rationale: "The public name suggests this behavior".to_owned(),
        citations: Vec::new(),
        incidental_reason: None,
        calling_test_ids: Vec::new(),
        unlinked_reason: None,
        unresolved_caller_reason: None,
        review_reason: None,
        decisive_clause: None,
        observation_strength: None,
        passing_counterexample: None,
        candidate_role: None,
        candidate_disposition: None,
    }
}

/// Drives generated bounded scripts through the same transition as submit and advance.
#[test]
fn fuzz_classification_transitions_against_a_map() {
    // Vary operation order and batch size without requiring a runtime,
    // time budget, external corpus, or nondeterministic failure reproduction.
    for seed in 0_u64..128 {
        let mut loaded = snapshot();
        let mut model = BTreeMap::new();
        let valid_ids: BTreeSet<_> = loaded
            .inventory
            .assertions
            .iter()
            .map(|site| site.id.clone())
            .collect();
        let mut random = seed;
        // Each script has a bounded prefix of generated operations followed by
        // a complete submission and its exact replay.
        for step in 0..18 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let choice = usize::try_from(random >> 32).unwrap();
            let finish = step >= 16;
            let compact = !finish && choice % 4 == 0;
            let stale = !compact && !finish && choice % 7 == 0;
            let rows = if compact {
                let export = build_batch(&loaded, NonZeroUsize::MIN);
                if export.remaining == 0 {
                    continue;
                }
                let submission = from_value(json!({
                    "schemaVersion": 1, "batchFingerprint": "unused in canonical transition",
                    "rows": export.assertions.iter().map(|site| json!({
                        "assertionId": site.assertion.id, "classification": "Implicit", "evidenceStatus": "Ambiguous",
                        "actualOutcome": "expected value", "rationale": "The public name suggests this behavior",
                        "citationHandles": [], "callingTestIds": []
                    })).collect::<Vec<_>>(),
                })).unwrap();
                resolve_submission(&loaded, &export, submission)
                    .unwrap()
                    .assertions
            } else if finish {
                (1..=SITES)
                    .map(|site| {
                        let id = format!("A{site:06}");
                        judgment(
                            site,
                            model.get(&id).map_or("expected value", String::as_str),
                        )
                    })
                    .collect()
            } else {
                let count = if choice % 3 == 0 {
                    1 + choice % MAX_BATCH_SIZE
                } else {
                    1 + choice % SITES
                };
                (0..count)
                    .map(|offset| {
                        let site = 1 + (choice.wrapping_add(offset.wrapping_mul(13))) % (SITES + 1);
                        let outcome = match (choice >> 4) % 4 {
                            0 => "changed value",
                            1 => " ",
                            _ => "expected value",
                        };
                        judgment(site, outcome)
                    })
                    .collect()
            };
            let mut pending = BTreeMap::new();
            let valid = !stale
                && rows.iter().all(|row| {
                    valid_ids.contains(&row.assertion_id)
                        && !row.actual_outcome.trim().is_empty()
                        && model
                            .get(&row.assertion_id)
                            .is_none_or(|value| *value == row.actual_outcome)
                        && pending
                            .insert(row.assertion_id.clone(), row.actual_outcome.clone())
                            .is_none()
                });
            let batch = ClassificationBatch {
                schema_version: 1,
                audit_id: loaded.state.audit_id.clone(),
                snapshot_sha256: if stale {
                    "wrong snapshot".to_owned()
                } else {
                    loaded.state.snapshot_sha256.clone()
                },
                assertions: rows,
            };
            let before = model.len();
            let result = accept_batch(
                &mut loaded.journal,
                batch,
                &loaded.state,
                &loaded.inventory,
                &loaded.index,
            );
            assert_eq!(result.is_ok(), valid, "seed {seed}, step {step}");
            if let Ok((added, unchanged)) = result {
                let submitted = pending.len();
                model.extend(pending);
                assert_eq!(added.len(), model.len() - before);
                assert_eq!(unchanged.len() + added.len(), submitted);
            }
            let actual: BTreeMap<_, _> = loaded
                .journal
                .rows()
                .iter()
                .map(|row| (row.assertion_id.clone(), row.actual_outcome.clone()))
                .collect();
            assert_eq!(actual, model, "seed {seed}, step {step}");
            let export = build_batch(&loaded, NonZeroUsize::MIN);
            assert_eq!(export.remaining, SITES - model.len());
            assert_eq!(
                next_action(export.remaining) == "report",
                model.len() == SITES
            );
        }
        assert_eq!(model.len(), SITES, "each script reaches exact coverage");
    }
}
