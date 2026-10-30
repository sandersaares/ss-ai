//! Synthetic checkpoint contracts exercise production records without providers or clocks.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Bounded synthetic fixture indices and counters"
)]

use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use serde_json::{Value, json};
use ss_ai_assessment::{
    Assessment, Change, Content, ContextRequest, Disposition, Inventory, Judgment, Note, Side,
    SourceSide, change_id,
};

/// Replacements retain exact CRLF and final-newline semantics without executing source.
fn fixture(discussion: &str, old: &str, new: &str) -> Assessment {
    let changes = if old == new {
        Vec::new()
    } else {
        let old = Some(Side {
            path: "file".into(),
            object: "a".repeat(40),
            mode: "100644".into(),
            content: Content::Text { text: old.into() },
        });
        let new = Some(Side {
            path: "file".into(),
            object: "b".repeat(40),
            mode: "100644".into(),
            content: Content::Text { text: new.into() },
        });
        vec![Change {
            id: change_id("M", old.as_ref(), new.as_ref()).unwrap(),
            kind: "M".into(),
            old,
            new,
        }]
    };
    let patches = if changes.is_empty() {
        Vec::new()
    } else {
        let mut patch = format!(
            "@@ -1,{} +1,{} @@\n",
            old.lines().count(),
            new.lines().count()
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
        vec![patch]
    };
    Assessment::new(
        Inventory {
            old_commit: "a".repeat(40),
            new_commit: "b".repeat(40),
            old_tree: "c".repeat(40),
            new_tree: if changes.is_empty() { "c" } else { "d" }.repeat(40),
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

/// Every delivered response is bounded and advances by its exact returned scalar count.
fn read(assessment: &mut Assessment, reference: &str, discussion: &str) -> String {
    let mut offset = 0;
    let mut text = String::new();
    loop {
        let page = assessment
            .read(reference, offset, usize::MAX, "policy", discussion)
            .unwrap();
        assert!(serde_json::to_vec_pretty(&page).unwrap().len() < 16_384);
        let part = page["text"].as_str().unwrap();
        text.push_str(part);
        if page["complete"] == true {
            break;
        }
        let next = usize::try_from(page["nextOffset"].as_u64().unwrap()).unwrap();
        assert!(next > offset);
        assert_eq!(next, offset + part.chars().count());
        assert!(next <= usize::try_from(page["totalCharacters"].as_u64().unwrap()).unwrap());
        offset = next;
    }
    text
}

fn units(assessment: &Assessment) -> Vec<Value> {
    let mut after = 0;
    let mut units = Vec::new();
    loop {
        let page = assessment.units(after, 100).unwrap();
        assert!(serde_json::to_vec_pretty(&page).unwrap().len() < 16_384);
        units.extend(page["items"].as_array().unwrap().iter().cloned());
        let Some(next) = page["nextOffset"].as_u64() else {
            break;
        };
        let next = usize::try_from(next).unwrap();
        assert!(next > after);
        after = next;
    }
    units
}

fn note(reference: &str) -> Note {
    Note {
        observations: "Observed the exact synthetic behavior.".into(),
        interfaces: "The synthetic contract remains explicit.".into(),
        concerns: Vec::new(),
        interactions: Vec::new(),
        evidence: vec![reference.into()],
    }
}

fn judgment(id: &str, disposition: Disposition) -> Judgment {
    Judgment {
        disposition,
        rationale: "Examined the complete synthetic effect.".into(),
        evidence: if id == "cumulative" {
            vec!["policy".into(), "discussion".into()]
        } else {
            vec![format!("{id}:patch")]
        },
        concerns: Vec::new(),
        interactions: Vec::new(),
    }
}

fn note_units(assessment: &mut Assessment, discussion: &str) {
    read(assessment, "policy", discussion);
    for unit in units(assessment) {
        let reference = unit["reference"].as_str().unwrap();
        read(assessment, reference, discussion);
        read(
            assessment,
            unit["bindingReference"].as_str().unwrap(),
            discussion,
        );
        assessment
            .note(
                unit["unit"]["id"].as_str().unwrap(),
                note(reference),
                None,
                "policy",
                discussion,
            )
            .unwrap();
        read(
            assessment,
            unit["noteReference"].as_str().unwrap(),
            discussion,
        );
    }
}

fn aggregate(assessment: &mut Assessment, discussion: &str) {
    let ids = assessment
        .inventory
        .changes
        .iter()
        .map(|change| change.id.clone())
        .collect::<Vec<_>>();
    for id in ids {
        assessment
            .record(
                &id,
                judgment(&id, Disposition::Insignificant),
                None,
                "policy",
                discussion,
            )
            .unwrap();
        read(assessment, &format!("judgment:{id}"), discussion);
    }
    assessment
        .note("discussion", note("discussion"), None, "policy", discussion)
        .unwrap();
    read(assessment, "discussion-note", discussion);
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            discussion,
        )
        .unwrap();
}

#[test]
fn discussion_json_spans_preserve_whitespace_order_and_escapes() {
    let documents = [
        r#" { "reviews" : [ { "body" : "a\u0062", "id" : 7 } ], "pullRequest" : { "body" : "literal \"reviews\": [x]" }, "comments" : [], "hunks" : {"z":"\u0061"} } "#.to_owned(),
        json!({"schemaVersion":1,"platform":"gitHub","pullRequest":{"body":"description"},
            "reviews":[{"id":7,"body":"objection"}],
            "reviewComments":[{"id":1,"body":"reply","hunkReference":"h"}],
            "comments":[],"hunks":{"h":"@@ -1 +1 @@\n"}}).to_string(),
    ];
    for discussion in documents {
        let mut assessment = fixture(&discussion, "", "");
        let mut ranges = Vec::new();
        for unit in units(&assessment) {
            let text = read(
                &mut assessment,
                unit["reference"].as_str().unwrap(),
                &discussion,
            );
            let mut offset = 0;
            for range in unit["unit"]["ranges"].as_array().unwrap() {
                let start = usize::try_from(range["startCharacter"].as_u64().unwrap()).unwrap();
                let end = usize::try_from(range["endCharacter"].as_u64().unwrap()).unwrap();
                ranges.push((
                    start,
                    end,
                    text.chars()
                        .skip(offset)
                        .take(end - start)
                        .collect::<String>(),
                ));
                offset += end - start;
            }
        }
        ranges.sort_by_key(|range| range.0);
        let mut result = String::new();
        let mut end = 0;
        for (start, next, text) in ranges {
            assert_eq!(start, end);
            end = next;
            result.push_str(&text);
        }
        assert_eq!(result, discussion);
        assessment
            .validate("snapshot", "policy", "policy", &discussion)
            .unwrap();
    }
}

#[test]
fn each_large_provider_thread_unit_fits_the_existing_worker_budget() {
    let long = "x".repeat(900);
    let ado = json!({"platform":"azureDevOps","threads":[{
        "id":42,"threadContext":{"filePath":"file","line":3},
        "comments":(0..100).map(|id| json!({"id":id,"parentCommentId":0,
            "content":format!("body {id}"),"author":{"id":format!("{id}-{long}")}})).collect::<Vec<_>>()
    }]});
    let github = json!({"platform":"gitHub","reviewComments":
        (0..100).map(|id| json!({"id":id+1,"in_reply_to_id":if id==0 { Value::Null } else {json!(1)},
            "body":format!("body {id}"),"path":format!("{id}-{long}"),"hunkReference":"h"})).collect::<Vec<_>>(),
        "hunks":{"h":"@@ -1 +1 @@\n"}});
    let escaped = json!({"platform":"azureDevOps","threads":[{
        "id":123,"threadContext":{"filePath":"file"},"comments":[{
            "id":1,"content":"\0\t\\\"".repeat(12_000)
        }]
    }]});
    for (discussion, collection) in [
        (ado.to_string(), "threads"),
        (github.to_string(), "reviewComments"),
        (escaped.to_string(), "threads"),
    ] {
        let mut assessment = fixture(&discussion, "", "");
        let all = units(&assessment);
        let mut group = None;
        let mut ranges = Vec::new();
        for (index, unit) in all.into_iter().enumerate() {
            let metadata = assessment.units(index, 1).unwrap();
            let policy = assessment
                .read("policy", 0, usize::MAX, "policy", &discussion)
                .unwrap();
            let mut delivered = serde_json::to_string_pretty(&metadata)
                .unwrap()
                .chars()
                .count()
                + serde_json::to_string_pretty(&policy)
                    .unwrap()
                    .chars()
                    .count()
                + 2;
            let mut raw = String::new();
            for reference in [
                unit["reference"].as_str().unwrap(),
                unit["bindingReference"].as_str().unwrap(),
            ] {
                let mut offset = 0;
                let mut complete = String::new();
                loop {
                    let page = assessment
                        .read(reference, offset, usize::MAX, "policy", &discussion)
                        .unwrap();
                    let serialized = serde_json::to_string_pretty(&page).unwrap();
                    assert!(serialized.len() < 16_384);
                    delivered += serialized.chars().count() + 1;
                    complete.push_str(page["text"].as_str().unwrap());
                    let Some(next) = page["nextOffset"].as_u64() else {
                        break;
                    };
                    let next = usize::try_from(next).unwrap();
                    assert!(next > offset);
                    offset = next;
                }
                if reference.starts_with("unit:") {
                    raw = complete;
                } else {
                    let binding: Value = serde_json::from_str(&complete).unwrap();
                    if binding
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry["collection"] == collection)
                    {
                        let parent = unit["unit"]["parent"].as_str().unwrap();
                        assert_eq!(group.get_or_insert_with(|| parent.to_owned()), parent);
                    }
                }
            }
            let digest = assessment
                .note(
                    unit["unit"]["id"].as_str().unwrap(),
                    note(unit["reference"].as_str().unwrap()),
                    None,
                    "policy",
                    &discussion,
                )
                .unwrap();
            let checkpoint = assessment.checkpoint().unwrap();
            delivered += digest.len()
                + serde_json::to_string_pretty(&checkpoint)
                    .unwrap()
                    .chars()
                    .count();
            assert!(
                delivered < 65_536,
                "individual unit requires {delivered} characters"
            );
            let mut local = 0;
            for range in unit["unit"]["ranges"].as_array().unwrap() {
                let start = usize::try_from(range["startCharacter"].as_u64().unwrap()).unwrap();
                let end = usize::try_from(range["endCharacter"].as_u64().unwrap()).unwrap();
                ranges.push((
                    start,
                    end,
                    raw.chars()
                        .skip(local)
                        .take(end - start)
                        .collect::<String>(),
                ));
                local += end - start;
            }
        }
        assert!(group.is_some());
        ranges.sort_by_key(|range| range.0);
        let mut end = 0;
        let mut reconstructed = String::new();
        for (start, next, text) in ranges {
            assert_eq!(start, end);
            end = next;
            reconstructed.push_str(&text);
        }
        assert_eq!(reconstructed, discussion);
        assessment
            .validate("snapshot", "policy", "policy", &discussion)
            .unwrap();
    }
}

#[test]
fn lossless_unicode_crlf_oversized_lines_and_interleaved_threads_have_exact_coverage() {
    let body = format!("first\r\n{}\u{1f916}\0last", "x".repeat(40_000));
    let discussion = json!({"schemaVersion":1, "platform":"gitHub", "pullRequest":{"body":body},
        "reviews":[{"id":7,"body":"Review-only objection"}],
        "reviewComments":[
            {"id":1,"body":"first thread","path":"a","hunkReference":"shared"},
            {"id":2,"body":"second thread","path":"b","hunkReference":"shared"},
            {"id":3,"in_reply_to_id":1,"body":"human reply","commit_id":"historical"}
        ], "comments":[], "hunks":{"shared":"@@ -1 +1 @@\n-old\n+new\n"}})
    .to_string();
    let mut assessment = fixture(&discussion, "old\r\n", &body);
    let all = units(&assessment);
    assert!(
        all.iter()
            .any(|unit| unit["unit"]["ranges"].as_array().unwrap().len() > 1)
    );
    let mut documents = BTreeMap::<String, Vec<(usize, usize, String)>>::new();
    for unit in all {
        let data = &unit["unit"];
        assert!(data["totalCharacters"].as_u64().unwrap() <= 16_384);
        let text = read(
            &mut assessment,
            unit["reference"].as_str().unwrap(),
            &discussion,
        );
        let mut offset = 0;
        for range in data["ranges"].as_array().unwrap() {
            let start = usize::try_from(range["startCharacter"].as_u64().unwrap()).unwrap();
            let end = usize::try_from(range["endCharacter"].as_u64().unwrap()).unwrap();
            let part = text
                .chars()
                .skip(offset)
                .take(end - start)
                .collect::<String>();
            offset += end - start;
            documents
                .entry(data["reference"].as_str().unwrap().into())
                .or_default()
                .push((start, end, part));
        }
        assert_eq!(offset, text.chars().count());
    }
    for (reference, mut ranges) in documents {
        ranges.sort_by_key(|range| range.0);
        let mut offset = 0;
        let mut reconstructed = String::new();
        for (start, end, text) in ranges {
            assert_eq!(start, offset);
            assert!(end >= start);
            offset = end;
            reconstructed.push_str(&text);
        }
        assert_eq!(
            reconstructed,
            read(&mut assessment, &reference, &discussion)
        );
    }
    assessment
        .validate("snapshot", "policy", "policy", &discussion)
        .unwrap();
}

#[test]
fn missing_units_block_all_dispositions_even_after_raw_delivery() {
    let discussion = "discussion".repeat(5000);
    for disposition in [
        Disposition::Insignificant,
        Disposition::Significant,
        Disposition::Uncertain,
    ] {
        let mut assessment = fixture(&discussion, "old", &"new".repeat(12_000));
        let id = assessment.inventory.changes[0].id.clone();
        read(&mut assessment, "policy", &discussion);
        read(&mut assessment, &format!("{id}:patch"), &discussion);
        read(&mut assessment, "discussion", &discussion);
        for document in [format!("{id}:patch"), "discussion".to_owned()] {
            let selected = units(&assessment)
                .into_iter()
                .filter(|unit| unit["unit"]["reference"] == document)
                .collect::<Vec<_>>();
            assert!(selected.len() > 1);
            for (index, unit) in selected.into_iter().enumerate() {
                if index == 1 {
                    continue;
                }
                let reference = unit["reference"].as_str().unwrap();
                read(&mut assessment, reference, &discussion);
                read(
                    &mut assessment,
                    unit["bindingReference"].as_str().unwrap(),
                    &discussion,
                );
                assessment
                    .note(
                        unit["unit"]["id"].as_str().unwrap(),
                        note(reference),
                        None,
                        "policy",
                        &discussion,
                    )
                    .unwrap();
                read(
                    &mut assessment,
                    unit["noteReference"].as_str().unwrap(),
                    &discussion,
                );
            }
        }
        assert!(
            assessment.progress()["completedUnitCount"]
                .as_u64()
                .unwrap()
                > 0
        );
        assessment
            .record(&id, judgment(&id, disposition), None, "policy", &discussion)
            .unwrap_err();
        assessment
            .note(
                "discussion",
                note("discussion"),
                None,
                "policy",
                &discussion,
            )
            .unwrap_err();
        assessment
            .record(
                "cumulative",
                judgment("cumulative", disposition),
                None,
                "policy",
                &discussion,
            )
            .unwrap_err();
        assessment.finish("policy", &discussion).unwrap_err();
        assert!(!assessment.needs_review());
    }
}

#[test]
fn checkpoint_replay_and_replacement_do_not_create_progress() {
    let mut assessment = fixture("discussion", "old", "new");
    assessment.checkpoint().unwrap_err();
    read(&mut assessment, "policy", "discussion");
    let unit = units(&assessment).remove(0);
    let id = unit["unit"]["id"].as_str().unwrap();
    let reference = unit["reference"].as_str().unwrap();
    read(&mut assessment, reference, "discussion");
    read(
        &mut assessment,
        unit["bindingReference"].as_str().unwrap(),
        "discussion",
    );
    assessment.checkpoint().unwrap_err();
    let digest = assessment
        .note(id, note(reference), None, "policy", "discussion")
        .unwrap();
    assessment.checkpoint().unwrap();
    read(&mut assessment, reference, "discussion");
    assessment
        .note(id, note(reference), None, "policy", "discussion")
        .unwrap();
    assessment.checkpoint().unwrap_err();
    let mut changed = note(reference);
    changed.observations.push_str(" More precise.");
    assessment
        .note(id, changed, Some(&digest), "policy", "discussion")
        .unwrap();
    assessment.checkpoint().unwrap_err();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

#[test]
fn replacing_notes_invalidates_dependent_aggregates_and_requires_conditional_digest() {
    let mut assessment = fixture("discussion", "old", "new");
    note_units(&mut assessment, "discussion");
    aggregate(&mut assessment, "discussion");
    let all = units(&assessment);
    let unit = all
        .iter()
        .find(|unit| unit["unit"]["reference"] != "discussion")
        .unwrap();
    let id = unit["unit"]["id"].as_str().unwrap();
    let reference = unit["reference"].as_str().unwrap();
    let mut discussion_note = note("discussion");
    discussion_note
        .evidence
        .push(unit["noteReference"].as_str().unwrap().to_owned());
    let previous_discussion =
        assessment.summary(0, 1, "policy", "discussion").unwrap()["discussionNoteDigest"]
            .as_str()
            .unwrap()
            .to_owned();
    assessment
        .note(
            "discussion",
            discussion_note,
            Some(&previous_discussion),
            "policy",
            "discussion",
        )
        .unwrap();
    read(&mut assessment, "discussion-note", "discussion");
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let mut replacement = note(reference);
    replacement
        .concerns
        .push("A material objection was overlooked.".into());
    assessment
        .note(id, note(reference), Some("wrong"), "policy", "discussion")
        .unwrap();
    // Idempotent replay does not replace anything; a changed payload must compare its digest.
    assessment
        .note(id, replacement, Some("wrong"), "policy", "discussion")
        .unwrap_err();
    let mut replacement = note(reference);
    replacement
        .concerns
        .push("A material objection was overlooked.".into());
    assessment
        .note(
            id,
            replacement,
            unit["noteDigest"].as_str(),
            "policy",
            "discussion",
        )
        .unwrap();
    assert_eq!(assessment.progress()["judgedChangeCount"], 0);
    assert!(
        assessment.summary(0, 1, "policy", "discussion").unwrap()["discussionNoteDigest"].is_null()
    );
    assessment.finish("policy", "discussion").unwrap_err();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    assert!(!assessment.needs_review());
}

#[test]
fn recorded_concerns_require_exact_dispositions_and_cannot_be_erased_by_favorable_aggregate() {
    let mut assessment = fixture("Historical objection.", "old", "new");
    note_units(&mut assessment, "Historical objection.");
    let unit = units(&assessment)
        .into_iter()
        .find(|unit| unit["unit"]["reference"] == "discussion")
        .unwrap();
    let reference = unit["reference"].as_str().unwrap();
    let mut objection = note(reference);
    objection
        .concerns
        .push("The historical body identifies changed behavior.".into());
    assessment
        .note(
            unit["unit"]["id"].as_str().unwrap(),
            objection,
            unit["noteDigest"].as_str(),
            "policy",
            "Historical objection.",
        )
        .unwrap();
    read(
        &mut assessment,
        unit["noteReference"].as_str().unwrap(),
        "Historical objection.",
    );
    let id = assessment.inventory.changes[0].id.clone();
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            None,
            "policy",
            "Historical objection.",
        )
        .unwrap();
    read(
        &mut assessment,
        &format!("judgment:{id}"),
        "Historical objection.",
    );
    assessment
        .note(
            "discussion",
            note("discussion"),
            None,
            "policy",
            "Historical objection.",
        )
        .unwrap();
    read(&mut assessment, "discussion-note", "Historical objection.");
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            "Historical objection.",
        )
        .unwrap_err();
    let check = assessment.checks(0, 100).unwrap()["items"][0].clone();
    read(
        &mut assessment,
        check["reference"].as_str().unwrap(),
        "Historical objection.",
    );
    let mut resolution = judgment("cumulative", Disposition::Significant);
    resolution.evidence = vec!["missing:reference".into()];
    assessment
        .resolve(
            check["id"].as_str().unwrap(),
            resolution,
            None,
            "policy",
            "Historical objection.",
        )
        .unwrap_err();
    assessment
        .resolve(
            check["id"].as_str().unwrap(),
            judgment("cumulative", Disposition::Significant),
            None,
            "policy",
            "Historical objection.",
        )
        .unwrap();
    read(
        &mut assessment,
        check["resolutionReference"].as_str().unwrap(),
        "Historical objection.",
    );
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            "Historical objection.",
        )
        .unwrap();
    assessment
        .finish("policy", "Historical objection.")
        .unwrap();
    assert_eq!(assessment.status(true), "needsReview");
}

#[test]
fn empty_inventory_still_requires_discussion_notes_and_independent_cumulative_work() {
    let mut assessment = fixture("", "", "");
    assert_eq!(assessment.progress()["unitCount"], 1);
    note_units(&mut assessment, "");
    assessment.finish("policy", "").unwrap_err();
    aggregate(&mut assessment, "");
    assessment.finish("policy", "").unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "")
        .unwrap();
    assert_eq!(assessment.status(true), "wouldReapprove");
    let mut corrupt = serde_json::to_value(&assessment).unwrap();
    corrupt["checkpoints"]["units"][0]["contentDigest"] = json!("changed");
    let corrupt: Assessment = serde_json::from_value(corrupt).unwrap();
    corrupt
        .validate("snapshot", "policy", "policy", "")
        .unwrap_err();
}

#[test]
fn tampered_note_evidence_and_dependency_records_are_rejected_at_load() {
    let mut assessment = fixture("discussion", "old", "new");
    note_units(&mut assessment, "discussion");
    let unit = units(&assessment).remove(0);
    let id = unit["unit"]["id"].as_str().unwrap();
    let original = serde_json::to_value(&assessment).unwrap();
    for field in ["value", "dependencies"] {
        let mut document = original.clone();
        if field == "value" {
            document["checkpoints"]["notes"][id]["value"]["evidence"] = json!(["policy"]);
        } else {
            document["checkpoints"]["notes"][id]["dependencies"] = json!({});
        }
        let corrupt: Assessment = serde_json::from_value(document).unwrap();
        corrupt
            .validate("snapshot", "policy", "policy", "discussion")
            .unwrap_err();
    }
}

#[test]
fn incomplete_records_reject_tampered_checkpoint_state_without_requested_context() {
    let mut assessment = fixture("discussion", "old", "new");
    note_units(&mut assessment, "discussion");
    aggregate(&mut assessment, "discussion");
    let original = serde_json::to_value(&assessment).unwrap();
    for (pointer, value) in [
        ("/checkpoints/manifestDigest", json!("changed")),
        ("/checkpoints/aggregates/cumulative", json!({})),
    ] {
        let mut corrupt = original.clone();
        *corrupt.pointer_mut(pointer).unwrap() = value;
        let corrupt: Assessment = serde_json::from_value(corrupt).unwrap();
        corrupt
            .validate("snapshot", "policy", "policy", "discussion")
            .unwrap_err();
    }
}

#[test]
fn replacing_check_origin_invalidates_other_resolutions_that_cite_it() {
    let mut assessment = fixture("discussion", "old", "new");
    note_units(&mut assessment, "discussion");
    let all = units(&assessment);
    for (index, unit) in all.iter().enumerate() {
        let mut replacement = note(unit["reference"].as_str().unwrap());
        replacement.concerns.push(format!("Concern {index}"));
        if index == 0 {
            replacement.concerns.push("Independent concern".into());
        }
        assessment
            .note(
                unit["unit"]["id"].as_str().unwrap(),
                replacement,
                unit["noteDigest"].as_str(),
                "policy",
                "discussion",
            )
            .unwrap();
        read(
            &mut assessment,
            unit["noteReference"].as_str().unwrap(),
            "discussion",
        );
    }
    let checks = assessment.checks(0, 100).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(checks.len(), 3);
    for check in &checks {
        read(
            &mut assessment,
            check["reference"].as_str().unwrap(),
            "discussion",
        );
    }
    let first_origin = all[0]["noteReference"].as_str().unwrap();
    let first = checks.iter().find(|c| c["origin"] == first_origin).unwrap();
    let other = checks.iter().find(|c| c["origin"] != first_origin).unwrap();
    for check in &checks {
        let mut resolution = judgment("cumulative", Disposition::Significant);
        if check["id"] == first["id"] {
            resolution
                .evidence
                .push(other["reference"].as_str().unwrap().into());
        }
        assessment
            .resolve(
                check["id"].as_str().unwrap(),
                resolution,
                None,
                "policy",
                "discussion",
            )
            .unwrap();
    }
    let other_unit = all
        .iter()
        .find(|unit| unit["noteReference"] == other["origin"])
        .unwrap();
    let current = units(&assessment)
        .into_iter()
        .find(|unit| unit["unit"]["id"] == other_unit["unit"]["id"])
        .unwrap();
    let mut replacement = note(current["reference"].as_str().unwrap());
    replacement.concerns.push("Changed concern".into());
    assessment
        .note(
            current["unit"]["id"].as_str().unwrap(),
            replacement,
            current["noteDigest"].as_str(),
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    let remaining = assessment.checks(0, 100).unwrap();
    let remaining = remaining["items"].as_array().unwrap();
    assert!(
        remaining.iter().find(|c| c["id"] == first["id"]).unwrap()["resolutionDigest"].is_null()
    );
    assert!(remaining.iter().any(|c| c["origin"] == first_origin
        && c["id"] != first["id"]
        && !c["resolutionDigest"].is_null()));
}

#[test]
fn replacing_change_judgment_invalidates_resolutions_citing_its_check() {
    let mut assessment = fixture("discussion", "old", "new");
    note_units(&mut assessment, "discussion");
    let unit = units(&assessment).remove(0);
    let mut observation = note(unit["reference"].as_str().unwrap());
    observation.concerns.push("Independent concern".into());
    assessment
        .note(
            unit["unit"]["id"].as_str().unwrap(),
            observation,
            unit["noteDigest"].as_str(),
            "policy",
            "discussion",
        )
        .unwrap();
    read(
        &mut assessment,
        unit["noteReference"].as_str().unwrap(),
        "discussion",
    );
    let id = assessment.inventory.changes[0].id.clone();
    let mut source = judgment(&id, Disposition::Significant);
    source.concerns.push("Changed source".into());
    let digest = assessment
        .record(&id, source, None, "policy", "discussion")
        .unwrap();
    read(&mut assessment, &format!("judgment:{id}"), "discussion");
    let checks = assessment.checks(0, 100).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone();
    let source_check = checks
        .iter()
        .find(|c| c["origin"] == format!("judgment:{id}"))
        .unwrap();
    let note_check = checks
        .iter()
        .find(|c| c["origin"] == unit["noteReference"])
        .unwrap();
    for check in &checks {
        read(
            &mut assessment,
            check["reference"].as_str().unwrap(),
            "discussion",
        );
    }
    let mut resolution = judgment("cumulative", Disposition::Significant);
    resolution
        .evidence
        .push(source_check["reference"].as_str().unwrap().into());
    assessment
        .resolve(
            note_check["id"].as_str().unwrap(),
            resolution,
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            Some(&digest),
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    let remaining = assessment.checks(0, 100).unwrap();
    assert!(
        remaining["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == note_check["id"])
            .unwrap()["resolutionDigest"]
            .is_null()
    );
}

#[test]
fn new_context_invalidates_affected_notes_aggregates_and_all_check_dispositions() {
    let mut assessment = fixture("discussion", "old", "new");
    note_units(&mut assessment, "discussion");
    let all = units(&assessment);
    for unit in &all {
        let mut observation = note(unit["reference"].as_str().unwrap());
        observation.concerns.push("Synthetic concern".into());
        assessment
            .note(
                unit["unit"]["id"].as_str().unwrap(),
                observation,
                unit["noteDigest"].as_str(),
                "policy",
                "discussion",
            )
            .unwrap();
        read(
            &mut assessment,
            unit["noteReference"].as_str().unwrap(),
            "discussion",
        );
    }
    let id = assessment.inventory.changes[0].id.clone();
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    read(&mut assessment, &format!("judgment:{id}"), "discussion");
    assessment
        .note(
            "discussion",
            note("discussion"),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    read(&mut assessment, "discussion-note", "discussion");
    let checks = assessment.checks(0, 100).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone();
    for check in &checks {
        read(
            &mut assessment,
            check["reference"].as_str().unwrap(),
            "discussion",
        );
        assessment
            .resolve(
                check["id"].as_str().unwrap(),
                judgment("cumulative", Disposition::Insignificant),
                None,
                "policy",
                "discussion",
            )
            .unwrap();
        read(
            &mut assessment,
            check["resolutionReference"].as_str().unwrap(),
            "discussion",
        );
    }
    assessment
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
        .source_context(request, 1, "policy", "discussion")
        .unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    let current = units(&assessment);
    for unit in &current {
        assert_eq!(unit["completed"], unit["unit"]["reference"] == "discussion");
    }
    let remaining = assessment.checks(0, 100).unwrap();
    let remaining = remaining["items"].as_array().unwrap();
    assert!(!remaining.is_empty());
    assert!(
        remaining
            .iter()
            .all(|check| check["resolutionDigest"].is_null())
    );
    assert!(remaining.iter().all(|check| {
        !checks.iter().any(|old| {
            old["id"] == check["id"] && old["origin"].as_str().unwrap().starts_with("judgment:")
        })
    }));
    let summary = assessment.summary(0, 10, "policy", "discussion").unwrap();
    assert!(summary["changes"][0]["decision"].is_null());
    assert!(summary["discussionNoteDigest"].is_null());
    assert!(summary["cumulative"].is_null());
    assessment.finish("policy", "discussion").unwrap_err();
    read(
        &mut assessment,
        first["reference"].as_str().unwrap(),
        "discussion",
    );
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap_err();
}

#[test]
fn large_thread_metadata_and_many_ranges_remain_accessible_without_losing_bindings() {
    let comments = (0..100)
                .map(|id|                 json!({"id": id, "parentCommentId": 0, "content": format!("body {id}"), "author": {"id": format!("author-{id}-{}", "x".repeat(300))}}))
                .collect::<Vec<_>>();
    let replies = (0..100)
                .map(|id| json!({"id": id + 1, "in_reply_to_id": if id == 0 { Value::Null } else { json!(1) }, "body": format!("reply {id}"), "hunkReference":"shared"}))
                .collect::<Vec<_>>();
    let discussion = json!({
        "threads":[{"id":42, "threadContext":{"filePath":"file"}, "comments":comments}],
        "reviewComments":replies,
        "hunks":{"shared":"historical excerpt"}
    })
    .to_string();
    let mut assessment = fixture(&discussion, "", "");
    read(&mut assessment, "policy", &discussion);
    let all = units(&assessment);
    assert!(all.len() > 2);
    let mut ranges = Vec::new();
    let mut saw_thread_binding = false;
    for unit in all {
        let reference = unit["reference"].as_str().unwrap();
        let text = read(&mut assessment, reference, &discussion);
        let id = unit["unit"]["id"].as_str().unwrap();
        assessment
            .note(id, note(reference), None, "policy", &discussion)
            .unwrap_err();
        let bindings = read(
            &mut assessment,
            unit["bindingReference"].as_str().unwrap(),
            &discussion,
        );
        let bindings: Value = serde_json::from_str(&bindings).unwrap();
        if bindings.to_string().contains("\"threads\"") {
            saw_thread_binding = true;
            assert!(bindings.to_string().contains("identityDigest"));
        }
        assessment
            .note(id, note(reference), None, "policy", &discussion)
            .unwrap();
        let mut offset = 0;
        for range in unit["unit"]["ranges"].as_array().unwrap() {
            let start = usize::try_from(range["startCharacter"].as_u64().unwrap()).unwrap();
            let end = usize::try_from(range["endCharacter"].as_u64().unwrap()).unwrap();
            let part = text
                .chars()
                .skip(offset)
                .take(end - start)
                .collect::<String>();
            offset += end - start;
            ranges.push((start, end, part));
        }
    }
    assert!(saw_thread_binding);
    ranges.sort_by_key(|range| range.0);
    let mut next = 0;
    let mut reconstructed = String::new();
    for (start, end, text) in ranges {
        assert_eq!(start, next);
        next = end;
        reconstructed.push_str(&text);
    }
    assert_eq!(reconstructed, discussion);
    assessment
        .validate("snapshot", "policy", "policy", &discussion)
        .unwrap();
}

#[test]
fn literal_escaped_hunk_headers_do_not_split_complete_small_hunks() {
    let assessment = fixture("", "old", "let example = \"\\n@@ -1 +1 @@\";");
    let id = &assessment.inventory.changes[0].id;
    let units = assessment
        .units_for(Some(&format!("{id}:patch")), 0, 100)
        .unwrap();
    assert_eq!(units["items"].as_array().unwrap().len(), 1);
}

#[test]
fn complete_small_hunks_keep_independent_exact_units() {
    let original = fixture("", "old\nmiddle\nlast\n", "new\nmiddle\nend\n");
    let mut assessment = Assessment::new(
        original.inventory,
        &["@@ -1 +1 @@\n-old\n+new\n@@ -3 +3 @@\n-last\n+end\n".into()],
        "snapshot",
        "policy",
        "",
    )
    .unwrap();
    let id = assessment.inventory.changes[0].id.clone();
    let units = assessment
        .units_for(Some(&format!("{id}:patch")), 0, 100)
        .unwrap();
    assert_eq!(units["items"].as_array().unwrap().len(), 2);
    let mut reconstructed = String::new();
    for unit in units["items"].as_array().unwrap() {
        reconstructed.push_str(&read(
            &mut assessment,
            unit["reference"].as_str().unwrap(),
            "",
        ));
    }
    assert_eq!(
        reconstructed,
        read(&mut assessment, &format!("{id}:patch"), "")
    );
}

#[test]
fn recursive_judgment_and_resolution_citations_fail_before_replacing_state() {
    let mut assessment = fixture("discussion", "old", "new");
    note_units(&mut assessment, "discussion");
    let id = assessment.inventory.changes[0].id.clone();
    let mut source = judgment(&id, Disposition::Significant);
    source.concerns.push("Synthetic changed behavior.".into());
    let digest = assessment
        .record(&id, source, None, "policy", "discussion")
        .unwrap();
    read(&mut assessment, &format!("judgment:{id}"), "discussion");
    let mut recursive = judgment(&id, Disposition::Significant);
    recursive.evidence.push(format!("judgment:{id}"));
    let before = serde_json::to_value(&assessment).unwrap();
    assessment
        .record(&id, recursive, Some(&digest), "policy", "discussion")
        .unwrap_err();
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);

    let check = assessment.checks(0, 1).unwrap()["items"][0].clone();
    let check_id = check["id"].as_str().unwrap();
    read(
        &mut assessment,
        check["reference"].as_str().unwrap(),
        "discussion",
    );
    let digest = assessment
        .resolve(
            check_id,
            judgment("cumulative", Disposition::Significant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let mut corrupt = serde_json::to_value(&assessment).unwrap();
    corrupt["checkpoints"]["resolutions"][check_id]["dependencies"] = json!({});
    let corrupt: Assessment = serde_json::from_value(corrupt).unwrap();
    corrupt
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap_err();
    read(
        &mut assessment,
        check["resolutionReference"].as_str().unwrap(),
        "discussion",
    );
    let mut recursive = judgment("cumulative", Disposition::Significant);
    recursive
        .evidence
        .push(check["resolutionReference"].as_str().unwrap().to_owned());
    let before = serde_json::to_value(&assessment).unwrap();
    assessment
        .resolve(check_id, recursive, Some(&digest), "policy", "discussion")
        .unwrap_err();
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
}
