//! Internal capture and authorship rejections retain their recovery category.

/// A capture production or classification invariant was rejected.
#[ohno::error]
pub(crate) struct ContextError;

// Keep the runtime classification in the chain without exposing error kinds.
macro_rules! classified_error {
    ($name:ident, $kind:literal, $message:literal) => {
        #[ohno::error]
        pub(crate) struct $name;

        impl $name {
            pub(crate) fn classified() -> ContextError {
                Self::caused_by(ss_ai_runtime::fail($kind, $message).into_std_error()).into()
            }
        }

        impl From<$name> for ContextError {
            fn from(error: $name) -> Self {
                Self::caused_by(error)
            }
        }
    };
    ($name:ident, $kind:literal) => {
        #[ohno::error]
        pub(crate) struct $name;

        impl $name {
            pub(crate) fn classified(message: impl Into<String>) -> ContextError {
                Self::caused_by(ss_ai_runtime::fail($kind, message).into_std_error()).into()
            }
        }

        impl From<$name> for ContextError {
            fn from(error: $name) -> Self {
                Self::caused_by(error)
            }
        }
    };
}

classified_error!(MissingCapture, "invalidInput");
classified_error!(UnsupportedCaptureVersion, "invalidInput");
classified_error!(
    StaleClassificationSnapshot,
    "invalidInput",
    "The classification snapshot changed"
);
classified_error!(
    InvalidClassificationBatch,
    "invalidInput",
    "Classifications must be a JSON array"
);
classified_error!(
    DuplicateClassification,
    "invalidInput",
    "Duplicate classification"
);
classified_error!(
    TooManyClassifications,
    "invalidInput",
    "Too many classifications"
);
classified_error!(ConflictingClassification, "invalidInput");
classified_error!(
    MissingCaptureThreads,
    "invalidInput",
    "Missing capture threads"
);
classified_error!(
    MissingCaptureComments,
    "invalidInput",
    "Missing capture comments"
);
classified_error!(
    InvalidCaptureComment,
    "invalidInput",
    "Capture comment is not an object"
);
classified_error!(
    InvalidClassificationShape,
    "invalidInput",
    "Classification requires only threadId, commentId and kind"
);
classified_error!(MissingClassificationField, "invalidInput");
classified_error!(InvalidClassificationKind, "invalidInput");
classified_error!(UnknownThread, "invalidInput");
classified_error!(UnknownComment, "invalidInput");
classified_error!(TooManyComments, "invalidInput", "Too many comments");
classified_error!(
    AliasedCaptureOutput,
    "invalidInput",
    "Give the capture a distinct -OutputPrefix"
);
classified_error!(
    MissingComparisonCapture,
    "invalidInput",
    "Comparison capture is missing"
);
classified_error!(
    MissingComparisonSidecarTarget,
    "invalidInput",
    "Comparison authorship sidecar points to a missing file"
);
classified_error!(
    ComparisonTargetMismatch,
    "invalidInput",
    "Cannot compare captures of different pull requests"
);
classified_error!(
    ExistingCaptureOutput,
    "invalidInput",
    "Capture output already exists; choose a new prefix"
);
classified_error!(
    ProviderTargetMismatch,
    "invalidInput",
    "Provider returned a different pull request"
);
classified_error!(
    InvalidAuthorshipRecord,
    "invalidInput",
    "Invalid authorship record"
);
classified_error!(
    MissingPathParent,
    "invalidInput",
    "File path requires a parent"
);
classified_error!(MissingPathName, "invalidInput", "File path requires a name");
classified_error!(
    MissingComparisonLock,
    "io",
    "Comparison sidecar lock disappeared"
);
classified_error!(
    InvalidOutputLockName,
    "io",
    "Cannot compare output lock name"
);
classified_error!(StagingNamesExhausted, "io");
classified_error!(InvalidProviderArray, "invalidInput");
classified_error!(
    MissingProviderIdentity,
    "invalidInput",
    "Provider context requires an authenticated identity"
);
classified_error!(
    MissingProviderUrl,
    "invalidInput",
    "Provider target has no URL"
);
classified_error!(
    MissingProviderPlatform,
    "invalidInput",
    "Provider target has no platform"
);
classified_error!(MissingProviderTargetField, "invalidInput");
classified_error!(
    UnsupportedProviderPlatform,
    "invalidInput",
    "Unsupported provider platform"
);
classified_error!(MissingPullRequest, "invalidInput", "Missing pull request");
classified_error!(
    InvalidProviderComments,
    "invalidInput",
    "Provider thread requires a comments array"
);
classified_error!(InvalidProviderComment, "invalidInput", "Invalid comment");
classified_error!(InvalidProviderThread, "invalidInput", "Invalid thread");
