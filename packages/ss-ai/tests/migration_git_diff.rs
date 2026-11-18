//! Diff interpretation contracts retained from the review runtime.
#![allow(
    clippy::indexing_slicing,
    reason = "The assertions inspect fixed test fixture shapes"
)]

use std::io;
use std::path::Path;

use serde_json::{Value, json};
use ss_ai_git::{compare, parse};
use ss_ai_runtime::{Process, ProcessOutput};

/// Checks paths, file operations and both sides of each unified hunk.
#[test]
fn file_operations_and_hunk_ranges_preserve_review_evidence() {
    let text = concat!(
        "diff --git a/src/lib.rs b/src/lib.rs\n",
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n",
        "@@ -10,7 +10,8 @@\n",
        " context one\n context two\n context three\n-removed\n+added\n+extra\n",
        " context four\n context five\n context six\n",
        "@@ -40,6 +41,6 @@\n alpha\n beta\n gamma\n-delta\n+DELTA\n epsilon\n zeta\n",
        "diff --git a/docs/new.md b/docs/new.md\nnew file mode 100644\n",
        "--- /dev/null\n+++ b/docs/new.md\n@@ -0,0 +1,3 @@\n+first\n+second\n+third\n",
        "diff --git a/old/gone.txt b/old/gone.txt\ndeleted file mode 100644\n",
        "--- a/old/gone.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-alpha\n-beta\n",
        "diff --git a/src/old-name.rs b/src/new-name.rs\n",
        "similarity index 88%\nrename from src/old-name.rs\nrename to src/new-name.rs\n",
        "--- a/src/old-name.rs\n+++ b/src/new-name.rs\n",
        "@@ -1,4 +1,4 @@\n alpha\n-beta\n+BETA\n gamma\n delta\n",
        "diff --git a/a/one.txt b/b/two.txt\nsimilarity index 100%\n",
        "rename from a/one.txt\nrename to b/two.txt\n",
        "diff --git a/docs/my file.md b/docs/my file.md\nold mode 100644\nnew mode 100755\n",
        "diff --git a/docs/old name.md b/docs/new name.md\nsimilarity index 100%\n",
        "rename from docs/old name.md\nrename to docs/new name.md\n",
    );
    let files = parse(text).unwrap();
    let values = serde_json::to_value(&files).unwrap();
    let expected = json!([
        {"path":"src/lib.rs","previousPath":null,"status":"modified","hunks":[
            {"oldStart":10,"oldCount":7,"newStart":10,"newCount":8},
            {"oldStart":40,"oldCount":6,"newStart":41,"newCount":6}]},
        {"path":"docs/new.md","previousPath":null,"status":"added","hunks":[
            {"oldStart":0,"oldCount":0,"newStart":1,"newCount":3}]},
        {"path":"old/gone.txt","previousPath":null,"status":"deleted","hunks":[
            {"oldStart":1,"oldCount":2,"newStart":0,"newCount":0}]},
        {"path":"src/new-name.rs","previousPath":"src/old-name.rs","status":"renamed","hunks":[
            {"oldStart":1,"oldCount":4,"newStart":1,"newCount":4}]},
        {"path":"b/two.txt","previousPath":"a/one.txt","status":"renamed","hunks":[]},
        {"path":"docs/my file.md","previousPath":null,"status":"modified","hunks":[]},
        {"path":"docs/new name.md","previousPath":"docs/old name.md","status":"renamed","hunks":[]}
    ]);
    assert_eq!(values, expected);

    let modified = &files[0];
    for (start, end, left, expected) in [
        (12, 12, false, true),
        (17, 17, false, true),
        (41, 41, false, true),
        (18, 18, false, false),
        (9, 9, false, false),
        (47, 47, false, false),
        (40, 40, false, false),
        (16, 16, true, true),
        (17, 17, true, false),
        (40, 40, true, true),
        (11, 15, false, true),
        (15, 20, false, false),
        (12, 44, false, false),
    ] {
        assert_eq!(modified.contains_range(start, end, left), expected);
    }
    assert!(!files[1].contains_range(1, 1, true));
    assert!(files[1].contains_range(1, 1, false));
    assert!(!files[2].contains_range(1, 1, false));
    assert!(files[2].contains_range(2, 2, true));
    assert!(!files[4].contains_range(1, 1, false));
    assert!(parse("").unwrap().is_empty());
}

/// Header-shaped hunk content and omitted counts do not alter file boundaries.
#[test]
fn header_shaped_lines_and_omitted_counts_are_preserved() {
    let text = concat!(
        "diff --git a/tricky.txt b/tricky.txt\n",
        "--- a/tricky.txt\n+++ b/tricky.txt\n",
        "@@ -1,2 +1,2 @@\n--- a/not-a-file.txt\n+++ b/not-a-file.txt\n tail\n",
        "diff --git a/after.txt b/after.txt\n--- a/after.txt\n+++ b/after.txt\n",
        "@@ -5 +5 @@\n-before\n+after\n\\ No newline at end of file\n",
    );
    let files = serde_json::to_value(parse(text).unwrap()).unwrap();
    assert_eq!(files[0]["path"], "tricky.txt");
    assert_eq!(files[0]["previousPath"], Value::Null);
    assert_eq!(files[1]["path"], "after.txt");
    assert_eq!(
        files[1]["hunks"][0],
        json!({"oldStart":5,"oldCount":1,"newStart":5,"newCount":1})
    );
}

/// The byte stream comes from Git, independent of the caller's console encoding.
struct GitBytes(Vec<u8>);

impl Process for GitBytes {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        _input: &[u8],
    ) -> io::Result<ProcessOutput> {
        assert_eq!(program, "git");
        let output = match arguments[4].as_str() {
            "rev-parse" => b".git\n".to_vec(),
            "cat-file" => b"commit\n".to_vec(),
            "diff" => self.0.clone(),
            verb => panic!("unexpected Git operation: {verb}"),
        };
        Ok(ProcessOutput::new(true, output, Vec::new()))
    }
}

/// Compares strict UTF-8 bytes, including supplementary-plane path characters.
#[test]
fn comparison_decodes_utf8_and_rejects_malformed_bytes() {
    let text = "diff --git a/src/naïve🤖.rs b/src/naïve🤖.rs\n--- a/src/naïve🤖.rs\n+++ b/src/naïve🤖.rs\n@@ -1 +1 @@\n-café\n+🤖\n";
    let old = "a".repeat(40);
    let new = "b".repeat(40);
    let diff = compare(
        &GitBytes(text.as_bytes().to_vec()),
        Path::new("repo"),
        &old,
        &new,
    )
    .unwrap();
    assert_eq!(diff.old_commit(), old);
    assert_eq!(diff.new_commit(), new);
    assert_eq!(diff.files()[0].path(), "src/naïve🤖.rs");
    compare(&GitBytes(vec![0xff]), Path::new("repo"), &old, &new).unwrap_err();
}
