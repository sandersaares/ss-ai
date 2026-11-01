//! Exercises reuse and rejection of frozen overview artifacts without remote writes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai_review_workflow::overview_artifact::{Preparation, freeze};
use ss_ai_runtime::{Clock, Process, ProcessOutput, classification, fail};

/// Rejects an unexpected delay instead of relying on real time.
struct NoWait;

impl Clock for NoWait {
    fn now(&self) -> Timestamp {
        Timestamp::MIN
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        Err(fail("contention", "Unexpected lock contention"))
    }
}

/// An existing manifest must not require tool discovery or rendering.
struct NoProcess;

impl Process for NoProcess {
    fn execute(&self, _: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        panic!("existing manifest must not invoke a process")
    }
}

/// Owns an isolated checkout-local fixture and removes it after the test.
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        // The counter only assigns distinct names; no fixture state is shared,
        // so it does not need to order other memory accesses.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".overview-manifest-contract-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        #[expect(clippy::create_dir, reason = "The fixture must be newly created")]
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Uses the persisted hash format to model a previously completed render.
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Reuses a manifest through the same public boundary used by publication.
fn reuse(root: &Path, manifest: &Value) -> Result<Preparation, AppError> {
    let output = root.join("generation");
    fs::write(
        output.join("html.json"),
        serde_json::to_vec(manifest).unwrap(),
    )
    .unwrap();
    freeze(
        &root.join("source.md"),
        &hash(b"overview"),
        &output,
        root,
        root,
        &NoProcess,
        &NoWait,
        |_, _, _, _| panic!("existing manifest must not render"),
    )
}

/// Both stored artifact hashes and paths protect the frozen publication.
#[test]
fn existing_manifest_reuses_both_layouts_and_rejects_modified_artifacts() {
    let fixture = Fixture::new();
    let root = &fixture.0;
    let output = root.join("generation");
    fs::create_dir_all(&output).unwrap();
    fs::write(root.join("source.md"), b"overview").unwrap();

    for directory in [output.clone(), output.join(".overview-attempt-1")] {
        fs::create_dir_all(&directory).unwrap();
        let html_path = directory.join("report.html");
        let archive_path = directory.join("report.zip");
        let html = b"<html>R\xc3\xa9sum\xc3\xa9</html>";
        let archive = b"frozen archive";
        fs::write(&html_path, html).unwrap();
        fs::write(&archive_path, archive).unwrap();
        let manifest = json!({
            "sourcePath": root.join("source.md"),
            "sourceHash": hash(b"overview"),
            "htmlPath": html_path,
            "htmlUri": "file:///report.html",
            "htmlHash": hash(html),
            "diagramCount": 0,
            "archivePath": archive_path,
            "archiveHash": hash(archive)
        });
        let Preparation::Ready(result) = reuse(root, &manifest).unwrap() else {
            panic!("existing artifacts require no tools");
        };
        assert_eq!(result.html_path, html_path);
        assert_eq!(result.archive_path, archive_path);
        assert_eq!(result.html_hash, hash(html));
        assert_eq!(result.archive_hash, hash(archive));

        for field in ["htmlHash", "archiveHash"] {
            let mut changed = manifest.clone();
            *changed.get_mut(field).unwrap() = json!(hash(b"other"));
            let error = reuse(root, &changed).unwrap_err();
            assert_eq!(classification(&error), "invalidInput");
            assert_eq!(fs::read(&html_path).unwrap(), html);
            assert_eq!(fs::read(&archive_path).unwrap(), archive);
        }

        for (path, original) in [
            (&html_path, html.as_slice()),
            (&archive_path, archive.as_slice()),
        ] {
            fs::write(path, b"modified").unwrap();
            assert_eq!(
                classification(&reuse(root, &manifest).unwrap_err()),
                "invalidInput"
            );
            assert_eq!(fs::read(path).unwrap(), b"modified");
            fs::write(path, original).unwrap();
        }

        for path in [
            root.join("report.html"),
            output.join("../report.html"),
            output.join(".overview-attempt-other/report.html"),
        ] {
            let mut changed = manifest.clone();
            *changed.get_mut("htmlPath").unwrap() = json!(path);
            assert_eq!(
                classification(&reuse(root, &changed).unwrap_err()),
                "invalidInput"
            );
        }

        let mut changed = manifest.clone();
        *changed.get_mut("archivePath").unwrap() = json!(root.join("outside.zip"));
        assert_eq!(
            classification(&reuse(root, &changed).unwrap_err()),
            "invalidInput"
        );
    }
}
