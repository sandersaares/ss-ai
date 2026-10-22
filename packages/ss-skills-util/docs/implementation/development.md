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

## Installation verification

Local installation appends `-C target-cpu=native` to the installer child's
`CARGO_ENCODED_RUSTFLAGS`, preserving the caller's existing encoded flags or
plain `RUSTFLAGS`. It leaves the caller's environment unchanged. These binaries
target the installing machine's CPU and are not portable across CPUs. Ordinary
build and CI commands do not receive this installation-only flag.

An isolated install test copies source into a test-owned directory, uses a
test-owned Cargo installation root already on the child `PATH`, installs with
`--locked`, changes that copy without changing the package version, and proves
reinstallation replaced the executable. It never runs full machine setup or
changes the user's installed utility. It calls only the installer function
against test-owned locations and rejects an older same-version binary on
`PATH`, whether the new installation directory is missing or shadowed.

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
