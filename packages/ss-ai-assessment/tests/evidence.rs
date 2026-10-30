//! Evidence delivery limits and semantic completion remain independent of comparison size.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Tests use bounded fixture arrays and counters"
)]

use serde_json::{Value, json};
use ss_ai_assessment::{
    Assessment, Change, Content, Disposition, Inventory, Judgment, Side, change_id,
};

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
            vec![format!("{id}:old"), format!("{id}:new")]
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
    let mut assessment = Assessment::new(inventory(Vec::new()), "snapshot", "policy", "").unwrap();
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
        let mut assessment = Assessment::new(
            inventory(vec![change_copy(&change)]),
            "snapshot",
            "policy",
            "discussion",
        )
        .unwrap();
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
    let mut assessment =
        Assessment::new(inventory(vec![change]), "snapshot", "policy", "").unwrap();
    read_all(&mut assessment, "policy", "", "", "");
    read_all(&mut assessment, "discussion", "", "", "");
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
    let mut assessment =
        Assessment::new(inventory(changes), "snapshot", "policy", &discussion).unwrap();
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
            read_all(
                &mut assessment,
                &format!("{id}:old"),
                &old,
                "policy",
                &discussion,
            );
            read_all(
                &mut assessment,
                &format!("{id}:new"),
                &new,
                "policy",
                &discussion,
            );
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
    let assessment = Assessment::new(inventory(changes), "snapshot", "policy", "").unwrap();
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
    let mut assessment =
        Assessment::new(inventory(Vec::new()), "snapshot", "policy", "original").unwrap();
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
    let corrupt: Assessment = serde_json::from_value(record).unwrap();
    corrupt
        .validate("snapshot", "policy", "policy", "original")
        .unwrap_err();
    let mut record = serde_json::to_value(&assessment).unwrap();
    record.as_object_mut().unwrap().remove("discussionDigest");
    serde_json::from_value::<Assessment>(record).unwrap_err();
}
