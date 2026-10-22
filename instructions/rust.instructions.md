---
applyTo: "**/*.rs"
---

# API documentation

When authoring an item's API documentation comment, target a first paragraph
that fits on a single 80-character line so it does not get word-wrapped in the
rendered docs. Condense the first paragraph to a concise summary, avoid
repetition with neighboring items, and provide any deeper explanation via
separate paragraphs.

Note: this applies to the rendered form, so it is fine to have markup that makes the first paragraph longer (e.g. for cross-links) as
long as the markup is invisible when the Markdown is rendered into HTML.

For reviews and automated checks, raise a finding only when the visible
rendered text exceeds 100 characters. This margin keeps minor deviations from
the authoring target out of reviews. Evaluate the rendered text rather than the
Markdown source length; word count is only a heuristic and must not be
presented as the requirement.

Example of good form:

```rust
/// Interpolates a [`Frobnicator`] with the [default configuration].
///
/// Use [`with_config()`][Self::with_config] to provide a custom configuration.
///
/// [default configuration]: crate::config::DefaultConfig
fn interpolate(f: Frobnicator) {
}
```

# mod.rs and lib.rs files are structural

These files exist to:

1. re-export types from lower level modules;
2. (if `pub`) to provide module-specific API documentation;
3. declare module-level metadata (attributes) inherited by all submodules.

Do not put type definitions or implementations into `mod.rs` files. All exposed types should come from submodules via re-exports.

# pub is pub

Only use `pub` for items that are truly visible in the public API surface of the crate. Do not rely on parents restricting visibility below `pub`.
For items needing cross-module visibility that are not part of the public API surface of the crate, use `pub(crate)` instead of `pub`.
You are not required to be more specific than `pub(crate)` (it is fine even if only part of a crate needs it).
Leave intentionally private items without a visibility modifier. Do not request widening visibility merely for uniformity.

For a package that is only published as a binary (not intended for consumption as a library), `pub` items in its lib target that exist
solely to wire the binary, its own tests, or its own maintainer tooling together are not public API surface for review purposes.

# Do not prefix builder functions with `with_`

The builder pattern does not have prefixes/suffixes, it is just `.foo()` and not `.with_foo()`. Remove unnecessary `with_` prefixes from builder methods.

# Safety requirements for trait implementations

If a trait has safety requirements that implementations must uphold, these requirements must be documented in a "Safety" chapter on the trait-level API documentation.
Function-level API documentation is only for documenting safety requirements imposed on callers of those functions, not on implementations.

A trait with safety requirements must also be marked `unsafe` (e.g. `unsafe trait Foo`). The corresponding unsafe implementations `unsafe impl Foo for Bar`
must document how the implementation safety requirements are satisfied.

Every `unsafe trait` must have a "Safety" chapter that documents the safety requirements.

# Test-only code must be gated as such

Do not suppress the `dead_code` lint for test-only code. You must use `cfg(test)` guards to conditionally compile test-only code instead.

# Do not use `#[path = "..."]` on modules

The logical module structure must follow the filesystem structure. Do not use `#[path = "..."]` to override this convention.

Sometimes people incorrectly use `#[path]` to reuse code. The correct approach is to extract the common code into a separate Cargo package.

# Code files must only be built once

Do not use a Cargo project structure that causes the same file to be built multiple times. An example of this anti-pattern is:

- tests/foo.rs containing `mod quu`
- tests/bar.rs containing `mod quu`
- tests/quu/mod.rs

The above pattern would make both `foo.rs` and `bar.rs` build `quu/mod.rs`.

Shared code belongs in a library crate, e.g. a non-published `mylibrary_dev` package for test and benchmark helpers. If the workspace
is using an "impl package" pattern (with `foo` being a minimized re-export of `foo_impl` that exposes both public and private API) then
the shared code might be exposed directly by `foo_impl` (potentially conditionally compiled via a `private-test-util` feature flag).

# `inline` attribute

Use the `#[inline]` attribute as follows:

- Do not add it to APIs that are not part of a library public API.
- Add it to all public API functions that are trivial forwarders.

Beyond the above, the `#[inline]` attribute should be used only when there is a clear performance benefit (proven by benchmarks).
When adding `#[inline]` to functions other than trivial forwarders, leave a comment explaining the nature of the measured
performance benefit. Ideally, proven by in-repo benchmarks that you reference. Also acceptable is to just state that you
measured it and how (e.g. via ad-hoc profiling), if a reproducible process is impractical to provide.

Do not use `#[inline(always)]` without documented justification that explains why `#[inline]` is not enough.

Do not use `#[inline(never)]` without documented justification that explains why inlining is forbidden.

# All features are enabled in test builds, all dependencies are present in dev-dependencies

We gate feature-dependent code behind `cfg(any(test, feature = "foo"))` so all features are enabled in test builds.
This avoids leaving accidental test execution gaps and means we never have to feature-gate tests (only platform-gate).

This depends on additive features, which we take as a hard requirement - mutually exclusive features are an invalid pattern.

This also requires that any feature-dependent dependencies be declared as non-optional dev-dependencies, to ensure they are present in test builds.
Regular dependencies may be optional (gated by a feature flag) but all optional dependencies must be present in dev-dependencies for this reason.
Dev-dependencies must declare all feature flags that regular dependencies do, to ensure that all required features are present in test builds.

# Do not allow locks to become poisoned

A lock becomes poisoned if code panics while the lock is held. The point of the poison is that a lock-guarded object is only expected to uphold
its logical invariants between lock acquisitions - transient states may violate any logical invariants it has.

An API is better designed if it does not allow locks to become poisoned. Inspect every line of code that calls into potentially-panicking code while holding a lock.
If possible, move such code out from the lock. If necessary, keep it in the lock but catch the panics, ensure that the locked state remains consistent
(which may require not doing anything at all - depends on what the state is), and re-raise the panic outside the locked scope. When acquiring a lock,
use `.expect()` with a message that explains why we never allow the lock to become poisoned and how we guarantee the state remains valid even on panic.

Exceptions are possible - it may be that some locked state genuinely cannot be kept consistent on panic, in which case it is fine to have the expect-message
state that an invalid state may be present; it should also try to explain why it may have happened (if this can be predicted from likely logic paths that panic).

# A panic is a panic

When evaluating the relevance or appropriateness of a panic, it does not matter what type of code is used to generate the panic.
A `panic!` is a panic, a `unwrap()` is a panic, a `expect()` is a panic, an `assert!` is a panic, and so on. The only relevant
question is whether the panic is appropriate in the context of the code that raised it. The exact form of the code is not important
and should be chosen for optimal readability and maintainability.

# std::alloc::System is a special name

We sometimes explicitly write out the string `std::alloc::System` instead of referencing it as `System` due to LLM limitations,
which can treat the short form of this name in special ways that cause it to get filtered out of the output. Do not try to
shorten this name.

# Hide async entrypoint in inline examples

In inline code examples that use `.await`, do not render the async
entrypoint/wrapper in generated API documentation.

Example:

```rust
use events::once::Event;
# use futures::executor::block_on;

# block_on(async {
let event = get_event();
let message = event.await;

assert_eq!(message, "Hello, World!");
# });
```

# Examples show good practices

Stand-alone examples and inline examples in API documentation are production code that is meant to show off good practices of using
the APIs we are demonstrating. Do not take shortcuts that we would otherwise not consider in production quality code, unless the
guidelines explicitly permit an exception to be made for examples.

# Avoid the `pin!` macro in examples

This macro is special-purpose and not intended for general use. It may be misleading for people not familiar with pinning.
Instead, use `Box::pin(value)` to pin a value in examples, as this is the most generally applicable form.

If there is some reason `Box::pin(value)` would not work, you can use
`std::pin::pin!(value)` as a last resort but leave a comment to justify why this
is the case.

Exception: if the example is showcasing a performance-critical allocation-free scenario, you may use `pin!`.

# Test-only code requires cfg(test)

If there are functions that are only used in tests, mark them (and their `use`
statements) with `#[cfg(test)]`. Do not just suppress "dead code" warnings.

# Avoid allocating memory

Memory allocation is the root of all evil. Allocation-free behavior and algorithms are always preferred.
Object pooling is preferred over allocation on hot paths. Zero-allocation stack-based logic is even better.
If you must allocate, reuse allocated objects whenever possible (either directly or via object pooling).

# Avoid parking_lot

The `parking_lot` crate has poor Miri compatibility, so logic that uses it cannot often be tested under Miri.
Unless a very strong justification exists, prefer standard library synchronization primitives over `parking_lot`.

# Test presence of traits via static assertions

Use `static_assertions::assert_impl_all!` and `static_assertions::assert_not_impl_any!` to check for the presence or absence of
traits where the API documentation makes claims about them, for example in terms of being thread-mobile (`Send + !Sync`),
thread-safe (`Send + Sync`) or single-threaded (neither).

If generic type parameters are involved, these assertions should use typical types that may be encountered in user code.
Ideally, you should avoid "inheriting" tested auto traits from the generic type parameters so the assertions are meaningful for
the type being tested.

# Rust has exclusive and shared references/borrows

Use the terms "exclusive reference" and "shared reference" (or "exclusive borrow" and "shared borrow") when describing Rust's reference types.
Do not use the term "mutable reference" or "mutable borrow" as this can be misleading - the primary property is exclusivity, not mutability.

# Enable API documentation feature detection in docsrs builds

The `lib.rs` of any library crate should contain the following line to automatically expose feature flag information in the generated documentation:

```rust
#![cfg_attr(docsrs, feature(doc_cfg))]
```

# Use many_cpus to detect processor count

Do NOT use `available_parallelism()` from the Rust standard library - this function is defective and fails to report the correct number
of processors on large Windows machines. Instead, use the processor count exposed by the `many_cpus` package.

# Atomic access ordering requires documentation

Whenever you use an atomic operation, document the memory ordering requirements and the reasoning behind the chosen ordering.
Treat it like a safety comment - you have to justify why the chosen memory ordering is correct and leads to the desired behavior.

# Safety comments are proofs, not claims

Safety comments must provide a logical argument proving that the safety requirements of the callee/trait are satisfied.
They cannot merely assert that the code fulfills the requirements without backing that up with at least references to where and how those requirements are met.

# Preferred packages

For timestamp handling, use `jiff`. For acquiring timestamps and maintaining clocks/delays/timers, use `tick`.