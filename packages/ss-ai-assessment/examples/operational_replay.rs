//! Offline operational exercise and input generator for synthetic assessments.
//!
//! See `operational_replay.md` for the measurement scope and invocation.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::time::Instant;

use ohno::AppError;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai_assessment::{
    Assessment, Change, Content, Disposition, Inventory, Judgment, Note, Side, change_id,
};
use ss_ai_runtime::fail;

// The incident-shaped dimensions are deliberate workload inputs, not service limits.
const PATHS: usize = 151;
const LINES_PER_SIDE: usize = 16;
const POLICY: &str = "Synthetic policy: assess the supplied changes and discussion.";

/// Complete source pairs match the input schema of `synthetic_assessment`.
#[derive(Serialize)]
struct SourcePair {
    old: String,
    new: String,
}

/// Contains only invented data; source strings are never executed.
#[derive(Serialize)]
struct Fixture {
    files: BTreeMap<String, SourcePair>,
    discussion: Value,
}

/// Counts in-process API calls and their compact JSON results, not service operations.
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Metrics {
    native_calls: usize,
    evidence_read_calls: usize,
    state_mutation_calls: usize,
    returned_json_bytes: usize,
    returned_evidence_characters: usize,
}

impl Metrics {
    /// Accounts for one successful native call and its returned value.
    fn response(&mut self, value: &Value, read: bool, mutation: bool) -> Result<(), AppError> {
        self.native_calls = self.native_calls.saturating_add(1);
        self.evidence_read_calls = self.evidence_read_calls.saturating_add(usize::from(read));
        self.state_mutation_calls = self
            .state_mutation_calls
            .saturating_add(usize::from(mutation));
        self.returned_json_bytes = self
            .returned_json_bytes
            .saturating_add(serde_json::to_vec(value)?.len());
        if read {
            self.returned_evidence_characters = self
                .returned_evidence_characters
                .saturating_add(field(value, "text")?.chars().count());
        }
        Ok(())
    }

    /// Counts a native call that has no JSON result.
    fn void(&mut self, mutation: bool) {
        self.native_calls = self.native_calls.saturating_add(1);
        self.state_mutation_calls = self
            .state_mutation_calls
            .saturating_add(usize::from(mutation));
    }
}

/// Captures a repeatable scenario and operation counts without reading a clock.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    changed_paths: usize,
    patch_bytes: usize,
    discussion_bytes: usize,
    units: usize,
    metrics: Metrics,
    simulated_critical_path_calls: usize,
    simulated_serial_calls: usize,
    spawned_commands: usize,
    filesystem_reads: usize,
    filesystem_writes: usize,
    status: String,
}

/// Generates text with varied paths, related discussion, and an incident-sized patch.
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

/// Produces exact full replacements accepted by the native reconstruction validator.
fn replacement(old: &str, new: &str) -> String {
    let old_count = old.split_inclusive('\n').count();
    let new_count = new.split_inclusive('\n').count();
    let mut patch = format!(
        "@@ -{},{old_count} +{},{new_count} @@\n",
        usize::from(old_count > 0),
        usize::from(new_count > 0)
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
    patch
}

/// Binds invented endpoints and patches to the real native assessment machinery.
fn initialize(fixture: &Fixture) -> Result<(Assessment, usize, String), AppError> {
    let mut changes = Vec::new();
    let mut patches = Vec::new();
    for (path, pair) in &fixture.files {
        let side = |text: &str| Side {
            path: path.clone(),
            object: format!("{:x}", Sha256::digest(text.as_bytes()))
                .chars()
                .take(40)
                .collect(),
            mode: "100644".into(),
            content: Content::Text { text: text.into() },
        };
        let old = Some(side(&pair.old));
        let new = Some(side(&pair.new));
        changes.push(Change {
            id: change_id("M", old.as_ref(), new.as_ref())?,
            kind: "M".into(),
            old,
            new,
        });
        patches.push(replacement(&pair.old, &pair.new));
    }
    let patch_bytes = patches.iter().map(String::len).sum();
    let discussion = serde_json::to_string(&fixture.discussion)?;
    let inventory = Inventory {
        old_commit: "a".repeat(40),
        new_commit: "b".repeat(40),
        old_tree: "c".repeat(40),
        new_tree: "d".repeat(40),
        changes,
        complete: true,
    };
    let assessment = Assessment::new(inventory, &patches, "snapshot", "policy", &discussion)?;
    Ok((assessment, patch_bytes, discussion))
}

/// Rejects malformed native responses instead of guessing at coverage.
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, AppError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| fail("invalidResponse", "Missing string in native response"))
}

/// Pages a complete reference and rejects non-progressing or inconsistent offsets.
fn deliver(
    assessment: &mut Assessment,
    metrics: &mut Metrics,
    reference: &str,
    discussion: &str,
) -> Result<usize, AppError> {
    let mut offset = 0;
    let mut calls = 0_usize;
    loop {
        let page = assessment.read(reference, offset, usize::MAX, POLICY, discussion)?;
        metrics.response(&page, true, true)?;
        calls = calls.saturating_add(1);
        let text_length = field(&page, "text")?.chars().count();
        let total = page
            .get("totalCharacters")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| fail("invalidResponse", "Missing evidence length"))?;
        let end = offset.saturating_add(text_length);
        if page.get("complete") == Some(&Value::Bool(true)) {
            if end != total || page.get("nextOffset") != Some(&Value::Null) {
                return Err(fail(
                    "invalidResponse",
                    "Inconsistent complete evidence page",
                ));
            }
            return Ok(calls);
        }
        let next = page
            .get("nextOffset")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| fail("invalidResponse", "Missing next evidence offset"))?;
        if next != end || next <= offset || next > total {
            return Err(fail("invalidResponse", "Evidence page did not advance"));
        }
        offset = next;
    }
}

/// Uses fixed labels solely to exercise record dependencies, not semantic reasoning.
fn note(reference: &str) -> Note {
    Note {
        observations: "Synthetic placeholder, not a model judgment.".into(),
        interfaces: "Synthetic placeholder for operational replay.".into(),
        concerns: Vec::new(),
        interactions: Vec::new(),
        evidence: vec![reference.into()],
    }
}

/// Placeholder dispositions satisfy the workflow but do not evaluate significance.
fn judgment(reference: &str) -> Judgment {
    Judgment {
        disposition: Disposition::Insignificant,
        rationale: "Synthetic placeholder; no semantic assessment was performed.".into(),
        evidence: if reference == "cumulative" {
            vec!["policy".into(), "discussion".into()]
        } else {
            vec![format!("{reference}:patch")]
        },
        concerns: Vec::new(),
        interactions: Vec::new(),
    }
}

/// Executes the full in-process dependency sequence and counts actual native calls.
fn replay(fixture: &Fixture) -> Result<Report, AppError> {
    let (mut assessment, patch_bytes, discussion) = initialize(fixture)?;
    let mut metrics = Metrics::default();
    metrics.void(false); // Construction has no JSON response.
    deliver(&mut assessment, &mut metrics, "policy", &discussion)?;
    deliver(&mut assessment, &mut metrics, "discussion", &discussion)?;
    let ids = assessment
        .inventory
        .changes
        .iter()
        .map(|c| c.id.clone())
        .collect::<Vec<_>>();
    for id in &ids {
        deliver(
            &mut assessment,
            &mut metrics,
            &format!("{id}:patch"),
            &discussion,
        )?;
    }

    let mut after = 0;
    let mut units = Vec::new();
    loop {
        let page = assessment.units(after, 100)?;
        metrics.response(&page, false, false)?;
        units.extend(
            page.get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| fail("invalidResponse", "Missing units"))?
                .iter()
                .cloned(),
        );
        let next = page
            .get("nextOffset")
            .ok_or_else(|| fail("invalidResponse", "Missing next unit offset"))?;
        if next.is_null() {
            break;
        }
        let next = next
            .as_u64()
            .ok_or_else(|| fail("invalidResponse", "Invalid next unit offset"))?;
        let next = usize::try_from(next)?;
        if next <= after {
            return Err(fail("invalidResponse", "Unit page did not advance"));
        }
        after = next;
    }
    let mut longest_unit = 0;
    let mut unit_calls = 0_usize;
    for unit in &units {
        let reference = field(unit, "reference")?;
        let mut calls = deliver(&mut assessment, &mut metrics, reference, &discussion)?;
        calls = calls.saturating_add(deliver(
            &mut assessment,
            &mut metrics,
            field(unit, "bindingReference")?,
            &discussion,
        )?);
        let digest = assessment.note(
            field(
                unit.get("unit")
                    .ok_or_else(|| fail("invalidResponse", "Missing unit metadata"))?,
                "id",
            )?,
            note(reference),
            None,
            POLICY,
            &discussion,
        )?;
        metrics.response(&json!(digest), false, true)?;
        calls = calls.saturating_add(1);
        calls = calls.saturating_add(deliver(
            &mut assessment,
            &mut metrics,
            field(unit, "noteReference")?,
            &discussion,
        )?);
        unit_calls = unit_calls.saturating_add(calls);
        longest_unit = longest_unit.max(calls);
    }
    for id in &ids {
        let digest = assessment.record(id, judgment(id), None, POLICY, &discussion)?;
        metrics.response(&json!(digest), false, true)?;
        deliver(
            &mut assessment,
            &mut metrics,
            &format!("judgment:{id}"),
            &discussion,
        )?;
    }
    let digest = assessment.note("discussion", note("discussion"), None, POLICY, &discussion)?;
    metrics.response(&json!(digest), false, true)?;
    deliver(
        &mut assessment,
        &mut metrics,
        "discussion-note",
        &discussion,
    )?;
    let digest = assessment.record(
        "cumulative",
        judgment("cumulative"),
        None,
        POLICY,
        &discussion,
    )?;
    metrics.response(&json!(digest), false, true)?;
    assessment.finish(POLICY, &discussion)?;
    metrics.void(true);

    // Idealized unlimited unit workers replace their serial sum with the longest
    // unit chain. All other calls remain serial, including all path aggregates.
    let simulated_serial_calls = metrics.native_calls;
    let simulated_critical_path_calls = simulated_serial_calls
        .saturating_sub(unit_calls)
        .saturating_add(longest_unit);
    Ok(Report {
        changed_paths: ids.len(),
        patch_bytes,
        discussion_bytes: discussion.len(),
        units: units.len(),
        metrics,
        simulated_critical_path_calls,
        simulated_serial_calls,
        spawned_commands: 0,
        filesystem_reads: 0,
        filesystem_writes: 0,
        status: "completed with synthetic placeholders; not an approval".into(),
    })
}

fn main() -> Result<(), AppError> {
    let argument = std::env::args().skip(1).collect::<Vec<_>>();
    if argument == ["--fixture"] {
        println!("{}", serde_json::to_string_pretty(&fixture())?);
    } else if argument.is_empty() {
        let start = Instant::now();
        let report = replay(&fixture())?;
        let elapsed = start.elapsed();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "report": report,
                "measuredInProcessElapsedMilliseconds": elapsed.as_millis(),
                "measurementScope": "fixture generation and native replay, excluding compilation and output serialization; no model, provider, subprocess, or publication",
                "warning": "synthetic placeholders do not assess model quality; elapsed time is not service or model latency"
            }))?
        );
    } else {
        return Err(fail("invalidInput", "Use no arguments or --fixture"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_page_counts_the_real_response() {
        let mut fixture = fixture();
        fixture.files.retain(|path, _| path.ends_with("000.txt"));
        let (mut measured, _, discussion) = initialize(&fixture).unwrap();
        let (mut reference, _, _) = initialize(&fixture).unwrap();
        let page = reference
            .read("policy", 0, usize::MAX, POLICY, &discussion)
            .unwrap();
        let mut metrics = Metrics::default();
        assert_eq!(
            deliver(&mut measured, &mut metrics, "policy", &discussion).unwrap(),
            1
        );
        assert_eq!(metrics.native_calls, 1);
        assert_eq!(metrics.evidence_read_calls, 1);
        assert_eq!(metrics.state_mutation_calls, 1);
        assert_eq!(
            metrics.returned_json_bytes,
            serde_json::to_vec(&page).unwrap().len()
        );
        assert_eq!(metrics.returned_evidence_characters, POLICY.chars().count());
    }

    #[test]
    #[cfg_attr(miri, ignore = "incident-sized replay is too large for Miri")]
    fn incident_fixture_completes_and_counts_deterministically() {
        let fixture = fixture();
        let report = replay(&fixture).unwrap();
        assert_eq!(report.changed_paths, PATHS);
        // The size range expresses an approximate incident-sized patch, not an exact byte budget.
        assert!(
            (290_000..=330_000).contains(&report.patch_bytes),
            "{}",
            report.patch_bytes
        );
        assert!(report.units >= PATHS);
        assert!(report.metrics.evidence_read_calls > PATHS);
        assert!(report.metrics.state_mutation_calls > PATHS);
        assert_eq!(report.spawned_commands, 0);
        assert_eq!(report.filesystem_reads, 0);
        assert_eq!(report.filesystem_writes, 0);
        assert!(report.simulated_critical_path_calls < report.simulated_serial_calls);
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::to_value(replay(&fixture).unwrap()).unwrap()
        );
    }
}
