# Development and installation

## Validation strategy

Validation exercises the [implementation boundaries](../implementation.md)
without changing the user's installation or contacting live review services.
The root workspace centralizes dependencies and inherits the full pinned Folo
Rust/rustdoc/Clippy configuration and its companion `clippy.toml`. Toolchain
upgrades copy both again without local customization. Optional-tool
`check-cfg` allowances do not enable additional validation tools.

The canonical source is [Folo at revision
5b7761ff275b865bdb9f8ac5a3b610cc50f88b6d](https://github.com/folo-rs/folo/tree/5b7761ff275b865bdb9f8ac5a3b610cc50f88b6d).
When upgrading the Rust toolchain, copy Folo's complete workspace Rust, rustdoc,
and Clippy lint tables and companion `clippy.toml` together, then update the
source URLs in the root `Cargo.toml` and `clippy.toml`. Those comments identify
the exact source files; keep local development and CI on the same configuration.

Unit tests use fake time and in-process provider evidence. Integration tests
exercise real files, retained locks, concurrent replacement, CLI processes,
Windows command shims, large UTF-8 streams, and installed-tool discovery.
Synchronization uses explicit signals and progress checks, not real-time
sleeps. Tests preserve valid schema-2 journals and ambiguous-write states.

## Shared test preparation

`scripts/Invoke-Tests.ps1` owns the `Prepare` and `Execute` phases; `just`
recipes only forward arguments. The dot-sourced
`scripts/lib/RustTestPreparation.ps1` supplies the reusable boundary.
`New-RustTestPreparation` takes an absolute repository root, an absolute
previously unused prerequisite record path, and optional integration target
names. `Read-RustTestPreparation` validates the same selection and identity.
`Invoke-PreparedRustTests` runs Cargo against the owned target directory.
The schema-1 successful record contains `status: complete`, null
`providerFailure`, `identity`, `testTargets`,
`ownedDirectory`, `targetDirectory`, `provider`, and `executables`.
Identity binds the checkout root, Rust source/manifests/lockfile/configuration
digest, verbose Rust compiler identity, Cargo version, all-feature test profile,
and compiler/build/target/profile environment. The source digest includes embedded
GraphQL queries, JSON fixtures, and the shared reapproval significance policy.
Provider source and executable
records, and Cargo-discovered executable records, contain exact absolute paths
and SHA-256 digests. The producer rechecks source identity and validates the
consumer boundary before atomically publishing. Failed Cargo preparation publishes
no record and removes only its own outputs. Provider-independent selections
omit the provider build. A provider-only failure retains compiled test artifacts
in an explicit `status: providerFailed` record with `providerFailure` text and
no provider artifact. Only the canonical orchestrator accepts that state: it
reports the failure, blocks native tests, runs independent tests and doctests,
and fails the overall invocation. Ordinary prerequisite readers reject it.
General test compilation failure remains an invocation-wide blocker because
its executable set is incomplete.
Publication opens a retained `<record>.lock` sibling exclusively and holds it
across the final nonexistence check and atomic move. Competing writers fail
explicitly; readers need no publication lock. The producer never deletes a
retained lock file or replaces an existing record.

Every preparation creates a unique owned directory beside the record. Cargo
uses its private `target` child, so another preparation cannot replace final
executables held by active tests. Explicit records retain these outputs until
their owner removes them after consumers exit; the combined runner removes
implicitly allocated outputs. Independent runs never clean another run's
directory. Native fixtures copy the provider into private launcher directories
and retain `CARGO_BIN_EXE_ss-skills-util` for the ordinary executable.
Each fixture validates provider source identity and artifact digests. Executable
hashing streams through a fixed-size stack buffer rather than retaining the
complete executable in memory.

These choices adopt [Folo's helper preparation and Cargo artifact discovery](https://github.com/folo-rs/folo/blob/eba896a8e20465b4efe0e041c34aabb92a466aaf/justfiles/just_testing.just#L153-L227),
[owned final artifacts](https://github.com/folo-rs/folo/blob/eba896a8e20465b4efe0e041c34aabb92a466aaf/packages/cargo-bench-history-faker/docs/implementation.md#L8-L47),
and [strict prerequisite lookup](https://github.com/folo-rs/folo/blob/eba896a8e20465b4efe0e041c34aabb92a466aaf/packages/dure-test-helper/src/locate.rs#L35-L58)
without adding a helper package, fallback compilation, or nextest dependency.
Phase elapsed times include preparation; Cargo artifact freshness appears in
the preparation log. Warm execution can reuse one retained preparation, but a
new invocation intentionally pays for a private build graph.

## Installation verification

Local installation appends `-C target-cpu=native` to the installer child's
`CARGO_ENCODED_RUSTFLAGS`, preserving the caller's existing encoded flags or
plain `RUSTFLAGS`. It leaves the caller's environment unchanged. These binaries
target the installing machine's CPU and are not portable across CPUs. Ordinary
build and CI commands do not receive this installation-only flag.

`tests/NativeInstallation.Tests.ps1` exercises our installer boundary without
running `cargo install`. Command construction tests assert locked local-source
arguments, caller flag precedence, native CPU optimization and child-only
environment changes. Simulated Cargo output covers process failures, JSON
parsing, exact executable selection and incomplete evidence. Existing PowerShell
executables drive test-owned launchers and version responses; isolated child
PATH values cover correct, stale, missing and shadowed executable discovery.
The tests preserve caller location and verify that failed installation blocks
instruction and skill activation. They neither compile fixtures nor change
the user's installation.

Cargo owns compilation, replacement, registration and source rebuilding.
Tests exercise our handling of Cargo's reported artifacts rather than repeat
those Cargo responsibilities with a real release installation.

Setup reads Cargo's compiler-artifact JSON and compares the build's SHA-256
with the executable resolved on `PATH`. This detects stale, shadowing copies
even at the same package version without reproducing Cargo's installation-root
precedence or depending on its human-readable progress output.

## Platform coverage

Windows and Ubuntu CI run Clippy and Cargo tests independently after setup,
alongside relevant Pester coverage. `scripts\Invoke-Tests.ps1` remains the
repository entry point. Native C/C++ compiler/linker prerequisites belong in
setup documentation; Cargo builds mimalloc rather than installing an allocator
separately.
