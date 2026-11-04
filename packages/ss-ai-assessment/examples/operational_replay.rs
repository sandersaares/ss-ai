//! Generates an incident-shaped synthetic fixture without accessing a provider.

use std::collections::BTreeMap;
use std::fmt::Write;

use ohno::AppError;
use serde::Serialize;
use serde_json::{Value, json};
use ss_ai_runtime::fail;

// Match the incident-shaped direct replay; these dimensions are not service limits.
const PATHS: usize = 151;
const LINES_PER_SIDE: usize = 16;

/// Complete source pairs match the input schema of `synthetic_assessment`.
#[derive(Serialize)]
struct SourcePair {
    old: String,
    new: String,
}

/// Contains invented data only; source strings are never executed.
#[derive(Serialize)]
struct Fixture {
    files: BTreeMap<String, SourcePair>,
    discussion: Value,
}

/// Supplies a realistic mix of related changes, objections, and replies.
fn fixture() -> Fixture {
    let mut files = BTreeMap::new();
    for path in 0..PATHS {
        let text = |version: &str| {
            let mut text = String::new();
            for line in 0..LINES_PER_SIDE {
                writeln!(
                    text,
                    "{version} component {path:03} line {line:02}: synthetic data only; no code runs"
                )
                .expect("formatting into a String cannot fail");
            }
            text
        };
        files.insert(
            format!("components/part-{path:03}.txt"),
            SourcePair {
                old: text("old"),
                new: text("new"),
            },
        );
    }
    let discussion = json!({
        "schemaVersion": 1,
        "platform": "gitHub",
        "pullRequest": {"body": "Synthetic large comparison; inspect interactions across components."},
        "reviews": [
            {"id": 10, "author": "reviewer", "body": "Consider the cumulative behavior.", "state": "COMMENTED"},
            {"id": 11, "author": "other-reviewer", "body": "Check the shared interface and the earlier discussion.", "state": "CHANGES_REQUESTED"}
        ],
        "reviewComments": [
            {"id": 20, "author": "reviewer", "body": "Does this interact with another path?", "path": "components/part-000.txt", "hunkReference": "excerpt-1"},
            {"id": 22, "author": "other-reviewer", "body": "Please also consider the shared context.", "path": "components/part-001.txt", "hunkReference": "excerpt-1", "resolved": false},
            {"id": 21, "author": "author", "body": "The example is synthetic.", "inReplyTo": 20, "path": "components/part-000.txt", "hunkReference": "excerpt-1"},
            {"id": 23, "author": "author", "body": "The interface is described in the synthetic source.", "inReplyTo": 22, "path": "components/part-001.txt", "hunkReference": "excerpt-1"},
            {"id": 24, "author": "reviewer", "body": "Does the other branch have the same behavior?", "path": "components/part-002.txt", "hunkReference": "excerpt-2", "resolved": true}
        ],
        "comments": [
            {"id": 30, "author": "reviewer", "body": "The entire change needs assessment."},
            {"id": 31, "author": "author", "body": "The comparison covers each generated component."}
        ],
        "hunks": {
            "excerpt-1": "@@ -1 +1 @@\n-old synthetic line\n+new synthetic line\n",
            "excerpt-2": "@@ -2 +2 @@\n-old branch data\n+new branch data\n"
        }
    });
    Fixture { files, discussion }
}

fn main() -> Result<(), AppError> {
    if std::env::args().skip(1).collect::<Vec<_>>() != ["--fixture"] {
        return Err(fail(
            "invalidInput",
            "Use --fixture to generate synthetic input",
        ));
    }
    println!("{}", serde_json::to_string_pretty(&fixture())?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_has_incident_shape_and_related_discussion() {
        let fixture = fixture();
        assert_eq!(fixture.files.len(), PATHS);
        assert!(fixture.files.values().all(|pair| {
            pair.old.lines().count() == LINES_PER_SIDE && pair.new.lines().count() == LINES_PER_SIDE
        }));
        assert!(
            fixture.discussion["reviewComments"]
                .as_array()
                .unwrap()
                .len()
                > 1
        );
    }
}
