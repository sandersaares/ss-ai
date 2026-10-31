//! Compile-time unwind contracts for source and judgment records.

use std::panic::{RefUnwindSafe, UnwindSafe};

use ss_ai_assessment::{
    Assessment, AssessmentError, Change, Content, Disposition, Inventory, Judgment, Note, Side,
};
use static_assertions::{assert_impl_all, assert_not_impl_any};

assert_impl_all!(Assessment: UnwindSafe, RefUnwindSafe);
assert_impl_all!(Change: UnwindSafe, RefUnwindSafe);
assert_impl_all!(Content: UnwindSafe, RefUnwindSafe);
assert_impl_all!(Disposition: UnwindSafe, RefUnwindSafe);
assert_impl_all!(Inventory: UnwindSafe, RefUnwindSafe);
assert_impl_all!(Judgment: UnwindSafe, RefUnwindSafe);
assert_impl_all!(Note: UnwindSafe, RefUnwindSafe);
assert_impl_all!(Side: UnwindSafe, RefUnwindSafe);
// The aggregate retains arbitrary source errors that need not be unwind-safe.
assert_not_impl_any!(AssessmentError: UnwindSafe, RefUnwindSafe);
