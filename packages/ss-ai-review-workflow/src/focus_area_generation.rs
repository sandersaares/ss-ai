//! Isolates focus-area preparation and publishes only the active generation.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ss_ai_review_context::bytes_hash;
use ss_ai_runtime::{Clock, FileTransaction, LockMode, ResourceLock, fail, read_bytes};

use crate::focus_area_expansion::Expansion;
use crate::focus_area_map::{CompleteMap, retry_contention};
use crate::focus_area_source::SourceSnapshot;
use crate::focus_cache::read_generation_key_locked;

/// The files that form a complete published preparation.
const OUTPUTS: [&str; 5] = [
    "instruction-set-raw",
    "focus-area-expansion-status.json",
    "focus-area-map.json",
    "instruction-set-categories.md",
    "focus-areas",
];
/// A restored cache entry does not include preparation records.
const HIT_OUTPUTS: [&str; 3] = [
    "instruction-set-raw",
    "instruction-set-categories.md",
    "focus-areas",
];
const ACTIVE: &str = "focus-area-generation.json";
const SNAPSHOT: &str = "focus-area-source-identity.json";
const GENERATIONS: &str = ".focus-area-generations";

/// The private session assigned to one preparation attempt.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Generation {
    /// Opaque identifier supplied when publishing.
    pub generation_id: String,
    /// Absolute path passed to the existing preparation commands.
    pub session_state: String,
}

/// The persisted generation schema, validated before conversion to live state.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActiveGenerationRecord {
    schema_version: u32,
    preparation_identity: Option<String>,
    generation_id: Option<String>,
    source_digest: Option<String>,
    cache_key: Option<String>,
    receipt: Option<String>,
    hit_receipt: Option<String>,
}

impl ActiveGenerationRecord {
    /// Represents a session with no preparation authorized to publish.
    fn invalidated() -> Self {
        Self {
            schema_version: 1,
            preparation_identity: None,
            generation_id: None,
            source_digest: None,
            cache_key: None,
            receipt: None,
            hit_receipt: None,
        }
    }
}

/// Fences old workers and binds a successful publication to its inputs.
struct ActiveGeneration {
    preparation: Option<Preparation>,
}

/// Keeps the identity, its source, and its publication state together.
struct Preparation {
    identity: String,
    source_digest: String,
    cache_key: Option<String>,
    work: PreparationWork,
}

/// A restore and a generation have mutually exclusive receipt types.
enum PreparationWork {
    Restore { receipt: Option<String> },
    Generation { id: String, receipt: Option<String> },
}

impl ActiveGeneration {
    /// Represents a session with no preparation authorized to publish.
    fn invalidated() -> Self {
        Self { preparation: None }
    }

    /// Converts the checked wire record without accepting partial identities.
    fn from_record(record: ActiveGenerationRecord) -> Result<Self, AppError> {
        record.validate()?;
        let preparation = match (record.preparation_identity, record.source_digest) {
            (Some(identity), Some(source_digest)) => Some(Preparation {
                identity,
                source_digest,
                cache_key: record.cache_key,
                work: match record.generation_id {
                    Some(id) => PreparationWork::Generation {
                        id,
                        receipt: record.receipt,
                    },
                    None => PreparationWork::Restore {
                        receipt: record.hit_receipt,
                    },
                },
            }),
            (None, None) => None,
            _ => {
                return Err(fail(
                    "invalidInput",
                    "Invalid preparation generation record",
                ));
            }
        };
        Ok(Self { preparation })
    }

    /// Produces the established flat JSON record for persistence.
    fn to_record(&self) -> ActiveGenerationRecord {
        let mut record = ActiveGenerationRecord::invalidated();
        if let Some(preparation) = &self.preparation {
            record.preparation_identity = Some(preparation.identity.clone());
            record.source_digest = Some(preparation.source_digest.clone());
            record.cache_key.clone_from(&preparation.cache_key);
            match &preparation.work {
                PreparationWork::Restore { receipt } => record.hit_receipt.clone_from(receipt),
                PreparationWork::Generation { id, receipt } => {
                    record.generation_id = Some(id.clone());
                    record.receipt.clone_from(receipt);
                }
            }
        }
        record
    }

    /// Updates the publication receipt only for the active generation.
    fn set_generation_receipt(&mut self, receipt: Option<String>) -> Result<(), AppError> {
        let Some(Preparation {
            work: PreparationWork::Generation {
                receipt: current, ..
            },
            ..
        }) = &mut self.preparation
        else {
            return Err(fail(
                "staleGeneration",
                "Preparation generation is not active",
            ));
        };
        *current = receipt;
        Ok(())
    }
}

impl ActiveGenerationRecord {
    /// Rejects records that cannot describe one coherent generation.
    fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != 1
            || self.preparation_identity.is_some() != self.source_digest.is_some()
            || self.preparation_identity.is_none()
                && (self.generation_id.is_some()
                    || self.cache_key.is_some()
                    || self.receipt.is_some()
                    || self.hit_receipt.is_some())
            || self.generation_id.is_none() && self.receipt.is_some()
            || self.generation_id.is_some() && self.hit_receipt.is_some()
            || self
                .preparation_identity
                .as_ref()
                .is_some_and(|id| !valid_epoch(id))
            || self.generation_id.as_ref().is_some_and(|id| !valid_id(id))
            || self
                .source_digest
                .as_ref()
                .is_some_and(|digest| !valid_digest(digest))
            || self
                .cache_key
                .as_ref()
                .is_some_and(|digest| !valid_digest(digest))
            || self
                .receipt
                .as_ref()
                .is_some_and(|digest| !valid_digest(digest))
            || self
                .hit_receipt
                .as_ref()
                .is_some_and(|digest| !valid_digest(digest))
        {
            return Err(fail(
                "invalidInput",
                "Invalid preparation generation record",
            ));
        }
        Ok(())
    }
}

/// Binds the discovery mapping returned by a successful source snapshot.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotIdentity {
    schema_version: u32,
    identity: Option<String>,
    source_digest: Option<String>,
}

/// Invalidates source discovery before attempting a new snapshot.
///
/// # Errors
///
/// Returns an error when the invalidation cannot be published.
pub fn invalidate_snapshot_locked(session: &Path, clock: &dyn Clock) -> Result<(), AppError> {
    let transaction = FileTransaction::open(&session.join(SNAPSHOT), clock)?;
    transaction.write_bytes(&serde_json::to_vec(&SnapshotIdentity {
        schema_version: 1,
        identity: None,
        source_digest: None,
    })?)
}

/// Binds source discovery to the exact successfully published source tree.
///
/// # Errors
///
/// Returns an error if sources cannot be identified or the record cannot be published.
pub fn bind_snapshot_locked(session: &Path, clock: &dyn Clock) -> Result<String, AppError> {
    let source_digest = tree_digest(&session.join("instruction-set-source"), clock)?;
    let identity = new_epoch(session)?;
    let transaction = FileTransaction::open(&session.join(SNAPSHOT), clock)?;
    transaction.write_bytes(&serde_json::to_vec(&SnapshotIdentity {
        schema_version: 1,
        identity: Some(identity.clone()),
        source_digest: Some(source_digest),
    })?)?;
    Ok(identity)
}

/// Verifies a caller's source discovery identity without adopting a newer one.
///
/// # Errors
///
/// Returns an error for malformed records or unreadable sources.
pub fn verify_snapshot_locked(
    session: &Path,
    expected: &str,
    clock: &dyn Clock,
) -> Result<bool, AppError> {
    let Some(value) = read_snapshot_identity(session, clock)? else {
        return Ok(false);
    };
    let Some(digest) = value.source_digest else {
        return Ok(false);
    };
    Ok(value.identity.as_deref() == Some(expected)
        && tree_digest(&session.join("instruction-set-source"), clock)? == digest)
}

/// Issues a fresh restore identity for exact matching source inputs.
///
/// # Errors
///
/// Returns an error when the input differs from the materialized source.
pub fn bind_restore_locked(
    session: &Path,
    input_directory: &Path,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    let source_digest = tree_digest(&session.join("instruction-set-source"), clock)?;
    let snapshot = read_snapshot_identity(session, clock)?;
    if snapshot
        .and_then(|identity| identity.source_digest)
        .as_deref()
        != Some(&source_digest)
        || tree_digest(input_directory, clock)? != source_digest
    {
        return Err(fail(
            "invalidInput",
            "Cache inputs do not match instruction sources",
        ));
    }
    let identity = new_epoch(session)?;
    let cache_key = read_generation_key_locked(session, clock)?;
    let transaction = FileTransaction::open(&session.join(ACTIVE), clock)?;
    write_active(
        &transaction,
        &ActiveGeneration {
            preparation: Some(Preparation {
                identity: identity.clone(),
                source_digest,
                cache_key,
                work: PreparationWork::Restore { receipt: None },
            }),
        },
    )?;
    Ok(identity)
}

/// Reads the persisted source identity without accepting malformed records.
fn read_snapshot_identity(
    session: &Path,
    clock: &dyn Clock,
) -> Result<Option<SnapshotIdentity>, AppError> {
    let transaction = FileTransaction::open(&session.join(SNAPSHOT), clock)?;
    let Some(bytes) = transaction.read_bytes()? else {
        return Ok(None);
    };
    let value: SnapshotIdentity = serde_json::from_slice(&bytes)?;
    if value.schema_version != 1
        || value.identity.is_some() != value.source_digest.is_some()
        || value.identity.as_ref().is_some_and(|id| !valid_epoch(id))
        || value
            .source_digest
            .as_ref()
            .is_some_and(|digest| !valid_digest(digest))
    {
        return Err(fail("invalidInput", "Invalid instruction source identity"));
    }
    Ok(Some(value))
}

/// Invalidates preparation while the caller owns the real session operation lock.
///
/// # Errors
///
/// Returns an error when the record cannot be published.
pub fn invalidate_locked(session: &Path, clock: &dyn Clock) -> Result<(), AppError> {
    let transaction = FileTransaction::open(&session.join(ACTIVE), clock)?;
    write_active(&transaction, &ActiveGeneration::invalidated())
}

/// Reports whether the session participates in coordinated preparation.
///
/// The caller holds the real session operation lock.
///
/// # Errors
///
/// Returns an error for malformed state or an unreadable record.
pub fn is_managed_locked(session: &Path, clock: &dyn Clock) -> Result<bool, AppError> {
    let transaction = FileTransaction::open(&session.join(ACTIVE), clock)?;
    let Some(bytes) = transaction.read_bytes()? else {
        return Ok(false);
    };
    let record: ActiveGenerationRecord = serde_json::from_slice(&bytes)?;
    ActiveGeneration::from_record(record)?;
    Ok(true)
}

/// Verifies a caller's restore identity while the real operation lock is held.
///
/// # Errors
///
/// Returns an error for malformed state or unreadable sources.
pub fn verify_preparation_locked(
    session: &Path,
    expected_identity: &str,
    clock: &dyn Clock,
) -> Result<bool, AppError> {
    let transaction = FileTransaction::open(&session.join(ACTIVE), clock)?;
    let active = read_active(&transaction)?;
    let Some(preparation) = active.preparation else {
        return Ok(false);
    };
    Ok(preparation.identity == expected_identity
        && tree_digest(&session.join("instruction-set-source"), clock)?
            == preparation.source_digest)
}

/// Computes the exact cache payload identity after staged validation.
///
/// # Errors
///
/// Returns an error if staged artifacts are missing, unsafe, or unreadable.
pub fn staged_hit_digest(staged_root: &Path, clock: &dyn Clock) -> Result<String, AppError> {
    hit_digest(staged_root, clock)
}

/// Binds a verified cache hit to its live artifact bytes.
///
/// The caller holds the real session operation and artifact locks and must
/// roll back the cache swap if this operation fails.
///
/// # Errors
///
/// Returns an error if the identity, key, staged digest, or live files differ.
pub fn record_hit_locked(
    session: &Path,
    expected_identity: &str,
    expected_key: &str,
    staged_digest: &str,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let transaction = FileTransaction::open(&session.join(ACTIVE), clock)?;
    let mut active = read_active(&transaction)?;
    let Some(preparation) = active.preparation.as_mut() else {
        return Err(fail(
            "staleGeneration",
            "Cache hit does not match the active preparation",
        ));
    };
    if preparation.identity != expected_identity
        || preparation.cache_key.as_deref() != Some(expected_key)
        || !matches!(preparation.work, PreparationWork::Restore { .. })
        || !valid_digest(staged_digest)
        || preparation.source_digest != tree_digest(&session.join("instruction-set-source"), clock)?
        || hit_digest(session, clock)? != staged_digest
    {
        return Err(fail(
            "staleGeneration",
            "Cache hit does not match the active preparation",
        ));
    }
    preparation.work = PreparationWork::Restore {
        receipt: Some(staged_digest.to_owned()),
    };
    write_active(&transaction, &active)
}

/// Checks whether the active preparation has the exact verified live artifacts.
///
/// The caller holds the real session operation and artifact locks.
///
/// # Errors
///
/// Returns an error for malformed state or unreadable files.
pub fn verify_ready_locked(
    session: &Path,
    expected_identity: &str,
    clock: &dyn Clock,
) -> Result<bool, AppError> {
    let transaction = FileTransaction::open(&session.join(ACTIVE), clock)?;
    let active = read_active(&transaction)?;
    let Some(preparation) = active.preparation else {
        return Ok(false);
    };
    if preparation.identity != expected_identity {
        return Ok(false);
    }
    if tree_digest(&session.join("instruction-set-source"), clock)? != preparation.source_digest {
        return Ok(false);
    }
    match preparation.work {
        PreparationWork::Generation {
            receipt: Some(receipt),
            ..
        } => {
            if !outputs_present(session, &OUTPUTS)? {
                return Ok(false);
            }
            Ok(publication_digest(session, clock)? == receipt)
        }
        PreparationWork::Restore {
            receipt: Some(receipt),
        } => {
            if !outputs_present(session, &HIT_OUTPUTS)? {
                return Ok(false);
            }
            Ok(hit_digest(session, clock)? == receipt)
        }
        _ => Ok(false),
    }
}

/// Clears failed artifact readiness without allowing a stale caller to change it.
///
/// The caller holds the real session operation lock.
///
/// # Errors
///
/// Returns an error if the identity is stale or the record cannot be updated.
pub fn invalidate_ready_locked(
    session: &Path,
    expected_identity: &str,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let transaction = FileTransaction::open(&session.join(ACTIVE), clock)?;
    let mut active = read_active(&transaction)?;
    let Some(preparation) = active
        .preparation
        .as_mut()
        .filter(|value| value.identity == expected_identity)
    else {
        return Err(fail(
            "staleGeneration",
            "Preparation identity is not active",
        ));
    };
    match &mut preparation.work {
        PreparationWork::Restore { receipt } | PreparationWork::Generation { receipt, .. } => {
            *receipt = None;
        }
    }
    write_active(&transaction, &active)
}

/// Starts a private preparation from an exact copy of the current sources.
///
/// # Errors
///
/// Returns an error if the session, source snapshot, or publication fails.
pub fn begin(
    session: &Path,
    expected_identity: &str,
    clock: &dyn Clock,
) -> Result<Generation, AppError> {
    let session = canonical_session(session)?;
    let _operation = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        clock,
    )?;
    let transaction = FileTransaction::open(&session.join(ACTIVE), clock)?;
    let active = read_active(&transaction)?;
    let Some(preparation) = active.preparation.filter(|preparation| {
        preparation.identity == expected_identity
            && matches!(preparation.work, PreparationWork::Restore { .. })
    }) else {
        return Err(fail(
            "staleGeneration",
            "Preparation identity is not active",
        ));
    };
    let source = session.join("instruction-set-source");
    let before = tree_digest(&source, clock)?;
    if preparation.source_digest != before {
        return Err(fail("invalidInput", "Preparation sources changed"));
    }
    let root = session.join(GENERATIONS);
    ensure_directory(&root)?;
    let (id, generation) = allocate(&root, "generation")?;
    let result = (|| {
        copy_tree(&source, &generation.join("instruction-set-source"), clock)?;
        copy_tree(&source, &generation.join("instruction-set-raw"), clock)?;
        if tree_digest(&source, clock)? != before
            || tree_digest(&generation.join("instruction-set-source"), clock)? != before
            || tree_digest(&generation.join("instruction-set-raw"), clock)? != before
        {
            return Err(fail(
                "invalidInput",
                "Instruction sources changed during preparation",
            ));
        }
        let state = ActiveGeneration {
            preparation: Some(Preparation {
                identity: preparation.identity,
                source_digest: before,
                cache_key: preparation.cache_key,
                work: PreparationWork::Generation {
                    id: id.clone(),
                    receipt: None,
                },
            }),
        };
        write_active(&transaction, &state)?;
        Ok(Generation {
            generation_id: id,
            session_state: path_text(&generation)?.to_owned(),
        })
    })();
    if let Err(error) = result {
        return match fs::remove_dir_all(&generation) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(fail(
                "io",
                format!("Preparation failed: {error}; cleanup failed: {cleanup}"),
            )),
        };
    }
    result
}

/// Publishes a fully validated active generation under the real session lock.
///
/// # Errors
///
/// Returns an error for stale generations, changed sources, invalid artifacts,
/// or a failed publication transaction.
#[expect(
    clippy::create_dir,
    reason = "The backup must be new inside a private staging directory"
)]
pub fn publish(session: &Path, generation_id: &str, clock: &dyn Clock) -> Result<Value, AppError> {
    let session = canonical_session(session)?;
    let _operation = ResourceLock::open(
        &session.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        clock,
    )?;
    let transaction = FileTransaction::open(&session.join(ACTIVE), clock)?;
    let mut active = read_active(&transaction)?;
    if !valid_id(generation_id)
        || !matches!(&active.preparation, Some(Preparation {
            work: PreparationWork::Generation { id, .. }, ..
        }) if id == generation_id)
    {
        return Err(fail(
            "staleGeneration",
            "Preparation generation is not active",
        ));
    }
    let root = session.join(GENERATIONS);
    ensure_existing_directory(&root)?;
    let generation = root.join(generation_id);
    ensure_existing_directory(&generation)?;
    let _generation_operation = ResourceLock::open(
        &generation.join("focus-area-cache-operation"),
        LockMode::Exclusive,
        clock,
    )?;
    let _generation_artifacts =
        ResourceLock::open(&generation.join("focus-areas"), LockMode::Exclusive, clock)?;
    let _real_artifacts =
        ResourceLock::open(&session.join("focus-areas"), LockMode::Exclusive, clock)?;
    let expected_source = active
        .preparation
        .as_ref()
        .map(|preparation| preparation.source_digest.clone())
        .ok_or_else(|| fail("invalidInput", "Preparation source identity is missing"))?;
    if tree_digest(&session.join("instruction-set-source"), clock)? != expected_source
        || tree_digest(&generation.join("instruction-set-source"), clock)? != expected_source
    {
        return Err(fail("invalidInput", "Preparation sources changed"));
    }
    let (_, stage) = allocate(&session, ".focus-area-publication")?;
    let mut rollback_failed = false;
    let result = (|| {
        copy_tree(
            &generation.join("instruction-set-source"),
            &stage.join("instruction-set-source"),
            clock,
        )?;
        for name in OUTPUTS {
            copy_entry(&generation.join(name), &stage.join(name), clock)?;
        }
        let staged = publication_digest(&stage, clock)?;
        if publication_digest(&generation, clock)? != staged
            || tree_digest(&stage.join("instruction-set-source"), clock)? != expected_source
        {
            return Err(fail("invalidInput", "Preparation changed during staging"));
        }
        validate_staged(&stage, clock)?;
        if publication_digest(&stage, clock)? != staged
            || publication_digest(&generation, clock)? != staged
            || tree_digest(&session.join("instruction-set-source"), clock)? != expected_source
        {
            return Err(fail(
                "invalidInput",
                "Preparation changed during validation",
            ));
        }
        active.set_generation_receipt(None)?;
        write_active(&transaction, &active)?;
        let backup = stage.join("backup");
        fs::create_dir(&backup)?;
        swap_with(
            &FilesystemSwap { clock },
            &session,
            &stage,
            &backup,
            &mut rollback_failed,
            || {
                active.set_generation_receipt(Some(staged.clone()))?;
                write_active(&transaction, &active)
            },
        )?;
        Ok(json!({"status":"Published","generationId":generation_id}))
    })();
    // Failed rollback leaves backup data intact for recovery.
    if rollback_failed {
        return result;
    }
    match (result, fs::remove_dir_all(&stage)) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(fail(
            "io",
            format!("Cannot remove publication staging: {error}"),
        )),
        (Err(error), Err(cleanup)) => Err(fail(
            "io",
            format!("Publication failed: {error}; staging cleanup failed: {cleanup}"),
        )),
    }
}

/// Checks a published receipt while operation and artifact locks are held.
///
/// # Errors
///
/// Returns an error for malformed records or unreadable source or artifact data.
pub fn validate_published_locked(
    session: &Path,
    input_directory: &Path,
    expected_key: &str,
    clock: &dyn Clock,
) -> Result<bool, AppError> {
    let transaction = FileTransaction::open(&session.join(ACTIVE), clock)?;
    let active = read_active(&transaction)?;
    let Some(Preparation {
        source_digest: source,
        cache_key,
        work:
            PreparationWork::Generation {
                receipt: Some(receipt),
                ..
            },
        ..
    }) = &active.preparation
    else {
        return Ok(false);
    };
    if cache_key.as_deref() != Some(expected_key)
        || tree_digest(&session.join("instruction-set-source"), clock)? != *source
        || tree_digest(input_directory, clock)? != *source
    {
        return Ok(false);
    }
    Ok(publication_digest(session, clock)? == *receipt)
}

/// Validates the staged manifest, map, and exact renderer output.
fn validate_staged(stage: &Path, clock: &dyn Clock) -> Result<(), AppError> {
    let sources = SourceSnapshot::read(stage, clock)?;
    let expansion = required_bytes(&stage.join("focus-area-expansion-status.json"), clock)?;
    let expansion = Expansion::parse(&expansion, &sources, true)?;
    let map = required_bytes(&stage.join("focus-area-map.json"), clock)?;
    let map = CompleteMap::parse(&map, &sources)?;
    map.validate_reference_alternatives(&expansion)?;
    let documents = map.render(&sources)?;
    let mut actual = BTreeMap::new();
    collect_files(stage, &stage.join("focus-areas"), clock, &mut actual)?;
    actual.insert(
        "instruction-set-categories.md".to_owned(),
        required_bytes(&stage.join("instruction-set-categories.md"), clock)?,
    );
    if documents.len() != actual.len()
        || documents.iter().any(|(path, text)| {
            actual
                .get(path)
                .is_none_or(|bytes| bytes != text.as_bytes())
        })
    {
        return Err(fail(
            "invalidInput",
            "Published documents do not match the validated map",
        ));
    }
    Ok(())
}

/// Reads and validates the current fencing record.
fn read_active(transaction: &FileTransaction<'_>) -> Result<ActiveGeneration, AppError> {
    let Some(bytes) = transaction.read_bytes()? else {
        return Ok(ActiveGeneration::invalidated());
    };
    let value: ActiveGenerationRecord = serde_json::from_slice(&bytes)?;
    ActiveGeneration::from_record(value)
}

/// Publishes an internally consistent fencing record.
fn write_active(
    transaction: &FileTransaction<'_>,
    value: &ActiveGeneration,
) -> Result<(), AppError> {
    let record = value.to_record();
    record.validate()?;
    transaction.write_bytes(&serde_json::to_vec(&record)?)
}

/// Identifies all staged files by exact spelling, kind, and bytes.
fn publication_digest(root: &Path, clock: &dyn Clock) -> Result<String, AppError> {
    let mut entries = BTreeMap::new();
    collect_entry(
        root,
        &root.join("instruction-set-source"),
        clock,
        &mut entries,
    )?;
    for name in OUTPUTS {
        collect_entry(root, &root.join(name), clock, &mut entries)?;
    }
    Ok(bytes_hash(&serde_json::to_vec(&entries)?).to_ascii_lowercase())
}

/// Identifies exactly the reusable outputs without requiring preparation records.
fn hit_digest(root: &Path, clock: &dyn Clock) -> Result<String, AppError> {
    let mut entries = BTreeMap::new();
    for name in HIT_OUTPUTS {
        collect_entry(root, &root.join(name), clock, &mut entries)?;
    }
    Ok(bytes_hash(&serde_json::to_vec(&entries)?).to_ascii_lowercase())
}

/// Treats a missing output as an unready preparation rather than valid coverage.
fn outputs_present(root: &Path, outputs: &[&str]) -> Result<bool, AppError> {
    for name in outputs {
        match fs::symlink_metadata(root.join(name)) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

/// Identifies a source tree without depending on its absolute location.
fn tree_digest(path: &Path, clock: &dyn Clock) -> Result<String, AppError> {
    ensure_existing_directory(path)?;
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        collect_entry(path, &entry?.path(), clock, &mut entries)?;
    }
    Ok(bytes_hash(&serde_json::to_vec(&entries)?).to_ascii_lowercase())
}

/// Records files and directories without following links.
fn collect_entry(
    root: &Path,
    path: &Path,
    clock: &dyn Clock,
    entries: &mut BTreeMap<String, String>,
) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path)?;
    let relative = path
        .strip_prefix(root)
        .map_err(|_error| fail("invalidInput", "Invalid artifact path"))?;
    let name = relative_name(relative)?;
    if metadata.file_type().is_symlink() {
        return Err(fail(
            "invalidInput",
            "Preparation trees must not contain links",
        ));
    }
    if metadata.is_dir() {
        entries.insert(name, "directory".to_owned());
        for entry in fs::read_dir(path)? {
            collect_entry(root, &entry?.path(), clock, entries)?;
        }
    } else if metadata.is_file() {
        let bytes = required_bytes(path, clock)?;
        entries.insert(
            name,
            format!("file:{}", bytes_hash(&bytes).to_ascii_lowercase()),
        );
    } else {
        return Err(fail(
            "invalidInput",
            "Preparation entries must be files or directories",
        ));
    }
    Ok(())
}

/// Collects renderer documents by their relative paths.
fn collect_files(
    root: &Path,
    path: &Path,
    clock: &dyn Clock,
    entries: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(fail(
            "invalidInput",
            "Preparation trees must not contain links",
        ));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            collect_files(root, &entry?.path(), clock, entries)?;
        }
    } else if metadata.is_file() {
        let name = relative_name(
            path.strip_prefix(root)
                .map_err(|_error| fail("invalidInput", "Invalid document path"))?,
        )?;
        entries.insert(name, required_bytes(path, clock)?);
    } else {
        return Err(fail("invalidInput", "Invalid document entry"));
    }
    Ok(())
}

/// Copies a complete source tree without following links.
#[expect(
    clippy::create_dir,
    reason = "Copies must not reuse a pre-existing directory"
)]
fn copy_tree(source: &Path, destination: &Path, clock: &dyn Clock) -> Result<(), AppError> {
    ensure_existing_directory(source)?;
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        copy_entry(&entry.path(), &destination.join(entry.file_name()), clock)?;
    }
    Ok(())
}

/// Copies one file or directory into a private staging tree.
fn copy_entry(source: &Path, destination: &Path, clock: &dyn Clock) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(fail(
            "invalidInput",
            "Preparation trees must not contain links",
        ));
    }
    if metadata.is_dir() {
        copy_tree(source, destination, clock)
    } else if metadata.is_file() {
        fs::write(destination, required_bytes(source, clock)?)?;
        Ok(())
    } else {
        Err(fail(
            "invalidInput",
            "Preparation entries must be files or directories",
        ))
    }
}

/// Reads a required file with the runtime's shared-reader behavior.
fn required_bytes(path: &Path, clock: &dyn Clock) -> Result<Vec<u8>, AppError> {
    read_bytes(path, clock)?
        .ok_or_else(|| fail("invalidInput", format!("Missing {}", path.display())))
}

/// Rejects redirected or non-directory session roots.
fn canonical_session(path: &Path) -> Result<PathBuf, AppError> {
    if !path.is_absolute() {
        return Err(fail("invalidInput", "Session path must be absolute"));
    }
    let canonical = fs::canonicalize(path)?;
    ensure_existing_directory(&canonical)?;
    Ok(canonical)
}

/// Creates or validates the private generation container.
#[expect(
    clippy::create_dir,
    reason = "An existing container must be checked instead of silently reused"
)]
fn ensure_directory(path: &Path) -> Result<(), AppError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => ensure_existing_directory(path),
        Err(error) => Err(error.into()),
    }
}

/// Refuses links and other entries where a directory is required.
fn ensure_existing_directory(path: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(fail(
            "invalidInput",
            "Preparation directory must not be a link",
        ));
    }
    Ok(())
}

/// Creates a unique directory without reusing another attempt's files.
#[expect(
    clippy::create_dir,
    reason = "A collision must never reuse another generation's directory"
)]
fn allocate(parent: &Path, prefix: &str) -> Result<(String, PathBuf), AppError> {
    // A bounded search cannot wait indefinitely on abandoned attempts.
    for index in 0..10_000_u32 {
        let id = format!("{prefix}-{}-{index}", std::process::id());
        let path = parent.join(&id);
        match fs::create_dir(&path) {
            Ok(()) => return Ok((id, path)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(fail("io", "Cannot allocate preparation directory"))
}

/// Reserves an identity permanently so process restarts cannot reuse it.
fn new_epoch(session: &Path) -> Result<String, AppError> {
    let root = session.join(GENERATIONS);
    ensure_directory(&root)?;
    let (identity, _) = allocate(&root, "epoch")?;
    Ok(identity)
}

/// Checks the syntax of a reserved identity.
fn valid_epoch(id: &str) -> bool {
    id.starts_with("epoch-")
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase() || byte == b'-')
}

/// Restricts generation identifiers to generated single path components.
fn valid_id(id: &str) -> bool {
    id.starts_with("generation-")
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase() || byte == b'-')
}

/// Rejects malformed stored hash values.
fn valid_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Requires stable Unicode spelling for serialized paths.
fn path_text(path: &Path) -> Result<&str, AppError> {
    path.to_str()
        .ok_or_else(|| fail("invalidInput", "Preparation path is not Unicode"))
}

/// Preserves path component boundaries on case-sensitive and case-insensitive filesystems.
fn relative_name(path: &Path) -> Result<String, AppError> {
    path.components()
        .map(|component| {
            let name = path_text(Path::new(component.as_os_str()))?;
            if name.contains('\\') {
                return Err(fail(
                    "invalidInput",
                    "Preparation path contains an ambiguous separator",
                ));
            }
            Ok(name)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|components| components.join("/"))
}

/// Supplies filesystem effects to the same swap logic used in fault tests.
trait Swap {
    fn exists(&self, path: &Path) -> Result<bool, AppError>;
    fn rename(&self, from: &Path, to: &Path) -> Result<(), AppError>;
    fn remove(&self, path: &Path) -> Result<(), AppError>;
}

/// Applies the generation swap to real files.
struct FilesystemSwap<'a> {
    clock: &'a dyn Clock,
}

impl Swap for FilesystemSwap<'_> {
    fn exists(&self, path: &Path) -> Result<bool, AppError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(fail(
                "invalidInput",
                "Publication destination must not be a link",
            )),
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), AppError> {
        retry_contention(self.clock, || fs::rename(from, to))
    }

    fn remove(&self, path: &Path) -> Result<(), AppError> {
        retry_contention(self.clock, || {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            }
        })
    }
}

/// Swaps every output, rolling back failures including receipt publication.
fn swap_with(
    io: &impl Swap,
    session: &Path,
    stage: &Path,
    backup: &Path,
    rollback_failed: &mut bool,
    publish_receipt: impl FnOnce() -> Result<(), AppError>,
) -> Result<(), AppError> {
    let mut moved = Vec::new();
    let mut installed = Vec::new();
    let result = (|| {
        for name in OUTPUTS {
            let destination = session.join(name);
            if io.exists(&destination)? {
                io.rename(&destination, &backup.join(name))?;
                moved.push(name);
            }
            io.rename(&stage.join(name), &destination)?;
            installed.push(name);
        }
        publish_receipt()
    })();
    let Err(error) = result else { return Ok(()) };
    for name in installed.iter().rev() {
        *rollback_failed |= io.remove(&session.join(name)).is_err();
    }
    for name in moved.iter().rev() {
        *rollback_failed |= io.rename(&backup.join(name), &session.join(name)).is_err();
    }
    if *rollback_failed {
        return Err(fail(
            "io",
            format!(
                "Publication failed: {error}; rollback incomplete; backups remain at {}",
                backup.display()
            ),
        ));
    }
    Err(error)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use ohno::AppError;
    use serde_json::{Value, from_value, json, to_value, to_vec};
    use ss_ai_runtime::fail;

    use super::{ActiveGeneration, ActiveGenerationRecord, OUTPUTS, Swap, swap_with};

    #[test]
    fn generation_records_preserve_wire_shape_and_reject_incoherent_states() {
        let digest = "a".repeat(64);
        for mask in 0..64 {
            let present = |bit| mask & (1 << bit) != 0;
            let record = json!({
                "schemaVersion": 1,
                "preparationIdentity": present(0).then_some("epoch-1"),
                "generationId": present(1).then_some("generation-1"),
                "sourceDigest": present(2).then_some(&digest),
                "cacheKey": present(3).then_some(&digest),
                "receipt": present(4).then_some(&digest),
                "hitReceipt": present(5).then_some(&digest),
            });
            // The persisted optional-field combinations require preparation identity
            // and source digest together. Other work requires preparation; a
            // generation receipt requires a generation ID, while a restore receipt
            // excludes it. Accepted records retain their flat JSON and bytes.
            let expected = present(0) == present(2)
                && (present(0) || !(present(1) || present(3) || present(4) || present(5)))
                && (present(1) || !present(4))
                && (!present(1) || !present(5));
            let wire: ActiveGenerationRecord = from_value(record.clone()).unwrap();
            let wire_bytes = to_vec(&wire).unwrap();
            let active = ActiveGeneration::from_record(wire);
            assert_eq!(active.is_ok(), expected, "record: {record}");
            if let Ok(active) = active {
                let serialized_record = active.to_record();
                assert_eq!(to_vec(&serialized_record).unwrap(), wire_bytes);
                let serialized = to_value(serialized_record).unwrap();
                assert_eq!(serialized, record);
            }
        }

        let mut record = json!({"schemaVersion":1,"preparationIdentity":"epoch-1",
            "generationId":null,"sourceDigest":digest,"cacheKey":null,"receipt":null,"hitReceipt":null});
        for (field, invalid) in [
            ("schemaVersion", json!(2)),
            ("preparationIdentity", json!("bad")),
            ("generationId", json!("bad")),
            ("sourceDigest", json!("bad")),
            ("cacheKey", json!("bad")),
            ("receipt", json!("bad")),
            ("hitReceipt", json!("bad")),
        ] {
            let original = record.get(field).unwrap().clone();
            *record.get_mut(field).unwrap() = invalid;
            let wire: ActiveGenerationRecord = from_value(record.clone()).unwrap();
            assert!(ActiveGeneration::from_record(wire).is_err(), "{field}");
            *record.get_mut(field).unwrap() = original;
        }
        *record.get_mut("generationId").unwrap() = json!("generation-1");
        *record.get_mut("receipt").unwrap() = json!("bad");
        assert!(ActiveGeneration::from_record(from_value(record.clone()).unwrap()).is_err());
        *record.get_mut("generationId").unwrap() = Value::Null;
        *record.get_mut("receipt").unwrap() = Value::Null;
        *record.get_mut("hitReceipt").unwrap() = json!("bad");
        assert!(ActiveGeneration::from_record(from_value(record.clone()).unwrap()).is_err());
        *record.get_mut("hitReceipt").unwrap() = Value::Null;
        record
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), Value::Bool(true));
        assert!(from_value::<ActiveGenerationRecord>(record).is_err());
    }

    /// Simulates the filesystem while injecting one or more rename failures.
    struct FakeSwap {
        paths: RefCell<BTreeMap<PathBuf, &'static str>>,
        calls: Cell<usize>,
        failures: BTreeSet<usize>,
        fail_remove: Cell<bool>,
    }

    impl FakeSwap {
        fn new(failures: impl IntoIterator<Item = usize>) -> Self {
            let mut paths = BTreeMap::new();
            for name in OUTPUTS {
                paths.insert(Path::new("session").join(name), "old");
                paths.insert(Path::new("stage").join(name), "new");
            }
            Self {
                paths: RefCell::new(paths),
                calls: Cell::new(0),
                failures: failures.into_iter().collect(),
                fail_remove: Cell::new(false),
            }
        }
    }

    impl Swap for FakeSwap {
        fn exists(&self, path: &Path) -> Result<bool, AppError> {
            Ok(self.paths.borrow().contains_key(path))
        }

        fn rename(&self, from: &Path, to: &Path) -> Result<(), AppError> {
            let call = self.calls.get().checked_add(1).unwrap();
            self.calls.set(call);
            if self.failures.contains(&call) {
                return Err(fail("io", "Injected rename failure"));
            }
            let mut paths = self.paths.borrow_mut();
            if paths.contains_key(to) {
                return Err(fail("io", "Unexpected simulated destination"));
            }
            let Some(contents) = paths.remove(from) else {
                return Err(fail("io", "Unexpected simulated rename"));
            };
            paths.insert(to.to_owned(), contents);
            Ok(())
        }

        fn remove(&self, path: &Path) -> Result<(), AppError> {
            if self.fail_remove.replace(false) {
                return Err(fail("io", "Injected removal failure"));
            }
            if self.paths.borrow_mut().remove(path).is_some() {
                Ok(())
            } else {
                Err(fail("io", "Unexpected simulated removal"))
            }
        }
    }

    /// Verifies the complete old set survives each swap or receipt failure.
    fn assert_rollback(io: &FakeSwap, receipt_error: bool) {
        let mut rollback_failed = false;
        let result = swap_with(
            io,
            Path::new("session"),
            Path::new("stage"),
            Path::new("backup"),
            &mut rollback_failed,
            || {
                if receipt_error {
                    Err(fail("io", "Injected receipt failure"))
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert!(!rollback_failed);
        for name in OUTPUTS {
            assert_eq!(
                io.paths.borrow().get(&Path::new("session").join(name)),
                Some(&"old")
            );
            assert!(
                !io.paths
                    .borrow()
                    .contains_key(&Path::new("backup").join(name))
            );
        }
    }

    #[test]
    fn every_partial_swap_and_receipt_failure_restores_old_outputs() {
        // Each existing output requires one backup rename and one installation rename.
        for failed_call in 1..=OUTPUTS.len() * 2 {
            assert_rollback(&FakeSwap::new([failed_call]), false);
        }
        assert_rollback(&FakeSwap::new([]), true);
    }

    #[test]
    fn rollback_failure_retains_the_backup() {
        // Fail installation after the first backup, then fail its restoration.
        let io = FakeSwap::new([2, 3]);
        let mut rollback_failed = false;
        let error = swap_with(
            &io,
            Path::new("session"),
            Path::new("stage"),
            Path::new("backup"),
            &mut rollback_failed,
            || Ok(()),
        )
        .unwrap_err();
        assert!(rollback_failed);
        assert!(error.to_string().contains("backups remain"));
        assert!(
            io.paths
                .borrow()
                .contains_key(&Path::new("backup").join(OUTPUTS[0]))
        );
    }

    #[test]
    fn rollback_removal_failure_retains_the_backup() {
        let io = FakeSwap::new([3]);
        io.fail_remove.set(true);
        let mut rollback_failed = false;
        assert!(
            swap_with(
                &io,
                Path::new("session"),
                Path::new("stage"),
                Path::new("backup"),
                &mut rollback_failed,
                || Ok(()),
            )
            .is_err()
        );
        assert!(rollback_failed);
        assert_eq!(
            io.paths.borrow().get(&Path::new("backup").join(OUTPUTS[0])),
            Some(&"old")
        );
    }
}
