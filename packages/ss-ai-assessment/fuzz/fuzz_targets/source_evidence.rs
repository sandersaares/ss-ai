//! Bounded malformed-patch and operation-order coverage for the exact evidence contract.
#![no_main]
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Bounded fuzz assertions inspect native response shapes and a proven nonzero unit count"
)]

use std::num::NonZeroUsize;

use libfuzzer_sys::fuzz_target;
use ss_ai_assessment::{
    Assessment, Change, Content, ContextRequest, Disposition, Inventory, Judgment, Note, Side,
    SourceSide, change_id,
};
use ss_ai_git::validate_patch;

// Keep individual parser and state-machine cases small, including oversized-line cases.
const MAX_INPUT_BYTES: usize = 64 * 1024;
// Exercise permutations without allowing a generated operation stream to run indefinitely.
const MAX_OPERATIONS: usize = 64;

fuzz_target!(|input: &[u8]| {
    if input.len() > MAX_INPUT_BYTES {
        return;
    }
    let Ok(text) = str::from_utf8(input) else {
        return;
    };
    // Independent arbitrary discussion reaches JSON/thread segmentation even when the
    // generated source patch is malformed. The immutable operation bound prevents loops.
    if let Ok(mut discussion) = Assessment::new(
        Inventory {
            old_commit: "a".repeat(40),
            new_commit: "b".repeat(40),
            old_tree: "c".repeat(40),
            new_tree: "c".repeat(40),
            changes: Vec::new(),
            complete: true,
        },
        &[],
        "snapshot",
        "policy",
        text,
    ) {
        for operation in input.iter().take(MAX_OPERATIONS) {
            unit_operation(&mut discussion, usize::from(*operation), text);
            discussion.validate("snapshot", "policy", "", text).unwrap();
        }
    }
    let mut fields = text.splitn(3, "\n===\n");
    let old_text = fields.next().unwrap_or("");
    let new_text = fields.next().unwrap_or("");
    let patch = fields.next().unwrap_or("");
    let _parsed = validate_patch(patch, old_text, new_text);
    let old = Some(Side {
        path: "file".into(),
        object: "a".repeat(40),
        mode: "100644".into(),
        content: Content::Text {
            text: old_text.into(),
        },
    });
    let new = Some(Side {
        path: "file".into(),
        object: "b".repeat(40),
        mode: "100644".into(),
        content: Content::Text {
            text: new_text.into(),
        },
    });
    let id = change_id("M", old.as_ref(), new.as_ref()).unwrap();
    let inventory = Inventory {
        old_commit: "a".repeat(40),
        new_commit: "b".repeat(40),
        old_tree: "c".repeat(40),
        new_tree: "d".repeat(40),
        complete: true,
        changes: vec![Change {
            id: id.clone(),
            kind: "M".into(),
            old,
            new,
        }],
    };
    let Ok(mut assessment) = Assessment::new(inventory, &[patch.into()], "snapshot", "policy", "")
    else {
        return;
    };
    for (index, operation) in input.iter().take(MAX_OPERATIONS).enumerate() {
        // The small selector deliberately includes invalid offsets/ranges and post-finalization calls.
        match operation % 7 {
            0 => {
                let _read = assessment.read(
                    &format!("{id}:patch"),
                    index,
                    usize::from(*operation).saturating_add(1),
                    "",
                    "",
                );
            }
            1 => {
                let _context = assessment.source_context(
                    ContextRequest {
                        change_id: id.clone(),
                        side: SourceSide::New,
                        start_line: NonZeroUsize::new(index.saturating_add(1)).unwrap(),
                        line_count: NonZeroUsize::MIN,
                    },
                    1,
                    "",
                    "",
                );
            }
            2 => {
                let _read = assessment.read("policy", 0, 1, "", "");
            }
            3 => {
                let _judgment = assessment.record(
                    &id,
                    Judgment {
                        concerns: Vec::new(),
                        interactions: Vec::new(),
                        disposition: Disposition::Significant,
                        rationale: "synthetic".into(),
                        evidence: vec![format!("{id}:patch")],
                    },
                    None,
                    "",
                    "",
                );
            }
            4 => {
                let _complete = assessment.finish("", "");
            }
            5 => unit_operation(&mut assessment, index, ""),
            _ => {
                let _checkpoint = assessment.checkpoint();
            }
        }
        assessment.validate("snapshot", "policy", "", "").unwrap();
        let restored: Assessment =
            serde_json::from_slice(&serde_json::to_vec(&assessment).unwrap()).unwrap();
        restored.validate("snapshot", "policy", "", "").unwrap();
    }
});

/// Generated unit work mixes partial delivery, idempotent notes, replacement, and paging.
fn unit_operation(assessment: &mut Assessment, selector: usize, discussion: &str) {
    let _policy = assessment.read("policy", 0, 1, "", discussion);
    let count = usize::try_from(assessment.progress()["unitCount"].as_u64().unwrap()).unwrap();
    assert!(count > 0);
    let page = assessment.units(selector % count, 1).unwrap();
    let unit = &page["items"][0];
    let reference = unit["reference"].as_str().unwrap();
    let offset = unit["readOffset"]
        .as_u64()
        .map_or(0, |offset| usize::try_from(offset).unwrap());
    let result = assessment.read(
        reference,
        offset,
        selector.saturating_add(1),
        "",
        discussion,
    );
    if let Ok(result) = result
        && result["complete"] == false
    {
        assert!(result["nextOffset"].as_u64().unwrap() > u64::try_from(offset).unwrap());
    }
    let previous = unit["noteDigest"].as_str();
    let _note = assessment.note(
        unit["unit"]["id"].as_str().unwrap(),
        Note {
            observations: format!("Synthetic {selector}"),
            interfaces: "Synthetic invariant".into(),
            concerns: vec!["Synthetic concern".into()],
            interactions: Vec::new(),
            evidence: vec![reference.into()],
        },
        previous,
        "",
        discussion,
    );
    let _read = assessment.read(
        unit["noteReference"].as_str().unwrap(),
        0,
        selector.saturating_add(1),
        "",
        discussion,
    );
    let _checkpoint = assessment.checkpoint();
}
