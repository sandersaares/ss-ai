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
- **Bash startup test output.** Isolate the interactive-shell assertion in
  `tests/BashEnvironment.Tests.ps1` from distribution startup banners, which
  can precede the environment value being checked.
- **Evaluation cache/task publication.** Assess shared access to
  `dev/ss-cargo-deps-tidy/evals/Generate-FoloFullTask.ps1` and add transaction
  protection where needed; use disposable checkouts for standalone preparation.
## Skill design

- **Deduplicate the workspace-flag exception list** (`std`, `alloc`,
  `clap/default`, `argh/default`, ...). Currently described twice in
  [`skills/ss-cargo-deps-tidy/SKILL.md`](skills/ss-cargo-deps-tidy/SKILL.md)
  - once as prose under "Workspace-level feature flags" and once as
  inline PowerShell arrays in stage 6. A single source of truth (e.g.
  emitted by `ss-ai cargo inventory`) would avoid drift, but
  representing both the data and the rationale concisely is non-trivial.
  Needs more design thought.
- **Cargo inventory/expectation readers.** Add schema-checked reading commands
  for the native Cargo inventory and its graders' expectation records.

## Migration design decisions

- Capture lock files are tool-owned. Detecting externally created hard-link
  aliases is not needed for the supported lock creation workflow.
- Instruction reference expansion uses the selected source location and its
  root mapping. The snapshot producer does not resolve references.
- The focus-cache refresh interval is checked against its complete positive
  range before its only division in `compute_descriptor`. A separate internal
  type would not remove a repeated check.
- The focus-generation build script reports build errors directly to Cargo.
  Application recovery categories have no consumer there, so adding a runtime
  build dependency only for error-type uniformity is unnecessary.
- Cache maintenance retains integer timestamps. Replacing them with Jiff
  timestamps would require compatibility handling for existing sentinel and
  out-of-range values without improving the current whole-second calculation.
- Source spans still require source lookup, range ordering, bounds and
  nonblank-content checks after parsing. A separate nonzero line type would
  not remove these checks. Reference outcomes retain their validated wire
  representation because a separate enum and conversion would duplicate the
  persisted schema without simplifying its boundary diagnostics.
- Saved-request diagnostics retain their existing validated read and write
  boundaries. A separate wrapper would require additional mutation machinery
  without protecting an unchecked caller.
- Workflow errors distinguish the finding, decision, source-selection and
  saved-request invariants that have concrete test consumers. Other projection,
  anchor and rendering failures retain classified application errors; an
  exhaustive error type for each diagnostic would add a taxonomy without a
  caller that needs it.
