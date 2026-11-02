//! Direct assessment checkpoint and evidence partition contracts.
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
    Assessment::new_direct(
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
        assert_eq!(page["offset"], offset);
        let part = page["text"].as_str().unwrap();
        text.push_str(part);
        if page["complete"] == true {
            assert!(page["nextOffset"].is_null());
            assert_eq!(page["totalCharacters"], text.chars().count());
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

/// Collects bounded metadata pages without assuming how many units fit each page.
fn units(assessment: &Assessment) -> Vec<Value> {
    let mut after = 0;
    let mut units = Vec::new();
    loop {
        let page = assessment.units(after, 100).unwrap();
        assert!(serde_json::to_vec_pretty(&page).unwrap().len() < 16_384);
        let items = page["items"].as_array().unwrap();
        assert!(!items.is_empty());
        units.extend(items.iter().cloned());
        let Some(next) = page["nextOffset"].as_u64() else {
            break;
        };
        let next = usize::try_from(next).unwrap();
        assert_eq!(next, after + items.len());
        assert!(next > after);
        after = next;
    }
    units
}

fn note() -> Note {
    Note {
        observations: "Observed the exact synthetic behavior.".into(),
        interfaces: "The synthetic contract remains explicit.".into(),
        concerns: Vec::new(),
        interactions: Vec::new(),
        evidence: vec!["discussion".into()],
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

/// Supplies direct raw evidence before exercising semantic state transitions.
fn deliver_required(assessment: &mut Assessment, discussion: &str) -> String {
    let id = assessment.inventory.changes[0].id.clone();
    read(assessment, "policy", discussion);
    read(assessment, "discussion", discussion);
    read(assessment, &format!("{id}:patch"), discussion);
    id
}

/// Reconstructs every partition and its exact original document in source order.
fn assert_partitions(assessment: &mut Assessment, discussion: &str) {
    let mut documents = BTreeMap::<String, Vec<(usize, usize, String)>>::new();
    for unit in units(assessment) {
        let data = &unit["unit"];
        assert!(data["totalCharacters"].as_u64().unwrap() <= 16_384);
        let text = read(assessment, unit["reference"].as_str().unwrap(), discussion);
        let binding = read(
            assessment,
            unit["bindingReference"].as_str().unwrap(),
            discussion,
        );
        assert!(serde_json::from_str::<Value>(&binding).unwrap().is_array());
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
    assert!(!documents.is_empty());
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
        assert_eq!(reconstructed, read(assessment, &reference, discussion));
        if reference == "discussion" {
            assert_eq!(reconstructed, discussion);
        }
    }
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
        assert_partitions(&mut assessment, &discussion);
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
    assert!(
        units(&assessment)
            .iter()
            .any(|unit| unit["unit"]["ranges"].as_array().unwrap().len() > 1)
    );
    assert_partitions(&mut assessment, &discussion);
    assessment
        .validate("snapshot", "policy", "policy", &discussion)
        .unwrap();
}

#[test]
fn provider_thread_bindings_and_escaped_content_survive_partitioning() {
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
        assert!(all.len() > 1);
        let mut group = None;
        for unit in all {
            let binding = read(
                &mut assessment,
                unit["bindingReference"].as_str().unwrap(),
                &discussion,
            );
            let bindings: Value = serde_json::from_str(&binding).unwrap();
            if bindings
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["collection"] == collection)
            {
                let parent = unit["unit"]["parent"].as_str().unwrap();
                assert_eq!(group.get_or_insert_with(|| parent.to_owned()), parent);
                assert!(binding.contains("identityDigest"));
            }
        }
        assert!(group.is_some());
        assert_partitions(&mut assessment, &discussion);
        assessment
            .validate("snapshot", "policy", "policy", &discussion)
            .unwrap();
    }
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
    let mut assessment = Assessment::new_direct(
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
fn direct_progress_distinguishes_delivery_from_semantic_work_and_replays() {
    let discussion = "A discussion objection.";
    let mut assessment = fixture(discussion, "old\n", "new\n");
    let id = assessment.inventory.changes[0].id.clone();
    let before = assessment
        .progress_with_evidence("policy", discussion)
        .unwrap();
    assert_eq!(before["requiredDocumentCount"], 3);
    assert_eq!(before["deliveredRequiredDocumentCount"], 0);
    assert_eq!(before["judgedChangeCount"], 0);
    assessment
        .read("policy", 0, 1, "policy", discussion)
        .unwrap();
    assessment
        .read("policy", 0, 1, "policy", discussion)
        .unwrap();
    let partial = assessment
        .progress_with_evidence("policy", discussion)
        .unwrap();
    assert_eq!(
        partial["nextRequiredRead"],
        json!({"reference":"policy","offset":1})
    );
    read(&mut assessment, "policy", discussion);
    read(&mut assessment, &format!("{id}:patch"), discussion);
    let after = assessment
        .progress_with_evidence("policy", discussion)
        .unwrap();
    assert_eq!(after["deliveredRequiredDocumentCount"], 2);
    assert_eq!(after["judgedChangeCount"], 0);
    assert_eq!(after["cumulativeJudgmentRecorded"], false);
    assessment.checkpoint().unwrap_err();
}

#[test]
fn unit_notes_are_rejected_without_changing_direct_state() {
    let mut assessment = fixture("discussion", "old", "new");
    deliver_required(&mut assessment, "discussion");
    let unit = units(&assessment).remove(0);
    let reference = unit["reference"].as_str().unwrap();
    read(&mut assessment, reference, "discussion");
    read(
        &mut assessment,
        unit["bindingReference"].as_str().unwrap(),
        "discussion",
    );
    let mut observation = note();
    observation.evidence = vec![reference.into()];
    let before = serde_json::to_value(&assessment).unwrap();
    assessment
        .note(
            unit["unit"]["id"].as_str().unwrap(),
            observation,
            None,
            "policy",
            "discussion",
        )
        .unwrap_err();
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
}

#[test]
fn checkpoint_replay_and_replacement_do_not_create_progress() {
    let mut assessment = fixture("discussion", "old", "new");
    assessment.checkpoint().unwrap_err();
    let id = deliver_required(&mut assessment, "discussion");
    assessment.checkpoint().unwrap_err();
    let digest = assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment.checkpoint().unwrap();
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment.checkpoint().unwrap_err();
    let mut changed = judgment(&id, Disposition::Insignificant);
    changed.rationale.push_str(" More precise.");
    assessment
        .record(&id, changed, Some(&digest), "policy", "discussion")
        .unwrap();
    assessment.checkpoint().unwrap_err();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

#[test]
fn empty_inventory_still_requires_discussion_observation_and_cumulative_work() {
    let mut assessment = fixture("", "", "");
    read(&mut assessment, "policy", "");
    read(&mut assessment, "discussion", "");
    assessment.finish("policy", "").unwrap_err();
    assessment
        .note("discussion", note(), None, "policy", "")
        .unwrap();
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            "",
        )
        .unwrap();
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
fn many_check_dependencies_remain_exact_and_reject_tampering() {
    let mut assessment = fixture("discussion", "old", "new");
    let id = deliver_required(&mut assessment, "discussion");
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let mut observation = note();
    observation.concerns = (0..32).map(|index| format!("Concern {index}")).collect();
    assessment
        .note("discussion", observation, None, "policy", "discussion")
        .unwrap();
    read(&mut assessment, "discussion-note", "discussion");
    let checks = assessment.checks(0, 100).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(checks.len(), 32);
    for check in &checks {
        let content: Value = serde_json::from_str(&read(
            &mut assessment,
            check["reference"].as_str().unwrap(),
            "discussion",
        ))
        .unwrap();
        assert_eq!(content["id"], check["id"]);
        assert_eq!(content["origin"], check["origin"]);
    }
    for check in &checks {
        let mut resolution = judgment("cumulative", Disposition::Insignificant);
        resolution
            .evidence
            .push(checks[0]["reference"].as_str().unwrap().into());
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
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    let saved = serde_json::to_value(&assessment).unwrap();
    let mut corrupt = saved.clone();
    let id = checks[1]["id"].as_str().unwrap();
    let reference = checks[0]["reference"].as_str().unwrap();
    corrupt["checkpoints"]["resolutions"][id]["dependencies"]
        .as_object_mut()
        .unwrap()
        .remove(reference);
    let corrupt: Assessment = serde_json::from_value(corrupt).unwrap();
    corrupt
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap_err();
    let mut corrupt = saved;
    corrupt["reads"][reference] = json!(0);
    let corrupt: Assessment = serde_json::from_value(corrupt).unwrap();
    corrupt
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap_err();
}

#[test]
fn replacing_change_judgment_invalidates_resolutions_citing_its_check() {
    let mut assessment = fixture("discussion", "old", "new");
    let id = deliver_required(&mut assessment, "discussion");
    let mut observation = note();
    observation.concerns.push("Independent concern".into());
    assessment
        .note("discussion", observation, None, "policy", "discussion")
        .unwrap();
    read(&mut assessment, "discussion-note", "discussion");
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
        .find(|c| c["origin"] == "discussion-note")
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
fn discussion_replacement_invalidates_cross_check_resolutions_and_requires_digest() {
    let mut assessment = fixture("discussion", "old", "new");
    let id = deliver_required(&mut assessment, "discussion");
    let mut source = judgment(&id, Disposition::Significant);
    source.concerns.push("Source concern".into());
    source.concerns.push("Independent source concern".into());
    assessment
        .record(&id, source, None, "policy", "discussion")
        .unwrap();
    read(&mut assessment, &format!("judgment:{id}"), "discussion");
    let mut observation = note();
    observation.concerns.push("Discussion concern".into());
    let digest = assessment
        .note("discussion", observation, None, "policy", "discussion")
        .unwrap();
    read(&mut assessment, "discussion-note", "discussion");
    let checks = assessment.checks(0, 100).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone();
    let source = checks
        .iter()
        .find(|check| check["origin"] == format!("judgment:{id}"))
        .unwrap();
    let independent = checks
        .iter()
        .find(|check| check["origin"] == format!("judgment:{id}") && check["id"] != source["id"])
        .unwrap();
    let discussion = checks
        .iter()
        .find(|check| check["origin"] == "discussion-note")
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
        .push(discussion["reference"].as_str().unwrap().into());
    assessment
        .resolve(
            source["id"].as_str().unwrap(),
            resolution,
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .resolve(
            independent["id"].as_str().unwrap(),
            judgment("cumulative", Disposition::Significant),
            None,
            "policy",
            "discussion",
        )
        .unwrap();
    let before = serde_json::to_value(&assessment).unwrap();
    let mut changed = note();
    changed.concerns.push("Changed discussion concern".into());
    assessment
        .note(
            "discussion",
            serde_json::from_value(serde_json::to_value(&changed).unwrap()).unwrap(),
            Some("stale"),
            "policy",
            "discussion",
        )
        .unwrap_err();
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
    assessment
        .note("discussion", changed, Some(&digest), "policy", "discussion")
        .unwrap();
    let remaining = assessment.checks(0, 100).unwrap();
    let remaining = remaining["items"].as_array().unwrap();
    assert!(
        remaining
            .iter()
            .find(|check| check["id"] == source["id"])
            .unwrap()["resolutionDigest"]
            .is_null()
    );
    assert!(
        remaining
            .iter()
            .find(|check| check["id"] == independent["id"])
            .unwrap()["resolutionDigest"]
            .is_string()
    );
    assert!(
        !remaining
            .iter()
            .any(|check| check["id"] == discussion["id"])
    );
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
}

#[test]
fn recorded_concerns_cannot_be_erased_by_a_favorable_aggregate() {
    let discussion = "Historical objection.";
    let mut assessment = fixture(discussion, "old", "new");
    let id = deliver_required(&mut assessment, discussion);
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            None,
            "policy",
            discussion,
        )
        .unwrap();
    let mut observation = note();
    observation
        .concerns
        .push("The historical body identifies changed behavior.".into());
    let digest = assessment
        .note("discussion", observation, None, "policy", discussion)
        .unwrap();
    read(&mut assessment, "discussion-note", discussion);
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            discussion,
        )
        .unwrap_err();
    let check = assessment.checks(0, 100).unwrap()["items"][0].clone();
    read(
        &mut assessment,
        check["reference"].as_str().unwrap(),
        discussion,
    );
    let mut invalid = judgment("cumulative", Disposition::Significant);
    invalid.evidence = vec!["missing:reference".into()];
    assessment
        .resolve(
            check["id"].as_str().unwrap(),
            invalid,
            None,
            "policy",
            discussion,
        )
        .unwrap_err();
    assessment
        .resolve(
            check["id"].as_str().unwrap(),
            judgment("cumulative", Disposition::Significant),
            None,
            "policy",
            discussion,
        )
        .unwrap();
    read(
        &mut assessment,
        check["resolutionReference"].as_str().unwrap(),
        discussion,
    );
    let mut recursive = note();
    recursive
        .evidence
        .push(check["resolutionReference"].as_str().unwrap().into());
    let before = serde_json::to_value(&assessment).unwrap();
    assessment
        .note("discussion", recursive, Some(&digest), "policy", discussion)
        .unwrap_err();
    assert_eq!(serde_json::to_value(&assessment).unwrap(), before);
    assessment
        .record(
            "cumulative",
            judgment("cumulative", Disposition::Insignificant),
            None,
            "policy",
            discussion,
        )
        .unwrap();
    assessment.finish("policy", discussion).unwrap();
    assert_eq!(assessment.status(true), "needsReview");
}

#[test]
fn new_context_invalidates_affected_judgment_discussion_and_checks() {
    let mut assessment = fixture("discussion", "old", "new");
    let id = deliver_required(&mut assessment, "discussion");
    let mut source = judgment(&id, Disposition::Insignificant);
    source.concerns.push("Source concern".into());
    assessment
        .record(&id, source, None, "policy", "discussion")
        .unwrap();
    read(&mut assessment, &format!("judgment:{id}"), "discussion");
    let mut observation = note();
    observation.concerns.push("Discussion concern".into());
    assessment
        .note("discussion", observation, None, "policy", "discussion")
        .unwrap();
    read(&mut assessment, "discussion-note", "discussion");
    let checks = assessment.checks(0, 100).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(checks.len(), 2);
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
    let first = assessment
        .source_context(
            ContextRequest {
                change_id: id.clone(),
                side: SourceSide::New,
                start_line: NonZeroUsize::MIN,
                line_count: NonZeroUsize::MIN,
            },
            1,
            "policy",
            "discussion",
        )
        .unwrap();
    assessment
        .validate("snapshot", "policy", "policy", "discussion")
        .unwrap();
    let remaining = assessment.checks(0, 100).unwrap();
    assert!(remaining["items"].as_array().unwrap().is_empty());
    let summary = assessment.summary(0, 10, "policy", "discussion").unwrap();
    assert!(summary["changes"][0]["decision"].is_null());
    assert!(summary["discussionNoteDigest"].is_null());
    assert!(summary["cumulative"].is_null());
    assessment.finish("policy", "discussion").unwrap_err();
    assessment
        .record(
            &id,
            judgment(&id, Disposition::Insignificant),
            None,
            "policy",
            "discussion",
        )
        .unwrap_err();
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
        .unwrap();
}

#[test]
fn incomplete_records_reject_tampered_checkpoint_state() {
    let mut assessment = fixture("discussion", "old", "new");
    let id = deliver_required(&mut assessment, "discussion");
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
        .note("discussion", note(), None, "policy", "discussion")
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
    let original = serde_json::to_value(&assessment).unwrap();
    for (pointer, value) in [
        ("/checkpoints/manifestDigest", json!("changed")),
        ("/checkpoints/aggregates/cumulative", json!({})),
        ("/checkpoints/discussion/value/evidence", json!(["policy"])),
        ("/checkpoints/discussion/dependencies", json!({})),
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
fn recursive_judgment_and_resolution_citations_fail_before_replacing_state() {
    let mut assessment = fixture("discussion", "old", "new");
    let id = deliver_required(&mut assessment, "discussion");
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
