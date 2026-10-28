//! Separates caller recovery categories from contextual diagnostic prose.

use ohno::AppError;

/// Attaches the caller-relevant classification without parsing diagnostic prose.
#[ohno::error]
#[display("{kind}: {message}")]
pub struct Failure {
    /// Machine-readable category used by the CLI and its calling skill.
    kind: &'static str,
    /// Human explanation that can evolve independently of the recovery category.
    message: String,
}

/// Returns the CLI recovery classification carried by an operation failure.
///
/// The binary prefixes diagnostics with this category so calling skills can
/// distinguish authentication, invalid state, and uncertain writes without
/// parsing prose. Unclassified errors use the general `service` category.
#[must_use]
pub fn classification(error: &AppError) -> &'static str {
    error
        .find_source::<Failure>()
        .map_or("service", |failure| failure.kind)
}

/// Creates a classified failure for an invalid input or external condition.
pub fn fail(kind: &'static str, message: impl Into<String>) -> AppError {
    Failure::new(kind, message.into()).into()
}

/// Adds classified context to a lower-level failure.
pub fn caused_by(
    kind: &'static str,
    message: impl Into<String>,
    error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> Failure {
    Failure::caused_by(kind, message.into(), error)
}
