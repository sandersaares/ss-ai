//! Exercises bounded reference registration, inheritance, and finalization.
#![no_main]
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "The generated operations and reference sets have fixed small bounds"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use libfuzzer_sys::fuzz_target;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review_workflow::focus_area_expansion::{
    Operation, ReferenceInput, ReferenceResult, update,
};
use ss_ai_runtime::Clock;

// Bound both execution time and the number of registered referring contexts.
const MAX_OPERATIONS: usize = 32;
const PARENT_SCOPES: u8 = 4;
const CHILD_SCOPES: u8 = 3;

/// Detects unexpected wall-clock access or contention in isolated fixtures.
struct FakeClock;

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp::MIN
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("Each fuzz input owns its fixtures without contention");
    }
}

/// Keeps each replay's source snapshot and manifest in a separate directory.
struct Fixture {
    root: PathBuf,
    manifest: PathBuf,
}

impl Fixture {
    #[expect(
        clippy::create_dir,
        reason = "Fixtures must never reuse another execution's files"
    )]
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The counter only assigns unique fixture names, not shared data.
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".focus-expansion-fuzz-{}-{id}", std::process::id()));
        fs::create_dir(&root).unwrap();
        for directory in ["instruction-set-source", "instruction-set-raw"] {
            fs::create_dir(root.join(directory)).unwrap();
        }
        fs::write(root.join("instruction-set-source/source.md"), "Direct\n").unwrap();
        for name in ["parent.md", "child.md", "extra.md"] {
            fs::write(root.join("instruction-set-raw").join(name), "Reference\n").unwrap();
        }
        Self {
            manifest: root.join("focus-area-expansion-status.json"),
            root,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Tracks expected reference outcomes independently of the stored manifest.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Pending,
    Complete,
    Failed,
}

/// Models the small acyclic graph and its ordered referring alternatives.
struct Model {
    parent_scopes: Vec<u8>,
    child_scopes: Vec<u8>,
    child: Outcome,
    extra: Option<Outcome>,
    finalized: bool,
}

/// Builds one exact source span shared by the authored reference and oracle.
fn span(root: &str, path: &str) -> Value {
    json!({"root": root, "path": path, "startLine": 1, "endLine": 1})
}

/// Constructs a direct or inherited edge with explicit declaration evidence.
fn referrer(parent: Option<&str>, scope: &str) -> Value {
    let source = if parent.is_some() {
        span("instruction-set-raw", "parent.md")
    } else {
        span("instruction-set-source", "source.md")
    };
    json!({"parentId": parent, "source": source, "scope": scope, "declarations": [source]})
}

/// Uses the public wire schema to build typed registration operations.
fn register(id: &str, uri: &str, parent: Option<&str>, scope: &str) -> Operation {
    let input: ReferenceInput = serde_json::from_value(json!({
        "id": id, "uri": uri, "referrers": [referrer(parent, scope)]
    }))
    .unwrap();
    Operation::Register(input)
}

/// Constructs a successful result for a target's unique output.
fn complete(id: &str, output: &str) -> Operation {
    Operation::Complete(ReferenceResult {
        reference_id: id.to_owned(),
        outputs: vec![output.to_owned()],
        error: None,
    })
}

/// Constructs a failed result without claiming an output.
fn fail(id: &str) -> Operation {
    Operation::Fail(ReferenceResult {
        reference_id: id.to_owned(),
        outputs: Vec::new(),
        error: Some("Unavailable".to_owned()),
    })
}

/// Applies the same operation to independent sessions and checks atomic rejection.
fn apply(fixtures: &[Fixture; 2], make: impl Fn() -> Operation, accepted: bool) {
    let mut previous_result = None;
    for fixture in fixtures {
        let before = fs::read(&fixture.manifest).ok();
        let result = update(&fixture.root, make(), &FakeClock);
        assert_eq!(result.is_ok(), accepted);
        if accepted {
            let result = result.unwrap();
            if let Some(previous) = &previous_result {
                assert_eq!(&result, previous);
            }
            previous_result = Some(result);
        } else {
            assert_eq!(fs::read(&fixture.manifest).ok(), before);
        }
    }
    assert_eq!(
        fs::read(&fixtures[0].manifest).ok(),
        fs::read(&fixtures[1].manifest).ok()
    );
}

/// Builds one expected stored record from the independent outcome model.
fn record(id: &str, referrers: &[Value], outcome: Outcome) -> Value {
    let (state, outputs, error) = match outcome {
        Outcome::Pending => ("Pending", Vec::new(), None),
        Outcome::Complete => ("Complete", vec![format!("{id}.md")], None),
        Outcome::Failed => ("Failed", Vec::new(), Some("Unavailable")),
    };
    json!({
        "id": id, "uri": format!("https://example.invalid/{id}"),
        "referrers": referrers, "state": state, "outputs": outputs, "error": error
    })
}

/// Returns the expected context link for an authored edge.
fn link(id: &str, parent: Option<&str>, scope: &str) -> Value {
    let edge = referrer(parent, scope);
    json!({
        "referenceId": id, "uri": format!("https://example.invalid/{id}"),
        "source": edge["source"], "scope": scope, "declarations": edge["declarations"]
    })
}

/// Compares the complete persisted record and inherited chains with the model.
fn check(fixtures: &[Fixture; 2], model: &Model) {
    let parent_referrers: Vec<_> = model
        .parent_scopes
        .iter()
        .map(|scope| referrer(None, &format!("parent-{scope}")))
        .collect();
    let child_referrers: Vec<_> = model
        .child_scopes
        .iter()
        .map(|scope| referrer(Some("parent"), &format!("child-{scope}")))
        .collect();
    let mut references = vec![
        record("parent", &parent_referrers, Outcome::Complete),
        record("child", &child_referrers, model.child),
    ];
    if let Some(outcome) = model.extra {
        references.push(record("extra", &[referrer(None, "extra")], outcome));
    }
    let state = if !model.finalized {
        "Unknown"
    } else if model.child == Outcome::Failed || model.extra == Some(Outcome::Failed) {
        "Degraded"
    } else {
        "Complete"
    };
    let expected = json!({"schemaVersion": 1, "state": state, "references": references});
    let contexts = model
        .child_scopes
        .iter()
        .flat_map(|child| {
            model.parent_scopes.iter().map(move |parent| {
                json!({"chain": [
                    link("parent", None, &format!("parent-{parent}")),
                    link("child", Some("parent"), &format!("child-{child}"))
                ]})
            })
        })
        .collect::<Vec<_>>();
    for fixture in fixtures {
        let before = fs::read(&fixture.manifest).unwrap();
        let actual: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(actual, expected);
        let summary = update(
            &fixture.root,
            Operation::Read(Some("child".to_owned())),
            &FakeClock,
        )
        .unwrap();
        assert_eq!(summary["state"], state);
        assert_eq!(summary["referenceCount"], references.len());
        assert_eq!(summary["contexts"], json!(contexts));
        assert_eq!(fs::read(&fixture.manifest).unwrap(), before);
    }
}

/// Applies a generated operation and updates the independent model on success.
#[expect(
    clippy::integer_division,
    reason = "The quotient selects a bounded scope independently of the operation remainder"
)]
fn step(fixtures: &[Fixture; 2], model: &mut Model, byte: u8) {
    let parent_scope = (byte / 12) % PARENT_SCOPES;
    let child_scope = (byte / 12) % CHILD_SCOPES;
    let mutable = !model.finalized;
    match byte % 12 {
        0 => {
            apply(
                fixtures,
                || {
                    register(
                        "parent",
                        "https://example.invalid/parent",
                        None,
                        &format!("parent-{parent_scope}"),
                    )
                },
                mutable,
            );
            if mutable && !model.parent_scopes.contains(&parent_scope) {
                model.parent_scopes.push(parent_scope);
            }
        }
        1 => {
            apply(
                fixtures,
                || {
                    register(
                        "child",
                        "https://example.invalid/child",
                        Some("parent"),
                        &format!("child-{child_scope}"),
                    )
                },
                mutable,
            );
            if mutable && !model.child_scopes.contains(&child_scope) {
                model.child_scopes.push(child_scope);
            }
        }
        2 => {
            apply(fixtures, || complete("child", "child.md"), mutable);
            if mutable {
                model.child = Outcome::Complete;
            }
        }
        3 => {
            apply(fixtures, || fail("child"), mutable);
            if mutable {
                model.child = Outcome::Failed;
            }
        }
        4 => {
            apply(
                fixtures,
                || register("extra", "https://example.invalid/extra", None, "extra"),
                mutable,
            );
            if mutable && model.extra.is_none() {
                model.extra = Some(Outcome::Pending);
            }
        }
        5 => {
            let accepted = mutable && model.extra.is_some();
            apply(fixtures, || complete("extra", "extra.md"), accepted);
            if accepted {
                model.extra = Some(Outcome::Complete);
            }
        }
        6 => {
            let accepted = mutable && model.extra.is_some();
            apply(fixtures, || fail("extra"), accepted);
            if accepted {
                model.extra = Some(Outcome::Failed);
            }
        }
        7 => {
            let accepted = model.extra != Some(Outcome::Pending);
            apply(fixtures, || Operation::Finalize, accepted);
            if accepted {
                model.finalized = true;
            }
        }
        // These transactions are invalid regardless of finalization.
        8 => apply(
            fixtures,
            || {
                register(
                    "parent",
                    "https://example.invalid/parent",
                    Some("parent"),
                    "cycle",
                )
            },
            false,
        ),
        9 => apply(
            fixtures,
            || {
                register(
                    "parent",
                    "https://example.invalid/conflict",
                    None,
                    "conflict",
                )
            },
            false,
        ),
        10 => apply(fixtures, || complete("child", "parent.md"), false),
        _ => apply(fixtures, || fail("parent"), false),
    }
    check(fixtures, model);
}

fuzz_target!(|input: &[u8]| {
    let fixtures = [Fixture::new(), Fixture::new()];
    apply(&fixtures, || Operation::Initialize, true);
    apply(
        &fixtures,
        || register("parent", "https://example.invalid/parent", None, "parent-0"),
        true,
    );
    apply(&fixtures, || Operation::Finalize, false);
    apply(&fixtures, || complete("parent", "parent.md"), true);
    apply(
        &fixtures,
        || {
            register(
                "child",
                "https://example.invalid/child",
                Some("parent"),
                "child-0",
            )
        },
        true,
    );
    apply(&fixtures, || complete("child", "child.md"), true);
    let mut model = Model {
        parent_scopes: vec![0],
        child_scopes: vec![0],
        child: Outcome::Complete,
        extra: None,
        finalized: false,
    };
    // A late parent referrer must immediately extend the child's chains.
    step(&fixtures, &mut model, 12);
    for byte in input.iter().take(MAX_OPERATIONS) {
        step(&fixtures, &mut model, *byte);
    }
    if model.extra == Some(Outcome::Pending) {
        step(&fixtures, &mut model, 5);
    }
    step(&fixtures, &mut model, 7);
    // Finalized records reject further writes while remaining readable.
    step(&fixtures, &mut model, 0);
    step(&fixtures, &mut model, 7);
});
