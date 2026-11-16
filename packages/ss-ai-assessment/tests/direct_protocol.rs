//! Direct protocol transactions and snapshot validation.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    reason = "The bounded fixtures and parser-produced byte offsets make these operations explicit"
)]
#![allow(
    clippy::assertions_on_result_states,
    reason = "These assertions check whether the operation is accepted or rejected"
)]
use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::panic::{RefUnwindSafe, UnwindSafe};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai_assessment::{
    Assessment, BatchJudgmentItem, BatchJudgmentRequest, BatchReadItem, BatchReadRequest, Change,
    ConclusionGroup, Content, ContextRequest, Disposition, FinalSynthesis, GroupedConclusions,
    Inventory, Judgment, Note, Side, SourceSide, change_id,
};
use static_assertions::assert_impl_all;

assert_impl_all!(BatchReadItem: UnwindSafe, RefUnwindSafe);
assert_impl_all!(BatchReadRequest: UnwindSafe, RefUnwindSafe);
assert_impl_all!(BatchJudgmentItem: UnwindSafe, RefUnwindSafe);
assert_impl_all!(BatchJudgmentRequest: UnwindSafe, RefUnwindSafe);
assert_impl_all!(GroupedConclusions: UnwindSafe, RefUnwindSafe);
assert_impl_all!(FinalSynthesis: UnwindSafe, RefUnwindSafe);

/// Deliver bounded native batches without deriving offsets in the worker.
fn deliver_required(assessment: &mut Assessment, discussion: &str) -> u64 {
    let token = assessment.read_token().unwrap();
    let initial = assessment
        .progress_with_evidence("policy", discussion)
        .unwrap();
    let remaining = initial["requiredDocumentCount"].as_u64().unwrap()
        - initial["deliveredRequiredDocumentCount"].as_u64().unwrap();
    let characters =
        assessment.summary(0, 100, "policy", discussion).unwrap()["progress"]["preflightEstimate"]
            ["transportFragmentCharacters"]
            .as_u64()
            .unwrap();
    // Each nonempty batch must advance at least one scalar or an empty-document receipt.
    for calls in 1..=remaining + characters + 1 {
        let before = serde_json::to_value(&*assessment).unwrap()["reads"].clone();
        let page = assessment.read_next(&token, "policy", discussion).unwrap();
        assert!(serde_json::to_vec_pretty(&page).unwrap().len() < 16_384);
        if page["items"].as_array().unwrap().is_empty() {
            return calls;
        }
        let after = serde_json::to_value(&*assessment).unwrap()["reads"].clone();
        assert_ne!(before, after);
    }
    panic!("Required evidence did not make bounded progress");
}

fn grouped(
    assessment: &Assessment,
    changes: &[&str],
    disposition: Disposition,
) -> GroupedConclusions {
    GroupedConclusions {
        state_token: assessment.state_token().unwrap(),
        groups: vec![ConclusionGroup {
            changes: changes
                .iter()
                .map(|reference| (*reference).to_owned())
                .collect(),
            disposition,
            rationale: "The examined edits preserve the relevant behavior.".into(),
            evidence: Vec::new(),
        }],
    }
}

fn synthesis(assessment: &Assessment) -> FinalSynthesis {
    FinalSynthesis {
        state_token: assessment.state_token().unwrap(),
        disposition: Disposition::Insignificant,
        rationale: "The combined changes preserve behavior and introduce no relevant regression."
            .into(),
        discussion: "The discussion presents no unresolved material objection.".into(),
        unresolved_concerns: Vec::new(),
        evidence: Vec::new(),
    }
}

#[test]
fn grouped_conclusions_and_single_synthesis_require_no_discussion_notes_or_checks() {
    let mut assessment = fixture("discussion", 2);
    let metadata = assessment.summary(0, 100, "policy", "discussion").unwrap();
    assert_eq!(metadata["changes"][0]["reference"], "c1");
    deliver_required(&mut assessment, "discussion");
    let response = assessment
        .conclude(
            grouped(&assessment, &["c1", "c2"], Disposition::Insignificant),
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(response["concludedChangeCount"], 2);
    assert!(response.get("items").is_none());
    assessment
        .synthesize(synthesis(&assessment), "policy", "discussion")
        .unwrap();
    assert!(assessment.rationale().unwrap().contains("combined changes"));
    assert_eq!(assessment.status(false), "readyToReapprove");
    let persisted = serde_json::to_value(&assessment).unwrap();
    assert!(persisted["checkpoints"]["discussion"].is_null());
    assert_eq!(persisted["checkpoints"]["resolutions"], json!({}));
    let reloaded: Assessment = serde_json::from_value(persisted).unwrap();
    reloaded
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

/// Notices explain blocking effects without exposing the retained discussion assessment.
#[test]
fn refusal_explanations_list_distinct_reasons_without_discussion_or_coverage_noise() {
    let mut assessment = fixture("discussion", 2);
    deliver_required(&mut assessment, "discussion");
    let mut conclusions = grouped(&assessment, &["c1"], Disposition::Significant);
    conclusions.groups[0].rationale =
        "The new API removes the caller's cancellation control.".into();
    conclusions.groups.push(ConclusionGroup {
        changes: vec!["c2".into()],
        disposition: Disposition::Uncertain,
        rationale: "The retry loop may submit the same payment twice.\nIts success path no longer records completion.".into(),
        evidence: Vec::new(),
    });
    assessment
        .conclude(conclusions, "policy", "discussion")
        .unwrap();
    let mut reasoning = synthesis(&assessment);
    reasoning.disposition = Disposition::Significant;
    reasoning.rationale = "The new API removes the caller's cancellation control.".into();
    reasoning.discussion = "Unrelated editorial changes need no further review.".into();
    reasoning.unresolved_concerns = vec!["The failure path can leave a payment untracked.".into()];
    assessment
        .synthesize(reasoning, "policy", "discussion")
        .unwrap();
    let explanation = assessment.refusal_explanation().unwrap();
    assert_eq!(
        explanation,
        "- The new API removes the caller's cancellation control.\n\n- The retry loop may submit the same payment twice.\n  Its success path no longer records completion.\n\n- The failure path can leave a payment untracked."
    );
    assert!(!explanation.contains("Material discussion"));
    assert!(!explanation.contains("editorial"));
    assert!(!explanation.contains("Partial assessment"));
}

#[test]
fn concise_json_needs_no_empty_fields_or_agent_authored_bookkeeping() {
    let mut assessment = fixture("discussion", 1);
    deliver_required(&mut assessment, "discussion");
    let request = serde_json::from_value(json!({
        "stateToken": assessment.state_token().unwrap(),
        "groups": [{"changes": ["c1"], "disposition": "insignificant", "rationale": "Behavior remains unchanged."}]
    })).unwrap();
    assessment
        .conclude(request, "policy", "discussion")
        .unwrap();
    let final_reasoning = serde_json::from_value(json!({
        "stateToken": assessment.state_token().unwrap(),
        "disposition": "insignificant",
        "rationale": "The combined changes preserve behavior.",
        "discussion": "No material objection remains."
    }))
    .unwrap();
    assessment
        .synthesize(final_reasoning, "policy", "discussion")
        .unwrap();
    assert_eq!(assessment.status(false), "readyToReapprove");
    assert!(
        assessment
            .conclude(
                grouped(&assessment, &["c1"], Disposition::Significant),
                "policy",
                "discussion"
            )
            .is_err()
    );
}

#[test]
fn additional_evidence_and_conditional_replacements_are_native_and_fail_closed() {
    let mut assessment = fixture("discussion", 2);
    deliver_required(&mut assessment, "discussion");
    let mut request = grouped(&assessment, &["c1", "c2"], Disposition::Insignificant);
    request.groups[0].evidence.push("c1:new".into());
    let before = serde_json::to_value(&assessment).unwrap();
    assert!(
        assessment
            .conclude(request, "policy", "discussion")
            .is_err()
    );
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
    read(&mut assessment, "c1:new", "policy", "discussion");
    let stale = grouped(&assessment, &["c2"], Disposition::Insignificant);
    assessment
        .conclude(
            grouped(&assessment, &["c1"], Disposition::Insignificant),
            "policy",
            "discussion",
        )
        .unwrap();
    assert!(assessment.conclude(stale, "policy", "discussion").is_err());
    let mut request = grouped(&assessment, &["c1", "c2"], Disposition::Insignificant);
    request.groups[0].evidence.push("c1:new".into());
    assessment
        .conclude(request, "policy", "discussion")
        .unwrap();
    let mut final_reasoning = synthesis(&assessment);
    final_reasoning.evidence.push("c2:new".into());
    let before = serde_json::to_value(&assessment).unwrap();
    assert!(
        assessment
            .synthesize(final_reasoning, "policy", "discussion")
            .is_err()
    );
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
    assessment
        .synthesize(synthesis(&assessment), "policy", "discussion")
        .unwrap();
}

#[test]
fn grouped_inputs_fail_atomically_for_missing_evidence_duplicates_and_stale_scope() {
    let mut assessment = fixture("discussion", 2);
    let before = serde_json::to_value(&assessment).unwrap();
    assert!(
        assessment
            .conclude(
                grouped(&assessment, &["c1", "c2"], Disposition::Significant),
                "policy",
                "discussion",
            )
            .is_err()
    );
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
    deliver_required(&mut assessment, "discussion");
    for changes in [&["c1", "c1"][..], &["c1", "c99"][..], &["c01"][..]] {
        let before = serde_json::to_value(&assessment).unwrap();
        assert!(
            assessment
                .conclude(
                    grouped(&assessment, changes, Disposition::Insignificant),
                    "policy",
                    "discussion"
                )
                .is_err()
        );
        assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
    }
    let mut stale = grouped(&assessment, &["c1"], Disposition::Insignificant);
    stale.state_token = fixture("another snapshot discussion", 2)
        .state_token()
        .unwrap();
    assert!(assessment.conclude(stale, "policy", "discussion").is_err());
    let failed = synthesis(&assessment);
    assert!(
        assessment
            .synthesize(failed, "policy", "discussion")
            .is_err()
    );
    assert_eq!(assessment.progress()["cumulativeJudgmentRecorded"], false);
}

#[test]
fn final_synthesis_cannot_hide_significant_uncertain_or_unresolved_conclusions() {
    for member in [
        Disposition::Insignificant,
        Disposition::Significant,
        Disposition::Uncertain,
    ] {
        let mut assessment = fixture("discussion", 1);
        deliver_required(&mut assessment, "discussion");
        assessment
            .conclude(
                grouped(&assessment, &["c1"], member),
                "policy",
                "discussion",
            )
            .unwrap();
        let mut final_reasoning = synthesis(&assessment);
        if member == Disposition::Insignificant {
            final_reasoning
                .unresolved_concerns
                .push("A relevant interaction remains uncertain.".into());
        }
        assessment
            .synthesize(final_reasoning, "policy", "discussion")
            .unwrap();
        assert_ne!(assessment.status(false), "readyToReapprove");
        assessment
            .validate("snapshot", "policy", "policy", "discussion")
            .unwrap();
    }
}

#[test]
fn source_context_changes_local_token_and_invalidates_grouped_coverage() {
    let mut assessment = fixture("discussion", 2);
    deliver_required(&mut assessment, "discussion");
    assessment
        .conclude(
            grouped(&assessment, &["c1", "c2"], Disposition::Insignificant),
            "policy",
            "discussion",
        )
        .unwrap();
    let stale = synthesis(&assessment);
    let token = assessment.read_token().unwrap();
    assessment
        .source_context_batch(
            vec![ContextRequest {
                change_id: "c1".into(),
                side: SourceSide::New,
                start_line: NonZeroUsize::new(1).unwrap(),
                line_count: NonZeroUsize::new(1).unwrap(),
            }],
            "policy",
            "discussion",
        )
        .unwrap();
    let before = serde_json::to_value(&assessment).unwrap();
    assert!(
        assessment
            .read_next(&token, "policy", "discussion")
            .is_err()
    );
    assert!(
        assessment
            .synthesize(stale, "policy", "discussion")
            .is_err()
    );
    assert!(
        assessment
            .conclude(
                grouped(&assessment, &["c1"], Disposition::Insignificant),
                "policy",
                "discussion"
            )
            .is_err()
    );
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
    deliver_required(&mut assessment, "discussion");
    assessment
        .conclude(
            grouped(&assessment, &["c1"], Disposition::Insignificant),
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .synthesize(synthesis(&assessment), "policy", "discussion")
        .unwrap();
    assert_eq!(assessment.status(false), "readyToReapprove");
}

#[test]
fn targeted_delivery_supports_partial_refusal_without_unrelated_reads() {
    let mut assessment = fixture("discussion", 3);
    assessment
        .source_context_batch(
            vec![ContextRequest {
                change_id: "c2".into(),
                side: SourceSide::New,
                start_line: NonZeroUsize::new(1).unwrap(),
                line_count: NonZeroUsize::new(1).unwrap(),
            }],
            "policy",
            "discussion",
        )
        .unwrap();
    let token = assessment.read_token().unwrap();
    let before = serde_json::to_value(&assessment).unwrap();
    for changes in [vec!["c4".into()], vec!["c2".into(), "c2".into()]] {
        assert!(
            assessment
                .read_next_for(&token, &changes, "policy", "discussion")
                .is_err()
        );
        assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
    }

    let mut references = BTreeSet::new();
    // This small selection fits comfortably within a few bounded responses.
    for _ in 0..4 {
        let response = assessment
            .read_next_for(&token, &["c2".into()], "policy", "discussion")
            .unwrap();
        let items = response["items"].as_array().unwrap();
        if items.is_empty() {
            break;
        }
        for item in items {
            references.insert(item["reference"].as_str().unwrap().to_owned());
        }
    }
    assert!(references.contains("policy"));
    assert!(references.contains("c2:patch"));
    assert!(
        references
            .iter()
            .any(|reference| reference.starts_with("c2:context:"))
    );
    assert_eq!(references.len(), 3);
    assert!(
        assessment
            .synthesize(synthesis(&assessment), "policy", "discussion")
            .is_err()
    );
    assessment
        .conclude(
            grouped(&assessment, &["c2"], Disposition::Significant),
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .finish_partial(
            "c2",
            "The examined change requires review.",
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(assessment.status(false), "needsReview");
    let progress = assessment
        .progress_with_evidence("policy", "discussion")
        .unwrap();
    assert_eq!(progress["judgedChangeCount"], 1);
    assert_eq!(progress["deliveredRequiredDocumentCount"], 3);
    assert_eq!(progress["requiredDocumentCount"], 6);
}

#[test]
fn native_evidence_batches_handle_escaped_unicode_empty_documents_and_partial_significance() {
    let discussion = "\\\"😀\n".repeat(4_000);
    let mut assessment = fixture(&discussion, 2);
    deliver_required(&mut assessment, &discussion);
    let progress = assessment
        .progress_with_evidence("policy", &discussion)
        .unwrap();
    assert_eq!(
        progress["requiredDocumentCount"],
        progress["deliveredRequiredDocumentCount"]
    );
    let mut empty = fixture("", 0);
    deliver_required(&mut empty, "");
    empty.synthesize(synthesis(&empty), "policy", "").unwrap();
    assert_eq!(empty.status(true), "wouldReapprove");
    assessment
        .conclude(
            grouped(&assessment, &["c1"], Disposition::Significant),
            "policy",
            &discussion,
        )
        .unwrap();
    assessment
        .finish_partial(
            "c1",
            "The examined change requires review.",
            "policy",
            &discussion,
        )
        .unwrap();
    assert_eq!(assessment.status(false), "needsReview");
    assert_eq!(assessment.progress()["judgedChangeCount"], 1);
    assert_eq!(assessment.progress()["partialSignificant"], true);
    assessment
        .validate("snapshot", "policy", "policy", &discussion)
        .unwrap();
}

#[test]
fn native_batch_limits_publish_only_delivered_receipts_and_compact_group_results() {
    // This inventory exceeds both one evidence response and a per-member digest response.
    let mut assessment = fixture("discussion", 100);
    let first = assessment
        .read_next(&assessment.read_token().unwrap(), "policy", "discussion")
        .unwrap();
    let pages = first["items"].as_array().unwrap();
    assert!(pages.len() < assessment.inventory.changes.len());
    let references = pages
        .iter()
        .map(|page| {
            let reference = page["reference"].as_str().unwrap();
            if let Some(number) = reference
                .strip_prefix('c')
                .and_then(|value| value.strip_suffix(":patch"))
            {
                let index = number.parse::<usize>().unwrap() - 1;
                let id = &assessment.inventory.changes[index].id;
                let document: Value = serde_json::from_str(page["text"].as_str().unwrap()).unwrap();
                assert_eq!(document["changeId"], *id);
                format!("{id}:patch")
            } else {
                reference.to_owned()
            }
        })
        .collect::<BTreeSet<_>>();
    let persisted = serde_json::to_value(&assessment).unwrap();
    let receipts = persisted["reads"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(references, receipts);
    // The small patches must travel in groups, rather than one model round
    // trip per change; the extra call establishes that delivery is finished.
    let remaining_calls = deliver_required(&mut assessment, "discussion");
    assert!(remaining_calls < 16);
    let references = (1..=assessment.inventory.changes.len())
        .map(|index| format!("c{index}"))
        .collect::<Vec<_>>();
    let response = assessment
        .conclude(
            GroupedConclusions {
                state_token: assessment.state_token().unwrap(),
                groups: vec![ConclusionGroup {
                    changes: references,
                    disposition: Disposition::Insignificant,
                    rationale: "The examined edits share the same behavioral effect.".into(),
                    evidence: Vec::new(),
                }],
            },
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(
        response["concludedChangeCount"],
        assessment.inventory.changes.len()
    );
    assert!(serde_json::to_vec_pretty(&response).unwrap().len() < 16_384);
    assessment
        .synthesize(synthesis(&assessment), "policy", "discussion")
        .unwrap();
    assert_eq!(assessment.status(false), "readyToReapprove");
}

fn fixture(discussion: &str, count: usize) -> Assessment {
    let mut changes = Vec::new();
    let mut patches = Vec::new();
    for index in 0..count {
        let side = |text: &str, object: &str| Side {
            path: format!("file{index}"),
            object: object.repeat(40),
            mode: "100644".into(),
            content: Content::Text { text: text.into() },
        };
        let old = Some(side("old\n", "a"));
        let new = Some(side("new\n", "b"));
        changes.push(Change {
            id: change_id("M", old.as_ref(), new.as_ref()).unwrap(),
            kind: "M".into(),
            old,
            new,
        });
        patches.push("@@ -1 +1 @@\n-old\n+new\n".into());
    }
    Assessment::new_direct(
        Inventory {
            old_commit: "a".repeat(40),
            new_commit: "b".repeat(40),
            old_tree: "c".repeat(40),
            new_tree: if count == 0 { "c" } else { "d" }.repeat(40),
            changes,
            complete: true,
        },
        &patches,
        "snapshot",
        "policy",
        discussion,
    )
    .unwrap()
}

fn read(assessment: &mut Assessment, reference: &str, policy: &str, discussion: &str) {
    let mut offset = 0;
    loop {
        let page = assessment
            .read(reference, offset, usize::MAX, policy, discussion)
            .unwrap();
        if page["complete"] == true {
            break;
        }
        offset = usize::try_from(page["nextOffset"].as_u64().unwrap()).unwrap();
    }
}

fn judgment(id: &str) -> Judgment {
    Judgment {
        disposition: Disposition::Insignificant,
        rationale: "Examined the complete evidence.".into(),
        evidence: if id == "cumulative" {
            vec!["policy".into(), "discussion".into()]
        } else {
            vec![format!("{id}:patch")]
        },
        concerns: Vec::new(),
        interactions: Vec::new(),
    }
}

fn note(reference: &str) -> Note {
    Note {
        observations: "Examined the discussion.".into(),
        interfaces: "No applicable interface.".into(),
        concerns: Vec::new(),
        interactions: Vec::new(),
        evidence: vec![reference.into()],
    }
}

fn request_read(assessment: &Assessment, items: Vec<BatchReadItem>) -> BatchReadRequest {
    BatchReadRequest {
        read_token: assessment.read_token().unwrap(),
        items,
    }
}

#[test]
fn significant_partial_completion_requires_support_but_not_unrelated_coverage() {
    let mut assessment = fixture("discussion", 2);
    let id = assessment.inventory.changes[0].id.clone();
    assert!(
        assessment
            .finish_partial(&id, "A significant change.", "policy", "discussion")
            .is_err()
    );
    read(&mut assessment, "policy", "policy", "discussion");
    let mut significant = judgment(&id);
    significant.disposition = Disposition::Significant;
    assert!(
        assessment
            .record(&id, significant, None, "policy", "discussion")
            .is_err()
    );
    read(
        &mut assessment,
        &format!("{id}:patch"),
        "policy",
        "discussion",
    );
    let mut significant = judgment(&id);
    significant.disposition = Disposition::Significant;
    assessment
        .record(&id, significant, None, "policy", "discussion")
        .unwrap();
    assessment
        .finish_partial(
            &id,
            "The examined change requires review.",
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(assessment.status(false), "needsReview");
    assert_eq!(assessment.status(true), "needsReview");
    assert_eq!(
        assessment.explanation().unwrap(),
        "The examined change requires review."
    );
    assert_eq!(
        assessment.refusal_explanation().unwrap(),
        "- The examined change requires review."
    );
    let progress = assessment
        .progress_with_evidence("policy", "discussion")
        .unwrap();
    assert_eq!(progress["partialSignificant"], true);
    assert_eq!(progress["judgedChangeCount"], 1);
    assert_eq!(progress["changeCount"], 2);
    let reloaded: Assessment =
        serde_json::from_value(serde_json::to_value(&assessment).unwrap()).unwrap();
    reloaded
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

#[test]
fn compact_patch_references_select_the_same_unit_windows_as_exact_references() {
    let mut assessment = fixture("discussion", 1);
    let exact = format!("{}:patch", assessment.inventory.changes[0].id);
    let compact = "c1:patch";
    let full_units = assessment.units_for(Some(&exact), 0, 100).unwrap();
    assert_eq!(
        assessment.units_for(Some(compact), 0, 100).unwrap(),
        full_units
    );
    assert_eq!(
        assessment.compact_units_for(Some(compact), 0, 100).unwrap(),
        assessment.compact_units_for(Some(&exact), 0, 100).unwrap()
    );
    let plan = assessment
        .unit_read_plan(compact, 0, 100, "policy", "discussion")
        .unwrap();
    assert_eq!(
        plan,
        assessment
            .unit_read_plan(&exact, 0, 100, "policy", "discussion")
            .unwrap()
    );
    let count = usize::try_from(plan["items"][0]["count"].as_u64().unwrap()).unwrap();
    let delivered = assessment
        .read_units(
            compact,
            0,
            count,
            assessment.read_token().unwrap(),
            "policy",
            "discussion",
        )
        .unwrap();
    assert!(!delivered["items"].as_array().unwrap().is_empty());
    assert!(assessment.units_for(Some("c2:patch"), 0, 100).is_err());
    assert!(
        assessment
            .unit_read_plan("c2:patch", 0, 100, "policy", "discussion")
            .is_err()
    );
}

#[test]
fn cumulative_concern_supplies_a_rationale_for_derived_uncertainty() {
    let mut assessment = fixture("discussion", 1);
    deliver_required(&mut assessment, "discussion");
    assessment
        .conclude(
            grouped(&assessment, &["c1"], Disposition::Insignificant),
            "policy",
            "discussion",
        )
        .unwrap();
    let mut cumulative = judgment("cumulative");
    cumulative.rationale = "The combined behavior remains unresolved.".into();
    cumulative.concerns.push("Review the interaction.".into());
    assessment
        .record("cumulative", cumulative, None, "policy", "discussion")
        .unwrap();
    assessment.finish("policy", "discussion").unwrap();
    assert_eq!(assessment.disposition(), Some(Disposition::Uncertain));
    assert_eq!(
        assessment.rationale(),
        Some("The combined behavior remains unresolved.")
    );
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

#[test]
fn adding_first_discussion_note_invalidates_prior_cumulative_reasoning() {
    let mut assessment = fixture("discussion", 1);
    deliver_required(&mut assessment, "discussion");
    assessment
        .conclude(
            grouped(&assessment, &["c1"], Disposition::Insignificant),
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .record(
            "cumulative",
            judgment("cumulative"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .note(
            "discussion",
            note("discussion"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(assessment.progress()["cumulativeJudgmentRecorded"], false);
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

#[test]
fn partial_completion_rejects_uncertainty_and_unread_requested_context() {
    let mut assessment = fixture("discussion", 2);
    let id = assessment.inventory.changes[0].id.clone();
    read(&mut assessment, "policy", "policy", "discussion");
    read(
        &mut assessment,
        &format!("{id}:patch"),
        "policy",
        "discussion",
    );
    let mut uncertain = judgment(&id);
    uncertain.disposition = Disposition::Uncertain;
    assessment
        .record(&id, uncertain, None, "policy", "discussion")
        .unwrap();
    assert!(
        assessment
            .finish_partial(
                &id,
                "Uncertainty is not significance.",
                "policy",
                "discussion"
            )
            .is_err()
    );
    let previous_digest = assessment.summary(0, 100, "policy", "discussion").unwrap()["changes"][0]
        ["decisionDigest"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut significant = judgment(&id);
    significant.disposition = Disposition::Significant;
    assessment
        .record(
            &id,
            significant,
            Some(&previous_digest),
            "policy",
            "discussion",
        )
        .unwrap();
    let other_id = assessment.inventory.changes[1].id.clone();
    let context = assessment
        .source_context(
            ContextRequest {
                change_id: other_id,
                side: SourceSide::New,
                start_line: NonZeroUsize::new(1).unwrap(),
                line_count: NonZeroUsize::new(1).unwrap(),
            },
            1,
            "policy",
            "discussion",
        )
        .unwrap();
    let reference = context["reference"].as_str().unwrap();
    assert!(
        assessment
            .finish_partial(&id, "A significant change.", "policy", "discussion")
            .is_err()
    );
    read(&mut assessment, reference, "policy", "discussion");
    assessment
        .finish_partial(
            &id,
            "The examined change requires review.",
            "policy",
            "discussion",
        )
        .unwrap();
}

#[test]
fn summary_exposes_the_current_state_token_within_the_response_budget() {
    let mut assessment = fixture("discussion", 1);
    let first = assessment.summary(0, 100, "policy", "discussion").unwrap();
    assert_eq!(first["stateToken"], assessment.state_token().unwrap());
    assert_eq!(first["readToken"], assessment.read_token().unwrap());
    assert!(serde_json::to_vec_pretty(&first).unwrap().len() < 16_384);
    read(&mut assessment, "policy", "policy", "discussion");
    let next = assessment.summary(0, 100, "policy", "discussion").unwrap();
    assert_eq!(first["stateToken"], next["stateToken"]);
    assert_eq!(next["stateToken"], assessment.state_token().unwrap());
    assert_eq!(first["readToken"], next["readToken"]);
    assert!(serde_json::to_vec_pretty(&next).unwrap().len() < 16_384);
}

#[test]
fn direct_progress_tracks_required_documents_and_context_continuations() {
    let mut assessment = fixture("discussion", 1);
    let id = assessment.inventory.changes[0].id.clone();
    let initial = assessment
        .progress_with_evidence("policy", "discussion")
        .unwrap();
    assert_eq!(initial["requiredDocumentCount"], 3);
    assert_eq!(initial["deliveredRequiredDocumentCount"], 0);
    assert_eq!(
        initial["nextRequiredRead"],
        json!({"reference":"policy", "offset":0})
    );
    assessment
        .read("policy", 0, 1, "policy", "discussion")
        .unwrap();
    let partial = assessment
        .progress_with_evidence("policy", "discussion")
        .unwrap();
    assert_eq!(partial["deliveredRequiredDocumentCount"], 0);
    assert_eq!(
        partial["nextRequiredRead"],
        json!({"reference":"policy", "offset":1})
    );
    read(&mut assessment, "policy", "policy", "discussion");
    read(&mut assessment, "discussion", "policy", "discussion");
    read(
        &mut assessment,
        &format!("{id}:patch"),
        "policy",
        "discussion",
    );
    let context = assessment
        .source_context(
            ContextRequest {
                change_id: id,
                side: SourceSide::New,
                start_line: NonZeroUsize::new(1).unwrap(),
                line_count: NonZeroUsize::new(1).unwrap(),
            },
            1,
            "policy",
            "discussion",
        )
        .unwrap();
    let reference = context["reference"].as_str().unwrap();
    let partial = assessment
        .progress_with_evidence("policy", "discussion")
        .unwrap();
    assert_eq!(partial["requiredDocumentCount"], 4);
    assert_eq!(partial["deliveredRequiredDocumentCount"], 3);
    assert_eq!(
        partial["nextRequiredRead"],
        json!({"reference":reference, "offset":1})
    );
    read(&mut assessment, reference, "policy", "discussion");
    let delivered = assessment
        .progress_with_evidence("policy", "discussion")
        .unwrap();
    assert_eq!(delivered["deliveredRequiredDocumentCount"], 4);
    assert_eq!(delivered["nextRequiredRead"], Value::Null);
}

#[test]
fn many_small_pages_keep_required_document_progress_exact() {
    let discussion = "😀x\n".repeat(12_000);
    let mut assessment = fixture(&discussion, 25);
    read(&mut assessment, "policy", "policy", &discussion);
    let token = assessment.read_token().unwrap();
    let mut offset = 0;
    loop {
        let response = assessment
            .read_batch(
                BatchReadRequest {
                    read_token: token.clone(),
                    items: vec![BatchReadItem {
                        reference: "discussion".into(),
                        offset,
                        limit: 1_000,
                        content_digest: None,
                    }],
                },
                "policy",
                &discussion,
            )
            .unwrap();
        let progress = assessment
            .progress_with_evidence("policy", &discussion)
            .unwrap();
        assert_eq!(progress["requiredDocumentCount"], 27);
        if response["items"][0]["complete"] == true {
            assert_eq!(progress["deliveredRequiredDocumentCount"], 2);
            break;
        }
        assert_eq!(progress["deliveredRequiredDocumentCount"], 1);
        let next = usize::try_from(response["items"][0]["nextOffset"].as_u64().unwrap()).unwrap();
        assert!(next > offset);
        assert_eq!(
            progress["nextRequiredRead"],
            json!({"reference":"discussion", "offset":next})
        );
        offset = next;
    }
}

#[test]
fn read_token_allows_independent_batches_and_rejects_stale_contexts() {
    let mut assessment = fixture("discussion", 1);
    let id = assessment.inventory.changes[0].id.clone();
    let read_token = assessment.read_token().unwrap();
    let initial_state = assessment.state_token().unwrap();
    let batch = |reference: &str| BatchReadRequest {
        read_token: read_token.clone(),
        items: vec![read_item(reference, usize::MAX)],
    };
    let first = assessment
        .read_batch(batch("policy"), "policy", "discussion")
        .unwrap();
    assert_eq!(first["stateToken"], initial_state);
    let second = assessment
        .read_batch(batch("discussion"), "policy", "discussion")
        .unwrap();
    assert_eq!(first["stateToken"], second["stateToken"]);
    assert_eq!(first["readToken"], read_token);
    assert_eq!(second["readToken"], read_token);
    assert!(second["items"][0].get("readToken").is_none());
    let judgment_batch = || BatchJudgmentRequest {
        state_token: initial_state.clone(),
        items: vec![BatchJudgmentItem {
            id: id.clone(),
            judgment: judgment(&id),
            previous_digest: None,
        }],
    };
    assert!(
        assessment
            .record_batch(judgment_batch(), "policy", "discussion")
            .is_err()
    );
    assessment
        .read_batch(batch(&format!("{id}:patch")), "policy", "discussion")
        .unwrap();
    assert_eq!(assessment.read_token().unwrap(), read_token);
    assessment
        .record_batch(judgment_batch(), "policy", "discussion")
        .unwrap();
    assert!(
        assessment
            .record_batch(judgment_batch(), "policy", "discussion")
            .is_err()
    );
    assert_eq!(assessment.read_token().unwrap(), read_token);
    let plan = assessment
        .unit_read_plan("discussion", 0, 100, "policy", "discussion")
        .unwrap();
    let page = assessment
        .source_context(
            ContextRequest {
                change_id: id,
                side: SourceSide::New,
                start_line: NonZeroUsize::new(1).unwrap(),
                line_count: NonZeroUsize::new(1).unwrap(),
            },
            usize::MAX,
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(page["readToken"], assessment.read_token().unwrap());
    assert_ne!(assessment.read_token().unwrap(), read_token);
    let state = assessment.state_token().unwrap();
    assert!(
        assessment
            .read_batch(batch("policy"), "policy", "discussion")
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), state);
    let count = usize::try_from(plan["items"][0]["count"].as_u64().unwrap()).unwrap();
    assert!(
        assessment
            .read_units(
                "discussion",
                0,
                count,
                read_token.clone(),
                "policy",
                "discussion"
            )
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), state);
    assessment
        .read_units(
            "discussion",
            0,
            count,
            page["readToken"].as_str().unwrap().into(),
            "policy",
            "discussion",
        )
        .unwrap();
    let retried = assessment
        .read_batch(
            BatchReadRequest {
                read_token: page["readToken"].as_str().unwrap().into(),
                items: vec![read_item("policy", 1)],
            },
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(retried["items"][0]["text"], "p");
    let unavailable_offset = BatchReadRequest {
        read_token: assessment.read_token().unwrap(),
        items: vec![BatchReadItem {
            reference: "discussion".into(),
            offset: 1000,
            limit: 1,
            content_digest: None,
        }],
    };
    assert!(
        assessment
            .read_batch(unavailable_offset, "policy", "discussion")
            .is_err()
    );
    assert_eq!(
        assessment.state_token().unwrap(),
        retried["stateToken"].as_str().unwrap()
    );
}

#[test]
fn context_batch_registers_ranges_atomically_without_claiming_delivery() {
    let mut assessment = fixture("discussion", 10);
    let requests = assessment
        .inventory
        .changes
        .iter()
        .map(|change| ContextRequest {
            change_id: change.id.clone(),
            side: SourceSide::New,
            start_line: NonZeroUsize::MIN,
            line_count: NonZeroUsize::MIN,
        })
        .collect::<Vec<_>>();
    let original = assessment.state_token().unwrap();
    let mut bad = requests.clone();
    bad[9].start_line = NonZeroUsize::new(2).unwrap();
    assert!(
        assessment
            .source_context_batch(bad, "policy", "discussion")
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), original);

    let old_token = assessment.read_token().unwrap();
    let batch = assessment
        .source_context_batch(requests, "policy", "discussion")
        .unwrap();
    assert_ne!(batch["readToken"], old_token);
    assert_eq!(batch["items"].as_array().unwrap().len(), 10);
    let progress = assessment
        .progress_with_evidence("policy", "discussion")
        .unwrap();
    assert_eq!(progress["requiredDocumentCount"], 22);
    assert_eq!(progress["deliveredRequiredDocumentCount"], 0);

    let items = batch["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| read_item(item["reference"].as_str().unwrap(), usize::MAX))
        .collect();
    let response = assessment
        .read_batch(
            BatchReadRequest {
                read_token: batch["readToken"].as_str().unwrap().into(),
                items,
            },
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(response["items"].as_array().unwrap().len(), 10);
    assert_eq!(
        assessment
            .progress_with_evidence("policy", "discussion")
            .unwrap()["deliveredRequiredDocumentCount"],
        10
    );
    assert!(
        response["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["text"] == "new\n")
    );
}

#[test]
fn incident_sized_inventories_need_fewer_pages_at_the_supported_limit() {
    // The path counts mirror the reported incidents without retaining their source.
    for count in [37, 50] {
        let assessment = fixture("discussion", count);
        let pages = |limit, compact| {
            let mut offset = 0;
            let mut ids = Vec::new();
            let mut calls = 0;
            loop {
                let page = if compact {
                    assessment.compact_summary(offset, limit, "policy", "discussion")
                } else {
                    assessment.summary(offset, limit, "policy", "discussion")
                }
                .unwrap();
                calls += 1;
                ids.extend(
                    page["changes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|change| change["reference"].as_str().unwrap().to_owned()),
                );
                match page["nextOffset"].as_u64() {
                    Some(next) => offset = usize::try_from(next).unwrap(),
                    None => break,
                }
            }
            (calls, ids)
        };
        let baseline = pages(10, false);
        let batched = pages(100, true);
        assert_eq!(baseline.1.len(), count);
        assert_eq!(batched.1, baseline.1);
        assert!(batched.0 < baseline.0);
    }
}

#[test]
fn compact_inventory_preserves_required_work_and_path_identity() {
    let assessment = fixture("discussion", 1);
    let full = assessment.summary(0, 100, "policy", "discussion").unwrap();
    let compact = assessment
        .compact_summary(0, 100, "policy", "discussion")
        .unwrap();
    for key in ["snapshotDigest", "readToken", "stateToken", "progress"] {
        assert_eq!(compact[key], full[key]);
    }
    let full_change = &full["changes"][0];
    let compact_change = &compact["changes"][0];
    for key in [
        "kind",
        "oldLineCount",
        "newLineCount",
        "contentAvailable",
        "hunkCount",
        "requestedContextCount",
        "nextContextReference",
    ] {
        assert_eq!(compact_change[key], full_change[key]);
    }
    assert_eq!(compact_change["reference"], "c1");
    assert_eq!(compact_change["requiredPatch"]["reference"], "c1:patch");
    assert!(compact_change.get("id").is_none());
    assert!(compact_change.get("decision").is_none());
    assert!(compact_change.get("decisionDigest").is_none());
    assert_eq!(compact_change["old"], full_change["old"]["path"]);
    assert_eq!(compact_change["new"], full_change["new"]["path"]);
    assert_eq!(compact_change["oldLineCount"], 1);
    assert_eq!(compact_change["newLineCount"], 1);
}

#[test]
fn compact_units_allow_independent_discussion_delivery() {
    let discussion = json!({
        "platform": "gitHub",
        "reviewComments": (0..4).map(|id| json!({
            "id": id + 1,
            "body": format!("objection {id}"),
            "path": format!("file{id}"),
        })).collect::<Vec<_>>(),
    })
    .to_string();
    let mut assessment = fixture(&discussion, 0);
    let full = assessment.units_for(Some("discussion"), 0, 100).unwrap();
    let compact = assessment
        .compact_units_for(Some("discussion"), 0, 100)
        .unwrap();
    let full = full["items"].as_array().unwrap();
    let compact = compact["items"].as_array().unwrap();
    assert!(compact.len() > 1);
    assert_eq!(compact.len(), full.len());
    for (item, original) in compact.iter().zip(full) {
        assert_eq!(item["reference"], original["reference"]);
        assert_eq!(item["bindingReference"], original["bindingReference"]);
        assert_eq!(item["unit"]["id"], original["unit"]["id"]);
        assert_eq!(
            item["unit"]["totalCharacters"],
            original["unit"]["totalCharacters"]
        );
    }
    // Independent unit receipts permit delivery in any order.
    for item in compact.iter().rev() {
        read(
            &mut assessment,
            item["reference"].as_str().unwrap(),
            "policy",
            &discussion,
        );
        read(
            &mut assessment,
            item["bindingReference"].as_str().unwrap(),
            "policy",
            &discussion,
        );
    }
    let summary = assessment.summary(0, 100, "policy", &discussion).unwrap();
    assert_eq!(summary["documentReads"][1]["complete"], true);
    assessment
        .validate("snapshot", "policy", "policy", &discussion)
        .unwrap();
}

#[test]
fn planned_discussion_pages_are_bounded_and_independently_readable() {
    let discussion = json!({
        "platform": "gitHub",
        // The retained incident contains this many discussion fragments; the
        // synthetic records keep the same scale without copying its contents.
        "reviewComments": (0..149).map(|id| json!({
            "id": id + 1,
            "body": format!("{id} {}", "\\\"\nλ".repeat(90)),
            "path": format!("file{id}"),
        })).collect::<Vec<_>>(),
    })
    .to_string();
    let mut assessment = fixture(&discussion, 1);
    let original_state = assessment.state_token().unwrap();
    let original_reads = serde_json::to_value(&assessment).unwrap()["reads"].clone();
    let mut plans = Vec::new();
    let mut after = 0;
    loop {
        let plan = assessment
            .unit_read_plan("discussion", after, 2, "policy", &discussion)
            .unwrap();
        assert_eq!(assessment.state_token().unwrap(), original_state);
        assert_eq!(
            serde_json::to_value(&assessment).unwrap()["reads"],
            original_reads
        );
        assert!(plan["items"].as_array().unwrap().len() <= 2);
        plans.extend(plan["items"].as_array().unwrap().iter().cloned());
        match plan["nextOffset"].as_u64() {
            Some(next) => after = usize::try_from(next).unwrap(),
            None => break,
        }
    }
    assert!(plans.len() > 2);
    // The response limit permits several small escaped records per window.
    assert!(plans.len() <= 30);
    let mut manifest_offset = 0;
    let mut unit_count = 0;
    loop {
        let page = assessment
            .compact_units_for(Some("discussion"), manifest_offset, 100)
            .unwrap();
        unit_count += page["items"].as_array().unwrap().len();
        match page["nextOffset"].as_u64() {
            Some(next) => manifest_offset = usize::try_from(next).unwrap(),
            None => break,
        }
    }
    assert_eq!(
        plans
            .iter()
            .map(|window| usize::try_from(window["count"].as_u64().unwrap()).unwrap())
            .sum::<usize>(),
        unit_count
    );
    let token = assessment.read_token().unwrap();
    let change = assessment.inventory.changes[0].id.clone();
    read(&mut assessment, "policy", "policy", &discussion);
    read(
        &mut assessment,
        &format!("{change}:patch"),
        "policy",
        &discussion,
    );
    assessment
        .record(&change, judgment(&change), None, "policy", &discussion)
        .unwrap();
    assert_ne!(assessment.state_token().unwrap(), original_state);
    assert_eq!(assessment.read_token().unwrap(), token);
    // Independent planned windows must work in reverse order as well as forward order.
    for window in plans.iter().rev() {
        let page = assessment
            .read_units(
                "discussion",
                usize::try_from(window["after"].as_u64().unwrap()).unwrap(),
                usize::try_from(window["count"].as_u64().unwrap()).unwrap(),
                token.clone(),
                "policy",
                &discussion,
            )
            .unwrap();
        assert!(serde_json::to_vec_pretty(&page).unwrap().len() < 16_384);
        assert!(page.get("progress").is_none());
        assert_eq!(page["readToken"], token);
        for item in page["items"].as_array().unwrap() {
            assert_eq!(item["complete"], true);
            assert!(item["reference"].as_str().unwrap().starts_with("unit:"));
            assert!(item["unit"].get("id").is_none());
            assert!(item["unit"]["bindingReference"].is_string());
            if item["unit"]["bindings"].is_null() {
                read(
                    &mut assessment,
                    item["unit"]["bindingReference"].as_str().unwrap(),
                    "policy",
                    &discussion,
                );
            }
        }
    }
    let summary = assessment.summary(0, 100, "policy", &discussion).unwrap();
    assert_eq!(summary["documentReads"][1]["complete"], true);
    assessment
        .validate("snapshot", "policy", "policy", &discussion)
        .unwrap();
}

#[test]
fn semantic_continuations_require_the_current_content_digest() {
    let mut assessment = fixture("discussion", 1);
    let id = assessment.inventory.changes[0].id.clone();
    let reference = format!("judgment:{id}");
    read(&mut assessment, "policy", "policy", "discussion");
    read(
        &mut assessment,
        &format!("{id}:patch"),
        "policy",
        "discussion",
    );
    let previous = assessment
        .record(&id, judgment(&id), None, "policy", "discussion")
        .unwrap();
    let token = assessment.read_token().unwrap();
    let initial = assessment
        .read(&reference, 0, 5, "policy", "discussion")
        .unwrap();
    let old_content = initial["contentDigest"].as_str().unwrap().to_owned();
    let mut replacement = judgment(&id);
    replacement.rationale.push_str(" A distinct judgment.");
    assessment
        .record(&id, replacement, Some(&previous), "policy", "discussion")
        .unwrap();
    assert_eq!(assessment.read_token().unwrap(), token);
    let fresh = assessment
        .read(&reference, 0, 5, "policy", "discussion")
        .unwrap();
    let current_content = fresh["contentDigest"].as_str().unwrap().to_owned();
    assert_ne!(old_content, current_content);
    assert_eq!(initial["text"], fresh["text"]);
    let continuation = |digest| BatchReadRequest {
        read_token: token.clone(),
        items: vec![BatchReadItem {
            reference: reference.clone(),
            offset: 5,
            limit: 5,
            content_digest: digest,
        }],
    };
    let state = assessment.state_token().unwrap();
    assert!(
        assessment
            .read(&reference, 5, 5, "policy", "discussion")
            .is_err()
    );
    assert!(
        assessment
            .read_batch(continuation(None), "policy", "discussion")
            .is_err()
    );
    assert!(
        assessment
            .read_batch(continuation(Some(old_content)), "policy", "discussion")
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), state);
    assert!(
        assessment
            .read_batch(continuation(Some(current_content)), "policy", "discussion")
            .is_ok()
    );
}

#[test]
fn minimum_page_bound_uses_unit_characters() {
    let discussion = "😀".repeat(17_000);
    let assessment = fixture(&discussion, 0);
    let progress = assessment.progress();
    assert_eq!(
        progress["preflightEstimate"]["transportFragmentCharacters"],
        17_000
    );
    assert_eq!(
        progress["preflightEstimate"]["minimumTransportEvidencePages"],
        2
    );
}

fn read_item(reference: &str, limit: usize) -> BatchReadItem {
    BatchReadItem {
        reference: reference.into(),
        offset: 0,
        limit,
        content_digest: None,
    }
}

/// Keeps each original JSON field's serialization while reconstructing the tuple.
fn digest_inputs(assessment: &Assessment, contract: &str) -> String {
    let serialized = serde_json::to_string(assessment).unwrap();
    let field = |name: &str| {
        let marker = format!("\"{name}\":");
        let start = if name == "cumulative" {
            serialized.rfind(&marker)
        } else {
            serialized.find(&marker)
        }
        .unwrap()
            + marker.len();
        let mut parser =
            serde_json::Deserializer::from_str(&serialized[start..]).into_iter::<Value>();
        parser.next().unwrap().unwrap();
        serialized[start..start + parser.byte_offset()].to_owned()
    };
    let mut fields = [
        "snapshotDigest",
        "inventoryDigest",
        "policyDigest",
        "discussionDigest",
        "sourceEvidenceDigest",
        "checkpoints",
        "contexts",
        "documents",
    ]
    .into_iter()
    .map(field)
    .collect::<Vec<_>>();
    fields.push(serde_json::to_string(contract).unwrap());
    fields.extend(
        ["decisions", "cumulative", "partial"]
            .into_iter()
            .map(field),
    );
    format!("[{}]", fields.join(","))
}

#[test]
fn read_batches_are_atomic_bounded_and_reject_stale_or_duplicate_items() {
    let policy = "😀".repeat(5000);
    let discussion = "終".repeat(5000);
    let mut assessment = fixture(&discussion, 0);
    let original = assessment.state_token().unwrap();
    let request = request_read(
        &assessment,
        vec![read_item("policy", 5000), read_item("discussion", 5000)],
    );
    assert!(
        assessment
            .read_batch(request, &policy, &discussion)
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), original);
    let request = request_read(
        &assessment,
        vec![read_item("policy", 2), read_item("missing", 2)],
    );
    assert!(
        assessment
            .read_batch(request, &policy, &discussion)
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), original);
    let request = request_read(
        &assessment,
        vec![read_item("policy", 1), read_item("policy", 1)],
    );
    assert!(
        assessment
            .read_batch(request, &policy, &discussion)
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), original);
    let response = assessment
        .read_batch(
            request_read(
                &assessment,
                vec![read_item("policy", 2), read_item("discussion", 2)],
            ),
            &policy,
            &discussion,
        )
        .unwrap();
    assert_eq!(response["items"][0]["text"], "😀😀");
    assert_eq!(response["items"][1]["text"], "終終");
    assert!(serde_json::to_vec_pretty(&response).unwrap().len() < 16_384);
    assert_eq!(response["stateToken"], assessment.state_token().unwrap());
    assert!(
        assessment
            .read_batch(
                BatchReadRequest {
                    read_token: original,
                    items: vec![]
                },
                &policy,
                &discussion
            )
            .is_err()
    );
}

#[test]
fn empty_documents_deliver_bindings_inline() {
    let mut assessment = fixture("", 0);
    let unit = assessment.units(0, 100).unwrap()["items"][0].clone();
    let reference = unit["reference"].as_str().unwrap();
    let binding = unit["bindingReference"].as_str().unwrap();
    read(&mut assessment, "policy", "", "");
    assert!(
        assessment
            .note("discussion", note("discussion"), None, "", "")
            .is_err()
    );
    let page = assessment.read(reference, 0, 1, "", "").unwrap();
    assert_eq!(page["unit"]["bindingReference"], binding);
    assert!(page["unit"]["bindings"].is_array());
    assert_eq!(
        assessment.progress_with_evidence("", "").unwrap()["deliveredRequiredDocumentCount"],
        2
    );
    let binding_page = assessment.read(binding, 0, usize::MAX, "", "").unwrap();
    let binding_content: Value =
        serde_json::from_str(binding_page["text"].as_str().unwrap()).unwrap();
    assert_eq!(page["unit"]["bindings"], binding_content);
    assessment
        .note("discussion", note("discussion"), None, "", "")
        .unwrap();
}

#[test]
fn malformed_batches_and_zero_limits_leave_state_unchanged() {
    let mut assessment = fixture("discussion", 1);
    let original = assessment.state_token().unwrap();
    assert!(
        assessment
            .read_batch(request_read(&assessment, vec![]), "policy", "discussion")
            .is_err()
    );
    assert!(
        assessment
            .read_batch(
                request_read(&assessment, vec![read_item("policy", 0)]),
                "policy",
                "discussion"
            )
            .is_err()
    );
    assert!(
        assessment
            .read("policy", 0, 0, "policy", "discussion")
            .is_err()
    );
    let id = assessment.inventory.changes[0].id.clone();
    assert!(
        assessment
            .source_context(
                ContextRequest {
                    change_id: id,
                    side: SourceSide::New,
                    start_line: NonZeroUsize::new(1).unwrap(),
                    line_count: NonZeroUsize::new(1).unwrap(),
                },
                0,
                "policy",
                "discussion"
            )
            .is_err()
    );
    assert!(
        assessment
            .record_batch(
                BatchJudgmentRequest {
                    state_token: original.clone(),
                    items: vec![],
                },
                "policy",
                "discussion"
            )
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), original);
}

#[test]
fn unit_pages_inline_bindings_with_unicode_and_contiguous_offsets() {
    let discussion = "a😀終\r\n".repeat(40);
    let mut assessment = fixture(&discussion, 0);
    let unit = assessment.units(0, 1).unwrap()["items"][0].clone();
    let reference = unit["reference"].as_str().unwrap();
    let binding = unit["bindingReference"].as_str().unwrap();
    let mut offset = 0;
    let mut content = String::new();
    loop {
        let response = assessment
            .read_batch(
                request_read(
                    &assessment,
                    vec![BatchReadItem {
                        reference: reference.into(),
                        offset,
                        limit: 7,
                        content_digest: None,
                    }],
                ),
                "policy",
                &discussion,
            )
            .unwrap();
        assert!(serde_json::to_vec_pretty(&response).unwrap().len() < 16_384);
        let page = &response["items"][0];
        assert_eq!(response["snapshotDigest"], "snapshot");
        assert!(page.get("snapshotDigest").is_none());
        assert_eq!(page["unit"]["bindingReference"], binding);
        assert!(page["unit"]["bindings"].is_array());
        let text = page["text"].as_str().unwrap();
        content.push_str(text);
        offset += text.chars().count();
        if page["complete"] == true {
            break;
        }
        assert_eq!(
            assessment
                .progress_with_evidence("policy", &discussion)
                .unwrap()["deliveredRequiredDocumentCount"],
            0
        );
        assert_eq!(page["nextOffset"], offset);
    }
    assert_eq!(content, discussion);
    assert_eq!(
        assessment
            .progress_with_evidence("policy", &discussion)
            .unwrap()["deliveredRequiredDocumentCount"],
        1
    );
}

#[test]
fn oversized_binding_uses_continuation_without_false_receipts() {
    let mut serialized = serde_json::to_value(fixture("x", 0)).unwrap();
    // The current partitioner bounds binding metadata; inject a future oversized
    // record to exercise continuation even though manifest validation rejects it.
    serialized["checkpoints"]["units"][0]["bindings"] = json!([{"label": "😀".repeat(10_000)}]);
    let mut assessment: Assessment = serde_json::from_value(serialized).unwrap();
    assert!(
        assessment
            .validate("snapshot", "policy", "policy", "x")
            .is_err()
    );
    let unit = assessment.units(0, 1).unwrap()["items"][0].clone();
    let reference = unit["reference"].as_str().unwrap();
    let binding = unit["bindingReference"].as_str().unwrap();
    let page = assessment.read(reference, 0, 1, "policy", "x").unwrap();
    assert_eq!(page["text"], "x");
    assert!(page["unit"].get("bindings").is_none());
    assert_eq!(page["unit"]["bindingReference"], binding);
    assert_eq!(
        assessment.progress_with_evidence("policy", "x").unwrap()["deliveredRequiredDocumentCount"],
        0
    );
    assert!(
        serde_json::to_value(&assessment).unwrap()["reads"]
            .get(binding)
            .is_none()
    );
    let page = assessment.read(binding, 0, 1, "policy", "x").unwrap();
    assert_eq!(page["complete"], false);
    assert_eq!(
        assessment.progress_with_evidence("policy", "x").unwrap()["deliveredRequiredDocumentCount"],
        0
    );
    let mut offset = 1;
    loop {
        let page = assessment
            .read(binding, offset, usize::MAX, "policy", "x")
            .unwrap();
        assert!(serde_json::to_vec_pretty(&page).unwrap().len() < 16_384);
        if page["complete"] == true {
            break;
        }
        let next = usize::try_from(page["nextOffset"].as_u64().unwrap()).unwrap();
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(
        assessment.progress_with_evidence("policy", "x").unwrap()["deliveredRequiredDocumentCount"],
        1
    );
}

#[test]
fn batch_envelope_can_fall_back_to_binding_continuation() {
    let mut serialized = serde_json::to_value(fixture("x", 0)).unwrap();
    // Inject metadata near the response boundary to cover the batch envelope.
    serialized["checkpoints"]["units"][0]["bindings"] = json!([{"label": "a".repeat(15_000)}]);
    let mut probe: Assessment = serde_json::from_value(serialized.clone()).unwrap();
    let unit = probe.units(0, 1).unwrap()["items"][0].clone();
    let reference = unit["reference"].as_str().unwrap();
    let binding = unit["bindingReference"].as_str().unwrap();
    let page = probe.read(reference, 0, 1, "policy", "x").unwrap();
    assert!(page["unit"]["bindings"].is_array());
    let baseline = serde_json::to_vec_pretty(&page).unwrap().len();
    assert!(baseline < 16_380);
    serialized["checkpoints"]["units"][0]["bindings"] =
        json!([{"label": "a".repeat(15_000 + 16_380 - baseline)}]);
    let mut planned: Assessment = serde_json::from_value(serialized.clone()).unwrap();
    let token = planned.read_token().unwrap();
    let original_state = planned.state_token().unwrap();
    let plan = planned
        .unit_read_plan("discussion", 0, 100, "policy", "x")
        .unwrap();
    assert_eq!(planned.state_token().unwrap(), original_state);
    assert_eq!(plan["items"], json!([{"after": 0, "count": 1}]));
    let planned_response = planned
        .read_units("discussion", 0, 1, token, "policy", "x")
        .unwrap();
    let mut assessment: Assessment = serde_json::from_value(serialized).unwrap();
    let response = assessment
        .read_batch(
            request_read(&assessment, vec![read_item(reference, 1)]),
            "policy",
            "x",
        )
        .unwrap();
    assert_eq!(planned_response, response);
    assert!(response["items"][0]["unit"].get("bindings").is_none());
    assert_eq!(response["items"][0]["unit"]["bindingReference"], binding);
    assert!(serde_json::to_vec_pretty(&response).unwrap().len() < 16_384);
    assert_eq!(
        assessment.progress_with_evidence("policy", "x").unwrap()["deliveredRequiredDocumentCount"],
        0
    );
    assert!(
        serde_json::to_value(&assessment).unwrap()["reads"]
            .get(binding)
            .is_none()
    );
}

#[test]
fn oversized_batch_does_not_commit_inline_binding_receipts() {
    let discussion = "😀".repeat(4096);
    let policy = "p".repeat(6000);
    let mut assessment = fixture(&discussion, 0);
    let unit = assessment.units(0, 1).unwrap()["items"][0].clone();
    let reference = unit["reference"].as_str().unwrap();
    let binding = unit["bindingReference"].as_str().unwrap();
    let token = assessment.state_token().unwrap();
    let request = request_read(
        &assessment,
        vec![read_item(reference, 4096), read_item("policy", 6000)],
    );
    assert!(
        assessment
            .read_batch(request, &policy, &discussion)
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), token);
    assert!(
        serde_json::to_value(&assessment).unwrap()["reads"]
            .get(binding)
            .is_none()
    );
    assert_eq!(
        assessment
            .progress_with_evidence(&policy, &discussion)
            .unwrap()["deliveredRequiredDocumentCount"],
        0
    );
}

#[test]
fn judgment_batch_preflights_and_rolls_back_replacements() {
    let mut assessment = fixture("discussion", 2);
    let ids = assessment
        .inventory
        .changes
        .iter()
        .map(|change| change.id.clone())
        .collect::<Vec<_>>();
    read(&mut assessment, "policy", "policy", "discussion");
    for id in &ids {
        read(
            &mut assessment,
            &format!("{id}:patch"),
            "policy",
            "discussion",
        );
    }
    let items = ids
        .iter()
        .map(|id| BatchJudgmentItem {
            id: id.clone(),
            judgment: judgment(id),
            previous_digest: None,
        })
        .collect();
    let request = BatchJudgmentRequest {
        state_token: assessment.state_token().unwrap(),
        items,
    };
    let response = assessment
        .record_batch(request, "policy", "discussion")
        .unwrap();
    assert_eq!(response["stateToken"], assessment.state_token().unwrap());
    let original = assessment.state_token().unwrap();
    let first = response["items"][0]["digest"].as_str().unwrap().to_owned();
    let request = BatchJudgmentRequest {
        state_token: original.clone(),
        items: vec![
            BatchJudgmentItem {
                id: ids[0].clone(),
                judgment: judgment(&ids[0]),
                previous_digest: Some(first),
            },
            BatchJudgmentItem {
                id: ids[1].clone(),
                judgment: judgment(&ids[1]),
                previous_digest: Some("stale".into()),
            },
        ],
    };
    assert!(
        assessment
            .record_batch(request, "policy", "discussion")
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), original);
    let response = assessment.summary(0, 10, "policy", "discussion").unwrap();
    let first = response["changes"][0]["decisionDigest"].as_str().unwrap();
    let second = response["changes"][1]["decisionDigest"].as_str().unwrap();
    let mut invalid = judgment(&ids[1]);
    invalid.rationale.clear();
    let request = BatchJudgmentRequest {
        state_token: original.clone(),
        items: vec![
            BatchJudgmentItem {
                id: ids[0].clone(),
                judgment: judgment(&ids[0]),
                previous_digest: Some(first.into()),
            },
            BatchJudgmentItem {
                id: ids[1].clone(),
                judgment: invalid,
                previous_digest: Some(second.into()),
            },
        ],
    };
    assert!(
        assessment
            .record_batch(request, "policy", "discussion")
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), original);
    let request = BatchJudgmentRequest {
        state_token: original.clone(),
        items: vec![BatchJudgmentItem {
            id: "cumulative".into(),
            judgment: judgment("cumulative"),
            previous_digest: None,
        }],
    };
    assert!(
        assessment
            .record_batch(request, "policy", "discussion")
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), original);
}

#[test]
fn oversized_judgment_batch_rolls_back_every_record() {
    let mut assessment = fixture("", 130);
    let ids = assessment
        .inventory
        .changes
        .iter()
        .map(|change| change.id.clone())
        .collect::<Vec<_>>();
    read(&mut assessment, "policy", "policy", "");
    for id in &ids {
        read(&mut assessment, &format!("{id}:patch"), "policy", "");
    }
    let token = assessment.state_token().unwrap();
    let items = ids
        .iter()
        .map(|id| BatchJudgmentItem {
            id: id.clone(),
            judgment: judgment(id),
            previous_digest: None,
        })
        .collect();
    assert!(
        assessment
            .record_batch(
                BatchJudgmentRequest {
                    state_token: token.clone(),
                    items,
                },
                "policy",
                "",
            )
            .is_err()
    );
    assert_eq!(assessment.state_token().unwrap(), token);
    assert_eq!(assessment.progress()["judgedChangeCount"], 0);
}

#[test]
fn replacement_batch_invalidates_old_checks_before_installing_all_judgments() {
    let mut assessment = fixture("discussion", 2);
    let ids = assessment
        .inventory
        .changes
        .iter()
        .map(|change| change.id.clone())
        .collect::<Vec<_>>();
    read(&mut assessment, "policy", "policy", "discussion");
    read(&mut assessment, "discussion", "policy", "discussion");
    let mut digests = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        read(
            &mut assessment,
            &format!("{id}:patch"),
            "policy",
            "discussion",
        );
        let mut change = judgment(id);
        if index == 0 {
            change.concerns.push("Check this behavior.".into());
        }
        digests.push(
            assessment
                .record(id, change, None, "policy", "discussion")
                .unwrap(),
        );
        read(
            &mut assessment,
            &format!("judgment:{id}"),
            "policy",
            "discussion",
        );
    }
    assessment
        .note(
            "discussion",
            note("discussion"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    read(&mut assessment, "discussion-note", "policy", "discussion");
    let check = assessment.checks(0, 10).unwrap()["items"][0].clone();
    let check_id = check["id"].as_str().unwrap();
    read(
        &mut assessment,
        &format!("check:{check_id}"),
        "policy",
        "discussion",
    );
    assessment
        .resolve(check_id, judgment(&ids[0]), None, "policy", "discussion")
        .unwrap();
    read(
        &mut assessment,
        &format!("resolution:{check_id}"),
        "policy",
        "discussion",
    );
    assessment
        .record(
            "cumulative",
            judgment("cumulative"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let before_retry = serde_json::to_value(&assessment).unwrap();
    let retry = ids
        .iter()
        .zip(&digests)
        .enumerate()
        .map(|(index, (id, digest))| {
            let mut value = judgment(id);
            if index == 0 {
                value.concerns.push("Check this behavior.".into());
            }
            BatchJudgmentItem {
                id: id.clone(),
                judgment: value,
                previous_digest: Some(digest.clone()),
            }
        })
        .collect();
    assessment
        .record_batch(
            BatchJudgmentRequest {
                state_token: assessment.state_token().unwrap(),
                items: retry,
            },
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before_retry);
    assert_eq!(assessment.progress()["cumulativeJudgmentRecorded"], true);
    assert_eq!(
        assessment.checks(0, 10).unwrap()["items"][0]["id"],
        check_id
    );
    assert!(assessment.checks(0, 10).unwrap()["items"][0]["resolutionDigest"].is_string());
    let mut first_unchanged = judgment(&ids[0]);
    first_unchanged.concerns.push("Check this behavior.".into());
    let mut second_changed = judgment(&ids[1]);
    second_changed.rationale = "The independent change was reassessed.".into();
    let mixed = assessment
        .record_batch(
            BatchJudgmentRequest {
                state_token: assessment.state_token().unwrap(),
                items: vec![
                    BatchJudgmentItem {
                        id: ids[0].clone(),
                        judgment: first_unchanged,
                        previous_digest: Some(digests[0].clone()),
                    },
                    BatchJudgmentItem {
                        id: ids[1].clone(),
                        judgment: second_changed,
                        previous_digest: Some(digests[1].clone()),
                    },
                ],
            },
            "policy",
            "discussion",
        )
        .unwrap();
    digests[1] = mixed["items"][1]["digest"].as_str().unwrap().into();
    assert_eq!(assessment.progress()["cumulativeJudgmentRecorded"], false);
    assert_eq!(
        assessment.checks(0, 10).unwrap()["items"][0]["id"],
        check_id
    );
    assert!(assessment.checks(0, 10).unwrap()["items"][0]["resolutionDigest"].is_string());
    assessment
        .record(
            "cumulative",
            judgment("cumulative"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let items = ids
        .iter()
        .zip(digests)
        .map(|(id, digest)| BatchJudgmentItem {
            id: id.clone(),
            judgment: judgment(id),
            previous_digest: Some(digest),
        })
        .collect();
    let response = assessment
        .record_batch(
            BatchJudgmentRequest {
                state_token: assessment.state_token().unwrap(),
                items,
            },
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(response["items"].as_array().unwrap().len(), ids.len());
    assert_eq!(assessment.progress()["judgedChangeCount"], 2);
    assert_eq!(assessment.progress()["cumulativeJudgmentRecorded"], false);
    assert!(
        assessment.checks(0, 10).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        assessment
            .read(
                &format!("resolution:{check_id}"),
                0,
                1,
                "policy",
                "discussion",
            )
            .is_err()
    );
}

#[test]
fn cumulative_binds_explicit_checks_and_resolutions() {
    let mut assessment = fixture("discussion", 1);
    let id = assessment.inventory.changes[0].id.clone();
    read(&mut assessment, "policy", "policy", "discussion");
    read(&mut assessment, "discussion", "policy", "discussion");
    read(
        &mut assessment,
        &format!("{id}:patch"),
        "policy",
        "discussion",
    );
    let mut change = judgment(&id);
    change.concerns.push("Potential changed behavior.".into());
    assessment
        .record(&id, change, None, "policy", "discussion")
        .unwrap();
    read(
        &mut assessment,
        &format!("judgment:{id}"),
        "policy",
        "discussion",
    );
    assessment
        .note(
            "discussion",
            note("discussion"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    read(&mut assessment, "discussion-note", "policy", "discussion");
    assert!(
        assessment
            .record(
                "cumulative",
                judgment("cumulative"),
                None,
                "policy",
                "discussion"
            )
            .is_err()
    );
    let check = assessment.checks(0, 10).unwrap()["items"][0].clone();
    let check_id = check["id"].as_str().unwrap();
    read(
        &mut assessment,
        &format!("check:{check_id}"),
        "policy",
        "discussion",
    );
    assessment
        .resolve(check_id, judgment(&id), None, "policy", "discussion")
        .unwrap();
    read(
        &mut assessment,
        &format!("resolution:{check_id}"),
        "policy",
        "discussion",
    );
    assessment
        .record(
            "cumulative",
            judgment("cumulative"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment.finish("policy", "discussion").unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    let summary = assessment.summary(0, 0, "policy", "discussion").unwrap();
    let progress = &summary["progress"];
    assert_eq!(progress["requiredDocumentCount"], 3);
    assert_eq!(progress["deliveredRequiredDocumentCount"], 3);
    assert_eq!(progress["nextRequiredRead"], Value::Null);
    assert_eq!(progress["judgedChangeCount"], progress["changeCount"]);
    assert_eq!(progress["nextChangeOffset"], Value::Null);
    assert_eq!(progress["discussionObservationRecorded"], true);
    assert_eq!(progress["unresolvedCheckCount"], 0);
    assert_eq!(progress["cumulativeJudgmentRecorded"], true);
    assert_eq!(progress["assessmentComplete"], true);
    for field in [
        "unitCount",
        "deliveredUnitCount",
        "completedUnitCount",
        "nextUnitId",
        "nextUnitOffset",
        "nextUndeliveredUnitId",
        "minimumUnitEvidencePages",
    ] {
        assert!(progress.get(field).is_none(), "{field}");
    }
    let inputs = digest_inputs(&assessment, summary["contract"].as_str().unwrap());
    let expected = format!(
        "{:x}",
        Sha256::digest(format!("[\"ss-ai-assessment/directV1\",{inputs}]").as_bytes())
    );
    assert_eq!(assessment.digest.as_deref(), Some(expected.as_str()));
    let mut downgraded = serde_json::to_value(&assessment).unwrap();
    downgraded.as_object_mut().unwrap().remove("protocol");
    assert!(serde_json::from_value::<Assessment>(downgraded).is_err());
}

#[test]
fn unsupported_protocols_and_per_unit_notes_are_rejected_at_deserialization() {
    let assessment = fixture("discussion", 1);
    let serialized = serde_json::to_value(&assessment).unwrap();
    assert_eq!(serialized["protocol"], "directV1");
    assert!(serialized["checkpoints"].get("notes").is_none());
    for protocol in [Value::Null, json!("legacy"), json!("future")] {
        let mut changed = serialized.clone();
        changed["protocol"] = protocol;
        assert!(serde_json::from_value::<Assessment>(changed).is_err());
    }
    let mut changed = serialized;
    changed["checkpoints"]["notes"] = json!({"unit": {}});
    assert!(serde_json::from_value::<Assessment>(changed).is_err());
}

#[test]
fn cumulative_binds_persisted_semantics_without_requiring_replay() {
    let mut assessment = fixture("discussion", 1);
    let id = assessment.inventory.changes[0].id.clone();
    read(&mut assessment, "policy", "policy", "discussion");
    read(&mut assessment, "discussion", "policy", "discussion");
    read(
        &mut assessment,
        &format!("{id}:patch"),
        "policy",
        "discussion",
    );
    let digest = assessment
        .record(&id, judgment(&id), None, "policy", "discussion")
        .unwrap();
    assessment
        .note(
            "discussion",
            note("discussion"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let mut cumulative = judgment("cumulative");
    cumulative
        .evidence
        .extend([format!("judgment:{id}"), "discussion-note".into()]);
    assessment
        .record("cumulative", cumulative, None, "policy", "discussion")
        .unwrap();
    let serialized = serde_json::to_value(&assessment).unwrap();
    assert!(serialized["reads"].get(format!("judgment:{id}")).is_none());
    assert!(serialized["reads"].get("discussion-note").is_none());
    let aggregate = &serialized["checkpoints"]["aggregates"]["cumulative"];
    assert!(aggregate.get(format!("judgment:{id}")).is_some());
    assert!(aggregate.get("discussion-note").is_some());
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    let mut replacement = judgment(&id);
    replacement.rationale.push_str(" A distinct conclusion.");
    assessment
        .record(&id, replacement, Some(&digest), "policy", "discussion")
        .unwrap();
    assert_eq!(assessment.progress()["cumulativeJudgmentRecorded"], false);
}

#[test]
fn cumulative_binds_unread_resolutions_but_resolution_still_requires_reads() {
    let mut assessment = fixture("discussion", 1);
    let id = assessment.inventory.changes[0].id.clone();
    read(&mut assessment, "policy", "policy", "discussion");
    read(&mut assessment, "discussion", "policy", "discussion");
    read(
        &mut assessment,
        &format!("{id}:patch"),
        "policy",
        "discussion",
    );
    assessment
        .record(&id, judgment(&id), None, "policy", "discussion")
        .unwrap();
    let mut observation = note("discussion");
    observation.concerns.push("Examine this concern.".into());
    assessment
        .note("discussion", observation, None, "policy", "discussion")
        .unwrap();
    let check = assessment.checks(0, 10).unwrap()["items"][0].clone();
    let check_id = check["id"].as_str().unwrap();
    let mut resolution = judgment(&id);
    resolution.evidence = vec!["discussion".into()];
    assert!(
        assessment
            .resolve(check_id, resolution, None, "policy", "discussion")
            .is_err()
    );
    read(&mut assessment, "discussion-note", "policy", "discussion");
    read(
        &mut assessment,
        &format!("check:{check_id}"),
        "policy",
        "discussion",
    );
    let mut resolution = judgment(&id);
    resolution.evidence = vec!["discussion".into()];
    assessment
        .resolve(check_id, resolution, None, "policy", "discussion")
        .unwrap();
    let mut cumulative = judgment("cumulative");
    cumulative.evidence.extend([
        format!("check:{check_id}"),
        format!("resolution:{check_id}"),
    ]);
    assessment
        .record("cumulative", cumulative, None, "policy", "discussion")
        .unwrap();
    let serialized = serde_json::to_value(&assessment).unwrap();
    assert!(
        serialized["reads"]
            .get(format!("resolution:{check_id}"))
            .is_none()
    );
    assert!(
        serialized["checkpoints"]["aggregates"]["cumulative"]
            .get(format!("resolution:{check_id}"))
            .is_some()
    );
    assessment.finish("policy", "discussion").unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

#[test]
fn direct_assessment_needs_no_unit_notes_and_context_invalidates_dependents() {
    let mut assessment = fixture("discussion", 2);
    let ids = assessment
        .inventory
        .changes
        .iter()
        .map(|change| change.id.clone())
        .collect::<Vec<_>>();
    read(&mut assessment, "policy", "policy", "discussion");
    read(&mut assessment, "discussion", "policy", "discussion");
    for id in &ids {
        read(
            &mut assessment,
            &format!("{id}:patch"),
            "policy",
            "discussion",
        );
        assessment
            .record(id, judgment(id), None, "policy", "discussion")
            .unwrap();
        read(
            &mut assessment,
            &format!("judgment:{id}"),
            "policy",
            "discussion",
        );
    }
    assessment
        .note(
            "discussion",
            note("discussion"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    read(&mut assessment, "discussion-note", "policy", "discussion");
    assessment
        .record(
            "cumulative",
            judgment("cumulative"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let page = assessment
        .source_context(
            ContextRequest {
                change_id: ids[0].clone(),
                side: SourceSide::New,
                start_line: NonZeroUsize::new(1).unwrap(),
                line_count: NonZeroUsize::new(1).unwrap(),
            },
            usize::MAX,
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(page["text"], "new\n");
    assert_eq!(assessment.progress()["judgedChangeCount"], 1);
    assert_eq!(assessment.progress()["cumulativeJudgmentRecorded"], false);
    assert!(
        assessment
            .read("discussion-note", 0, 1, "policy", "discussion")
            .is_err()
    );
    assert!(
        assessment
            .read(
                &format!("judgment:{}", ids[1]),
                0,
                1,
                "policy",
                "discussion"
            )
            .is_ok()
    );
    assessment
        .record(&ids[0], judgment(&ids[0]), None, "policy", "discussion")
        .unwrap();
    read(
        &mut assessment,
        &format!("judgment:{}", ids[0]),
        "policy",
        "discussion",
    );
    assessment
        .note(
            "discussion",
            note("discussion"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    read(&mut assessment, "discussion-note", "policy", "discussion");
    assessment
        .record(
            "cumulative",
            judgment("cumulative"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment.finish("policy", "discussion").unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

#[test]
fn direct_handoffs_have_a_shared_fixed_limit() {
    let mut assessment = fixture("discussion", 3);
    let ids = assessment
        .inventory
        .changes
        .iter()
        .map(|change| change.id.clone())
        .collect::<Vec<_>>();
    read(&mut assessment, "policy", "policy", "discussion");
    for id in ids {
        read(
            &mut assessment,
            &format!("{id}:patch"),
            "policy",
            "discussion",
        );
        assessment
            .record(&id, judgment(&id), None, "policy", "discussion")
            .unwrap();
        if assessment.progress()["directHandoffCount"] != 2 {
            assessment.checkpoint().unwrap();
        }
    }
    assert_eq!(assessment.progress()["directHandoffCount"], 2);
    assert_eq!(assessment.progress()["directHandoffLimit"], 2);
    assert!(assessment.checkpoint().is_err());
}

#[test]
fn direct_handoff_requires_new_semantic_work() {
    let mut assessment = fixture("", 0);
    let before = serde_json::to_value(&assessment).unwrap();
    assert!(assessment.checkpoint().is_err());
    assert_eq!(assessment.progress()["completedWorkCount"], 0);
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
    read(&mut assessment, "policy", "policy", "");
    read(&mut assessment, "discussion", "policy", "");
    let after_reads = serde_json::to_value(&assessment).unwrap();
    assert!(assessment.checkpoint().is_err());
    assert_eq!(serde_json::to_value(&assessment).unwrap(), after_reads);
    assessment
        .note("discussion", note("discussion"), None, "policy", "")
        .unwrap();
    assessment.checkpoint().unwrap();
    let after = serde_json::to_value(&assessment).unwrap();
    assert!(assessment.checkpoint().is_err());
    assert_eq!(serde_json::to_value(&assessment).unwrap(), after);
    let serialized = serde_json::to_value(&assessment).unwrap();
    let restored: Assessment = serde_json::from_value(serialized).unwrap();
    restored
        .validate("snapshot", "policy", "policy", "")
        .unwrap();
}

#[test]
fn direct_assessment_rejects_transport_unit_notes_without_mutation() {
    let mut assessment = fixture("discussion", 1);
    let unit = assessment.units(0, 1).unwrap()["items"][0].clone();
    assert!(unit.get("noteReference").is_none());
    assert!(unit.get("noteDigest").is_none());
    assert!(unit.get("completed").is_none());
    let before = serde_json::to_value(&assessment).unwrap();
    assert!(
        assessment
            .note(
                unit["unit"]["id"].as_str().unwrap(),
                note(unit["reference"].as_str().unwrap()),
                None,
                "policy",
                "discussion",
            )
            .is_err()
    );
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
}
