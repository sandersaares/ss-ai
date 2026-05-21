# TODO

Improvement ideas to revisit when the repo matures.

## Tooling

- **Pin a Waza version in CI and docs.** Current installs (local +
  `.github/workflows/eval.yml`) drift independently. Hold off until
  Waza's installation story is more stable.
- **Add PSScriptAnalyzer.** Ship a `PSScriptAnalyzerSettings.psd1` and
  a CI lint job for `skills/**/*.ps1`, `evals/**/*.ps1`, and
  `tests/**/*.ps1`. Cheap insurance for a repo whose product is
  PowerShell scripts.
- **Add an `.editorconfig`.** Lock indentation and EOL conventions.

## Skill design

- **Deduplicate the workspace-flag exception list** (`std`, `alloc`,
  `clap/default`, `argh/default`, ...). Currently described twice in
  [`skills/ss-cargo-deps-tidy/SKILL.md`](skills/ss-cargo-deps-tidy/SKILL.md)
  — once as prose under "Workspace-level feature flags" and once as
  inline PowerShell arrays in stage 6. A single source of truth (e.g.
  emitted by `Generate-Inventory.ps1`) would avoid drift, but
  representing both the data and the rationale concisely is non-trivial.
  Needs more design thought.

## Evaluations

- **Replace the upstream-bug workaround paragraph** in folo task prompts
  once Waza ships with copilot-cli >= 1.0.49. Currently sourced from
  [`evals/ss-cargo-deps-tidy/prompts/upstream-bug-workaround.md`](evals/ss-cargo-deps-tidy/prompts/upstream-bug-workaround.md).
- **Bring eval `model` field in line with the README.** Waza 0.31.0's
  hard-coded model list does not include `claude-opus-4.7`, so
  [`evals/ss-cargo-deps-tidy/eval.yaml`](evals/ss-cargo-deps-tidy/eval.yaml)
  pins `claude-opus-4.6`. Revisit when Waza updates the list.
