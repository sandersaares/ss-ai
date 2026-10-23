# ss-skills-util

This nonpublished package supplies operations shared by this repository's skills.
It is not a Rust SDK or a general machine-configuration utility.

Read `docs/design.md` for behavior and `docs/implementation.md` when changing the
implementation. `README.md` defines the public command and record contracts.
Apply the repository's Rust instructions under `instructions`
by responsibility; do not import another project's instruction tree.

Preserve Windows and Linux behavior. Run Cargo from the repository root.
Do not add test-only production behavior, install globally during tests, or
contact live review services from tests. Provider and time dependencies are
ordinary injected Rust interfaces.
