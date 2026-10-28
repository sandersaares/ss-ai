//! Compile-time unwind contracts for shared runtime boundaries.

use std::panic::{RefUnwindSafe, UnwindSafe};

use ss_skills_runtime::{Failure, FileTransaction};
use static_assertions::assert_not_impl_any;

// Error causes and injected clocks need not support unwind safety.
assert_not_impl_any!(Failure: UnwindSafe, RefUnwindSafe);
assert_not_impl_any!(FileTransaction<'static>: UnwindSafe, RefUnwindSafe);
