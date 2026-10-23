---
applyTo: "**/Cargo.toml"
---

# MSRV change is a regular dependency change

A change in MSRV is not a breaking change - it is the same as upgrading the version of a dependency. It does not require any special notice or marking and it is not a breaking change that requires a major version increment.

# Publish only the code that must be compiled

A Cargo manifest should explicitly list the files to include/exclude from the published package so that only the minimum set of mandatory
files are included. For example, `src/**` obviously is required but `benches/**`, `docs/**` (maintainer-only documentation), `examples/**`
and `tests/**` are only used for development of the crate, so should not be included in the published Cargo package.

# Ensure documentation enables all features

Ensure `Cargo.toml` contains:

```toml
[package.metadata.docs.rs]
all-features = true
```

# Dev-dependencies within workspace packages must be path dependencies

Do not use `version = "1.2.3"` or `workspace = true` when adding a package from the same workspace as a dev-dependency.
Within the same workspace, dev-dependencies must always be `path = "../foo"` style path-references to ensure proper
dependency resolution at publishing-time.

# Declare example/test/benchmark binary feature requirements in Cargo.toml

Use `required-features` on an `[[example]]`, `[[test]]` or `[[bench]]` target that needs non-default
features. This ensures that Cargo rejects explicit selection of the example, test or benchmark and reports the
missing features, while aggregate target builds silently skip it. Do not perform runtime feature-checking in
auxilliary targets like examples, tests or benchmarks - if it cannot run, it should not even be built.

# Cargo features exist to toggle API surface on/off

Cargo feature flags should not result in behavioral differences beyond disabling/enabling API surface.
Behavioral differences such as "sort the outputs" being configured via feature flags is an invalid usage of Cargo feature flags.

# Cargo.toml dependency version changes do not require documentation unless pinning

Regular dependency version adjustments declared as semver-compatible minimums (e.g. `1.2.3` or `^1.2.3`) do not require any inline comments.
Such dependency versions declared in a Cargo manifest are a technical detail not worth commenting on. They are a side-effect of what the code requires.

The exception is a pinned or otherwise irregular version declaration (e.g. `=1.2.3`). If we do not merely declare a semver-minimum, we need to leave a comment to explain why.

# Cargo feature flags must not determine behavior beyond API surface

Changing behavior based on feature flag interactions is invalid design (e.g. deciding what behavior to use based on feature flag A overriding feature flag B, or whether feature C is enabled). Feature flags exist only to enable/disable API surface. Any behavioral differences must be confined on the API surface itself (e.g. by accepting trait implementations or enabling additional configuration API based on presence of Cargo feature flags).