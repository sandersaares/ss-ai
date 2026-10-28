//! Compile-time unwind contracts for immutable Git comparison records.

use std::panic::{RefUnwindSafe, UnwindSafe};

use ss_skills_git::{Comparison, FileDiff};
use static_assertions::assert_impl_all;

assert_impl_all!(Comparison: UnwindSafe, RefUnwindSafe);
assert_impl_all!(FileDiff: UnwindSafe, RefUnwindSafe);
