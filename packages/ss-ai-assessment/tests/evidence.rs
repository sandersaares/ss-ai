//! Evidence delivery limits and semantic completion remain independent of comparison size.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Tests use bounded fixture arrays and counters"
)]

use serde_json::{Value, json};
use ss_ai_assessment::{
    Assessment, AssessmentError, BatchJudgmentItem, BatchJudgmentRequest, BatchReadItem,
    BatchReadRequest, Change, Content, ContextRequest, Disposition, Inventory, Judgment, Note,
    Side, SourceSide, change_id,
};
use std::num::NonZeroUsize;

/// Full replacement patches isolate coverage tests from the Git producer.
fn assessment(inventory: Inventory, discussion: &str) -> Assessment {
    let patches = inventory
        .changes
        .iter()
        .map(|change| {
            let old = side_text(change.old.as_ref());
            let new = side_text(change.new.as_ref());
            if old == new {
                return String::new();
            }
            let old_count = old.split_inclusive('\n').count();
            let new_count = new.split_inclusive('\n').count();
            let mut patch = format!(
                "@@ -{},{old_count} +{},{new_count} @@\n",
                usize::from(old_count > 0),
                usize::from(new_count > 0)
            );
            for (prefix, text) in [('-', old), ('+', new)] {
                for line in text.split_inclusive('\n') {
                    patch.push(prefix);
                    patch.push_str(line);
                    if !line.ends_with('\n') {
                        patch.push_str("\n\\ No newline at end of file\n");
                    }
                }
            }
            patch
        })
        .collect::<Vec<_>>();
    Assessment::new_direct(inventory, &patches, "snapshot", "policy", discussion).unwrap()
}

fn side_text(side: Option<&Side>) -> &str {
    match side.map(|side| &side.content) {
        Some(Content::Text { text }) => text,
        _ => "",
    }
}

/// Required operation documents are exact native serialization, not reconstructed receipts.
fn read_patch(assessment: &mut Assessment, id: &str, policy: &str, discussion: &str) {
    let first = assessment
        .read(&format!("{id}:patch"), 0, usize::MAX, policy, discussion)
        .unwrap();
    let total = usize::try_from(first["totalCharacters"].as_u64().unwrap()).unwrap();
    let mut offset = first["text"].as_str().unwrap().chars().count();
    assert_bounded(&first);
    while offset < total {
        let page = assessment
            .read(
                &format!("{id}:patch"),
                offset,
                usize::MAX,
                policy,
                discussion,
            )
            .unwrap();
        assert_bounded(&page);
        let count = page["text"].as_str().unwrap().chars().count();
        assert!(count > 0);
        offset += count;
        assert!(offset <= total);
    }
}

/// Matches the public delivered-response contract, including pretty formatting and newline.
fn assert_bounded(value: &Value) {
    assert!(serde_json::to_vec_pretty(value).unwrap().len() < 16_384);
}

/// Ordinary immutable endpoints let each scenario vary only evidence content and coverage.
fn inventory(changes: Vec<Change>) -> Inventory {
    Inventory {
        old_commit: "a".repeat(40),
        new_commit: "b".repeat(40),
        old_tree: "c".repeat(40),
        new_tree: if changes.is_empty() { "c" } else { "d" }.repeat(40),
        changes,
        complete: true,
    }
}

/// Both sides remain explicit even when their content is unreadable.
fn change(path: &str, old: Content, new: Content) -> Change {
    let old = Some(Side {
        path: path.to_owned(),
        object: "a".repeat(40),
        mode: "100644".to_owned(),
        content: old,
    });
    let new = Some(Side {
        path: path.to_owned(),
        object: "b".repeat(40),
        mode: "100644".to_owned(),
        content: new,
    });
    Change {
        id: change_id("M", old.as_ref(), new.as_ref()).unwrap(),
        kind: "M".to_owned(),
        old,
        new,
    }
}

/// Uses representative policy-grounded rationale without importing another worker's conclusions.
fn judgment(id: &str, disposition: Disposition) -> Judgment {
    Judgment {
        disposition,
        concerns: Vec::new(),
        interactions: Vec::new(),
        rationale:
            "Examined the exact evidence against the design, performance and correctness criteria."
                .to_owned(),
        evidence: if id == "cumulative" {
            vec!["policy".to_owned(), "discussion".to_owned()]
        } else {
            vec![format!("{id}:patch")]
        },
    }
}

/// Discussion observations carry semantic input to the cumulative judgment.
fn discussion_note() -> Note {
    Note {
        observations: "Examined complete fixture evidence.".into(),
        interfaces: "Fixture invariants are preserved.".into(),
        concerns: Vec::new(),
        interactions: Vec::new(),
        evidence: vec!["discussion".into()],
    }
}

fn deliver(
    assessment: &mut Assessment,
    reference: &str,
    policy: &str,
    discussion: &str,
) -> Result<(), AssessmentError> {
    let mut offset = 0;
    loop {
        let page = assessment.read(reference, offset, usize::MAX, policy, discussion)?;
        let Some(next) = page["nextOffset"].as_u64() else {
            return Ok(());
        };
        let next = usize::try_from(next).unwrap();
        assert!(next > offset);
        offset = next;
    }
}
/// Follows native offsets, bounding progress by captured length rather than a watchdog.
fn read_all(
    assessment: &mut Assessment,
    reference: &str,
    expected: &str,
    policy: &str,
    discussion: &str,
) -> usize {
    let mut offset = 0;
    let mut received = String::new();
    let mut pages = 0;
    loop {
        let page = assessment
            .read(reference, offset, usize::MAX, policy, discussion)
            .unwrap();
        pages += 1;
        assert_bounded(&page);
        assert_eq!(page["offset"], offset);
        assert_eq!(page["totalCharacters"], expected.chars().count());
        let text = page["text"].as_str().unwrap();
        received.push_str(text);
        if page["complete"] == true {
            assert!(page["nextOffset"].is_null());
            break;
        }
        let next = usize::try_from(page["nextOffset"].as_u64().unwrap()).unwrap();
        assert!(next > offset && next <= expected.chars().count());
        assert_eq!(next, offset + text.chars().count());
        offset = next;
    }
    assert_eq!(received, expected);
    pages
}

/// Reassembles the operation document and checks its captured patch and binding.
fn read_source(assessment: &mut Assessment, id: &str, patch: &str, discussion: &str) -> usize {
    let reference = format!("{id}:patch");
    let mut offset = 0;
    let mut received = String::new();
    let mut pages = 0;
    loop {
        let page = assessment
            .read(&reference, offset, usize::MAX, "policy", discussion)
            .unwrap();
        pages += 1;
        assert_bounded(&page);
        assert_eq!(page["offset"], offset);
        let text = page["text"].as_str().unwrap();
        received.push_str(text);
        offset += text.chars().count();
        if page["complete"] == true {
            assert!(page["nextOffset"].is_null());
            assert_eq!(page["totalCharacters"], offset);
            break;
        }
        assert!(!text.is_empty());
        assert_eq!(page["nextOffset"], offset);
    }
    let document: Value = serde_json::from_str(&received).unwrap();
    assert_eq!(document["changeId"], id);
    assert_eq!(document["snapshotDigest"], "snapshot");
    assert_eq!(document["patch"], patch);
    pages
}

#[test]
fn escaped_and_unicode_evidence_is_delivered_exactly_with_a_serialized_byte_bound() {
    // Escaping expands control characters; multibyte text distinguishes byte and character offsets.
    let text = "\0\u{1}\n\t\"\\\u{1f600}".repeat(8192);
    let mut assessment = assessment(inventory(Vec::new()), "");
    assessment.read("policy", 1, 100, &text, "").unwrap_err();
    read_all(&mut assessment, "policy", &text, &text, "");
    // Replayed reads must not reduce retained coverage.
    let replay = assessment.read("policy", 0, 1, &text, "").unwrap();
    assert_eq!(replay["text"], "\0");
    let summary = assessment.summary(0, 10, &text, "").unwrap();
    assert_eq!(summary["documentReads"][0]["complete"], true);
    assert_eq!(summary["documentReads"][1]["complete"], false);
}

#[test]
fn context_pages_fit_long_escaped_bindings_and_preserve_required_delivery() {
    // Escaped path metadata exceeds the ordinary text-page reserve while remaining
    // a valid captured Git path. Escaped text and Unicode exercise byte sizing.
    let path = "\u{1}nested/".repeat(450) + "file";
    let text = "\u{1}\u{1f916}\t".repeat(4096);
    let change = change(
        &path,
        Content::Text { text: text.clone() },
        Content::Text {
            text: format!("{text}changed"),
        },
    );
    let id = change.id.clone();
    let mut assessment = assessment(inventory(vec![change]), "discussion");
    read_all(&mut assessment, "policy", "policy", "policy", "discussion");
    read_patch(&mut assessment, &id, "policy", "discussion");
    let request = ContextRequest {
        change_id: id.clone(),
        side: SourceSide::Old,
        start_line: NonZeroUsize::MIN,
        line_count: NonZeroUsize::MIN,
    };
    let first = assessment
        .source_context(request.clone(), usize::MAX, "policy", "discussion")
        .unwrap();
    assert_bounded(&first);
    assert_eq!(first["context"]["source"]["path"], path);
    assert!(!first["text"].as_str().unwrap().is_empty());
    assert_eq!(first["complete"], false);
    let reference = first["reference"].as_str().unwrap();
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Significant),
            None,
            "policy",
            "discussion",
        )
        .unwrap_err();
    let retry = assessment
        .source_context(request, 1, "policy", "discussion")
        .unwrap();
    assert_eq!(retry["reference"], reference);
    assert_eq!(retry["text"], "\u{1}");
    read_all(&mut assessment, reference, &text, "policy", "discussion");
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Significant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

#[test]
fn context_binding_that_cannot_fit_does_not_record_evidence_delivery() {
    let change = change(
        &("nested/".repeat(3_000) + "file"),
        Content::Text { text: "old".into() },
        Content::Text { text: "new".into() },
    );
    let id = change.id.clone();
    let mut assessment = assessment(inventory(vec![change]), "discussion");
    let request = ContextRequest {
        change_id: id,
        side: SourceSide::Old,
        start_line: NonZeroUsize::MIN,
        line_count: NonZeroUsize::MIN,
    };
    assessment
        .source_context(request, usize::MAX, "policy", "discussion")
        .unwrap_err();
    let saved = serde_json::to_value(&assessment).unwrap();
    assert!(saved["reads"].as_object().unwrap().is_empty());
    assert_eq!(saved["contexts"].as_object().unwrap().len(), 1);
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

#[test]
fn failed_first_context_page_retains_required_work_and_invalidates_prior_judgments() {
    let change = change(
        &("nested/".repeat(3_000) + "file"),
        Content::Text { text: "old".into() },
        Content::Text { text: "new".into() },
    );
    let id = change.id.clone();
    let mut assessment = assessment(inventory(vec![change]), "discussion");
    for reference in ["policy", "discussion", &format!("{id}:patch")] {
        deliver(&mut assessment, reference, "policy", "discussion").unwrap();
    }
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .note(
            "discussion",
            discussion_note(),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .source_context(
            ContextRequest {
                change_id: id.clone(),
                side: SourceSide::Old,
                start_line: NonZeroUsize::MIN,
                line_count: NonZeroUsize::MIN,
            },
            usize::MAX,
            "policy",
            "discussion",
        )
        .unwrap_err();
    let saved = serde_json::to_value(&assessment).unwrap();
    assert_eq!(saved["contexts"].as_object().unwrap().len(), 1);
    assert!(saved["decisions"].get(&id).is_none());
    assert!(saved["cumulative"].is_null());
    let mut reloaded: Assessment = serde_json::from_value(saved).unwrap();
    reloaded
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    reloaded.finish("policy", "discussion").unwrap_err();
}

#[test]
fn unread_source_cannot_be_bulk_judged_uncertain_or_completed_as_a_refusal() {
    let change = change(
        "source.rs",
        Content::Text { text: "old".into() },
        Content::Text { text: "new".into() },
    );
    let id = change.id.clone();
    for disposition in [
        Disposition::Insignificant,
        Disposition::Significant,
        Disposition::Uncertain,
    ] {
        let mut assessment = assessment(inventory(vec![change_copy(&change)]), "discussion");
        read_all(&mut assessment, "policy", "policy", "policy", "discussion");
        assessment
            .record(
                &id,
                judgment(&id, disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap_err();
        read_all(
            &mut assessment,
            &format!("{id}:old"),
            "old",
            "policy",
            "discussion",
        );
        assessment
            .record(
                &id,
                judgment(&id, disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap_err();
        read_all(
            &mut assessment,
            &format!("{id}:new"),
            "new",
            "policy",
            "discussion",
        );
        assessment
            .record(
                &id,
                judgment(&id, disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap_err();
        read_patch(&mut assessment, &id, "policy", "discussion");
        assessment
            .record(
                &id,
                judgment(&id, disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap();
        assessment
            .record(
                "cumulative",
                judgment("cumulative", disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap_err();
        assessment.finish("policy", "discussion").unwrap_err();
        assert_eq!(assessment.status(false), "assessmentRequired");
        assert!(!assessment.needs_review());
        assert!(assessment.digest.is_none());
        read_all(
            &mut assessment,
            "discussion",
            "discussion",
            "policy",
            "discussion",
        );
        assessment
            .note(
                "discussion",
                discussion_note(),
                None,
                "policy",
                "discussion",
            )
            .unwrap();
        assessment
            .record(
                "cumulative",
                judgment("cumulative", disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap();
        assessment.finish("policy", "discussion").unwrap();
        assert_eq!(
            assessment.needs_review(),
            disposition != Disposition::Insignificant
        );
    }
}

/// Round-tripping tests the real durable record rather than a test-only clone implementation.
fn change_copy(change: &Change) -> Change {
    serde_json::from_value(serde_json::to_value(change).unwrap()).unwrap()
}

#[test]
fn opaque_source_still_requires_semantic_uncertainty_without_reading_nonexistent_text() {
    let change = change("binary", Content::Binary { size: 12 }, Content::Submodule);
    // A submodule content classification belongs to its Git mode.
    let mut change = change;
    change.new.as_mut().unwrap().mode = "160000".into();
    change.kind = "T".into();
    change.id = change_id("T", change.old.as_ref(), change.new.as_ref()).unwrap();
    let id = change.id.clone();
    let mut assessment = assessment(inventory(vec![change]), "");
    read_all(&mut assessment, "policy", "", "", "");
    read_all(&mut assessment, "discussion", "", "", "");
    read_patch(&mut assessment, &id, "", "");
    assessment
        .note("discussion", discussion_note(), None, "", "")
        .unwrap();
    assessment
        .record(&id, judgment(&id, Disposition::Insignificant), None, "", "")
        .unwrap_err();
    assessment
        .record(&id, judgment(&id, Disposition::Uncertain), None, "", "")
        .unwrap();
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "",
            "",
        )
        .unwrap();
    assessment.finish("", "").unwrap();
    assert_eq!(assessment.status(false), "failed");
    assert!(assessment.needs_review());
}

/// Checks that bounded inventory pages deliver every change in order.
fn assert_inventory_pages(assessment: &Assessment, ids: &[String], discussion: &str) {
    let mut after = 0;
    let mut calls = 0_usize;
    loop {
        let page = assessment
            .summary(after, 100, "policy", discussion)
            .unwrap();
        calls += 1;
        assert_bounded(&page);
        assert_eq!(page["pendingChangeCount"], ids.len());
        assert_eq!(page["nextChangeOffset"], 0);
        let changes = page["changes"].as_array().unwrap();
        assert!(!changes.is_empty());
        for (index, change) in changes.iter().enumerate() {
            assert_eq!(change["id"], ids[after + index]);
        }
        let end = after + changes.len();
        if page["nextOffset"].is_null() {
            assert_eq!(end, ids.len());
            break;
        }
        assert_eq!(page["nextOffset"], end);
        assert!(end > after && end < ids.len());
        after = end;
    }
    // Require fewer pages than half the path count, rounding down.
    assert!(
        calls.saturating_mul(2) < ids.len().saturating_sub(1),
        "{calls} summary pages"
    );
}

#[test]
fn large_exact_comparison_can_be_examined_incrementally_without_a_whole_inventory_response() {
    // The successful replay models the smaller incident shape, with full-size changes
    // at both ends and independent evidence and judgment for every other change.
    const PATHS: usize = 151;
    const BATCH: usize = 10;
    let old = "old source\n".repeat(819);
    let new = "new source\n".repeat(819);
    let discussion = "discussion\n".repeat(27_910);
    let changes = (0..PATHS)
        .map(|index| {
            let large = index == 0 || index == PATHS - 1;
            change(
                &format!("source-{index}.rs"),
                Content::Text {
                    text: if large { old.clone() } else { "old\n".into() },
                },
                Content::Text {
                    text: if large { new.clone() } else { "new\n".into() },
                },
            )
        })
        .collect();
    let mut assessment = assessment(inventory(changes), &discussion);
    let ids = assessment
        .inventory
        .changes
        .iter()
        .map(|change| change.id.clone())
        .collect::<Vec<_>>();
    read_all(&mut assessment, "policy", "policy", "policy", &discussion);
    let discussion_pages = read_all(
        &mut assessment,
        "discussion",
        &discussion,
        "policy",
        &discussion,
    );
    assert!(discussion_pages > 1 && discussion_pages < 100);
    assert_inventory_pages(&assessment, &ids, &discussion);

    let large_patch = format!(
        "@@ -1,819 +1,819 @@\n{}{}",
        "-old source\n".repeat(819),
        "+new source\n".repeat(819)
    );
    for id in [&ids[0], &ids[PATHS - 1]] {
        let pages = read_source(&mut assessment, id, &large_patch, &discussion);
        assert!(pages > 1 && pages < 10);
    }

    let token = assessment.read_token().unwrap();
    let before = assessment.state_token().unwrap();
    assessment
        .read_batch(
            BatchReadRequest {
                read_token: token.clone(),
                items: vec![
                    BatchReadItem {
                        reference: format!("{}:patch", ids[1]),
                        offset: 0,
                        limit: usize::MAX,
                        content_digest: None,
                    },
                    BatchReadItem {
                        reference: "missing:patch".into(),
                        offset: 0,
                        limit: usize::MAX,
                        content_digest: None,
                    },
                ],
            },
            "policy",
            &discussion,
        )
        .unwrap_err();
    assert_eq!(assessment.state_token().unwrap(), before);
    let mut read_calls = 0;
    for group in ids[1..PATHS - 1].chunks(BATCH) {
        let response = assessment
            .read_batch(
                BatchReadRequest {
                    read_token: token.clone(),
                    items: group
                        .iter()
                        .map(|id| BatchReadItem {
                            reference: format!("{id}:patch"),
                            offset: 0,
                            limit: usize::MAX,
                            content_digest: None,
                        })
                        .collect(),
                },
                "policy",
                &discussion,
            )
            .unwrap();
        read_calls += 1;
        assert_bounded(&response);
        let pages = response["items"].as_array().unwrap();
        assert_eq!(pages.len(), group.len());
        for (page, id) in pages.iter().zip(group) {
            assert_eq!(page["reference"], format!("{id}:patch"));
            let document: Value = serde_json::from_str(page["text"].as_str().unwrap()).unwrap();
            assert_eq!(document["changeId"], *id);
            assert_eq!(document["snapshotDigest"], "snapshot");
            assert_eq!(document["patch"], "@@ -1,1 +1,1 @@\n-old\n+new\n");
            assert_eq!(page["complete"], true);
        }
    }
    assert_eq!(read_calls, (PATHS - 2).div_ceil(BATCH));

    // A resumed worker must retain all delivery receipts before recording judgments.
    assessment = serde_json::from_slice(&serde_json::to_vec(&assessment).unwrap()).unwrap();
    assessment
        .validate("snapshot", "policy", "policy", &discussion)
        .unwrap();
    let before = assessment.state_token().unwrap();
    let mut invalid = judgment(&ids[1], Disposition::Insignificant);
    invalid.rationale.clear();
    assessment
        .record_batch(
            BatchJudgmentRequest {
                state_token: before.clone(),
                items: vec![
                    BatchJudgmentItem {
                        id: ids[0].clone(),
                        judgment: judgment(&ids[0], Disposition::Insignificant),
                        previous_digest: None,
                    },
                    BatchJudgmentItem {
                        id: ids[1].clone(),
                        judgment: invalid,
                        previous_digest: None,
                    },
                ],
            },
            "policy",
            &discussion,
        )
        .unwrap_err();
    assert_eq!(assessment.state_token().unwrap(), before);
    assert_eq!(assessment.progress()["judgedChangeCount"], 0);
    let mut judged = 0;
    let mut judgment_calls = 0;
    for group in ids.chunks(BATCH) {
        let response = assessment
            .record_batch(
                BatchJudgmentRequest {
                    state_token: assessment.state_token().unwrap(),
                    items: group
                        .iter()
                        .map(|id| BatchJudgmentItem {
                            id: id.clone(),
                            judgment: judgment(id, Disposition::Insignificant),
                            previous_digest: None,
                        })
                        .collect(),
                },
                "policy",
                &discussion,
            )
            .unwrap();
        judgment_calls += 1;
        assert_bounded(&response);
        assert_eq!(response["items"].as_array().unwrap().len(), group.len());
        judged += group.len();
        assert_eq!(assessment.progress()["judgedChangeCount"], judged);
        assert_eq!(
            assessment.progress()["nextChangeOffset"],
            json!((judged < PATHS).then_some(judged))
        );
    }
    assert_eq!(judgment_calls, PATHS.div_ceil(BATCH));
    assessment
        .note("discussion", discussion_note(), None, "policy", &discussion)
        .unwrap();
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            &discussion,
        )
        .unwrap();
    assessment.finish("policy", &discussion).unwrap();
    assessment
        .validate("snapshot", "policy", "policy", &discussion)
        .unwrap();
    assert_eq!(assessment.status(false), "readyToReapprove");
    let replay: Assessment =
        serde_json::from_slice(&serde_json::to_vec(&assessment).unwrap()).unwrap();
    replay
        .validate("snapshot", "policy", "policy", &discussion)
        .unwrap();
    assert_eq!(replay.digest, assessment.digest);
}

#[test]
fn incident_scale_inventory_pages_without_skipping_changes_or_completing_early() {
    // The larger incident exercises inventory pagination independently of the
    // successful replay, avoiding repeated work across hundreds of full judgments.
    const PATHS: usize = 923;
    let discussion = "discussion\n".repeat(74_603);
    let changes = (0..PATHS)
        .map(|index| {
            change(
                &format!("source-{index}.rs"),
                Content::Text {
                    text: "old\n".into(),
                },
                Content::Text {
                    text: "new\n".into(),
                },
            )
        })
        .collect();
    let mut assessment = assessment(inventory(changes), &discussion);
    let ids = assessment
        .inventory
        .changes
        .iter()
        .map(|change| change.id.clone())
        .collect::<Vec<_>>();
    read_all(&mut assessment, "policy", "policy", "policy", &discussion);
    let pages = read_all(
        &mut assessment,
        "discussion",
        &discussion,
        "policy",
        &discussion,
    );
    assert!(pages > 1 && pages < 100);
    assert_inventory_pages(&assessment, &ids, &discussion);
    assessment = serde_json::from_slice(&serde_json::to_vec(&assessment).unwrap()).unwrap();
    assessment
        .validate("snapshot", "policy", "policy", &discussion)
        .unwrap();
    assert_eq!(assessment.progress()["judgedChangeCount"], 0);
    assessment.finish("policy", &discussion).unwrap_err();
}

#[test]
fn metadata_pages_shrink_without_skipping_judgments_or_repeating_an_unbounded_cumulative() {
    let changes = (0..30)
        .map(|index| {
            change(
                &format!("{}-{index}", "long-path".repeat(100)),
                Content::Text {
                    text: String::new(),
                },
                Content::Text {
                    text: String::new(),
                },
            )
        })
        .collect();
    let assessment = assessment(inventory(changes), "");
    let page = assessment.summary(0, 100, "", "").unwrap();
    assert_bounded(&page);
    let count = page["changes"].as_array().unwrap().len();
    assert!(count > 0 && count < 30);
    assert_eq!(page["nextOffset"], count);
    assert_eq!(
        page["changes"][0]["old"]["content"]["progress"]["readOffset"],
        json!(null)
    );
    let next = assessment.summary(count, 100, "", "").unwrap();
    assert_ne!(page["changes"][0]["id"], next["changes"][0]["id"]);
}

#[test]
fn discussion_binding_rejects_replaced_text_even_when_read_lengths_match() {
    let mut assessment = assessment(inventory(Vec::new()), "original");
    read_all(&mut assessment, "policy", "policy", "policy", "original");
    read_all(
        &mut assessment,
        "discussion",
        "original",
        "policy",
        "original",
    );
    assessment
        .note("discussion", discussion_note(), None, "policy", "original")
        .unwrap();
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            "original",
        )
        .unwrap();
    assessment.finish("policy", "original").unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "original")
        .unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "replaced")
        .unwrap_err();
    let mut record = serde_json::to_value(&assessment).unwrap();
    record["discussionDigest"] = json!("0".repeat(64));
    let record: Assessment = serde_json::from_value(record).unwrap();
    record
        .validate("snapshot", "policy", "policy", "original")
        .unwrap_err();
    let mut record = serde_json::to_value(&assessment).unwrap();
    record.as_object_mut().unwrap().remove("discussionDigest");
    serde_json::from_value::<Assessment>(record).unwrap_err();
}

#[test]
fn requested_context_is_exact_contiguous_required_and_snapshot_bound_for_every_disposition() {
    let text = "first\r\n\u{1f916} definition\r\nlast";
    let change = change(
        "unusual\tpath",
        Content::Text { text: text.into() },
        Content::Text {
            text: text.replace("definition", "changed"),
        },
    );
    let id = change.id.clone();
    for disposition in [
        Disposition::Insignificant,
        Disposition::Significant,
        Disposition::Uncertain,
    ] {
        let mut assessment = assessment(inventory(vec![change_copy(&change)]), "discussion");
        read_patch(&mut assessment, &id, "policy", "discussion");
        assessment
            .record(
                &id,
                judgment(&id, disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap_err();
        read_all(&mut assessment, "policy", "policy", "policy", "discussion");
        let request = ContextRequest {
            change_id: id.clone(),
            side: SourceSide::Old,
            start_line: NonZeroUsize::new(2).unwrap(),
            line_count: NonZeroUsize::new(2).unwrap(),
        };
        let first = assessment
            .source_context(request.clone(), 1, "policy", "discussion")
            .unwrap();
        assert_eq!(first["text"], "\u{1f916}");
        assert_eq!(first["context"]["startCharacter"], 7);
        assert_eq!(
            first,
            assessment
                .source_context(request.clone(), 1, "policy", "discussion")
                .unwrap()
        );
        let reference = first["reference"].as_str().unwrap();
        assert_eq!(
            assessment.summary(0, 10, "policy", "discussion").unwrap()["nextContextReference"],
            reference
        );
        assessment
            .read(reference, 2, 1, "policy", "discussion")
            .unwrap_err();
        assessment
            .record(
                &id,
                judgment(&id, disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap_err();
        read_all(
            &mut assessment,
            "discussion",
            "discussion",
            "policy",
            "discussion",
        );
        assessment
            .record(
                "cumulative",
                judgment("cumulative", disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap_err();
        assessment.finish("policy", "discussion").unwrap_err();
        assert!(!assessment.needs_review());
        let saved = serde_json::to_value(&assessment).unwrap();
        for field in [
            "snapshotDigest",
            "source",
            "startByte",
            "endByte",
            "startCharacter",
            "contentDigest",
            "request",
        ] {
            let mut corrupted = saved.clone();
            let value = &mut corrupted["contexts"][reference][field];
            *value = match field {
                "source" => {
                    let mut value = value.clone();
                    value["object"] = json!("c".repeat(40));
                    value
                }
                "request" => {
                    let mut value = value.clone();
                    value["side"] = json!("new");
                    value
                }
                "startByte" | "endByte" | "startCharacter" => json!(1),
                _ => json!("changed"),
            };
            let corrupt: Assessment = serde_json::from_value(corrupted).unwrap();
            corrupt
                .validate("snapshot", "policy", "policy", "discussion")
                .unwrap_err();
        }
        assessment = serde_json::from_value(saved).unwrap();
        assessment
            .validate("snapshot", "policy", "policy", "discussion")
            .unwrap();
        assessment
            .validate("another-snapshot", "policy", "policy", "discussion")
            .unwrap_err();
        read_all(
            &mut assessment,
            reference,
            "\u{1f916} definition\r\nlast",
            "policy",
            "discussion",
        );
        assessment
            .record(
                &id,
                judgment(&id, disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap();
        assessment
            .note(
                "discussion",
                discussion_note(),
                None,
                "policy",
                "discussion",
            )
            .unwrap();
        assessment
            .record(
                "cumulative",
                judgment("cumulative", disposition),
                None,
                "policy",
                "discussion",
            )
            .unwrap();
        assessment.finish("policy", "discussion").unwrap();
        let digest = assessment.digest.clone();
        assessment
            .source_context(request.clone(), 1, "policy", "discussion")
            .unwrap();
        let mut another = request;
        another.start_line = NonZeroUsize::MIN;
        assessment
            .source_context(another, 1, "policy", "discussion")
            .unwrap_err();
        assessment
            .validate("snapshot", "policy", "policy", "discussion")
            .unwrap();
        assert_eq!(digest, assessment.digest);
    }
}

#[test]
fn new_context_invalidates_affected_judgments_until_they_are_recorded_again() {
    let change = change(
        "file",
        Content::Text {
            text: "old\n".into(),
        },
        Content::Text {
            text: "new\n".into(),
        },
    );
    let id = change.id.clone();
    let mut other = change_copy(&change);
    other.old.as_mut().unwrap().path = "other".into();
    other.new.as_mut().unwrap().path = "other".into();
    other.id = change_id("M", other.old.as_ref(), other.new.as_ref()).unwrap();
    let other_id = other.id.clone();
    let mut assessment = assessment(inventory(vec![change, other]), "discussion");
    read_all(&mut assessment, "policy", "policy", "policy", "discussion");
    read_all(
        &mut assessment,
        "discussion",
        "discussion",
        "policy",
        "discussion",
    );
    read_patch(&mut assessment, &id, "policy", "discussion");
    read_patch(&mut assessment, &other_id, "policy", "discussion");
    let digest = assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let other_digest = assessment
        .record(
            &other_id,
            judgment(&other_id, Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .note(
            "discussion",
            discussion_note(),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let cumulative_digest = assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let request = ContextRequest {
        change_id: id.clone(),
        side: SourceSide::New,
        start_line: NonZeroUsize::MIN,
        line_count: NonZeroUsize::MIN,
    };
    let first = assessment
        .source_context(request.clone(), 1, "policy", "discussion")
        .unwrap();
    // Persisted judgments cannot precede an unread required range.
    let mut malformed = serde_json::to_value(&assessment).unwrap();
    malformed["decisions"][&id] =
        serde_json::to_value(judgment(&id, Disposition::Insignificant)).unwrap();
    let malformed: Assessment = serde_json::from_value(malformed).unwrap();
    malformed
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap_err();
    let mut malformed = serde_json::to_value(&assessment).unwrap();
    malformed["cumulative"] =
        serde_json::to_value(judgment("cumulative", Disposition::Insignificant)).unwrap();
    let malformed: Assessment = serde_json::from_value(malformed).unwrap();
    malformed
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap_err();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    assessment.finish("policy", "discussion").unwrap_err();
    read_all(
        &mut assessment,
        first["reference"].as_str().unwrap(),
        "new\n",
        "policy",
        "discussion",
    );
    let saved = serde_json::to_value(&assessment).unwrap();
    assert!(saved["decisions"].get(&id).is_none());
    assert_eq!(
        saved["decisions"][&other_id],
        serde_json::to_value(judgment(&other_id, Disposition::Insignificant)).unwrap()
    );
    assert!(saved["cumulative"].is_null());
    assessment = serde_json::from_value(saved).unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    assessment.finish("policy", "discussion").unwrap_err();
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Significant),
            Some(&digest),
            "policy",
            "discussion",
        )
        .unwrap_err();
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Significant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .note(
            "discussion",
            discussion_note(),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Significant),
            Some(&cumulative_digest),
            "policy",
            "discussion",
        )
        .unwrap_err();
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Significant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .source_context(request, 1, "policy", "discussion")
        .unwrap();
    let summary = assessment.summary(0, 10, "policy", "discussion").unwrap();
    assert_eq!(summary["cumulative"]["disposition"], "significant");
    assert_eq!(summary["changes"][1]["decisionDigest"], other_digest);
    assert_eq!(
        summary["changes"][0]["decision"]["disposition"],
        "significant"
    );
    assessment.finish("policy", "discussion").unwrap();
    assert_eq!(assessment.status(false), "needsReview");
}

#[test]
fn malformed_source_manifests_and_missing_documents_never_become_empty_evidence() {
    let change = change(
        "file",
        Content::Text {
            text: "old\n".into(),
        },
        Content::Text {
            text: "new\n".into(),
        },
    );
    let id = change.id.clone();
    let assessment = assessment(inventory(vec![change]), "");
    let _diagnostic = format!("{assessment:?}");
    let original = serde_json::to_value(&assessment).unwrap();
    for field in ["sourceEvidence", "sourceEvidenceDigest", "contexts"] {
        let mut record = original.clone();
        record.as_object_mut().unwrap().remove(field);
        serde_json::from_value::<Assessment>(record).unwrap_err();
    }
    for pointer in [
        "/patch",
        "/hunks/0/oldStart",
        "/old/object",
        "/new/mode",
        "/snapshotDigest",
        "/changeId",
        "/kind",
    ] {
        let mut record = original.clone();
        let target = record["sourceEvidence"][&id].pointer_mut(pointer).unwrap();
        *target = if pointer.contains("Start") {
            json!(2)
        } else {
            json!("changed")
        };
        let record: Assessment = serde_json::from_value(record).unwrap();
        record
            .validate("snapshot", "policy", "policy", "")
            .unwrap_err();
    }
    let mut record = original;
    record["sourceEvidence"] = json!({});
    let record: Assessment = serde_json::from_value(record).unwrap();
    record
        .validate("snapshot", "policy", "policy", "")
        .unwrap_err();
}
