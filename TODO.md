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
- **Make concurrent worker-input diagnostics testing deterministic.** The
  default-parallel Rust suite can fail
  `worker_input::concurrent_reads_preserve_diagnostics` when a reader exhausts
  the bounded shared-file contention retries. The same failure occurred in
  full-suite runs of parent 848274ecbd117bfabceae6732eac506012d43fb9 and
  descendant 6808c65a42ea87d7cac204042e8a4e5ed580cb5f, with only the test
  assertion instrumented to print its error. Replace the test's retry
  scheduling with controlled synchronization and assert the diagnostics
  explicitly, without increasing production retry budgets or serializing the
  suite.

## Skill design

- **Detect hard-linked capture lock files.** Reject aliased retained sibling
  locks before waiting when portable file identity can be checked without
  interfering with other lock holders. Until then, self-contention fails
  through the existing bounded lock-contention path.
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
- **Type validated focus preparation state.** Represent the coupled identities
  and mutually exclusive publication receipts in the internal generation
  record, while preserving the persisted format and its validation boundary.
  Complete this when each transition constructs only coherent state.
- **Type the cache refresh interval.** Represent the positive refresh interval
  after input validation while retaining the existing CLI rejection behavior
  and upper bound.
- **Type validated instruction references and spans.** Represent reference
  outcomes as distinct internal states and one-based source lines as nonzero
  values after parsing. Preserve the existing wire format and diagnostics.
- **Clarify symlinked instruction references.** Decide whether local references
  inside a symlinked instruction file use the link's directory or the target's
  directory, then document and test the chosen behavior.
- **Unify build-script errors.** Use the shared application error type for the
  focus-generation fingerprint builder when its build dependencies can carry
  that type without undermining the minimum-dependency checks.
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
