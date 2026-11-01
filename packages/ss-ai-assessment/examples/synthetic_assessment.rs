//! Offline semantic-evaluation driver for the production assessment state machine.
//!
//! This developer-only binary has no provider, publication, or installed-skill access.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use clap::Parser;
use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ss_ai_assessment::{
    Assessment, Change, Content, ContextRequest, Disposition, Inventory, Judgment, Note, Side,
    SourceSide, change_id,
};
use ss_ai_runtime::{FileTransaction, SystemClock, fail, read_bytes};

/// Named options mirror the production operations without exposing service effects.
#[derive(Debug, Parser)]
struct Arguments {
    #[arg(long)]
    operation: String,
    #[arg(long)]
    state_path: PathBuf,
    #[arg(long)]
    fixture_path: Option<PathBuf>,
    #[arg(long)]
    policy_path: Option<PathBuf>,
    #[arg(long)]
    reference: Option<String>,
    #[arg(long)]
    unit_id: Option<String>,
    #[arg(long)]
    change_id: Option<String>,
    #[arg(long)]
    check_id: Option<String>,
    #[arg(long)]
    observations: Option<String>,
    #[arg(long)]
    interfaces: Option<String>,
    #[arg(long)]
    disposition: Option<Disposition>,
    #[arg(long)]
    rationale: Option<String>,
    #[arg(long)]
    concern: Vec<String>,
    #[arg(long)]
    interaction: Vec<String>,
    #[arg(long)]
    evidence: Vec<String>,
    #[arg(long)]
    previous_digest: Option<String>,
    #[arg(long, default_value = "0")]
    after: usize,
    #[arg(long, default_value = "0")]
    offset: usize,
    #[arg(long, default_value = "4096")]
    limit: NonZeroUsize,
    #[arg(long)]
    side: Option<SourceSide>,
    #[arg(long)]
    start_line: Option<NonZeroUsize>,
    #[arg(long)]
    line_count: Option<NonZeroUsize>,
}

/// The fixture schema deliberately has no expected-outcome or baseline-answer fields.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    files: BTreeMap<String, SourcePair>,
    discussion: Value,
}

/// Complete synthetic endpoints are data, never executed programs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePair {
    old: String,
    new: String,
}

/// The native assessment is retained atomically with bounded delivery measurements.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct State {
    schema_version: u32,
    snapshot: String,
    policy_digest: String,
    policy: String,
    discussion: String,
    assessment: Assessment,
    responses: usize,
    delivered_bytes: usize,
    delivered_characters: usize,
}

fn main() -> Result<(), AppError> {
    let arguments = Arguments::parse();
    let clock = SystemClock::default();
    let transaction = FileTransaction::open(&arguments.state_path, &clock)?;
    let existing = transaction.read_bytes()?;
    let mut state = if arguments.operation == "init" {
        if existing.is_some() {
            return Err(fail("invalidInput", "Synthetic state already exists"));
        }
        initialize(&arguments, &clock)?
    } else {
        let bytes =
            existing.ok_or_else(|| fail("invalidInput", "Initialize synthetic state first"))?;
        serde_json::from_slice::<State>(&bytes)?
    };
    if state.schema_version != 1 {
        return Err(fail(
            "invalidState",
            "Unsupported synthetic evaluation state",
        ));
    }
    state.assessment.validate(
        &state.snapshot,
        &state.policy_digest,
        &state.policy,
        &state.discussion,
    )?;
    let value = execute(&arguments, &mut state)?;
    state.assessment.validate(
        &state.snapshot,
        &state.policy_digest,
        &state.policy,
        &state.discussion,
    )?;
    let output = serde_json::to_string_pretty(&value)?;
    state.responses = state.responses.saturating_add(1);
    state.delivered_bytes = state
        .delivered_bytes
        .saturating_add(output.len())
        .saturating_add(1);
    state.delivered_characters = state.delivered_characters.saturating_add(
        value
            .get("text")
            .and_then(Value::as_str)
            .map_or(0, |text| text.chars().count()),
    );
    transaction.write_bytes(&serde_json::to_vec(&state)?)?;
    println!("{output}");
    Ok(())
}

/// Every operation invokes the same native methods used by ss-ai's locked work dispatcher.
fn execute(arguments: &Arguments, state: &mut State) -> Result<Value, AppError> {
    let assessment = &mut state.assessment;
    let policy = &state.policy;
    let discussion = &state.discussion;
    let previous = arguments.previous_digest.as_deref();
    let result = match arguments.operation.as_str() {
        "init" | "assessment" => assessment.summary(
            arguments.after,
            arguments.limit.get().min(100),
            policy,
            discussion,
        )?,
        "units" => assessment.units_for(
            arguments.reference.as_deref(),
            arguments.after,
            arguments.limit.get().min(100),
        )?,
        "checks" => assessment.checks(arguments.after, arguments.limit.get().min(100))?,
        "read" => assessment.read(
            required(arguments.reference.as_deref())?,
            arguments.offset,
            arguments.limit.get(),
            policy,
            discussion,
        )?,
        "source-context" => assessment.source_context(
            ContextRequest {
                change_id: required(arguments.change_id.as_deref())?.to_owned(),
                side: arguments
                    .side
                    .ok_or_else(|| fail("invalidInput", "Missing side"))?,
                start_line: arguments
                    .start_line
                    .ok_or_else(|| fail("invalidInput", "Missing start line"))?,
                line_count: arguments
                    .line_count
                    .ok_or_else(|| fail("invalidInput", "Missing line count"))?,
            },
            arguments.limit.get(),
            policy,
            discussion,
        )?,
        "note" => {
            json!({"noteDigest":assessment.note(required(arguments.unit_id.as_deref())?, Note {
            observations: required(arguments.observations.as_deref())?.to_owned(),
            interfaces: required(arguments.interfaces.as_deref())?.to_owned(),
            concerns: arguments.concern.clone(), interactions: arguments.interaction.clone(), evidence: arguments.evidence.clone(),
        }, previous, policy, discussion)?})
        }
        "judge" => {
            json!({"judgmentDigest":assessment.record(required(arguments.change_id.as_deref())?, judgment(arguments)?, previous, policy, discussion)?})
        }
        "resolve" => {
            json!({"resolutionDigest":assessment.resolve(required(arguments.check_id.as_deref())?, judgment(arguments)?, previous, policy, discussion)?})
        }
        "checkpoint" => assessment.checkpoint()?,
        "complete" => {
            assessment.finish(policy, discussion)?;
            json!({"status":assessment.status(true), "assessmentDigest":assessment.digest, "rationale":assessment.rationale()})
        }
        "results" => json!({"status":assessment.status(true), "assessmentDigest":assessment.digest,
            "rationale":assessment.rationale(), "progress":assessment.progress(),
            "responses":state.responses, "deliveredBytes":state.delivered_bytes, "deliveredCharacters":state.delivered_characters}),
        _ => return Err(fail("invalidInput", "Unknown synthetic operation")),
    };
    Ok(result)
}

fn judgment(arguments: &Arguments) -> Result<Judgment, AppError> {
    Ok(Judgment {
        disposition: arguments
            .disposition
            .ok_or_else(|| fail("invalidInput", "Missing disposition"))?,
        rationale: required(arguments.rationale.as_deref())?.to_owned(),
        evidence: arguments.evidence.clone(),
        concerns: arguments.concern.clone(),
        interactions: arguments.interaction.clone(),
    })
}

fn required(value: Option<&str>) -> Result<&str, AppError> {
    value.ok_or_else(|| fail("invalidInput", "Missing required operation argument"))
}

/// Synthetic complete replacements enter the normal exact-reconstruction validator.
fn initialize(arguments: &Arguments, clock: &SystemClock) -> Result<State, AppError> {
    let fixture_path = arguments
        .fixture_path
        .as_deref()
        .ok_or_else(|| fail("invalidInput", "Missing fixture path"))?;
    let policy_path = arguments
        .policy_path
        .as_deref()
        .ok_or_else(|| fail("invalidInput", "Missing policy path"))?;
    let fixture_bytes = required_file(fixture_path, clock)?;
    let fixture: Fixture = serde_json::from_slice(&fixture_bytes)?;
    let policy = String::from_utf8(required_file(policy_path, clock)?)?;
    let discussion = serde_json::to_string(&fixture.discussion)?;
    let policy_digest = digest(policy.as_bytes());
    let snapshot = digest(&fixture_bytes);
    let mut changes = Vec::new();
    let mut patches = Vec::new();
    for (path, pair) in fixture.files {
        if pair.old == pair.new {
            continue;
        }
        let side = |text: &str| Side {
            path: path.clone(),
            // Captured fixtures use SHA-1-width Git identities; these are synthetic, not objects.
            object: digest(text.as_bytes()).chars().take(40).collect(),
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
    // Distinct synthetic object identities exercise endpoint binding without a real Git repository.
    let inventory = Inventory {
        old_commit: "a".repeat(40),
        new_commit: "b".repeat(40),
        old_tree: "c".repeat(40),
        new_tree: if changes.is_empty() { "c" } else { "d" }.repeat(40),
        changes,
        complete: true,
    };
    let assessment = Assessment::new(inventory, &patches, &snapshot, &policy_digest, &discussion)?;
    Ok(State {
        schema_version: 1,
        snapshot,
        policy_digest,
        policy,
        discussion,
        assessment,
        responses: 0,
        delivered_bytes: 0,
        delivered_characters: 0,
    })
}

fn required_file(path: &Path, clock: &SystemClock) -> Result<Vec<u8>, AppError> {
    read_bytes(path, clock)?.ok_or_else(|| fail("invalidInput", "Missing synthetic input file"))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

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
