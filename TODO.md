# TODO

Improvement ideas to revisit when the repo matures.

## Tooling

- **Add PSScriptAnalyzer.** Ship a `PSScriptAnalyzerSettings.psd1` and
  a CI lint job for `skills/**/*.ps1`, `evals/**/*.ps1`, and
  `tests/**/*.ps1`. Cheap insurance for a repo whose product is
  PowerShell scripts.
- **Add an `.editorconfig`.** Lock indentation and EOL conventions.

## Skill design

- **Deduplicate the workspace-flag exception list** (`std`, `alloc`,
  `clap/default`, `argh/default`, ...). Currently described twice in
  [`skills/ss-cargo-deps-tidy/SKILL.md`](skills/ss-cargo-deps-tidy/SKILL.md)
  - once as prose under "Workspace-level feature flags" and once as
  inline PowerShell arrays in stage 6. A single source of truth (e.g.
  emitted by `Generate-Inventory.ps1`) would avoid drift, but
  representing both the data and the rationale concisely is non-trivial.
  Needs more design thought.
