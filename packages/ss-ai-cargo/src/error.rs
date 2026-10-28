use std::io;
use std::path::{Path, PathBuf};

/// Failure to produce a complete inventory.
#[ohno::error]
#[from(ScanError)]
pub struct InventoryError;

/// Groups the internal failures that can occur during one inventory.
#[ohno::error]
#[from(
    FilesystemFailure,
    DependencyPathFailure,
    ManifestParse,
    InvalidToml,
    InvalidTable,
    InvalidString,
    InvalidBoolean,
    InvalidArray,
    InvalidArrayElement,
    InvalidDependency,
    MissingSourceSpan,
    InvalidSourceSpan,
    InvalidSourceOffset,
    MissingMemberPattern,
    MissingMemberManifest,
    MemberManifestMissing,
    MemberManifestNotFile,
    InvalidPattern,
    TraversalLimit,
    NonUnicodePath,
    MissingParent,
    DifferentFilesystemRoot,
    RelativePathFailure,
    DuplicatePackage
)]
pub(crate) struct ScanError;

/// Identifies an unsuccessful filesystem operation and retains its I/O cause.
#[ohno::error]
#[display("Cannot {operation} {}", path.display())]
pub(crate) struct FilesystemFailure {
    pub(crate) operation: &'static str,
    pub(crate) path: PathBuf,
}

/// Reports an inaccessible dependency path in its declaring manifest.
#[ohno::error]
#[display("Cannot resolve dependency path {path} in {manifest}")]
pub(crate) struct DependencyPathFailure {
    pub(crate) path: String,
    pub(crate) manifest: String,
}

/// Adds a manifest path to a parser rejection without erasing its cause.
#[ohno::error]
#[display("Cannot parse manifest {}", path.display())]
pub(crate) struct ManifestParse {
    pub(crate) path: PathBuf,
}

/// Retains the TOML parser's diagnostic and source location.
#[ohno::error]
#[display("Invalid TOML")]
pub(crate) struct InvalidToml;

/// Reports an invalid table-valued manifest field.
#[ohno::error]
#[display("{name} must be a table")]
pub(crate) struct InvalidTable {
    pub(crate) name: String,
}

/// Reports an invalid string-valued manifest field.
#[ohno::error]
#[display("{name} must be a string")]
pub(crate) struct InvalidString {
    pub(crate) name: String,
}

/// Reports an invalid boolean-valued manifest field.
#[ohno::error]
#[display("{name} must be a boolean")]
pub(crate) struct InvalidBoolean {
    pub(crate) name: String,
}

/// Reports an invalid array-valued manifest field.
#[ohno::error]
#[display("{name} must be an array")]
pub(crate) struct InvalidArray {
    pub(crate) name: String,
}

/// Reports an invalid element of a string array.
#[ohno::error]
#[display("{name} must contain strings")]
pub(crate) struct InvalidArrayElement {
    pub(crate) name: String,
}

/// Reports a dependency that is neither a string nor a table.
#[ohno::error]
#[display("Invalid dependency declaration {name}")]
pub(crate) struct InvalidDependency {
    pub(crate) name: String,
}

/// Reports a parser item that does not contain its required source span.
#[ohno::error]
#[display("Missing source span for {name}")]
pub(crate) struct MissingSourceSpan {
    pub(crate) name: String,
}

/// Reports an invalid parser-provided source span.
#[ohno::error]
#[display("Invalid source span for {name}")]
pub(crate) struct InvalidSourceSpan {
    pub(crate) name: String,
}

/// Reports an invalid parser-provided source offset.
#[ohno::error]
#[display("Invalid source offset")]
pub(crate) struct InvalidSourceOffset;

/// Reports a member pattern that names no directory.
#[ohno::error]
#[display("Workspace member pattern matched nothing: {pattern}")]
pub(crate) struct MissingMemberPattern {
    pub(crate) pattern: String,
}

/// Reports a member pattern that names no usable manifest.
#[ohno::error]
#[display("No member manifest matched: {pattern}")]
pub(crate) struct MissingMemberManifest {
    pub(crate) pattern: String,
}

/// Reports an explicitly listed member whose manifest cannot be found.
#[ohno::error]
#[display("Cannot access member manifest {}", path.display())]
pub(crate) struct MemberManifestMissing {
    pub(crate) path: PathBuf,
}

/// Reports a literal member whose manifest path is not a file.
#[ohno::error]
#[display("Not a manifest file: {}", path.display())]
pub(crate) struct MemberManifestNotFile {
    pub(crate) path: PathBuf,
}

/// Reports an invalid workspace member or exclusion pattern.
#[ohno::error]
#[display("Invalid workspace pattern: {pattern}")]
pub(crate) struct InvalidPattern {
    pub(crate) pattern: String,
}

/// Reports a bounded traversal that cannot complete within its limit.
#[ohno::error]
#[display("Workspace traversal exceeds {scope} limit: {subject}")]
pub(crate) struct TraversalLimit {
    pub(crate) scope: &'static str,
    pub(crate) subject: String,
}

/// Reports a path that cannot be represented in the inventory.
#[ohno::error]
#[display("Path is not Unicode: {}", path.display())]
pub(crate) struct NonUnicodePath {
    pub(crate) path: PathBuf,
}

/// Reports a manifest path that has no parent.
#[ohno::error]
#[display("Manifest has no parent directory: {}", path.display())]
pub(crate) struct MissingParent {
    pub(crate) path: PathBuf,
}

/// Reports a manifest that cannot be expressed relative to the workspace root.
#[ohno::error]
#[display("Manifest {} is on a different filesystem root from {}", path.display(), root.display())]
pub(crate) struct DifferentFilesystemRoot {
    pub(crate) path: PathBuf,
    pub(crate) root: PathBuf,
}

/// Retains the path-strip error when constructing a relative manifest path.
#[ohno::error]
#[display("Cannot make manifest path relative")]
pub(crate) struct RelativePathFailure;

/// Reports duplicate names that prevent deterministic member classification.
#[ohno::error]
#[display("Duplicate workspace package name: {name}")]
pub(crate) struct DuplicatePackage {
    pub(crate) name: String,
}

/// Adds operation and path context while retaining the original I/O error.
pub(crate) fn filesystem_failure(
    operation: &'static str,
    path: &Path,
    error: io::Error,
) -> ScanError {
    FilesystemFailure::caused_by(operation, path.to_path_buf(), error).into()
}
