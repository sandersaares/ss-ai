# TODO

Improvement ideas to revisit when the repo matures.

## Tooling

- **Add PSScriptAnalyzer.** Ship a `PSScriptAnalyzerSettings.psd1` and
  a CI lint job for `skills/**/*.ps1`, `evals/**/*.ps1`, and
  `tests/**/*.ps1`. Cheap insurance for a repo whose product is
  PowerShell scripts.
- **Add an `.editorconfig`.** Lock indentation and EOL conventions.
- **Generated nvm fixture on Ubuntu.** Correct the generated `nvm.sh`
  syntax exercised by `SsReviewReportTools.Tests.ps1`, including its real
  POSIX-shell activation guard.
- **Linux audit file URIs.** Use explicit file URI construction in
  `skills/ss-review/scripts/ConvertTo-FindingMarkdown.ps1` and cover it in
  `SsReviewFindingMarkdown.Tests.ps1`.
- **Evaluation cache/task publication.** Assess shared access to
  `dev/ss-cargo-deps-tidy/evals/Generate-FoloFullTask.ps1` and add transaction
  protection where needed; use disposable checkouts for standalone preparation.

## Skill design

- **Deduplicate the workspace-flag exception list** (`std`, `alloc`,
  `clap/default`, `argh/default`, ...). Currently described twice in
  [`skills/ss-cargo-deps-tidy/SKILL.md`](skills/ss-cargo-deps-tidy/SKILL.md)
  - once as prose under "Workspace-level feature flags" and once as
  inline PowerShell arrays in stage 6. A single source of truth (e.g.
  emitted by `ss-skills-util cargo inventory`) would avoid drift, but
  representing both the data and the rationale concisely is non-trivial.
  Needs more design thought.
- **Cargo inventory/expectation readers.** Add schema-checked reading commands
  for the native Cargo inventory and its graders' expectation records.
