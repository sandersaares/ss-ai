//! Exercises the actual build fingerprint recipe with isolated input trees.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Keeps the simulated checkout local and independent of checked-in files.
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // The counter only assigns unique fixture names.
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "build-fingerprint-test-{}-{index}",
                std::process::id()
            ));
        for directory in [
            "packages/ss-ai/src",
            "packages/ss-ai-review-workflow/src",
            "packages/ss-ai-review-context/src",
            "packages/ss-ai-runtime/src",
            "packages/ss-ai-git/src",
            "skills/ss-review/schemas",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        for file in [
            "Cargo.toml",
            "Cargo.lock",
            "packages/ss-ai/Cargo.toml",
            "packages/ss-ai-review-workflow/Cargo.toml",
            "packages/ss-ai-review-workflow/build.rs",
            "packages/ss-ai-review-context/Cargo.toml",
            "packages/ss-ai-runtime/Cargo.toml",
            "packages/ss-ai-git/Cargo.toml",
        ] {
            fs::write(root.join(file), file).unwrap();
        }
        Self(root)
    }

    /// Runs Cargo's compiled build script against the fixture, not the checkout.
    fn run(&self) -> Output {
        Command::new(env!("SS_AI_FOCUS_BUILD_SCRIPT"))
            .env(
                "CARGO_MANIFEST_DIR",
                self.0.join("packages/ss-ai-review-workflow"),
            )
            .env("TARGET", "test-target")
            .env("PROFILE", "test-profile")
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Retrieves the fingerprint produced by a successful build-script invocation.
fn fingerprint(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("cargo:rustc-env=SS_AI_FOCUS_BUILD_FINGERPRINT="))
        .unwrap()
        .to_owned()
}

/// The real build script must track and hash the selected file's actual bytes.
#[test]
#[cfg_attr(miri, ignore = "requires an isolated native subprocess")]
fn selected_input_bytes_change_the_fingerprint() {
    let fixture = Fixture::new();
    let path = fixture.0.join("packages/ss-ai/src/rule.rs");
    fs::write(&path, "first").unwrap();
    let first = fixture.run();
    let output = String::from_utf8_lossy(&first.stdout);
    assert!(
        output
            .lines()
            .any(|line| { line == format!("cargo:rerun-if-changed={}", path.display()) })
    );
    let first = fingerprint(&first);
    fs::write(&path, "second").unwrap();
    assert_ne!(first, fingerprint(&fixture.run()));
}

#[cfg(unix)]
#[test]
#[cfg_attr(miri, ignore = "requires an isolated native subprocess")]
fn literal_backslash_inputs_fail_before_fingerprinting() {
    for nested_exists in [false, true] {
        let fixture = Fixture::new();
        let source = fixture.0.join("packages/ss-ai/src");
        let literal = source.join("nested\\rule.rs");
        fs::write(&literal, "literal").unwrap();
        if nested_exists {
            fs::create_dir_all(source.join("nested")).unwrap();
            fs::write(source.join("nested/rule.rs"), "nested").unwrap();
        }
        let output = fixture.run();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("literal backslash"), "{error}");
        assert!(error.contains("rename"), "{error}");
        assert!(error.contains("packages/ss-ai/src/nested"), "{error}");
        assert!(error.contains("rule.rs"), "{error}");
        assert!(
            !String::from_utf8_lossy(&output.stdout)
                .contains("cargo:rustc-env=SS_AI_FOCUS_BUILD_FINGERPRINT=")
        );
    }
}

#[cfg(unix)]
#[test]
#[cfg_attr(miri, ignore = "requires an isolated native subprocess")]
fn literal_backslash_in_parent_directory_is_rejected() {
    let fixture = Fixture::new();
    let source = fixture.0.join("packages/ss-ai/src");
    fs::create_dir_all(source.join("nested\\inner")).unwrap();
    fs::write(source.join("nested\\inner/rule.rs"), "source").unwrap();
    let output = fixture.run();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("literal backslash"), "{error}");
    assert!(error.contains("rename"), "{error}");
}
