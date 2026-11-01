//! Offline `DirectV1` regression exercise with invented evidence and judgments.
//!
//! Run `cargo run -p ss-ai-assessment --example direct_replay`. The JSON
//! describes in-process operations, not model quality or service timing.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write;

use ohno::AppError;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use ss_ai_assessment::{
    Assessment, BatchJudgmentItem, BatchJudgmentRequest, BatchReadItem, BatchReadRequest, Change,
    Content, Disposition, Inventory, Judgment, Note, Side, change_id,
};
use ss_ai_runtime::{classification, fail};

// Match the incident-shaped workload in operational_replay, not a service limit.
const PATHS: usize = 151;
const LINES_PER_SIDE: usize = 16;
const POLICY: &str = "Synthetic policy: assess the supplied changes and discussion.";

/// Controls the in-memory batching of this synthetic workload.
#[derive(Clone, Copy)]
struct Sizing {
    read_items: usize,
    fragment_chars: usize,
    judgment_items: usize,
}

// The response limit is the native pretty-printed JSON boundary. The initial
// sizes are starting points; observed response sizes adjust subsequent batches.
const RESPONSE_BYTES: usize = 16_384;
// Incident mean estimates are synthetic inputs to the arithmetic, not timers.
const MODEL_REQUEST_MILLISECONDS: usize = 6_870;
const NATIVE_COMMAND_MILLISECONDS: usize = 900;
const THRESHOLD_MILLISECONDS: usize = 9 * 60 * 1000;
const SIZING: Sizing = Sizing {
    read_items: 6,
    fragment_chars: 4096,
    judgment_items: 50,
};

/// Counts successful calls on an existing assessment and returned evidence.
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Metrics {
    batch_read_calls: usize,
    rejected_oversized_read_calls: usize,
    batch_judgment_calls: usize,
    state_mutation_calls: usize,
    token_calls: usize,
    progress_calls: usize,
    delivered_characters: usize,
    delivered_utf8_bytes: usize,
    batch_response_json_bytes: usize,
    max_read_response_bytes: usize,
}

/// Reports an operational replay, with no claim about semantic assessment.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    paths: usize,
    patch_bytes: usize,
    discussion_bytes: usize,
    units: usize,
    metrics: Metrics,
    native_calls: usize,
    modeled_minimum_model_turns: usize,
    model_assumptions: &'static str,
    simulated_latency_critical_path: SimulatedLatency,
    completed: bool,
    scope: &'static str,
}

/// Explains a deterministic arithmetic scenario rather than a latency prediction.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SimulatedLatency {
    model_request_milliseconds: usize,
    native_command_milliseconds: usize,
    native_calls: usize,
    idealized_model_turns: usize,
    serialized_native_milliseconds: usize,
    idealized_model_milliseconds: usize,
    total_milliseconds: usize,
    threshold_milliseconds: usize,
    within_threshold: bool,
    interpretation: &'static str,
}

/// Captures output for deterministic checks without filesystem access.
struct Replay {
    report: Report,
    delivered: BTreeMap<String, String>,
    digest: String,
}

/// Applies the incident's mean inputs to an explicitly serialized scenario.
fn simulated_latency(
    native_calls: usize,
    idealized_model_turns: usize,
) -> Result<SimulatedLatency, AppError> {
    let serialized_native_milliseconds = native_calls
        .checked_mul(NATIVE_COMMAND_MILLISECONDS)
        .ok_or_else(|| fail("invalidInput", "Native latency arithmetic overflow"))?;
    let idealized_model_milliseconds = idealized_model_turns
        .checked_mul(MODEL_REQUEST_MILLISECONDS)
        .ok_or_else(|| fail("invalidInput", "Model latency arithmetic overflow"))?;
    let total_milliseconds = serialized_native_milliseconds
        .checked_add(idealized_model_milliseconds)
        .ok_or_else(|| fail("invalidInput", "Total latency arithmetic overflow"))?;
    Ok(SimulatedLatency {
        model_request_milliseconds: MODEL_REQUEST_MILLISECONDS,
        native_command_milliseconds: NATIVE_COMMAND_MILLISECONDS,
        native_calls,
        idealized_model_turns,
        serialized_native_milliseconds,
        idealized_model_milliseconds,
        total_milliseconds,
        threshold_milliseconds: THRESHOLD_MILLISECONDS,
        within_threshold: total_milliseconds <= THRESHOLD_MILLISECONDS,
        interpretation: "Deterministic simulated-latency critical path: native calls are serialized and assigned the incident's mean native-command input; idealized minimum model turns are assigned the incident's mean model-request input. The sum assumes no overlap. The threshold comparison is a model result only, not p95, real latency, or an actual model-turn count.",
    })
}

/// Builds the same incident-sized class of invented replacements as the legacy replay.
fn initialize(count: usize) -> Result<(Assessment, usize, String), AppError> {
    let mut changes = Vec::new();
    let mut patches = Vec::new();
    for path in 0..count {
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
        let old_text = text("old");
        let new_text = text("new");
        let name = format!("components/part-{path:03}.txt");
        let side = |text: &str| Side {
            path: name.clone(),
            object: format!("{:x}", Sha256::digest(text.as_bytes()))
                .chars()
                .take(40)
                .collect(),
            mode: "100644".into(),
            content: Content::Text { text: text.into() },
        };
        let old = Some(side(&old_text));
        let new = Some(side(&new_text));
        changes.push(Change {
            id: change_id("M", old.as_ref(), new.as_ref())?,
            kind: "M".into(),
            old,
            new,
        });
        let mut patch = format!("@@ -1,{LINES_PER_SIDE} +1,{LINES_PER_SIDE} @@\n");
        for (prefix, text) in [('-', &old_text), ('+', &new_text)] {
            for line in text.split_inclusive('\n') {
                patch.push(prefix);
                patch.push_str(line);
            }
        }
        patches.push(patch);
    }
    let patch_bytes = patches.iter().map(String::len).sum();
    let discussion = serde_json::to_string(&json!({
        "schemaVersion": 1,
        "platform": "gitHub",
        "pullRequest": {"body": "Synthetic comparison of related components."},
        "reviews": [{"id": 10, "author": "reviewer", "body": "Consider cumulative behavior.", "state": "COMMENTED"}],
        "reviewComments": [
            {"id": 20, "author": "reviewer", "body": "Does this interact with another path?", "path": "components/part-000.txt", "hunkReference": "excerpt-1"},
            {"id": 21, "author": "author", "body": "This example is synthetic.", "inReplyTo": 20, "path": "components/part-000.txt", "hunkReference": "excerpt-1"}
        ],
        "comments": [{"id": 30, "author": "reviewer", "body": "Assess the complete comparison."}],
        "hunks": {"excerpt-1": "@@ -1 +1 @@\n-old synthetic line\n+new synthetic line\n"}
    }))?;
    let inventory = Inventory {
        old_commit: "a".repeat(40),
        new_commit: "b".repeat(40),
        old_tree: "c".repeat(40),
        new_tree: if count == 0 { "c" } else { "d" }.repeat(40),
        changes,
        complete: true,
    };
    let assessment =
        Assessment::new_direct(inventory, &patches, "snapshot", "policy", &discussion)?;
    Ok((assessment, patch_bytes, discussion))
}

/// Supplies protocol-shaped placeholders without evaluating significance.
fn judgment(id: &str) -> Judgment {
    Judgment {
        disposition: Disposition::Insignificant,
        rationale: "Synthetic placeholder; no semantic assessment was performed.".into(),
        evidence: if id == "cumulative" {
            vec!["policy".into(), "discussion".into()]
        } else {
            vec![format!("{id}:patch")]
        },
        concerns: Vec::new(),
        interactions: Vec::new(),
    }
}

/// Verifies and collects contiguous evidence fragments in bounded batches.
fn deliver(
    assessment: &mut Assessment,
    discussion: &str,
    references: &[String],
    sizing: Sizing,
    metrics: &mut Metrics,
    delivered: &mut BTreeMap<String, String>,
) -> Result<(), AppError> {
    let mut pending = references
        .iter()
        .map(|reference| (reference.clone(), 0_usize, None))
        .collect::<VecDeque<_>>();
    // All reads in this phase share one token; only receipt state changes.
    let token = assessment.read_token()?;
    metrics.token_calls = metrics.token_calls.saturating_add(1);
    let mut width = sizing.read_items;
    let mut fragment_chars = sizing.fragment_chars;
    while !pending.is_empty() {
        let group = pending.iter().take(width).cloned().collect::<Vec<_>>();
        let items = group
            .iter()
            .map(|(reference, offset, content_digest)| BatchReadItem {
                reference: reference.clone(),
                offset: *offset,
                limit: fragment_chars,
                content_digest: content_digest.clone(),
            })
            .collect();
        let response = assessment.read_batch(
            BatchReadRequest {
                read_token: token.clone(),
                items,
            },
            POLICY,
            discussion,
        );
        metrics.batch_read_calls = metrics.batch_read_calls.saturating_add(1);
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let error: AppError = error.into();
                // Batch delivery is atomic. Only the response-budget
                // classification permits retry with a smaller request.
                if classification(&error) != "evidenceDelivery" {
                    return Err(error);
                }
                metrics.rejected_oversized_read_calls =
                    metrics.rejected_oversized_read_calls.saturating_add(1);
                if group.len() > 1 {
                    width = group.len().checked_div(2).unwrap_or(1);
                } else if fragment_chars > 1 {
                    fragment_chars /= 2;
                } else {
                    return Err(error);
                }
                continue;
            }
        };
        metrics.state_mutation_calls = metrics.state_mutation_calls.saturating_add(1);
        metrics.batch_response_json_bytes = metrics
            .batch_response_json_bytes
            .saturating_add(serde_json::to_vec(&response)?.len());
        let pages = response
            .get("items")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| fail("invalidResponse", "Missing batch pages"))?;
        if pages.len() != group.len() {
            return Err(fail("invalidResponse", "Unexpected batch page count"));
        }
        let response_bytes = serde_json::to_vec_pretty(&response)?
            .len()
            .saturating_add(1);
        metrics.max_read_response_bytes = metrics.max_read_response_bytes.max(response_bytes);
        if response_bytes > RESPONSE_BYTES {
            return Err(fail("invalidResponse", "Batch exceeds the response budget"));
        }
        // Reserve part of the boundary for variation in metadata and
        // escaping; failed estimates are narrowed by the atomic retry.
        if group.len() == width {
            width = RESPONSE_BYTES
                .saturating_mul(15)
                .saturating_div(16)
                .saturating_mul(group.len())
                .checked_div(response_bytes)
                .unwrap_or(1)
                .max(1);
        }
        if response
            .get("readToken")
            .and_then(serde_json::Value::as_str)
            != Some(token.as_str())
        {
            return Err(fail(
                "invalidResponse",
                "Read token changed during delivery",
            ));
        }
        for ((reference, offset, _), page) in group.iter().zip(pages) {
            _ = pending.pop_front();
            let text = page["text"]
                .as_str()
                .ok_or_else(|| fail("invalidResponse", "Missing evidence text"))?;
            let end = offset.saturating_add(text.chars().count());
            let total = page["totalCharacters"]
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| fail("invalidResponse", "Missing evidence length"))?;
            metrics.delivered_characters = metrics
                .delivered_characters
                .saturating_add(text.chars().count());
            metrics.delivered_utf8_bytes = metrics.delivered_utf8_bytes.saturating_add(text.len());
            delivered
                .entry(reference.clone())
                .or_default()
                .push_str(text);
            if page["complete"] == true {
                if end != total || !page["nextOffset"].is_null() {
                    return Err(fail("invalidResponse", "Inconsistent complete page"));
                }
            } else {
                let continuation = page["nextOffset"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| fail("invalidResponse", "Missing continuation"))?;
                if continuation != end || continuation <= *offset || continuation > total {
                    return Err(fail("invalidResponse", "Evidence page did not advance"));
                }
                let digest = page
                    .get("contentDigest")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| fail("invalidResponse", "Missing content digest"))?;
                pending.push_back((reference.clone(), continuation, Some(digest.into())));
            }
        }
    }
    Ok(())
}

/// Exercises the direct assessment dependency chain and returns observed counts.
fn replay(count: usize, sizing: Sizing) -> Result<Replay, AppError> {
    let (mut assessment, patch_bytes, discussion) = initialize(count)?;
    let progress = assessment.progress();
    let units = progress
        .get("unitCount")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| fail("invalidResponse", "Missing unit count"))?;
    let ids = assessment
        .inventory
        .changes
        .iter()
        .map(|change| change.id.clone())
        .collect::<Vec<_>>();
    let mut references = vec!["policy".into(), "discussion".into()];
    references.extend(ids.iter().map(|id| format!("{id}:patch")));
    let mut metrics = Metrics {
        progress_calls: 1,
        ..Metrics::default()
    };
    let mut delivered = BTreeMap::new();
    deliver(
        &mut assessment,
        &discussion,
        &references,
        sizing,
        &mut metrics,
        &mut delivered,
    )?;

    assessment.note(
        "discussion",
        Note {
            observations: "Synthetic placeholder, not a model observation.".into(),
            interfaces: "Synthetic placeholder for operational replay.".into(),
            concerns: Vec::new(),
            interactions: Vec::new(),
            evidence: vec!["discussion".into()],
        },
        None,
        POLICY,
        &discussion,
    )?;
    metrics.state_mutation_calls = metrics.state_mutation_calls.saturating_add(1);

    for group in ids.chunks(sizing.judgment_items) {
        let items = group
            .iter()
            .map(|id| BatchJudgmentItem {
                id: id.clone(),
                judgment: judgment(id),
                previous_digest: None,
            })
            .collect();
        let token = assessment.state_token()?;
        metrics.token_calls = metrics.token_calls.saturating_add(1);
        let response = assessment.record_batch(
            BatchJudgmentRequest {
                state_token: token,
                items,
            },
            POLICY,
            &discussion,
        )?;
        metrics.batch_judgment_calls = metrics.batch_judgment_calls.saturating_add(1);
        metrics.state_mutation_calls = metrics.state_mutation_calls.saturating_add(1);
        metrics.batch_response_json_bytes = metrics
            .batch_response_json_bytes
            .saturating_add(serde_json::to_vec(&response)?.len());
    }
    let mut references = vec!["discussion-note".into()];
    references.extend(ids.iter().map(|id| format!("judgment:{id}")));
    deliver(
        &mut assessment,
        &discussion,
        &references,
        sizing,
        &mut metrics,
        &mut delivered,
    )?;
    assessment.record(
        "cumulative",
        judgment("cumulative"),
        None,
        POLICY,
        &discussion,
    )?;
    metrics.state_mutation_calls = metrics.state_mutation_calls.saturating_add(1);
    assessment.finish(POLICY, &discussion)?;
    metrics.state_mutation_calls = metrics.state_mutation_calls.saturating_add(1);
    let digest = assessment
        .digest
        .clone()
        .ok_or_else(|| fail("invalidResponse", "Missing assessment digest"))?;
    // Construction and the observation, cumulative record, and finish are
    // native operations in addition to the counted token, progress, and batches.
    let native_calls = 4_usize
        .saturating_add(metrics.token_calls)
        .saturating_add(metrics.progress_calls)
        .saturating_add(metrics.batch_read_calls)
        .saturating_add(metrics.batch_judgment_calls);
    // See the phase assumptions in the report: this is not an observed turn count.
    let modeled_minimum_model_turns = 4;
    let simulated_latency = simulated_latency(native_calls, modeled_minimum_model_turns)?;
    Ok(Replay {
        report: Report {
            paths: ids.len(),
            patch_bytes,
            discussion_bytes: discussion.len(),
            units,
            metrics,
            native_calls,
            modeled_minimum_model_turns,
            model_assumptions: "Idealized phases: read source and discussion; make path judgments and discussion observation; read their records; make cumulative judgment and finish. One model turn can schedule all independently addressable reads, and an orchestrator handles continuations, budget retries, tokens, and serial state-changing writes without further model turns. This is a lower bound, not a measured model request count.",
            simulated_latency_critical_path: simulated_latency,
            completed: true,
            scope: "Synthetic placeholders only; no model, semantic evaluation, network, or publication.",
        },
        delivered,
        digest,
    })
}

fn main() -> Result<(), AppError> {
    let replay = replay(PATHS, SIZING)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "report": replay.report,
            "evidenceReferences": replay.delivered.len(),
            "assessmentDigest": replay.digest,
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_and_batch_sizes_preserve_delivered_content_and_outcome() {
        let first = replay(
            3,
            Sizing {
                read_items: 1,
                fragment_chars: 97,
                judgment_items: 1,
            },
        )
        .unwrap();
        let second = replay(
            3,
            Sizing {
                read_items: 4,
                fragment_chars: 1000,
                judgment_items: 3,
            },
        )
        .unwrap();
        assert_eq!(first.delivered, second.delivered);
        assert_eq!(first.digest, second.digest);
        assert_eq!(
            first.report.metrics.delivered_utf8_bytes,
            second.report.metrics.delivered_utf8_bytes
        );
        assert_ne!(
            first.report.metrics.batch_read_calls,
            second.report.metrics.batch_read_calls
        );
        assert!(first.report.completed && second.report.completed);
    }

    #[test]
    #[cfg_attr(miri, ignore = "incident-sized replay is too large for Miri")]
    fn incident_sized_replay_completes() {
        let replay = replay(PATHS, SIZING).unwrap();
        assert_eq!(replay.report.paths, PATHS);
        assert!((290_000..=330_000).contains(&replay.report.patch_bytes));
        assert!(replay.report.units >= PATHS);
        assert!(replay.report.metrics.batch_read_calls > 0);
        assert!(replay.report.metrics.batch_judgment_calls > 0);
        assert!(replay.report.metrics.delivered_utf8_bytes >= replay.report.patch_bytes);
        assert!(replay.report.metrics.max_read_response_bytes <= RESPONSE_BYTES);
        assert!(replay.report.modeled_minimum_model_turns <= 40);
        assert!(replay.report.completed);
    }

    #[test]
    fn oversized_batch_adapts_without_losing_evidence() {
        let expected = replay(8, SIZING).unwrap();
        let oversized = replay(
            8,
            Sizing {
                read_items: 100,
                fragment_chars: 4096,
                judgment_items: 8,
            },
        )
        .unwrap();
        assert!(oversized.report.metrics.rejected_oversized_read_calls > 0);
        assert_eq!(oversized.delivered, expected.delivered);
        assert_eq!(oversized.digest, expected.digest);
        assert_eq!(
            oversized.report.metrics.delivered_utf8_bytes,
            expected.report.metrics.delivered_utf8_bytes
        );
        assert!(oversized.report.metrics.max_read_response_bytes <= RESPONSE_BYTES);
    }

    #[test]
    fn simulated_latency_uses_serial_native_calls_and_idealized_turns() {
        let model = simulated_latency(63, 4).unwrap();
        assert_eq!(model.serialized_native_milliseconds, 56_700);
        assert_eq!(model.idealized_model_milliseconds, 27_480);
        assert_eq!(model.total_milliseconds, 84_180);
        assert_eq!(model.threshold_milliseconds, 540_000);
        assert!(model.within_threshold);
        assert_eq!(model, simulated_latency(63, 4).unwrap());
    }

    #[test]
    fn simulated_threshold_includes_the_boundary() {
        assert!(simulated_latency(600, 0).unwrap().within_threshold);
        assert!(!simulated_latency(601, 0).unwrap().within_threshold);
        assert!(simulated_latency(usize::MAX, 1).is_err());
    }
}
