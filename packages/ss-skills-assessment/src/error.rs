//! Distinct assessment invariants with existing application recovery classifications.

/// A source inventory or judgment operation could not complete.
///
/// This aggregate is an internal workspace boundary, not a supported SDK.
#[ohno::error]
pub struct AssessmentError;

// The classification remains in the source chain so application callers retain
// their existing recovery categories without making error kinds a public API.
macro_rules! classified_error {
    ($name:ident, $kind:literal, $message:literal) => {
        #[ohno::error]
        pub(crate) struct $name;

        impl $name {
            pub(crate) fn classified() -> AssessmentError {
                Self::caused_by(ss_skills_runtime::fail($kind, $message).into_std_error()).into()
            }
        }

        impl From<$name> for AssessmentError {
            fn from(error: $name) -> Self {
                Self::caused_by(error)
            }
        }
    };
}

classified_error!(
    MissingCumulativeRationale,
    "invalidState",
    "Missing cumulative rationale"
);
classified_error!(
    InvalidAssessmentBinding,
    "invalidState",
    "Assessment binding, context selection, or completion is invalid"
);
classified_error!(
    ReadBeyondEvidence,
    "invalidState",
    "Evidence read exceeds captured content"
);
classified_error!(
    ChangedAssessmentDigest,
    "invalidState",
    "Completed assessment digest changed"
);
classified_error!(
    NoncontiguousRead,
    "invalidInput",
    "Evidence reads require contiguous snapshot-bound offsets"
);
classified_error!(
    ReadOffsetOverflow,
    "invalidInput",
    "Evidence offset overflow"
);
classified_error!(
    ImmutableAssessment,
    "invalidState",
    "Completed assessments are immutable"
);
classified_error!(
    StaleJudgment,
    "invalidState",
    "Judgment changed; refresh its previous digest before replacing it"
);
classified_error!(
    IncompleteEvidenceReads,
    "invalidState",
    "Insignificant assessment requires complete policy, discussion, and changed-source reads"
);
classified_error!(
    MissingJudgmentEvidence,
    "invalidState",
    "Judgment requires a rationale and evidence"
);
classified_error!(
    RepeatedJudgmentEvidence,
    "invalidState",
    "Repeated judgment evidence"
);
classified_error!(
    MissingCumulativeEvidence,
    "invalidState",
    "Cumulative judgment requires policy and discussion evidence"
);
classified_error!(UnknownChange, "invalidInput", "Unknown change identifier");
classified_error!(
    UncitedChange,
    "invalidState",
    "Change judgment must cite that change"
);
classified_error!(
    UnavailableSource,
    "invalidState",
    "Unavailable source content cannot support reapproval"
);
classified_error!(
    IncompleteCoverage,
    "invalidState",
    "Assessment lacks exact change and cumulative coverage"
);
classified_error!(
    InvalidReference,
    "invalidInput",
    "Invalid evidence reference"
);
classified_error!(
    UnknownEvidenceChange,
    "invalidInput",
    "Unknown evidence change"
);
classified_error!(UnknownEvidenceSide, "invalidInput", "Unknown evidence side");
classified_error!(
    UnreadableEvidence,
    "invalidInput",
    "This evidence side has no readable text; record a non-insignificant judgment"
);
classified_error!(
    IncompleteInventory,
    "invalidState",
    "Incomplete or contradictory source inventory"
);
classified_error!(
    InvalidChangeId,
    "invalidState",
    "Invalid or repeated change ID"
);
classified_error!(
    InvalidChangeShape,
    "invalidState",
    "Invalid changed-path shape"
);
classified_error!(
    ContradictoryChange,
    "invalidState",
    "Contradictory source change"
);
classified_error!(
    InvalidInventorySide,
    "invalidState",
    "Invalid inventory side"
);
classified_error!(InvalidCommit, "service", "Invalid GitHub commit identifier");

// JSON serialization errors keep the same unclassified application fallback.
impl From<serde_json::Error> for AssessmentError {
    fn from(error: serde_json::Error) -> Self {
        Self::caused_by(error)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use ohno::AppError;
    use ss_skills_runtime::classification;

    use crate::{Assessment, Change, Inventory, change_id};

    use super::{
        AssessmentError, IncompleteCoverage, IncompleteInventory, InvalidChangeId,
        InvalidChangeShape, InvalidCommit, InvalidReference, NoncontiguousRead,
    };

    /// Confirms both the owning invariant and the application's recovery category.
    fn assert_failure<T: Error + 'static>(error: AssessmentError, expected_kind: &str) {
        assert!(error.to_string().starts_with(&format!("{expected_kind}: ")));
        let error: AppError = error.into();
        assert!(error.find_source::<T>().is_some());
        assert_eq!(classification(&error), expected_kind);
    }

    /// Supplies a complete empty comparison so each test can isolate one invariant.
    fn inventory() -> Inventory {
        Inventory {
            old_commit: "a".repeat(40),
            new_commit: "b".repeat(40),
            old_tree: "c".repeat(40),
            new_tree: "c".repeat(40),
            changes: Vec::new(),
            complete: true,
        }
    }

    #[test]
    fn inventory_failures_keep_distinct_types_and_categories() {
        let mut inventory = inventory();
        inventory.complete = false;
        assert_failure::<IncompleteInventory>(inventory.validate().unwrap_err(), "invalidState");
        inventory.complete = true;
        inventory.old_commit = "bad".to_owned();
        assert_failure::<InvalidCommit>(inventory.validate().unwrap_err(), "service");
        inventory.old_commit = "a".repeat(40);
        inventory.new_tree = "d".repeat(40);
        inventory.changes.push(Change {
            id: "bad".to_owned(),
            kind: "M".to_owned(),
            old: None,
            new: None,
        });
        assert_failure::<InvalidChangeId>(inventory.validate().unwrap_err(), "invalidState");
        inventory.changes.first_mut().unwrap().id = change_id("M", None, None).unwrap();
        assert_failure::<InvalidChangeShape>(inventory.validate().unwrap_err(), "invalidState");
    }

    #[test]
    fn assessment_failures_keep_distinct_types_and_categories() {
        let mut assessment = Assessment::new(inventory(), "snapshot", "policy").unwrap();
        assert_failure::<IncompleteCoverage>(
            assessment.finish("policy", "discussion").unwrap_err(),
            "invalidState",
        );
        assert_failure::<InvalidReference>(
            assessment
                .read("invalid", 0, 1, "policy", "discussion")
                .unwrap_err(),
            "invalidInput",
        );
        assert_failure::<NoncontiguousRead>(
            assessment
                .read("policy", 1, 1, "policy", "discussion")
                .unwrap_err(),
            "invalidInput",
        );
    }
}
