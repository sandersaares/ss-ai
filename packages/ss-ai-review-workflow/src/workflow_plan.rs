//! Typed authoring and validation for finding workflow plans.

use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use ohno::AppError;
use serde::de::{Error as _, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use ss_ai_runtime::{Clock, FileTransaction, caused_by, fail, read_bytes};

/// Selects the schema and list in a workflow plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[expect(
    clippy::exhaustive_enums,
    reason = "The variants are the supported plan schema names"
)]
pub enum PlanKind {
    /// Candidate groups for adjudication.
    MergeCandidates,
    /// Accepted groups to merge.
    MergePlan,
    /// Finding replacements.
    Updates,
    /// Finding removals.
    Removals,
    /// Audit decisions.
    Decisions,
}

/// Maps schema names to their single required array.
impl PlanKind {
    fn field(self) -> &'static str {
        match self {
            Self::MergeCandidates => "clusters",
            Self::MergePlan => "groups",
            Self::Updates => "updates",
            Self::Removals => "removals",
            Self::Decisions => "decisions",
        }
    }
}

/// A candidate group whose members may overlap other candidate groups.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "The schema requires these exact fields"
)]
pub struct Cluster {
    /// Identifiers considered together.
    pub finding_ids: Vec<String>,
}

/// A merge group and the authored replacement finding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "The schema requires these exact fields"
)]
pub struct Merge {
    /// Identifiers replaced by the merged finding.
    pub source_finding_ids: Vec<String>,
    /// Path to authored finding content.
    pub finding_path: String,
}

/// One replacement finding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "The schema requires these exact fields"
)]
pub struct Update {
    /// Identifier to replace.
    pub finding_id: String,
    /// Path to authored finding content.
    pub finding_path: String,
}

/// One finding to remove.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "The schema requires these exact fields"
)]
pub struct Removal {
    /// Identifier to remove.
    pub finding_id: String,
    /// Explanation retained in the artifact.
    pub reason: String,
}

/// One audit decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "The schema requires these exact fields"
)]
pub struct Decision {
    /// Identifier being decided.
    pub finding_id: String,
    /// Decision in the workflow vocabulary.
    pub decision: DecisionValue,
    /// Optional explanation.
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub reason: Option<String>,
    /// Optional user feedback.
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub feedback: Option<String>,
}

/// Audit decision vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[expect(
    clippy::exhaustive_enums,
    reason = "The schema defines this closed vocabulary"
)]
pub enum DecisionValue {
    /// Approved by the user.
    Approved,
    /// Rejected by the user.
    Rejected,
    /// Dropped from consideration.
    Dropped,
    /// Approved under the automatic policy.
    AutoApproved,
    /// Requires user audit.
    NeedsAudit,
}

/// A typed workflow item.
#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::exhaustive_enums,
    reason = "The variants are the supported plan item schemas"
)]
pub enum PlanItem {
    /// Candidate cluster.
    Cluster(Cluster),
    /// Merge group.
    Merge(Merge),
    /// Replacement.
    Update(Update),
    /// Removal.
    Removal(Removal),
    /// Audit decision.
    Decision(Decision),
}

/// Validated plan contents, including an explicitly empty list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    kind: PlanKind,
    items: Vec<PlanItem>,
}

impl Plan {
    /// Returns the schema selected by the caller.
    #[must_use]
    #[inline]
    pub fn kind(&self) -> PlanKind {
        self.kind
    }

    /// Returns the validated items in authoring order.
    #[must_use]
    #[inline]
    pub fn items(&self) -> &[PlanItem] {
        &self.items
    }
}

/// Refuses explicit null for optional strings, which the schema does not allow.
fn present_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

/// Preserves object entries until duplicate names have been rejected.
struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::from(v)))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::from(v)))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Number::from_f64(v)
                    .map(|value| StrictValue(Value::Number(value)))
                    .ok_or_else(|| E::custom("Non-finite number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v.to_owned())))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictValue(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some((key, StrictValue(value))) =
                    map.next_entry::<String, StrictValue>()?
                {
                    if values.keys().any(|existing| {
                        existing == &key
                            || (existing.is_ascii()
                                && key.is_ascii()
                                && existing.eq_ignore_ascii_case(&key))
                    }) {
                        return Err(A::Error::custom("Duplicate or ambiguous JSON property"));
                    }
                    values.insert(key, value);
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(StrictVisitor)
    }
}

/// Parses JSON without discarding conflicting object keys.
pub(crate) fn parse_json(bytes: &[u8]) -> Result<Value, AppError> {
    // PowerShell's BOM-detecting reader accepts UTF-8 BOMs before strict parsing.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let StrictValue(value) = serde_json::from_slice(bytes)
        .map_err(|error| caused_by("invalidInput", "Invalid JSON", error))?;
    Ok(value)
}

/// Enforces the same schema for every input and output boundary.
fn parse_item(kind: PlanKind, value: Value) -> Result<PlanItem, AppError> {
    let invalid = |error| caused_by("invalidInput", "Invalid workflow item", error);
    let item = match kind {
        PlanKind::MergeCandidates => {
            PlanItem::Cluster(serde_json::from_value(value).map_err(invalid)?)
        }
        PlanKind::MergePlan => PlanItem::Merge(serde_json::from_value(value).map_err(invalid)?),
        PlanKind::Updates => PlanItem::Update(serde_json::from_value(value).map_err(invalid)?),
        PlanKind::Removals => PlanItem::Removal(serde_json::from_value(value).map_err(invalid)?),
        PlanKind::Decisions => PlanItem::Decision(serde_json::from_value(value).map_err(invalid)?),
    };
    validate_item(&item)?;
    Ok(item)
}

/// Rejects values that match the JSON types but violate schema constraints.
fn validate_item(item: &PlanItem) -> Result<(), AppError> {
    let text = |value: &str| !value.trim().is_empty();
    let ids = |values: &[String]| {
        values.len() >= 2
            && values.iter().all(|id| text(id))
            && values.iter().collect::<HashSet<_>>().len() == values.len()
    };
    let valid = match item {
        PlanItem::Cluster(item) => ids(&item.finding_ids),
        PlanItem::Merge(item) => ids(&item.source_finding_ids) && text(&item.finding_path),
        PlanItem::Update(item) => text(&item.finding_id) && text(&item.finding_path),
        PlanItem::Removal(item) => text(&item.finding_id) && text(&item.reason),
        PlanItem::Decision(item) => text(&item.finding_id),
    };
    if valid {
        Ok(())
    } else {
        Err(fail("invalidInput", "Invalid workflow item values"))
    }
}

/// Serializes a typed item without adding a separate discriminator to the schema.
fn item_value(item: &PlanItem) -> Result<Value, AppError> {
    match item {
        PlanItem::Cluster(item) => serde_json::to_value(item),
        PlanItem::Merge(item) => serde_json::to_value(item),
        PlanItem::Update(item) => serde_json::to_value(item),
        PlanItem::Removal(item) => serde_json::to_value(item),
        PlanItem::Decision(item) => serde_json::to_value(item),
    }
    .map_err(|error| caused_by("invalidInput", "Cannot serialize workflow item", error).into())
}

/// Reads and validates a named workflow plan.
///
/// # Errors
///
/// Returns an error for missing, malformed, or incompatible input.
pub fn read(kind: PlanKind, path: &Path, clock: &dyn Clock) -> Result<Plan, AppError> {
    let bytes =
        read_bytes(path, clock)?.ok_or_else(|| fail("invalidInput", "Workflow plan is missing"))?;
    parse_plan(kind, &bytes)
}

/// Parses the required versioned envelope before accepting its typed entries.
fn parse_plan(kind: PlanKind, bytes: &[u8]) -> Result<Plan, AppError> {
    let mut value = parse_json(bytes)?;
    let value = value
        .as_object_mut()
        .ok_or_else(|| fail("invalidInput", "Workflow plan must be an object"))?;
    if value.len() != 2
        || value.get("schemaVersion") != Some(&Value::from(1))
        || !value.get(kind.field()).is_some_and(Value::is_array)
    {
        return Err(fail("invalidInput", "Invalid workflow plan envelope"));
    }
    let Some(Value::Array(items)) = value.remove(kind.field()) else {
        return Err(fail("invalidInput", "Missing workflow list"));
    };
    let items = items
        .into_iter()
        .map(|value| parse_item(kind, value))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Plan { kind, items })
}

/// Creates an explicit empty plan without replacing existing work.
///
/// # Errors
///
/// Returns an error if the destination exists or cannot be published.
pub fn initialize(kind: PlanKind, path: &Path, clock: &dyn Clock) -> Result<(), AppError> {
    let transaction = FileTransaction::open(path, clock)?;
    if transaction.read_bytes()?.is_some() {
        return Err(fail("invalidInput", "Workflow plan already exists"));
    }
    write_plan(
        &transaction,
        &Plan {
            kind,
            items: Vec::new(),
        },
    )
}

/// Adds or explicitly replaces one validated item under the retained file lock.
///
/// `draft_path` is the source of the item, used to resolve relative finding paths.
/// An exact append retry is idempotent.
///
/// # Errors
///
/// Returns an error for invalid items, conflicting IDs, or file failures.
pub fn append(
    kind: PlanKind,
    path: &Path,
    draft_path: &Path,
    replace_index: Option<usize>,
    clock: &dyn Clock,
) -> Result<(), AppError> {
    let bytes = read_bytes(draft_path, clock)?
        .ok_or_else(|| fail("invalidInput", "Workflow item is missing"))?;
    let value = parse_json(&bytes)?;
    let mut item = parse_item(kind, value)?;
    let base = draft_path
        .parent()
        .ok_or_else(|| fail("invalidInput", "Draft path requires a parent"))?;
    match &mut item {
        PlanItem::Merge(merge) => merge.finding_path = absolute_path(base, &merge.finding_path)?,
        PlanItem::Update(update) => {
            update.finding_path = absolute_path(base, &update.finding_path)?;
        }
        _ => {}
    }
    let transaction = FileTransaction::open(path, clock)?;
    let bytes = transaction
        .read_bytes()?
        .ok_or_else(|| fail("invalidInput", "Workflow plan is missing"))?;
    let mut plan = parse_plan(kind, &bytes)?;
    if replace_index.is_some_and(|index| index >= plan.items.len()) {
        return Err(fail(
            "invalidInput",
            "Replacement index is outside the plan",
        ));
    }
    for (index, existing) in plan.items.iter().enumerate() {
        if Some(index) == replace_index {
            continue;
        }
        if replace_index.is_none() && existing == &item {
            return Ok(());
        }
        let conflict = match (existing, &item) {
            (PlanItem::Merge(a), PlanItem::Merge(b)) => a
                .source_finding_ids
                .iter()
                .any(|id| b.source_finding_ids.contains(id)),
            (PlanItem::Update(a), PlanItem::Update(b)) => a.finding_id == b.finding_id,
            (PlanItem::Removal(a), PlanItem::Removal(b)) => a.finding_id == b.finding_id,
            (PlanItem::Decision(a), PlanItem::Decision(b)) => a.finding_id == b.finding_id,
            _ => false,
        };
        if conflict {
            return Err(fail(
                "invalidInput",
                "Workflow item conflicts with an existing item",
            ));
        }
    }
    if let Some(index) = replace_index {
        let slot = plan
            .items
            .get_mut(index)
            .ok_or_else(|| fail("invalidInput", "Replacement index is outside the plan"))?;
        *slot = item;
    } else {
        plan.items.push(item);
    }
    write_plan(&transaction, &plan)
}

/// Makes references independent of subsequent plan relocation.
fn absolute_path(base: &Path, path: &str) -> Result<String, AppError> {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let absolute = std::path::absolute(absolute)
        .map_err(|error| caused_by("io", "Cannot resolve finding path", error))?;
    let mut clean = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other.as_os_str()),
        }
    }
    clean
        .into_os_string()
        .into_string()
        .map_err(|_path| fail("invalidInput", "Finding path is not Unicode"))
}

/// Publishes only after every item has been checked.
fn write_plan(transaction: &FileTransaction<'_>, plan: &Plan) -> Result<(), AppError> {
    let items = plan
        .items
        .iter()
        .map(item_value)
        .collect::<Result<Vec<_>, _>>()?;
    let mut object = Map::new();
    object.insert("schemaVersion".to_owned(), Value::from(1));
    object.insert(plan.kind.field().to_owned(), Value::Array(items));
    let bytes = serde_json::to_vec(&Value::Object(object))
        .map_err(|error| caused_by("invalidInput", "Cannot serialize workflow plan", error))?;
    transaction.write_bytes(&bytes)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{PlanItem, PlanKind, parse_plan};

    #[test]
    fn plan_parsing_moves_valid_items_and_rejects_invalid_envelopes() {
        let plan = parse_plan(
            PlanKind::Updates,
            br#"{"schemaVersion":1,"updates":[{"findingId":"a","findingPath":"a.json"},{"findingId":"b","findingPath":"b.json"}]}"#,
        )
        .unwrap();
        assert!(matches!(
            plan.items(),
            [PlanItem::Update(first), PlanItem::Update(second)]
                if first.finding_id == "a" && second.finding_id == "b"
        ));

        for invalid in [
            "[]",
            r#"{"schemaVersion":2,"updates":[]}"#,
            r#"{"schemaVersion":1,"updates":null}"#,
            r#"{"schemaVersion":1,"removals":[]}"#,
            r#"{"schemaVersion":1,"updates":[],"extra":true}"#,
            r#"{"schemaVersion":1,"updates":[],"updates":[]}"#,
            r#"{"schemaVersion":1,"updates":[],"Updates":[]}"#,
            r#"{"schemaVersion":1,"updates":[{"findingId":"a","findingId":"b","findingPath":"a.json"}]}"#,
            r#"{"schemaVersion":1,"updates":[{"findingId":"a","FindingId":"b","findingPath":"a.json"}]}"#,
            r#"{"schemaVersion":1,"updates":[{"findingId":"a","findingPath":"a.json","extra":true}]}"#,
        ] {
            assert!(
                parse_plan(PlanKind::Updates, invalid.as_bytes()).is_err(),
                "{invalid}"
            );
        }
    }
}
