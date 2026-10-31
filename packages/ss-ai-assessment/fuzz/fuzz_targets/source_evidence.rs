//! Bounded malformed-patch and operation-order coverage for the exact evidence contract.
#![no_main]

use std::num::NonZeroUsize;

use libfuzzer_sys::fuzz_target;
use ss_ai_assessment::{
    Assessment, Change, Content, ContextRequest, Disposition, Inventory, Judgment, Side,
    SourceSide, change_id,
};
use ss_ai_git::validate_patch;

// Keep individual parser and state-machine cases small, including oversized-line cases.
const MAX_INPUT_BYTES: usize = 16 * 1024;
// Exercise permutations without allowing a generated operation stream to run indefinitely.
const MAX_OPERATIONS: usize = 64;

fuzz_target!(|input: &[u8]| {
    if input.len() > MAX_INPUT_BYTES {
        return;
    }
    let Ok(text) = str::from_utf8(input) else {
        return;
    };
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
    let mut patch_offset = 0;
    for (index, operation) in input.iter().take(MAX_OPERATIONS).enumerate() {
        // The small selector deliberately includes invalid offsets/ranges and post-finalization calls.
        match operation % 8 {
            0 => {
                let read = assessment.read(
                    &format!("{id}:patch"),
                    patch_offset,
                    MAX_INPUT_BYTES,
                    "",
                    "",
                );
                if let Ok(page) = read {
                    patch_offset = page["nextOffset"]
                        .as_u64()
                        .and_then(|offset| usize::try_from(offset).ok())
                        .unwrap_or(patch_offset);
                }
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
                let _judgment = assessment.record(
                    "cumulative",
                    Judgment {
                        disposition: Disposition::Significant,
                        rationale: "synthetic".into(),
                        evidence: vec!["policy".into(), "discussion".into()],
                    },
                    None,
                    "",
                    "",
                );
            }
            5 => {
                let _read = assessment.read("discussion", 0, 1, "", "");
            }
            6 => {
                let _complete = assessment.finish("", "");
            }
            _ => {
                let _read = assessment.read(&format!("{id}:patch"), index, 1, "", "");
            }
        }
        assessment.validate("snapshot", "policy", "", "").unwrap();
        let restored: Assessment =
            serde_json::from_slice(&serde_json::to_vec(&assessment).unwrap()).unwrap();
        restored.validate("snapshot", "policy", "", "").unwrap();
    }
});
