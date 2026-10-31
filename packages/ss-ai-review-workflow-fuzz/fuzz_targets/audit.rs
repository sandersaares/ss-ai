//! Exercises ordered audit decisions and rejected batches over bounded sequences.
#![no_main]
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "The generated identifiers and operation sequences are bounded"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};
use ss_ai_review_workflow::audit::{add_decisions, last_decision, read_decisions};
use ss_ai_runtime::SystemClock;

// Bound filesystem work per input while allowing repeated decisions and replays.
const MAX_OPERATIONS: usize = 24;
const IDS: [&str; 4] = ["F-1", "F-2", "F-3", "F-4"];
// A fixed timestamp makes independently replayed journals directly comparable.
const TIMESTAMP: &str = "2026-01-01T00:00:00Z";

/// Isolates journals and plans from other inputs and concurrent workers.
struct Files {
    root: PathBuf,
    plan: PathBuf,
    journals: [PathBuf; 2],
}

impl Files {
    #[expect(
        clippy::create_dir,
        reason = "An existing directory must not be reused"
    )]
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The counter only generates distinct names within the process.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".audit-fuzz-{}-{id}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self {
            plan: root.join("plan.json"),
            journals: [root.join("first.json"), root.join("replay.json")],
            root,
        }
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Applies a complete batch to independent journals and checks an explicit model.
fn apply(files: &Files, entries: Vec<Value>, policy: bool, accepted: bool, model: &mut Vec<Value>) {
    fs::write(
        &files.plan,
        serde_json::to_vec(&json!({"schemaVersion": 1, "decisions": entries})).unwrap(),
    )
    .unwrap();
    let clock = SystemClock::default();
    for path in &files.journals {
        let before = fs::read(path).ok();
        let result = add_decisions(path, &files.plan, None, policy, TIMESTAMP, &clock);
        assert_eq!(result.is_ok(), accepted);
        if !accepted {
            assert_eq!(fs::read(path).ok(), before, "rejected batch changed state");
        }
    }
    if accepted {
        for mut entry in entries {
            entry["recordedUtc"] = json!(TIMESTAMP);
            model.push(entry);
        }
    }
    let expected = json!({"schemaVersion": 1, "decisions": model});
    for path in &files.journals {
        let actual = read_decisions(path, &clock).unwrap();
        assert_eq!(actual, expected);
        for id in IDS.into_iter().chain(["missing"]) {
            let last = model
                .iter()
                .rev()
                .find(|record| record["findingId"] == id)
                .map(|record| record["decision"].as_str().unwrap());
            assert_eq!(last_decision(&actual, id).unwrap(), last);
        }
    }
}

fuzz_target!(|input: &[u8]| {
    let files = Files::new();
    let mut model = Vec::new();
    // Every input starts with policy classifications followed by user decisions
    // for the same findings, so the sequence always reaches migrated states.
    apply(
        &files,
        vec![
            json!({"findingId": IDS[0], "decision": "needs-audit"}),
            json!({"findingId": IDS[1], "decision": "auto-approved"}),
        ],
        true,
        true,
        &mut model,
    );
    apply(
        &files,
        vec![
            json!({"findingId": IDS[0], "decision": "approved", "feedback": "  Reviewed\n  "}),
            json!({"findingId": IDS[1], "decision": "rejected"}),
        ],
        false,
        true,
        &mut model,
    );
    // Reject whole batches after a valid prefix, including policy restrictions.
    for (entries, policy) in [
        (
            vec![
                json!({"findingId": IDS[2], "decision": "needs-audit"}),
                json!({"findingId": IDS[3], "decision": "approved"}),
            ],
            true,
        ),
        (
            vec![
                json!({"findingId": IDS[2], "decision": "auto-approved"}),
                json!({"findingId": IDS[2], "decision": "needs-audit"}),
            ],
            true,
        ),
        (
            vec![
                json!({"findingId": IDS[2], "decision": "needs-audit", "feedback": "user response"}),
            ],
            true,
        ),
    ] {
        apply(&files, entries, policy, false, &mut model);
    }

    for byte in input.iter().take(MAX_OPERATIONS) {
        let id = IDS[usize::from(byte / 8) % IDS.len()];
        let other = IDS[(usize::from(byte / 8) + 1) % IDS.len()];
        let (entries, policy, accepted) = match byte % 8 {
            0 => (
                vec![json!({"findingId": id, "decision": "needs-audit"})],
                true,
                true,
            ),
            1 => (
                vec![json!({"findingId": id, "decision": "auto-approved"})],
                true,
                true,
            ),
            2 => (
                vec![json!({"findingId": id, "decision": "approved", "reason": "reviewed"})],
                false,
                true,
            ),
            3 => (
                vec![
                    json!({"findingId": id, "decision": "dropped", "feedback": "  user response  "}),
                ],
                false,
                true,
            ),
            4 => (
                vec![
                    json!({"findingId": id, "decision": "needs-audit"}),
                    json!({"findingId": other, "decision": "approved"}),
                ],
                true,
                false,
            ),
            5 => (
                vec![
                    json!({"findingId": id, "decision": "auto-approved"}),
                    json!({"findingId": id, "decision": "needs-audit"}),
                ],
                true,
                false,
            ),
            6 => (
                vec![
                    json!({"findingId": id, "decision": "needs-audit", "feedback": "user response"}),
                ],
                true,
                false,
            ),
            _ => (
                vec![
                    json!({"findingId": id, "decision": "approved"}),
                    json!({"findingId": other, "decision": "invalid"}),
                ],
                false,
                false,
            ),
        };
        apply(&files, entries, policy, accepted, &mut model);
    }
});
