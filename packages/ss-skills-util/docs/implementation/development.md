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
When installation is selected, the boundary also invokes the explicit,
compiler-bearing installation preparation before ordinary runtime execution.

The schema-1 successful record contains `status: complete`, null
`providerFailure`/`installationFailure`, `identity`, `testTargets`,
`ownedDirectory`, `targetDirectory`, `provider`, `executables`, and `installation`.
Identity binds the checkout root, Rust source/manifests/lockfile/configuration
digest (including root `Install.ps1` and installer PowerShell libraries),
verbose Rust compiler identity, Cargo version, native CPU compilation
capabilities reported by `rustc --print cfg -C target-cpu=native`, all-feature test profile,
and compiler/build/target/profile environment. Provider source and executable
records, and Cargo-discovered executable records, contain exact absolute paths
and SHA-256 digests. The producer rechecks source identity and validates the
consumer boundary before atomically publishing. Failed Cargo preparation publishes
no record and removes only its own outputs. Provider-independent selections
omit the provider build. A provider-only failure retains compiled test artifacts
in an explicit `status: providerFailed` record with `providerFailure` text and
no provider artifact. Only the canonical orchestrator accepts that state: it
reports the failure, blocks native tests, runs independent tests and doctests,
and fails the overall invocation. Ordinary prerequisite readers reject it.
An installation-only failure uses `status: installationFailed`, nonempty
`installationFailure`, and no installation evidence. Simultaneous failures use
`providerAndInstallationFailed` and both failure messages. The canonical reader's
`-AllowPrerequisiteFailure` switch permits these explicit failed records only
for continuing already prepared independent tests. It excludes precisely the
failed targets, preserves unit tests and doctests in a full selection, and
still fails the overall run. Native consumers accept installation-only failure
when their own provider is intact. General test compilation failure remains
an invocation-wide blocker because its executable set is incomplete.
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

The explicitly ignored `prepare_real_release_installation` integration runs
only during the canonical preparation phase. It copies source into an owned directory, uses a
test-owned Cargo installation root already on the child `PATH`, installs with
`--locked`, changes that copy without changing the package version, and proves
reinstallation replaced the executable. It never runs full machine setup or
changes the user's installed utility. It calls only the installer function
against test-owned locations and rejects an older same-version binary on
`PATH`, whether the new installation directory is missing or shadowed. It
also rejects a PATH without any utility. Ambient utility directories are
excluded from these child PATH values; necessary Cargo/Rustup launchers in
those directories receive private copies.

The root record's optional `installation` field is an absolute, SHA-256-bound
artifact record for `ownedDirectory/installation/evidence.json`. It is required
for full or installation selections and null for other selections. The
schema-1 evidence contains completion status, native compiler flags, retained
artifact hashes, baseline/changed version and help observations, and exact
command outcomes with Cargo artifact JSON. Both
boundaries validate the record before use. Preparation uses one copied source
workspace and one release target graph, separate from `targetDirectory`.
The same-version edit genuinely rebuilds the real utility; dependencies must
remain fresh. Repeated setup calls must report only fresh artifacts. The old
binary has an independent owned path, so rebuilding cannot replace it.
Unsuccessful installation preparation publishes no successful evidence. Its
explicit failed outer record retains independent executables and diagnostics.

The canonical reader checks toolchain and CPU compatibility before running
tests; these compiler identity queries do not generate code. The ordinary,
prominently named installation test revalidates source and
lockfile bytes, ownership, hashes, expected command outcomes and freshness,
then compares version/help from the exact retained executables. It launches
no compiler, Cargo, or installer and never builds absent evidence. Preparation
elapsed time includes the real release compilation; report it alongside
execution rather than treating relocation as a speedup. A new preparation
uses a private release graph; repeated execution can reuse retained evidence.
No cross-run cache or shared Cargo intermediate-directory benefit is assumed.

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
