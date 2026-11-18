//! Exercises authored provenance refusals through the native command boundary.

#![allow(
    clippy::indexing_slicing,
    reason = "The test uses fixed JSON fixtures and checks their documented fields"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

/// Keeps the independent native command fixture inside the checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".finding-provenance-{}", std::process::id()));
        #[expect(clippy::create_dir, reason = "Fixture collisions must fail")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Invokes the public workflow command without shell interpolation.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ss-ai"))
        .arg("workflow")
        .args(args)
        .output()
        .unwrap()
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Tool-owned fields cannot override operation-assigned finding provenance.
#[test]
#[cfg_attr(miri, ignore = "requires a native subprocess")]
#[cfg_attr(mutants, ignore = "mutated native subprocesses may block")]
fn authored_finding_provenance_fields_identify_source_and_preserve_artifact() {
    let workspace = Workspace::new();
    let artifact = workspace.write(
        "reviewer.json",
        &json!({"schemaVersion":1,"kind":"reviewer","reviewerId":"g1",
            "focusArea":"correctness","status":"open","findings":[]}),
    );
    let original = fs::read(&artifact).unwrap();
    let authored = json!({
        "title":"Problem","description":"Details","reasoning":"Why",
        "consequence":"Impact","recommendedAction":"Fix",
        "anchor":{"kind":"file","path":"src/lib.rs"}
    });
    for (field, value) in [
        ("id", json!("manual")),
        ("origin", json!("reviewer")),
        ("schemaVersion", json!(1)),
        ("sourceFindingIds", json!(["source-1"])),
    ] {
        for value in [value, Value::Null] {
            let mut input = authored.clone();
            input[field] = value;
            let path = workspace.write("finding.json", &input);
            let output = run(&[
                "add-finding",
                "--artifact-path",
                text(&artifact),
                "--finding-path",
                text(&path),
                "--origin",
                "synthesis",
                "--source-id",
                "source-1",
            ]);
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{field}: {error}");
            assert!(output.stdout.is_empty());
            assert!(error.contains(text(&path)), "{error}");
            assert!(
                error.contains(&format!("'{field}', which the tooling assigns")),
                "{error}"
            );
            assert_eq!(fs::read(&artifact).unwrap(), original);
        }
    }

    let input = workspace.write("finding.json", &authored);
    let output = run(&[
        "add-finding",
        "--artifact-path",
        text(&artifact),
        "--finding-path",
        text(&input),
        "--origin",
        "synthesis",
        "--source-id",
        "source-1",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["findingId"], "g1-1");
    let document: Value = serde_json::from_slice(&fs::read(&artifact).unwrap()).unwrap();
    assert_eq!(document["findings"][0]["id"], "g1-1");
    assert_eq!(document["findings"][0]["schemaVersion"], 1);
    assert_eq!(document["findings"][0]["origin"], "synthesis");
    assert_eq!(
        document["findings"][0]["sourceFindingIds"],
        json!(["source-1"])
    );
}
