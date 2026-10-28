//! Review journal validation built on the shared file transaction.

use crate::Clock;
use crate::selection::{Journal, State};
use ohno::AppError;
use ss_skills_runtime::FileTransaction;
pub(crate) use ss_skills_runtime::read_bytes;
use std::ops::Deref;
use std::path::Path;

/// Holds review state validation together with its file transaction.
pub(crate) struct FileJournal<'a>(FileTransaction<'a>);

impl<'a> FileJournal<'a> {
    /// Retains the shared lock for the entire review transaction.
    pub(crate) fn open(path: &Path, clock: &'a dyn Clock) -> Result<Self, AppError> {
        Ok(Self(FileTransaction::open(path, clock)?))
    }
}

impl<'a> Deref for FileJournal<'a> {
    type Target = FileTransaction<'a>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Journal for FileJournal<'_> {
    /// Reads and validates one complete version while retaining transaction ownership.
    fn load(&mut self) -> Result<Option<State>, AppError> {
        let Some(bytes) = self.read_bytes()? else {
            return Ok(None);
        };
        // PowerShell readers accept a UTF-8 BOM; valid existing journals retain that support.
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        let state: State = serde_json::from_slice(bytes).map_err(|error| {
            crate::error::caused_by("invalidState", "Malformed journal JSON".to_owned(), error)
        })?;
        state.validate().map_err(|error| {
            crate::error::caused_by(
                "invalidState",
                "Invalid journal".to_owned(),
                error.into_std_error(),
            )
        })?;
        Ok(Some(state))
    }

    /// Validates a transition before replacing the last complete published record.
    fn save(&mut self, state: &State) -> Result<(), AppError> {
        state.validate().map_err(|error| {
            crate::error::caused_by(
                "invalidState",
                "Cannot publish invalid journal".to_owned(),
                error.into_std_error(),
            )
        })?;
        self.write_bytes(&serde_json::to_vec_pretty(state)?)
    }
}
