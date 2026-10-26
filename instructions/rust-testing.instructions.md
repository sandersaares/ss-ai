---
applyTo: "**/*.rs"
---

# Miri tests must be fast

Miri slows down tests but tests should still not take unreasonable amounts of time under Miri.

If a test is longer than 10 seconds under Miri, fix the test so it runs faster. Most of the time, the problem is that too much data is
covered by the test - shorten arrays (`cfg` gated to Miri specific settings, if necessary) and reduce the workload in the test to make
it faster. As a last resort, if the test truly relies on a large data set, skip it under Miri.

# Unit tests must be in-process

Do not start new processes in unit tests - any logic that is more than "testing some logic in this process" belongs in Cargo integration test files.

Similarly, other resources outside the current process are inappropriate in unit tests - do not access the filesystem or the network.
Unit tests must use mock/simulated external dependencies and leave testing with real resources to integration tests.

# Prepare shared test prerequisites before execution

Prepare shared test executables before invoking the test runner and pass their
exact paths. Fixture constructors must not invoke Cargo or rustc. Share immutable
artifacts, not mutable test state. Keep real subprocess coverage where process
behavior is the contract; use injected effects for unrelated scenario matrices.
Compilation-bearing installation or compiler integration scenarios must be
explicit and reuse compatible prerequisites.

# Scope Miri incompatibilities to individual tests

When only some tests execute dependency behavior that Miri does not support,
keep the package in the normal Miri run and annotate only the affected test
functions with `#[cfg_attr(miri, ignore = "reason")]`. Do not replace
test-level ignores with a package exclusion or package-specific Miri command.

# Exclude test implementation from coverage

Where a repository defines a coverage-exclusion annotation, apply it to test
modules. Coverage gates measure production behavior; counting the test
implementation itself creates maintenance work without identifying untested
production code.

This may look like the following:

```rust
// In lib.rs or main.rs file.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// In any .rs file.
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {

}
```

Note that this requires the repository tooling to actually specify `coverage_nightly` during coverage builds and to use a nightly toolchain where the `coverage` attribute can be enabled.

# Tests should not depend on a specific test tool being used

Tests (whether regular or mutation tests) should yield the same result regardless of whether `cargo test` is used or `cargo nextest` is used. A dependency on a test tool for correct behavior is not acceptable.

It is acceptable to use tool-specific configuration for handling environmental concerns like retries or parallelism, as these can be assumed to be controllable (one way or another) regardless of the test tool.
However, reliance on things like order of test execution or whether the tests are process-isolated goes too far - tests must work in any order and regardless of whether process isolation is enabled.
Where process isolation is mandatory, it must be implemented by placing the tests in separately executed integration test binaries.

# Hand-rolled synchronization requires multithreaded tests and Miri

Any hand-rolled synchronization logic (e.g. use of atomic variables) requires there to be a thorough test suite that actually exercises
the logic from different threads. It also requires this test suite to run under Miri and to have (at least as part of a scheduled run)
many-seeds Miri testing to catch potential race conditions and undefined behavior under different thread interleavings.