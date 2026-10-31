//! Evidence delivery limits and semantic completion remain independent of comparison size.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Tests use bounded fixture arrays and counters"
)]

use serde_json::{Value, json};
use ss_ai_assessment::{
    Assessment, Change, Content, ContextRequest, Disposition, Inventory, Judgment, Side,
    SourceSide, change_id,
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
    Assessment::new(inventory, &patches, "snapshot", "policy", discussion).unwrap()
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

/// Follows native offsets, bounding progress by captured length rather than a watchdog.
fn read_all(
    assessment: &mut Assessment,
    reference: &str,
    expected: &str,
    policy: &str,
    discussion: &str,
) {
    let mut offset = 0;
    let mut received = String::new();
    loop {
        let page = assessment
            .read(reference, offset, usize::MAX, policy, discussion)
            .unwrap();
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

#[test]
fn large_exact_comparison_can_be_examined_incrementally_without_a_whole_inventory_response() {
    // Match the observed inventory scale and approximate source/discussion volumes, without PR data.
    let old = "old source\n".repeat(819);
    let new = "new source\n".repeat(819);
    let discussion = "discussion\n".repeat(74_603);
    let changes = (0..923)
        .map(|index| {
            change(
                &format!("source-{index}.rs"),
                Content::Text { text: old.clone() },
                Content::Text { text: new.clone() },
            )
        })
        .collect();
    let mut assessment = assessment(inventory(changes), &discussion);
    read_all(&mut assessment, "policy", "policy", "policy", &discussion);
    read_all(
        &mut assessment,
        "discussion",
        &discussion,
        "policy",
        &discussion,
    );
    let mut after = 0;
    let mut examined = 0;
    loop {
        let page = assessment
            .summary(after, 100, "policy", &discussion)
            .unwrap();
        assert_bounded(&page);
        assert_eq!(page["pendingChangeCount"], 923 - examined);
        assert_eq!(page["nextChangeOffset"], examined);
        let changes = page["changes"].as_array().unwrap();
        assert!(!changes.is_empty());
        for change in changes {
            let id = change["id"].as_str().unwrap();
            read_patch(&mut assessment, id, "policy", &discussion);
            assessment
                .record(
                    id,
                    judgment(id, Disposition::Insignificant),
                    None,
                    "policy",
                    &discussion,
                )
                .unwrap();
            examined += 1;
        }
        assert_eq!(
            assessment.progress(),
            json!({
                "changeCount": 923, "judgedChangeCount": examined,
                "nextChangeOffset": (examined < 923).then_some(examined)
            })
        );
        if after == 0 {
            // A fresh worker resumes the native record, not another worker's in-memory source.
            assessment = serde_json::from_slice(&serde_json::to_vec(&assessment).unwrap()).unwrap();
            assessment
                .validate("snapshot", "policy", "policy", &discussion)
                .unwrap();
        }
        if page["nextOffset"].is_null() {
            break;
        }
        let next = usize::try_from(page["nextOffset"].as_u64().unwrap()).unwrap();
        assert_eq!(next, after + changes.len());
        assert!(next > after && next < 923);
        after = next;
    }
    assert_eq!(examined, 923);
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
