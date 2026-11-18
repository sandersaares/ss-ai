//! Compile-time unwind contracts for shared runtime boundaries.

use std::panic::{RefUnwindSafe, UnwindSafe};

use ss_ai_runtime::{Failure, FileTransaction, LockMode, ResourceLock};
use static_assertions::{assert_impl_all, assert_not_impl_any};

assert_impl_all!(LockMode: UnwindSafe, RefUnwindSafe);
assert_impl_all!(ResourceLock: UnwindSafe, RefUnwindSafe);

// Error causes and injected clocks need not support unwind safety.
assert_not_impl_any!(Failure: UnwindSafe, RefUnwindSafe);
assert_not_impl_any!(FileTransaction<'static>: UnwindSafe, RefUnwindSafe);
