---
applyTo: "**/*.rs,**/Cargo.toml"
---

# Changes in feature gates are breaking changes

Adding a Cargo feature flag gate to an API is a breaking change (e.g. when an API now requires a `foo` feature).
This is true even if the required feature is implicitly enabled by the `default` feature (because this will still break users who do not use default features).

Removing implicit feature dependencies is a breaking change (e.g. when feature `foo` no longer always enables feature `bar`).