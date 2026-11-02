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
use std::num::NonZeroUsize;
use std::panic::{RefUnwindSafe, UnwindSafe};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai_assessment::{
    Assessment, BatchJudgmentItem, BatchJudgmentRequest, BatchReadItem, BatchReadRequest, Change,
    Content, ContextRequest, Disposition, Inventory, Judgment, Note, Side, SourceSide, change_id,
};
use static_assertions::assert_impl_all;

assert_impl_all!(BatchReadItem: UnwindSafe, RefUnwindSafe);
assert_impl_all!(BatchReadRequest: UnwindSafe, RefUnwindSafe);
assert_impl_all!(BatchJudgmentItem: UnwindSafe, RefUnwindSafe);
assert_impl_all!(BatchJudgmentRequest: UnwindSafe, RefUnwindSafe);

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
fn summary_exposes_the_current_state_token_within_the_response_budget() {
    let mut assessment = fixture("discussion", 1);
    let first = assessment.summary(0, 100, "policy", "discussion").unwrap();
    assert_eq!(first["stateToken"], assessment.state_token().unwrap());
    assert_eq!(first["readToken"], assessment.read_token().unwrap());
    assert!(serde_json::to_vec_pretty(&first).unwrap().len() < 16_384);
    read(&mut assessment, "policy", "policy", "discussion");
    let next = assessment.summary(0, 100, "policy", "discussion").unwrap();
    assert_ne!(first["stateToken"], next["stateToken"]);
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
        let progress = &response["progress"];
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
    assert_ne!(first["stateToken"], initial_state);
    let second = assessment
        .read_batch(batch("discussion"), "policy", "discussion")
        .unwrap();
    assert_ne!(first["stateToken"], second["stateToken"]);
    assert_eq!(first["readToken"], read_token);
    assert_eq!(second["items"][0]["readToken"], read_token);
    assessment
        .read_batch(batch(&format!("{id}:patch")), "policy", "discussion")
        .unwrap();
    assert_eq!(assessment.read_token().unwrap(), read_token);
    let stale_judgment = BatchJudgmentRequest {
        state_token: initial_state,
        items: vec![BatchJudgmentItem {
            id: id.clone(),
            judgment: judgment(&id),
            previous_digest: None,
        }],
    };
    assert!(
        assessment
            .record_batch(stale_judgment, "policy", "discussion")
            .is_err()
    );
    assessment
        .record(&id, judgment(&id), None, "policy", "discussion")
        .unwrap();
    assert_eq!(assessment.read_token().unwrap(), read_token);
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
    assert_eq!(assessment.state_token().unwrap(), state);
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
    fields.extend(["decisions", "cumulative"].into_iter().map(field));
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
        assert_eq!(page["snapshotDigest"], "snapshot");
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
    let mut assessment: Assessment = serde_json::from_value(serialized).unwrap();
    let response = assessment
        .read_batch(
            request_read(&assessment, vec![read_item(reference, 1)]),
            "policy",
            "x",
        )
        .unwrap();
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
