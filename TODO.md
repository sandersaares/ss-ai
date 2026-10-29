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
- **Portable schema-error assertions.** Make the diagnostic assertion in
  `tests/SsReviewWorkflowData.Tests.ps1` independent of PowerShell's
  console-width line wrapping.
- **Evaluation cache/task publication.** Assess shared access to
  `dev/ss-cargo-deps-tidy/evals/Generate-FoloFullTask.ps1` and add transaction
  protection where needed; use disposable checkouts for standalone preparation.

## Skill design

- **Migrate ss-review command families to Rust.** Replace the remaining complete
  families in order: coverage and worker inputs; instruction
  preparation and cache; provider capture; report tooling; overview publication;
  final review publication; and trackers. Local context, finding, audit, and
  rendering commands use native operations through PowerShell adapters. Retire
  the adapters after their callers
  migrate while preserving the hybrid workflow's records, transaction boundaries
  and authority gates. Keep manifest-based review-work and coverage orchestration
  in PowerShell until the coverage and worker-input migration; finding validation
  must remain available to that orchestration.
- **Type ss-review workflow errors.** Replace generic application errors at the
  finding, plan, audit, projection, anchor, rendering and diff-validation
  boundaries with typed invariant errors and aggregates, preserving command-level
  error categories and diagnostics. Complete this when focused tests can
  distinguish rejected invariants without matching prose.
- **Type validated finding artifacts.** Keep validated persisted artifacts
  distinct from raw JSON throughout finding mutations and consolidation.
  Complete this when those operations accept the validated representation rather
  than repeatedly recovering artifact fields from unrestricted values.
- **Type validated review evidence and decisions.** Establish diff invariants
  during construction and represent validated decision journals with typed
  verdicts. Complete this when anchor resolution and decision replay consume
  those validated types without repeated checks or string verdict matching.
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
