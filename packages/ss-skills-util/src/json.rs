//! Enforces record and service shapes before workflow logic consumes evidence.
//!
//! These readers distinguish absent evidence from legitimate empty collections
//! and explicit nulls; adapters must not invent success-shaped defaults.

use ohno::AppError;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::error::fail;

/// Requires nullable record fields to be present rather than silently defaulting.
///
/// Use as a Serde field deserializer where schema 2 requires a named field even
/// when its value is null.
pub(crate) fn required_optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Rejects incomplete service evidence at the adapter boundary.
pub(crate) fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, AppError> {
    value
        .get(name)
        .filter(|value| !value.is_null())
        .ok_or_else(|| {
            fail(
                "service",
                format!("Missing required service field {name:?}"),
            )
        })
}

/// Distinguishes an explicitly unknown service value from a truncated response.
pub(crate) fn nullable_field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, AppError> {
    value.get(name).ok_or_else(|| {
        fail(
            "service",
            format!("Missing required nullable service field {name:?}"),
        )
    })
}

/// Reads a required string without converting arbitrary JSON values to text.
pub(crate) fn text(value: &Value) -> Result<&str, AppError> {
    value
        .as_str()
        .ok_or_else(|| fail("service", "Expected a string in service evidence"))
}

/// Reads a required collection without interpreting missing data as no work.
pub(crate) fn array(value: &Value) -> Result<&[Value], AppError> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| fail("service", "Expected an array in service evidence"))
}

/// Reads an actual Boolean rather than accepting truthy malformed values.
pub(crate) fn boolean(value: &Value) -> Result<bool, AppError> {
    value
        .as_bool()
        .ok_or_else(|| fail("service", "Expected a Boolean in service evidence"))
}

/// Normalizes service identifiers for ownership checks and persistence.
///
/// Providers supply strings or positive integer JSON values. A common textual
/// representation lets comments, threads, and accounts retain their identifiers
/// without depending on one provider's wire type.
pub(crate) fn identifier(value: &Value) -> Result<String, AppError> {
    if let Some(text) = value.as_str().filter(|text| !text.trim().is_empty()) {
        return Ok(text.to_owned());
    }
    value
        .as_u64()
        .filter(|value| *value > 0)
        .map(|value| value.to_string())
        .ok_or_else(|| fail("service", "Missing or invalid service identifier"))
}
