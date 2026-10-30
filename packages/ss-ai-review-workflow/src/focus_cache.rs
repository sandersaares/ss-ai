//! Transactional reuse of complete focus-area generations.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ss_ai_review_context::bytes_hash;
use ss_ai_runtime::{Clock, FileTransaction, LockMode, ResourceLock, classification, fail};

use crate::{focus_area_expansion, focus_area_generation};

/// Distinguishes this layout and key format from the PowerShell cache.
const SCHEMA: u32 = 3;
/// Keeps the identity separate from hashes of other workflow documents.
const DOMAIN: &str = "ss-review-native-focus-area-cache";
/// Cache retention and count match the focus-area reuse policy.
const RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;
const MAX_ENTRIES: usize = 64;
/// Abandoned staging directories remain long enough for an interrupted writer.
const STAGING_RETENTION_SECONDS: i64 = 24 * 60 * 60;
/// Only these generation outputs form the reusable artifact set.
const ARTIFACTS: [&str; 3] = [
    "instruction-set-raw",
    "instruction-set-categories.md",
    "focus-areas",
];
/// Explicit installed inputs follow the generation contract, not reviewed content.
const GENERATION_FILES: &[&str] = &[
    "scripts/Copy-ReviewInstructionSources.ps1",
    "scripts/lib/InstructionSources.ps1",
    "pwsh-calls/copy-instruction-sources.md",
    "focus-area-format.md",
    "schemas/focus-area-map.schema.json",
    "scripts/lib/FocusAreaPreparation.ps1",
    "scripts/lib/JsonValue.ps1",
    "scripts/lib/PullRequestModel.ps1",
    "scripts/Test-FocusAreaMap.ps1",
    "scripts/New-FocusAreaArtifacts.ps1",
    "pwsh-calls/validate-focus-area-map.md",
    "pwsh-calls/assemble-focus-areas.md",
    "workflow-data.md",
    "data-guides/focus-areas.md",
    "schemas/workflow-data.schema.json",
    "scripts/lib/WorkflowData.ps1",
    "scripts/New-ReviewWorkerAssignment.ps1",
    "scripts/Read-ReviewWorkerDocument.ps1",
    "scripts/lib/NativeWorkflowBridge.ps1",
    "pwsh-calls/prepare-worker-assignment.md",
    "scripts/lib/FocusAreaExpansion.ps1",
    "scripts/Update-FocusAreaExpansion.ps1",
    "scripts/Update-FocusAreaMap.ps1",
    "pwsh-calls/begin-focus-area-generation.md",
    "pwsh-calls/publish-focus-area-generation.md",
    "pwsh-calls/update-focus-area-expansion.md",
    "pwsh-calls/update-focus-area-map.md",
    "scripts/Save-FocusAreaCache.ps1",
];

/// Inputs that define a reusable generation, without review or session identity.
#[derive(Debug)]
#[expect(
    clippy::exhaustive_structs,
    reason = "The internal CLI constructs the options directly"
)]
pub struct CacheOptions<'a> {
    /// Session whose artifacts and diagnostic record are updated.
    pub session: &'a Path,
    /// Materialized source snapshot.
    pub input_directory: &'a Path,
    /// Installed skill directory containing the generation contract.
    pub skill_root: &'a Path,
    /// Generation recipe, independently selected by the caller.
    pub recipe: &'a Path,
    /// Private cache root.
    pub cache_directory: &'a Path,
    /// Model profile for the generated guidance.
    pub generator_profile: &'a str,
    /// External-reference refresh interval, in days.
    pub freshness_days: u32,
}

/// Finalized expansion state returned by the native validation boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputState {
    /// Every required reference was expanded.
    Complete,
    /// The current review may proceed, but the result cannot be reused.
    Degraded,
}

/// Cache outcome and the input identity needed for a later store.
#[derive(Debug, Serialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct CacheResult {
    /// Operation that produced the result.
    pub operation: &'static str,
    /// Hit, Miss, Stored, Existing, or Bypassed.
    pub status: &'static str,
    /// Reason for a miss or bypass, when applicable.
    pub reason: Option<&'static str>,
    /// Generation identity, absent when it cannot be computed safely.
    pub key: Option<String>,
    /// Identity required to begin preparation from this restore.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preparation_identity: Option<String>,
    /// Number of focus-area files in the result.
    pub focus_area_count: usize,
    /// Count of user instruction files in the current snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_instruction_file_count: Option<usize>,
    /// Cleanup and maintenance problems that did not prevent review coverage.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Summary of the complete artifact set accepted by the review workflow.
#[derive(Debug, Serialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct ArtifactValidation {
    /// Operation that inspected the artifact set.
    pub operation: &'static str,
    /// Validation result.
    pub status: &'static str,
    /// Validation reason; null on success.
    pub reason: Option<&'static str>,
    /// Validation is independent of the cache key.
    pub key: Option<String>,
    /// Number of top-level focus-area files.
    pub focus_area_count: usize,
    /// Number of files in the complete artifact set.
    pub artifact_file_count: usize,
}

/// An exact relative path and byte digest within one input or output tree.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    path: String,
    length: u64,
    sha256: String,
}

/// Complete key material for the current snapshot and installed generator.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    domain: &'static str,
    schema: u32,
    generator_profile: String,
    freshness_days: u32,
    freshness_bucket: i64,
    inputs: Vec<Record>,
}

/// Immutable description of the published payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    key: String,
    created_at_unix: i64,
    inputs: Vec<Record>,
    outputs: Vec<Record>,
}

/// Carries an identity together with its digest during a transaction.
struct Descriptor {
    key: String,
    identity: Identity,
}

/// Binds a later save to the exact snapshot observed before generation.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    key: Option<String>,
}

/// Resolves configured aliases once before taking locks or inspecting payloads.
struct ResolvedPaths {
    session: PathBuf,
    input: PathBuf,
    skill: PathBuf,
    recipe: PathBuf,
}

impl ResolvedPaths {
    fn new(options: &CacheOptions<'_>, session: PathBuf) -> Result<Self, AppError> {
        Ok(Self {
            session,
            input: fs::canonicalize(options.input_directory)?,
            skill: fs::canonicalize(options.skill_root)?,
            recipe: fs::canonicalize(options.recipe)?,
        })
    }

    fn options<'a>(&'a self, original: &'a CacheOptions<'_>) -> CacheOptions<'a> {
        CacheOptions {
            session: &self.session,
            input_directory: &self.input,
            skill_root: &self.skill,
            recipe: &self.recipe,
            cache_directory: original.cache_directory,
            generator_profile: original.generator_profile,
            freshness_days: original.freshness_days,
        }
    }
}

/// Creates an isolated staging directory without relying on wall-clock timing.
///
/// The counter only supplies unique names; atomicity prevents threads in this
/// process from choosing the same candidate, and directory creation arbitrates
/// collisions with other processes.
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// Restores a verified cache entry or reports a miss that requires generation.
///
/// The caller supplies the identity returned by source snapshot publication.
/// A successful result supplies the identity needed to begin preparation.
///
/// # Errors
///
/// Returns an error if session coordination, diagnostics, or rollback fails.
pub fn restore(
    options: &CacheOptions<'_>,
    refresh: bool,
    expected_snapshot_identity: &str,
    clock: &dyn Clock,
) -> Result<CacheResult, AppError> {
    let session = fs::canonicalize(options.session)?;
    let _operation = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        clock,
    )?;
    let snapshot_valid =
        focus_area_generation::verify_snapshot_locked(&session, expected_snapshot_identity, clock);
    focus_area_generation::invalidate_locked(&session, clock)?;
    // A failed new restore must never leave a previous generation authorized.
    write_snapshot(&session, None, clock)?;
    if !snapshot_valid? {
        return Err(fail(
            "invalidInput",
            "Instruction snapshot identity does not match",
        ));
    }
    let paths = ResolvedPaths::new(options, session)?;
    let options = &paths.options(options);
    let count = user_instruction_count(options.input_directory)?;
    let descriptor = compute_descriptor(options, clock)?;
    write_snapshot(options.session, Some(&descriptor.key), clock)?;
    let preparation_identity = focus_area_generation::bind_restore_locked(
        options.session,
        options.input_directory,
        clock,
    )?;
    let result = match descriptor {
        descriptor if refresh => outcome(
            "Restore",
            "Miss",
            Some("RefreshRequested"),
            Some(descriptor.key),
            0,
        ),
        descriptor => restore_entry(options, &descriptor, &preparation_identity, clock)?,
    };
    let mut result = result;
    result.preparation_identity = Some(preparation_identity);
    result.user_instruction_file_count = Some(count);
    write_diagnostics(
        options.session,
        &json!({"cache": result.status, "reason": result.reason, "userInstructionFileCount": count, "warnings": result.warnings}),
        false,
        clock,
    )?;
    Ok(result)
}

/// Stores a complete validated generation under the identity observed at restore.
///
/// A session record binds this operation to the snapshot observed at restore.
/// A missing record bypasses caching rather than guessing an identity.
/// The caller supplies the preparation identity returned by the restore.
/// The transaction validates finalized expansion and the generated artifacts.
///
/// # Errors
///
/// Returns an error for invalid generation state or failed diagnostics.
pub fn store(
    options: &CacheOptions<'_>,
    expected_preparation_identity: &str,
    clock: &dyn Clock,
) -> Result<CacheResult, AppError> {
    let session = fs::canonicalize(options.session)?;
    let _operation = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        clock,
    )?;
    require_preparation(&session, expected_preparation_identity, clock)?;
    let paths = ResolvedPaths::new(options, session)?;
    let options = &paths.options(options);
    let descriptor = compute_descriptor(options, clock)?;
    let snapshot = read_snapshot(options.session, clock)?;
    let (result, input_state) = match (descriptor, snapshot) {
        (descriptor, None) => {
            bypass_store(options, Some(descriptor.key), "SnapshotUnavailable", clock)?
        }
        (descriptor, Some(snapshot)) if snapshot.key.as_deref() != Some(&descriptor.key) => {
            bypass_store(options, Some(descriptor.key), "InputsChanged", clock)?
        }
        (descriptor, Some(_)) => {
            store_entry(options, &descriptor, expected_preparation_identity, clock)?
        }
    };
    let mut diagnostics = json!({"store": result.status, "storeReason": result.reason, "storeWarnings": result.warnings});
    if let Some(state) = input_state {
        let fields = diagnostics
            .as_object_mut()
            .ok_or_else(|| fail("invalidInput", "Invalid diagnostic update"))?;
        fields.insert(
            "inputState".to_owned(),
            json!(match state {
                InputState::Complete => "Complete",
                InputState::Degraded => "Degraded",
            }),
        );
    }
    write_diagnostics(options.session, &diagnostics, true, clock)?;
    Ok(result)
}

/// Resets a previous hit after validation fails for the same preparation.
///
/// # Errors
///
/// Returns an error if coordination or diagnostics publication fails.
pub fn record_validation_failure(
    session: &Path,
    expected_preparation_identity: &str,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let session = fs::canonicalize(session)?;
    let _operation = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        clock,
    )?;
    require_preparation(&session, expected_preparation_identity, clock)?;
    focus_area_generation::invalidate_ready_locked(&session, expected_preparation_identity, clock)?;
    record_validation_failure_locked(&session, clock)
}

/// Validates artifact structure without acquiring session locks.
///
/// The caller coordinates writes when inspecting active session artifacts.
///
/// # Errors
///
/// Returns an error when the artifact set is incomplete or unreadable.
pub fn check_artifacts(root: &Path) -> Result<ArtifactValidation, AppError> {
    let root = fs::canonicalize(root)?;
    let (records, focus_area_count) = artifact_records(&root)?;
    Ok(ArtifactValidation {
        operation: "Validate",
        status: "Valid",
        reason: None,
        key: None,
        focus_area_count,
        artifact_file_count: records.len(),
    })
}

/// Validates session artifacts and resets an invalidated cache hit.
///
/// The caller supplies the preparation identity returned by the restore.
///
/// # Errors
///
/// Returns an error for invalid artifacts, coordination, or diagnostics.
pub fn validate_artifacts(
    session: &Path,
    expected_preparation_identity: &str,
    clock: &dyn Clock,
) -> Result<ArtifactValidation, AppError> {
    let session = fs::canonicalize(session)?;
    let _operation = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        clock,
    )?;
    require_preparation(&session, expected_preparation_identity, clock)?;
    let _artifacts = ResourceLock::open(&session.join("focus-areas"), LockMode::Exclusive, clock)?;
    let validation = check_artifacts(&session).and_then(|result| {
        if focus_area_generation::verify_ready_locked(
            &session,
            expected_preparation_identity,
            clock,
        )? {
            Ok(result)
        } else {
            Err(fail("invalidInput", "Focus-area artifacts are not ready"))
        }
    });
    match validation {
        Ok(result) => Ok(result),
        Err(error) => {
            focus_area_generation::invalidate_ready_locked(
                &session,
                expected_preparation_identity,
                clock,
            )?;
            record_validation_failure_locked(&session, clock)?;
            Err(error)
        }
    }
}

/// Prevents a delayed validation from observing or changing a later operation.
fn require_preparation(
    session: &Path,
    expected_identity: &str,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    if !focus_area_generation::verify_preparation_locked(session, expected_identity, clock)? {
        return Err(fail(
            "staleGeneration",
            "Preparation identity is not active",
        ));
    }
    Ok(())
}

/// Resets diagnostics while the caller owns the operation lock.
fn record_validation_failure_locked(session: &Path, clock: &dyn Clock) -> Result<(), AppError> {
    let path = session.join("focus-area-cache-diagnostics.json");
    let transaction = FileTransaction::open(&path, clock)?;
    let previous = transaction.read_bytes()?;
    let count = match previous {
        Some(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            let object = value
                .as_object()
                .ok_or_else(|| fail("invalidInput", "Invalid cache diagnostics"))?;
            match object.get("userInstructionFileCount") {
                Some(count) if count.as_u64().is_some() => Some(count.clone()),
                Some(_) => return Err(fail("invalidInput", "Invalid user instruction file count")),
                None => None,
            }
        }
        None => None,
    };
    let mut value = json!({"cache":"Miss", "reason":"ValidationFailed"});
    if let Some(count) = count {
        value
            .as_object_mut()
            .ok_or_else(|| fail("invalidInput", "Invalid diagnostics"))?
            .insert("userInstructionFileCount".to_owned(), count);
    }
    transaction.write_bytes(&serde_json::to_vec(&value)?)
}

/// Computes input identity only from the current source snapshot and generator.
fn compute_descriptor(
    options: &CacheOptions<'_>,
    clock: &dyn Clock,
) -> Result<Descriptor, AppError> {
    // Keep the accepted freshness range aligned with the cache wrapper parameters.
    if options.generator_profile.trim().is_empty() || !(1..=3650).contains(&options.freshness_days)
    {
        return Err(fail("invalidInput", "Invalid cache profile or freshness"));
    }
    let mut inputs = tree_records(options.input_directory, "input")?;
    if inputs.is_empty() {
        return Err(fail("invalidInput", "Instruction snapshot is empty"));
    }
    inputs.push(file_record(
        options.recipe,
        "__recipe__/prepare-focus-areas.md",
    )?);
    for path in GENERATION_FILES {
        let input = fs::canonicalize(options.skill_root.join(path))?;
        if !input.starts_with(options.skill_root) {
            return Err(fail(
                "invalidInput",
                "Installed generation input escapes the skill root",
            ));
        }
        inputs.push(file_record(&input, &format!("__generation__/{path}"))?);
    }
    inputs.push(record(
        "__generation__/native-build",
        generator_build_fingerprint().as_bytes(),
    ));
    inputs.sort_by(|a, b| a.path.cmp(&b.path));
    let seconds = i64::from(options.freshness_days).saturating_mul(24 * 60 * 60);
    let identity = Identity {
        domain: DOMAIN,
        schema: SCHEMA,
        generator_profile: options.generator_profile.to_owned(),
        freshness_days: options.freshness_days,
        freshness_bucket: clock.now().as_second().div_euclid(seconds),
        inputs,
    };
    let key = bytes_hash(&serde_json::to_vec(&identity)?).to_ascii_lowercase();
    Ok(Descriptor { key, identity })
}

/// Identifies the selected native generation inputs captured at build time.
#[must_use]
pub fn generator_build_fingerprint() -> &'static str {
    env!("SS_AI_FOCUS_BUILD_FINGERPRINT")
}

/// Persists the identity before generation in a separately locked record.
fn write_snapshot(session: &Path, key: Option<&str>, clock: &dyn Clock) -> Result<(), AppError> {
    let transaction =
        FileTransaction::open(&session.join("focus-area-cache-identity.json"), clock)?;
    let snapshot = Snapshot {
        schema_version: SCHEMA,
        key: key.map(str::to_owned),
    };
    transaction.write_bytes(&serde_json::to_vec(&snapshot)?)
}

/// Rejects malformed or obsolete records instead of treating them as completed work.
fn read_snapshot(session: &Path, clock: &dyn Clock) -> Result<Option<Snapshot>, AppError> {
    let transaction =
        FileTransaction::open(&session.join("focus-area-cache-identity.json"), clock)?;
    let Some(bytes) = transaction.read_bytes()? else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value
        .as_object()
        .is_some_and(|object| object.contains_key("key"))
    {
        return Err(fail("invalidInput", "Invalid focus-area cache snapshot"));
    }
    let snapshot: Snapshot = serde_json::from_value(value)?;
    if snapshot.schema_version != SCHEMA || snapshot.key.as_ref().is_some_and(|key| !is_key(key)) {
        return Err(fail("invalidInput", "Invalid focus-area cache snapshot"));
    }
    Ok(Some(snapshot))
}

/// Reads the restore identity while the caller owns the session operation lock.
///
/// # Errors
///
/// Returns an error if the identity record is malformed or unavailable.
pub fn read_generation_key_locked(
    session: &Path,
    clock: &dyn Clock,
) -> Result<Option<String>, AppError> {
    Ok(read_snapshot(session, clock)?.and_then(|snapshot| snapshot.key))
}

/// Resolves an optional cache root and rejects a redirected lock directory.
fn cache_root(path: &Path) -> Result<PathBuf, AppError> {
    fs::create_dir_all(path)?;
    let root = fs::canonicalize(path)?;
    let locks = root.join(".locks");
    fs::create_dir_all(&locks)?;
    ensure_directory(&locks)?;
    Ok(root)
}

/// Checks a cache payload before staging and swapping it into the session.
fn restore_entry(
    options: &CacheOptions<'_>,
    descriptor: &Descriptor,
    preparation_identity: &str,
    clock: &dyn Clock,
) -> Result<CacheResult, AppError> {
    let key = Some(descriptor.key.clone());
    let cache = match cache_root(options.cache_directory) {
        Ok(cache) => cache,
        Err(error) => {
            return Ok(warn(
                outcome("Restore", "Miss", Some("CacheUnavailable"), key, 0),
                error,
            ));
        }
    };
    let key_lock = ResourceLock::open(
        &key_path(&cache, &descriptor.key),
        LockMode::Exclusive,
        clock,
    );
    let _key_lock = match key_lock {
        Ok(lock) => lock,
        Err(error) => {
            return Ok(warn(
                outcome("Restore", "Miss", Some(lock_reason(&error)), key, 0),
                error,
            ));
        }
    };
    let entry = cache.join(&descriptor.key);
    let manifest = match valid_entry(&entry, &descriptor.key) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => return Ok(outcome("Restore", "Miss", Some("NotFound"), key, 0)),
        Err(error) => {
            return Ok(warn(
                outcome("Restore", "Miss", Some("InvalidEntry"), key, 0),
                error,
            ));
        }
    };
    let _artifacts = match ResourceLock::open(
        &options.session.join("focus-areas"),
        LockMode::Exclusive,
        clock,
    ) {
        Ok(lock) => lock,
        Err(error) => {
            return Ok(warn(
                outcome("Restore", "Miss", Some("SessionBusy"), key, 0),
                error,
            ));
        }
    };
    let stage = match new_directory(options.session, ".focus-area-cache-restore") {
        Ok(path) => path,
        Err(error) => {
            return Ok(warn(
                outcome("Restore", "Miss", Some("SessionUnavailable"), key, 0),
                error,
            ));
        }
    };
    let staged = (|| -> Result<(usize, String), AppError> {
        copy_artifacts(&entry.join("payload"), &stage)?;
        let (records, count) = artifact_records(&stage)?;
        let staged_digest = focus_area_generation::staged_hit_digest(&stage, clock)?;
        verify_staged(
            &records,
            &manifest.outputs,
            &staged_digest,
            || check_artifacts(&stage).map(|_| ()),
            || {
                Ok((
                    artifact_records(&stage)?.0,
                    focus_area_generation::staged_hit_digest(&stage, clock)?,
                ))
            },
        )?;
        Ok((count, staged_digest))
    })();
    let (count, staged_digest) = match staged {
        Ok(result) => result,
        Err(error) => {
            let mut result = warn(
                outcome("Restore", "Miss", Some("InvalidEntry"), key, 0),
                error,
            );
            cleanup(&stage, &mut result.warnings);
            return Ok(result);
        }
    };
    let backup = match new_directory(options.session, ".focus-area-cache-backup") {
        Ok(path) => path,
        Err(error) => {
            let mut result = warn(
                outcome("Restore", "Miss", Some("SessionUnavailable"), key, 0),
                error,
            );
            cleanup(&stage, &mut result.warnings);
            return Ok(result);
        }
    };
    let swap = swap_artifacts(options.session, &stage, &backup, || {
        focus_area_generation::record_hit_locked(
            options.session,
            preparation_identity,
            &descriptor.key,
            &staged_digest,
            clock,
        )
    });
    match swap {
        Ok(None) => {
            let mut result = outcome("Restore", "Hit", None, key, count);
            cleanup(&stage, &mut result.warnings);
            cleanup(&backup, &mut result.warnings);
            Ok(result)
        }
        Ok(Some(error)) => {
            let mut result = warn(
                outcome("Restore", "Miss", Some("SessionUnavailable"), key, 0),
                error,
            );
            cleanup(&stage, &mut result.warnings);
            cleanup(&backup, &mut result.warnings);
            Ok(result)
        }
        Err(error) => {
            let mut warnings = Vec::new();
            cleanup(&stage, &mut warnings);
            if warnings.is_empty() {
                Err(error)
            } else {
                Err(fail("io", format!("{error}; {}", warnings.join("; "))))
            }
        }
    }
}

/// Verifies the copied set before and after the structural validation boundary.
fn verify_staged(
    records: &[Record],
    expected: &[Record],
    staged_digest: &str,
    validate: impl FnOnce() -> Result<(), AppError>,
    rehash: impl FnOnce() -> Result<(Vec<Record>, String), AppError>,
) -> Result<(), AppError> {
    if records != expected {
        return Err(fail("invalidInput", "Staged cache payload changed"));
    }
    validate()?;
    let (records, digest) = rehash()?;
    if records != expected || digest != staged_digest {
        return Err(fail(
            "invalidInput",
            "Validated staged cache payload changed",
        ));
    }
    Ok(())
}

/// Moves all active outputs aside before installing, and preserves failed rollback backups.
fn swap_artifacts(
    session: &Path,
    stage: &Path,
    backup: &Path,
    publish_receipt: impl FnOnce() -> Result<(), AppError>,
) -> Result<Option<AppError>, AppError> {
    swap_with(&FilesystemSwap, session, stage, backup, publish_receipt)
}

/// Isolates swap effects so failure and rollback follow the same production logic.
trait SwapIo {
    /// Detects an existing active artifact without following a link.
    fn exists(&self, path: &Path) -> Result<bool, AppError>;
    /// Moves an artifact without copying it between volumes.
    fn rename(&self, source: &Path, destination: &Path) -> Result<(), AppError>;
    /// Removes an artifact installed by this swap.
    fn remove(&self, path: &Path) -> Result<(), AppError>;
}

/// Uses filesystem operations for the coordinated artifact swap.
struct FilesystemSwap;

impl SwapIo for FilesystemSwap {
    fn exists(&self, path: &Path) -> Result<bool, AppError> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn rename(&self, source: &Path, destination: &Path) -> Result<(), AppError> {
        fs::rename(source, destination)?;
        Ok(())
    }

    fn remove(&self, path: &Path) -> Result<(), AppError> {
        remove_path(path)
    }
}

/// Completes a swap or attempts every possible rollback before reporting failure.
fn swap_with(
    io: &impl SwapIo,
    session: &Path,
    stage: &Path,
    backup: &Path,
    publish_receipt: impl FnOnce() -> Result<(), AppError>,
) -> Result<Option<AppError>, AppError> {
    let mut moved = Vec::new();
    let mut installed = Vec::new();
    let swap = (|| -> Result<(), AppError> {
        for name in ARTIFACTS {
            let source = session.join(name);
            if io.exists(&source)? {
                io.rename(&source, &backup.join(name))?;
                moved.push(name);
            }
        }
        for name in ARTIFACTS {
            io.rename(&stage.join(name), &session.join(name))?;
            installed.push(name);
        }
        publish_receipt()
    })();
    let Err(error) = swap else { return Ok(None) };
    let mut rollback_failed = false;
    for name in installed {
        if io.remove(&session.join(name)).is_err() {
            rollback_failed = true;
        }
    }
    for name in moved {
        if io.rename(&backup.join(name), &session.join(name)).is_err() {
            rollback_failed = true;
        }
    }
    if rollback_failed {
        return Err(fail(
            "io",
            format!(
                "Focus-area restore failed: {error}; rollback incomplete; backup retained at {}",
                backup.display(),
            ),
        ));
    }
    Ok(Some(error))
}

/// Publishes one validated, immutable entry while holding both relevant locks.
#[expect(
    clippy::create_dir,
    reason = "The payload must not reuse a pre-existing directory"
)]
fn store_entry(
    options: &CacheOptions<'_>,
    descriptor: &Descriptor,
    expected_preparation_identity: &str,
    clock: &dyn Clock,
) -> Result<(CacheResult, Option<InputState>), AppError> {
    let key = Some(descriptor.key.clone());
    let cache = match cache_root(options.cache_directory) {
        Ok(cache) => cache,
        Err(error) => {
            let (result, state) = bypass_store(options, key, "CacheUnavailable", clock)?;
            return Ok((warn(result, error), state));
        }
    };
    let _key_lock = match ResourceLock::open(
        &key_path(&cache, &descriptor.key),
        LockMode::Exclusive,
        clock,
    ) {
        Ok(lock) => lock,
        Err(error) => {
            let (result, state) = bypass_store(options, key, lock_reason(&error), clock)?;
            return Ok((warn(result, error), state));
        }
    };
    let _artifacts = ResourceLock::open(
        &options.session.join("focus-areas"),
        LockMode::Exclusive,
        clock,
    )?;
    let published = focus_area_generation::validate_published_locked(
        options.session,
        options.input_directory,
        &descriptor.key,
        clock,
    )?;
    if !published
        && focus_area_generation::verify_ready_locked(
            options.session,
            expected_preparation_identity,
            clock,
        )?
    {
        // A cache hit is usable without a local expansion, but is not a new
        // published generation and therefore cannot authorize another store.
        let (_, count) = artifact_records(options.session)?;
        return Ok((
            outcome(
                "Store",
                "Bypassed",
                Some("GenerationNotPublished"),
                key,
                count,
            ),
            None,
        ));
    }
    let state = expansion_state(options.session, clock)?;
    let (outputs, count) = artifact_records(options.session)?;
    if state == InputState::Degraded {
        return Ok((
            outcome("Store", "Bypassed", Some("DegradedInputs"), key, count),
            Some(state),
        ));
    }
    if !published {
        return Ok((
            outcome(
                "Store",
                "Bypassed",
                Some("GenerationNotPublished"),
                key,
                count,
            ),
            Some(state),
        ));
    }
    let entry = cache.join(&descriptor.key);
    let mut warnings = Vec::new();
    match valid_entry(&entry, &descriptor.key) {
        Ok(Some(_)) => {
            let mut result = outcome("Store", "Existing", None, key, count);
            result.warnings.extend(maintenance(
                &cache,
                &descriptor.key,
                clock.now().as_second(),
            ));
            return Ok((result, Some(state)));
        }
        Ok(None) => {}
        Err(error) => warnings.push(format!("Replacing invalid cache entry: {error}")),
    }
    let mut input_error = None;
    let mut inputs_changed = false;
    let publish = (|| -> Result<(), AppError> {
        match fs::symlink_metadata(&entry) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(fail("invalidInput", "Cache entry must not be a link"));
            }
            Ok(_) => remove_path(&entry)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let stage = new_directory(&cache, &format!(".{}.", descriptor.key))?;
        let result = (|| -> Result<(), AppError> {
            let payload = stage.join("payload");
            fs::create_dir(&payload)?;
            copy_artifacts(options.session, &payload)?;
            let (copied, _) = artifact_records(&payload)?;
            if outputs != copied {
                return Err(fail(
                    "invalidInput",
                    "Focus-area artifacts changed during staging",
                ));
            }
            match compute_descriptor(options, clock) {
                Ok(current) if current.key == descriptor.key => {}
                Ok(_) => {
                    inputs_changed = true;
                    return Err(fail("invalidInput", "Cache inputs changed during staging"));
                }
                Err(error) => {
                    input_error = Some(error);
                    return Err(fail("invalidInput", "Cannot revalidate cache inputs"));
                }
            }
            let manifest = Manifest {
                schema_version: SCHEMA,
                key: descriptor.key.clone(),
                created_at_unix: clock.now().as_second(),
                inputs: descriptor.identity.inputs.clone(),
                outputs: copied,
            };
            fs::write(stage.join("manifest.json"), serde_json::to_vec(&manifest)?)?;
            fs::rename(&stage, &entry)?;
            Ok(())
        })();
        cleanup(&stage, &mut warnings);
        result
    })();
    if let Some(error) = input_error {
        return Err(error);
    }
    if let Err(error) = publish {
        let reason = if inputs_changed {
            "InputsChanged"
        } else {
            "CacheUnavailable"
        };
        let mut result = warn(
            outcome("Store", "Bypassed", Some(reason), key, count),
            error,
        );
        result.warnings.extend(warnings);
        return Ok((result, Some(state)));
    }
    let mut result = outcome("Store", "Stored", None, key, count);
    result.warnings.extend(warnings);
    result.warnings.extend(maintenance(
        &cache,
        &descriptor.key,
        clock.now().as_second(),
    ));
    Ok((result, Some(state)))
}

/// Keeps generation validation and diagnostics even when caching is unavailable.
fn bypass_store(
    options: &CacheOptions<'_>,
    key: Option<String>,
    reason: &'static str,
    clock: &dyn Clock,
) -> Result<(CacheResult, Option<InputState>), AppError> {
    let _artifacts = ResourceLock::open(
        &options.session.join("focus-areas"),
        LockMode::Exclusive,
        clock,
    )?;
    let state = expansion_state(options.session, clock)?;
    let (_, count) = artifact_records(options.session)?;
    let reason = if state == InputState::Degraded {
        "DegradedInputs"
    } else {
        reason
    };
    Ok((
        outcome("Store", "Bypassed", Some(reason), key, count),
        Some(state),
    ))
}

/// Accepts only a final expansion validated against the current source trees.
fn expansion_state(session: &Path, clock: &dyn Clock) -> Result<InputState, AppError> {
    let expansion = focus_area_expansion::read_locked(session, clock, true)?;
    Ok(if expansion.is_complete() {
        InputState::Complete
    } else {
        InputState::Degraded
    })
}

/// Reads and rehashes the manifest and payload rather than trusting persisted metadata.
fn valid_entry(entry: &Path, key: &str) -> Result<Option<Manifest>, AppError> {
    match fs::symlink_metadata(entry) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(fail("invalidInput", "Invalid cache entry"));
        }
        Ok(_) => {}
    }
    let manifest_path = entry.join("manifest.json");
    ensure_file(&manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    let (outputs, _) = artifact_records(&entry.join("payload"))?;
    if manifest.schema_version != SCHEMA || manifest.key != key || manifest.outputs != outputs {
        return Err(fail(
            "invalidInput",
            "Cache manifest does not match payload",
        ));
    }
    Ok(Some(manifest))
}

/// Identifies the retained key lock independently of the removable entry.
fn key_path(cache: &Path, key: &str) -> PathBuf {
    cache.join(".locks").join(key)
}

/// Classifies contention separately from an unusable cache location.
fn lock_reason(error: &AppError) -> &'static str {
    if classification(error) == "contention" {
        "CacheBusy"
    } else {
        "CacheUnavailable"
    }
}

/// Builds the machine-readable result shared by both operations.
fn outcome(
    operation: &'static str,
    status: &'static str,
    reason: Option<&'static str>,
    key: Option<String>,
    focus_area_count: usize,
) -> CacheResult {
    CacheResult {
        operation,
        status,
        reason,
        key,
        preparation_identity: None,
        focus_area_count,
        user_instruction_file_count: None,
        warnings: Vec::new(),
    }
}

/// Preserves optional-cache failures for the session diagnostics.
fn warn(mut result: CacheResult, error: impl std::fmt::Display) -> CacheResult {
    result.warnings.push(error.to_string());
    result
}

/// Reports cleanup failures without changing a successful cache outcome.
fn cleanup(path: &Path, warnings: &mut Vec<String>) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warnings.push(format!("Cannot clean {}: {error}", path.display())),
    }
}

/// Publishes diagnostics under their own retained read-modify-write lock.
fn write_diagnostics(
    session: &Path,
    updates: &Value,
    merge: bool,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let transaction =
        FileTransaction::open(&session.join("focus-area-cache-diagnostics.json"), clock)?;
    let mut result = if merge {
        match transaction.read_bytes()? {
            Some(bytes) => serde_json::from_slice::<Value>(&bytes)?,
            None => json!({}),
        }
    } else {
        json!({})
    };
    let object = result
        .as_object_mut()
        .ok_or_else(|| fail("invalidInput", "Invalid cache diagnostics"))?;
    let updates = updates
        .as_object()
        .ok_or_else(|| fail("invalidInput", "Invalid diagnostic update"))?;
    object.extend(updates.clone());
    transaction.write_bytes(&serde_json::to_vec(&result)?)
}

/// Enumerates exactly the files in a tree, rejecting links and special files.
fn tree_records(root: &Path, prefix: &str) -> Result<Vec<Record>, AppError> {
    ensure_directory(root)?;
    let mut records = Vec::new();
    collect_records(root, root, prefix, &mut records)?;
    records.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(records)
}

/// Traverses without following links so the digest describes the copied tree.
fn collect_records(
    root: &Path,
    current: &Path,
    prefix: &str,
    records: &mut Vec<Record>,
) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(current)?;
    if metadata.file_type().is_symlink() {
        return Err(fail("invalidInput", "Cache trees must not contain links"));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(current)? {
            collect_records(root, &entry?.path(), prefix, records)?;
        }
    } else if metadata.is_file() {
        let relative = current.strip_prefix(root)?;
        let mut path = String::from(prefix);
        for component in relative.components() {
            let part = component
                .as_os_str()
                .to_str()
                .ok_or_else(|| fail("invalidInput", "Cache path is not Unicode"))?;
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
        }
        records.push(file_record(current, &path)?);
    } else {
        return Err(fail(
            "invalidInput",
            "Cache trees must contain only regular files",
        ));
    }
    Ok(())
}

/// Requires an ordinary file and binds its full bytes to its logical path.
fn file_record(path: &Path, name: &str) -> Result<Record, AppError> {
    ensure_file(path)?;
    Ok(record(name, &fs::read(path)?))
}

/// Rejects links and non-files before reading a cache input.
fn ensure_file(path: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(fail(
            "invalidInput",
            format!("Expected a regular file: {}", path.display()),
        ));
    }

    Ok(())
}

/// Requires an ordinary directory as the root of a cached tree.
fn ensure_directory(path: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(fail("invalidInput", "Expected a cache tree directory"));
    }
    Ok(())
}

/// Computes the portable record used in identities and manifests.
fn record(path: &str, bytes: &[u8]) -> Record {
    Record {
        path: path.to_owned(),
        length: bytes.len() as u64,
        sha256: bytes_hash(bytes).to_ascii_lowercase(),
    }
}

/// Verifies the complete artifact shape before hashing its files.
fn artifact_records(root: &Path) -> Result<(Vec<Record>, usize), AppError> {
    ensure_directory(root)?;
    let raw = tree_records(&root.join(ARTIFACTS[0]), ARTIFACTS[0])?;
    if raw.is_empty() {
        return Err(fail("invalidInput", "Expanded instruction set is empty"));
    }
    let category = file_record(&root.join(ARTIFACTS[1]), ARTIFACTS[1])?;
    if category.length == 0 {
        return Err(fail("invalidInput", "Instruction categories are empty"));
    }
    let focus_path = root.join(ARTIFACTS[2]);
    let focus = tree_records(&focus_path, ARTIFACTS[2])?;
    let mut count = 0_usize;
    for entry in fs::read_dir(&focus_path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_file() {
            if entry.path().extension().and_then(|value| value.to_str()) != Some("md") {
                return Err(fail("invalidInput", "Unexpected focus-area file"));
            }
            let content = fs::read_to_string(entry.path())?;
            if content.trim().is_empty() {
                return Err(fail("invalidInput", "Focus-area file is empty"));
            }
            count = count.saturating_add(1);
        }
    }
    if count == 0 {
        return Err(fail("invalidInput", "Focus-area set is empty"));
    }
    let mut records = raw;
    records.push(category);
    records.extend(focus);
    records.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((records, count))
}

/// Counts only discovered user-scoped snapshot files.
fn user_instruction_count(input: &Path) -> Result<usize, AppError> {
    let external = input.join("external");
    match fs::symlink_metadata(&external) {
        Ok(_) => Ok(tree_records(&external, "")?.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

/// Copies only declared artifacts, preserving each file's complete contents.
fn copy_artifacts(source: &Path, destination: &Path) -> Result<(), AppError> {
    for name in ARTIFACTS {
        copy_tree(&source.join(name), &destination.join(name))?;
    }
    Ok(())
}

/// Copies an artifact without following links or accepting special files.
#[expect(
    clippy::create_dir,
    reason = "Copies must not reuse existing artifact directories"
)]
fn copy_tree(source: &Path, destination: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(fail("invalidInput", "Cache payload contains a link"));
    }
    if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        fs::copy(source, destination)?;
    } else {
        return Err(fail(
            "invalidInput",
            "Cache payload contains a special file",
        ));
    }
    Ok(())
}

/// Creates a unique sibling directory; ownership comes from exclusive creation.
#[expect(
    clippy::create_dir,
    reason = "Exclusive creation arbitrates staging-name collisions"
)]
fn new_directory(parent: &Path, prefix: &str) -> Result<PathBuf, AppError> {
    // A finite collision budget prevents abandoned directories from causing a hang.
    for _ in 0..1024 {
        let number = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!("{prefix}{}-{number}.tmp", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(fail(
        "io",
        "Cannot allocate a unique cache staging directory",
    ))
}

/// Removes a cache-owned path without following a link at its root.
fn remove_path(path: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Prunes only recognized old entries while never waiting on another writer.
fn maintenance(cache: &Path, current: &str, now: i64) -> Vec<String> {
    let mut warnings = Vec::new();
    let _maintenance =
        match ResourceLock::try_open(&cache.join(".locks/maintenance"), LockMode::Exclusive) {
            Ok(Some(lock)) => lock,
            Ok(None) => return warnings,
            Err(error) => {
                warnings.push(format!("Cannot lock cache maintenance: {error}"));
                return warnings;
            }
        };
    let entries = match fs::read_dir(cache) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("Cannot enumerate cache: {error}"));
            return warnings;
        }
    };
    let mut candidates = Vec::new();
    let mut staging = Vec::new();
    let mut current_exists = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!("Cannot inspect cache directory entry: {error}"));
                continue;
            }
        };
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                warnings.push(format!(
                    "Cannot inspect {}: {error}",
                    entry.path().display()
                ));
                continue;
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if is_key(&name) {
            // The caller already owns this key lock, so it must not be reacquired.
            if name == current {
                current_exists = true;
                continue;
            }
            let lock = match ResourceLock::try_open(&key_path(cache, &name), LockMode::Exclusive) {
                Ok(Some(lock)) => lock,
                Ok(None) => continue,
                Err(error) => {
                    warnings.push(format!("Cannot lock cache entry {name}: {error}"));
                    continue;
                }
            };
            let created = maintenance_created(&entry.path(), &mut warnings);
            drop(lock);
            candidates.push((name, entry.path(), created));
        } else if let Some(key) = staging_key(&name) {
            let modified = match metadata.modified() {
                Ok(time) => time
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .and_then(|duration| i64::try_from(duration.as_secs()).ok()),
                Err(error) => {
                    warnings.push(format!(
                        "Cannot inspect staging age {}: {error}",
                        entry.path().display()
                    ));
                    None
                }
            };
            if modified.is_some_and(|time| now.saturating_sub(time) > STAGING_RETENTION_SECONDS) {
                staging.push((key.to_owned(), entry.path(), modified));
            }
        }
    }
    candidates.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));
    let limit = MAX_ENTRIES.saturating_sub(usize::from(current_exists));
    for (index, (key, path, created)) in candidates.into_iter().enumerate() {
        if index >= limit || now.saturating_sub(created) > RETENTION_SECONDS {
            remove_if_unlocked(cache, &key, &path, Some(created), None, &mut warnings);
        }
    }
    for (key, path, modified) in staging {
        if key != current {
            remove_if_unlocked(cache, &key, &path, None, modified, &mut warnings);
        }
    }
    warnings
}

/// Deletes only after acquiring the same key lock used by readers and writers.
fn remove_if_unlocked(
    cache: &Path,
    key: &str,
    path: &Path,
    created: Option<i64>,
    modified: Option<i64>,
    warnings: &mut Vec<String>,
) {
    match ResourceLock::try_open(&key_path(cache, key), LockMode::Exclusive) {
        Ok(Some(_lock)) => {
            if created.is_some_and(|expected| maintenance_created(path, warnings) != expected) {
                return;
            }
            if let Some(expected) = modified {
                let current = fs::symlink_metadata(path)
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .and_then(|duration| i64::try_from(duration.as_secs()).ok());
                if current != Some(expected) {
                    return;
                }
            }
            if let Err(error) = remove_path(path) {
                warnings.push(format!("Cannot remove {}: {error}", path.display()));
            }
        }
        Ok(None) => {}
        Err(error) => warnings.push(format!("Cannot lock cache entry {key}: {error}")),
    }
}

/// Reads maintenance metadata only from an ordinary manifest under its key lock.
fn maintenance_created(entry: &Path, warnings: &mut Vec<String>) -> i64 {
    let path = entry.join("manifest.json");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return i64::MIN,
        Err(error) => {
            warnings.push(format!("Cannot inspect {}: {error}", path.display()));
            return i64::MIN;
        }
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            warnings.push(format!(
                "Cache manifest is not a regular file: {}",
                path.display()
            ));
            return i64::MIN;
        }
        Ok(_) => {}
    }
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Manifest>(&bytes) {
            Ok(manifest) => manifest.created_at_unix,
            Err(error) => {
                warnings.push(format!(
                    "Invalid cache manifest {}: {error}",
                    path.display()
                ));
                i64::MIN
            }
        },
        Err(error) => {
            warnings.push(format!(
                "Cannot read cache manifest {}: {error}",
                path.display()
            ));
            i64::MIN
        }
    }
}

/// Recognizes the exact cache-entry key alphabet.
fn is_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Recognizes only native staging names, leaving unrelated files untouched.
fn staging_key(name: &str) -> Option<&str> {
    let (key, suffix) = name.strip_prefix('.')?.split_once('.')?;
    let (pid, counter) = suffix.strip_suffix(".tmp")?.split_once('-')?;
    if is_key(key)
        && !pid.is_empty()
        && !counter.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && counter.bytes().all(|byte| byte.is_ascii_digit())
    {
        Some(key)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use ohno::AppError;
    use ss_ai_runtime::fail;

    use super::{ARTIFACTS, Record, SwapIo, swap_with, verify_staged};

    /// Simulates installation and rollback failures without filesystem timing.
    struct MockSwap {
        paths: RefCell<HashSet<PathBuf>>,
        fail_backup: bool,
        fail_install: bool,
        fail_rollback: bool,
    }

    impl MockSwap {
        fn new(fail_backup: bool, fail_rollback: bool) -> Self {
            let mut paths = HashSet::new();
            for name in ARTIFACTS {
                paths.insert(Path::new("session").join(name));
                paths.insert(Path::new("stage").join(name));
            }
            Self {
                paths: RefCell::new(paths),
                fail_backup,
                fail_install: !fail_backup,
                fail_rollback,
            }
        }
    }

    impl SwapIo for MockSwap {
        fn exists(&self, path: &Path) -> Result<bool, AppError> {
            Ok(self.paths.borrow().contains(path))
        }

        fn rename(&self, source: &Path, destination: &Path) -> Result<(), AppError> {
            if (self.fail_backup && source == Path::new("session/instruction-set-categories.md"))
                || (self.fail_install && source == Path::new("stage/focus-areas"))
                || (self.fail_rollback
                    && source == Path::new("backup/instruction-set-categories.md"))
            {
                return Err(fail("io", "injected rename failure"));
            }
            let mut paths = self.paths.borrow_mut();
            if !paths.remove(source) || paths.contains(destination) {
                return Err(fail("io", "invalid simulated move"));
            }
            paths.insert(destination.to_owned());
            Ok(())
        }

        fn remove(&self, path: &Path) -> Result<(), AppError> {
            if self.paths.borrow_mut().remove(path) {
                Ok(())
            } else {
                Err(fail("io", "invalid simulated removal"))
            }
        }
    }

    #[test]
    fn failed_install_restores_every_previous_artifact() {
        let io = MockSwap::new(false, false);
        assert!(
            swap_with(
                &io,
                Path::new("session"),
                Path::new("stage"),
                Path::new("backup"),
                || Ok(())
            )
            .unwrap()
            .is_some()
        );
        for name in ARTIFACTS {
            assert!(io.paths.borrow().contains(&Path::new("session").join(name)));
            assert!(!io.paths.borrow().contains(&Path::new("backup").join(name)));
        }
    }

    #[test]
    fn failed_rollback_retains_the_unrecovered_backup() {
        let io = MockSwap::new(false, true);
        let result = swap_with(
            &io,
            Path::new("session"),
            Path::new("stage"),
            Path::new("backup"),
            || Ok(()),
        );
        let _error = result.unwrap_err();
        assert!(
            io.paths
                .borrow()
                .contains(&Path::new("backup/instruction-set-categories.md").to_owned())
        );
        assert!(
            io.paths
                .borrow()
                .contains(&Path::new("session/instruction-set-raw").to_owned())
        );
        assert!(
            io.paths
                .borrow()
                .contains(&Path::new("session/focus-areas").to_owned())
        );
    }

    #[test]
    fn failed_backup_restores_already_moved_artifacts() {
        let io = MockSwap::new(true, false);
        assert!(
            swap_with(
                &io,
                Path::new("session"),
                Path::new("stage"),
                Path::new("backup"),
                || Ok(())
            )
            .unwrap()
            .is_some()
        );
        for name in ARTIFACTS {
            assert!(io.paths.borrow().contains(&Path::new("session").join(name)));
            assert!(!io.paths.borrow().contains(&Path::new("backup").join(name)));
        }
    }

    #[test]
    fn failed_hit_receipt_restores_every_previous_artifact() {
        let mut io = MockSwap::new(false, false);
        io.fail_install = false;
        let result = swap_with(
            &io,
            Path::new("session"),
            Path::new("stage"),
            Path::new("backup"),
            || Err(fail("io", "injected receipt failure")),
        )
        .unwrap();
        assert!(result.is_some());
        for name in ARTIFACTS {
            assert!(io.paths.borrow().contains(&Path::new("session").join(name)));
            assert!(!io.paths.borrow().contains(&Path::new("backup").join(name)));
        }
    }

    #[test]
    fn staged_validation_failure_does_not_accept_the_copy() {
        let expected = vec![Record {
            path: "focus-areas/a.md".into(),
            length: 1,
            sha256: "a".into(),
        }];
        let result = verify_staged(
            &expected,
            &expected,
            "before",
            || Err(fail("invalidInput", "injected validation failure")),
            || Ok((expected.clone(), "before".into())),
        );
        let _error = result.unwrap_err();
    }

    #[test]
    fn staged_changes_after_validation_are_not_accepted() {
        let expected = vec![Record {
            path: "focus-areas/a.md".into(),
            length: 1,
            sha256: "a".into(),
        }];
        let changed = vec![Record {
            path: "focus-areas/a.md".into(),
            length: 1,
            sha256: "b".into(),
        }];
        let result = verify_staged(
            &expected,
            &expected,
            "before",
            || Ok(()),
            || Ok((changed, "before".into())),
        );
        let _error = result.unwrap_err();
    }

    #[test]
    fn staged_digest_changes_after_validation_are_not_accepted() {
        let expected = vec![Record {
            path: "focus-areas/a.md".into(),
            length: 1,
            sha256: "a".into(),
        }];
        let result = verify_staged(
            &expected,
            &expected,
            "before",
            || Ok(()),
            || Ok((expected.clone(), "after".into())),
        );
        let _error = result.unwrap_err();
    }

    #[test]
    fn staged_rehash_failure_does_not_accept_the_copy() {
        let expected = vec![Record {
            path: "focus-areas/a.md".into(),
            length: 1,
            sha256: "a".into(),
        }];
        let result = verify_staged(
            &expected,
            &expected,
            "before",
            || Ok(()),
            || Err(fail("io", "injected rehash failure")),
        );
        let _error = result.unwrap_err();
    }
}
