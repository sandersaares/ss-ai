# No real time in unit tests

Applies to: code containing unit tests.

Using a real-time clock for measurements or timeouts in unit test code is forbidden due to the flakiness and slowness it causes.

Prefer a fake clock that can be controlled by test logic. This is generally only possible in unit tests that use mock dependencies (or dependencies that also support a fake clock).

Reliance on real time in a unit test usually indicates a missing mockable abstraction. Fix the abstraction, not just the test.

In tests that cannot use a fake clock, all timeouts must be so large that they will never be encountered. Critically, timeouts must also not be encountered in case of test failures! Waiting for a timeout to fail a test is invalid test design.

## Fuzzing exception

It is typical to run fuzzing for a specific real-time duration because fuzzing can be a long-running process with no predetermined end. We simply fuzz for 60 seconds (for example) each time to incrementally add fuzz coverage each attempt.

In such incremental fuzzing, it is important to ensure that each fuzzing attempt is unique. Ensure that such fuzz test patterns use unique seeds or iteratively persist their state and continue from the last attempt.

## Last chance watchdog exception

It is acceptable to have "last chance" watchdog timeouts that are several orders of magnitude higher than expected test duration (10 seconds for a 10ms test, or 5 minutes for a 5 second test), just to avoid hung build pipelines.
Even then, such watchdog timers must be disabled under mutation testing because it is not acceptable to "catch" mutations by the test failing on a watchdog timeout - under mutation testing, a hung test must truly hang forever.

## Rust doctest exception

Doctests are more examples than tests, so it is fine to bias toward realism and show off real-time clock usage in doctests as long as
that usage is limited to reading the clock (delays/sleeps are still forbidden).

# Code must not hang even under mutation testing

Applies to: code containing tests.

No mutation caused by mutation testing is allowed to produce infinite loops or otherwise hung tests. Such mutations must either be caught by the test suite immediately and deterministically, or the mutation must be skipped.

One option to catch mutations that may cause infinite loops is to apply progress checks - add debug assertions into loop iterations that verify some nonzero progress was made during the iteration. There may be other approaches, as well.

Watchdog timers that turn hangs into test failures are not an acceptable solution - this is invalid because it masks the underlying problem rather than addressing it, and because it causes very slow test suite execution.

# Mutation tests should not depend on test ordering

Applies to: code containing tests, or scripts/workflows/configuration related to mutation testing.

It is invalid to manually arrange tests in a specific order to make mutations caught. Mutations must be caught independently of the order in which tests are executed.

Note that this means the "mutations must not cause tests to hang" requirement applies to **every test**. It is not enough for one test to catch a mutation - EVERY test must either pass or fail immediately and deterministically under EVERY mutation in scope.

# Production code must not change in test builds

Applies to: code executed in production scenarios.

It is acceptable to swap in test helpers (mocks, fakes) via specific abstractions intended for the purpose.

If unavoidable, it is acceptable to add conditionally compiled test hooks (and associated fields) into production code to allow tests to receive signals part way through function execution (e.g. to set a "execution reached point X" flag). Ideally, this would be accomplished by mocking instead but we acknowledge that is not always practical.

It is NOT acceptable for any conditionally compiled test-only code to actually change what the unconditionally compiled code does. The logic executed by production code must match in both test and production builds, for the same inputs.

# Test code may be biased toward brevity and clarity

Applies to: code containing tests.

Test code exists to set up scenarios and verify conditions. It operates under a narrow set of circumstances and does not need to
be as robust as production code - for example, a test working with 5 items does not need to worry about what happens if 5 million
items are present.

It is fine to use `.unwrap()` in tests. It is fine to use raw `foo + 1` style arithmetic in tests, even if ordinarily the Clippy
strict arithmetic rule is enabled and requires you to explicitly call `.wrapping_add(1)` or `.checked_add(1).expect("overflow")`
in production code.

# Fuzz complex state machines and parsers

Applies to: code containing complex state machines and parsers.

A state machine controls which operations are valid as an input moves through
distinct states. State machines and parsers whose behavior combines segmented
input, operation ordering, limits and transitions should have a bounded fuzz
target. Bound generated input and operation counts, enforce progress, and
prefer one parameterized target when multiple implementations share the same
contract.
