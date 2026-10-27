# Development

This document covers how to develop and test the skills in this repo. For end-user usage, see [README.md](README.md). For agent-facing conventions (skill layout, Waza usage, repo idioms), see [AGENTS.md](AGENTS.md).

## Development prerequisites

Required regardless of which area you work in:

- Current stable PowerShell (`pwsh`), targeting the 7.6 release line.
- Git.
- Rustup and the repository-pinned Rust toolchain, including Clippy/rustfmt.
  Run Cargo from this checkout's root. Windows requires Visual Studio C++
  Build Tools and Windows SDK; Ubuntu requires native compiler/linker tools
  for mimalloc. Evaluation manifest fixtures are not workspace members.
- [`just`](https://github.com/casey/just) command runner. The repo's
  `justfile` supplies shortcuts; `scripts/Invoke-Tests.ps1` is the canonical
  test runner and works without `just`.
  Installs via `cargo` once the Rust toolchain above is in place:

  ```powershell
  cargo install just --locked
  ```

Required for skill-helper unit tests:

- [Pester](https://pester.dev/) 5.x, at least 5.5.0. The runner deliberately
  selects version 5 even when a newer major version is installed:

  ```powershell
  Install-Module Pester -Scope CurrentUser -RequiredVersion 5.7.1 -Force
  ```

Required for eval tooling that reads/writes YAML (e.g. the
`Generate-FoloFullTask.ps1` task generator):

- [`powershell-yaml`](https://github.com/cloudbase/powershell-yaml) 0.4.12 or newer:

  ```powershell
  Install-Module powershell-yaml -Scope CurrentUser -MinimumVersion 0.4.12 -Force
  ```

  This is a *development-only* dependency; end users of the skills do
  not need it. Skill helper scripts (`skills/**/*.ps1`) intentionally
  avoid taking any module dependencies so they can run on a clean
  PowerShell 7 install.

# Testing

Some skills ship with executable helper scripts that have their own unit tests. Tests live under `tests/<skill-name>/` (parallel to `skills/`) and use [Pester](https://pester.dev/) 5.x.

## Running tests

Run from the active checkout root. `just --list` lists shortcuts.
The runner supports `-Suite All|Pester|Rust`, `-Path`, and `-FullNameFilter`.
An explicit Pester path does not run Rust checks, but builds the local
native utility when selected review suites need it. The runner never uses
the user's installed utility as the test prerequisite. It propagates
failure status; `All` retains Rust failures after Pester completes.

Rust checks report separate Clippy, preparation, and execution elapsed times.
Preparation compiles the shared provider once and discovers selected test
executables from Cargo artifact JSON. Every run owns its build directory; no
mutable fixture state or final binaries are shared between invocations.
An unfiltered run retains all integration tests and doctests. Installer coverage
lives in `tests/NativeInstallation.Tests.ps1`: simulated Cargo responses exercise
our arguments, errors and artifact parsing, while isolated fixture launchers
exercise PATH discovery and executable identity. These tests use the existing
PowerShell executable; they never compile or install a utility. Cargo owns
compilation, replacement and source-rebuild behavior.

Run every test in the repo:

```powershell
just test
just test-all
just test-rust
```

Prepare once for repeated native execution, using an existing directory outside
the checkout for the invocation's record:

```powershell
$record = Join-Path ([IO.Path]::GetTempPath()) ('rust-prerequisites-' + [guid]::NewGuid() + '.json')
just test-rust-prepare -PrerequisitePath $record -RustTest native
just test-rust-execute -PrerequisitePath $record -RustTest native
just test-rust-execute -PrerequisitePath $record -RustTest native -RustFilter provider
```

The PowerShell equivalents use `scripts/Invoke-Tests.ps1 -Suite Rust` with
`-Phase Prepare` or `-Phase Execute`. Omit `-RustTest` from both phases for the
full Rust suite. `-RustFilter` selects runtime test names without rebuilding.
Preparation refuses an existing record. Execution rejects missing, incomplete,
modified, stale-source, mismatched-selection, and incompatible-toolchain or
flag records instead of building a missing helper. Cargo still checks freshness;
ordinary execution uses its prepared target directory and preserves
`CARGO_BIN_EXE` and doctest behavior.
Prerequisite schema version 2 binds source content and last-write timestamps,
including manifests, Cargo configuration, and embedded inputs. A touch or
byte-identical save requires preparation again, because Cargo can rebuild from
timestamps alone. Execution rejects older records explicitly. Keep these inputs
unchanged throughout preparation and execution.
Publication holds a retained `<record>.lock` sibling across the final
existence check and atomic move. Competing publishers fail explicitly rather
than overwrite a run. Leave the lock file in place during overlapping use.

For a direct Cargo invocation, first prepare the matching selection, set
`SS_AI_RUST_TEST_PREREQUISITES` to the absolute record path, and pass the
record's `targetDirectory` as `--target-dir`. Prefer the canonical execution
phase, which also validates the complete source/toolchain/flag identity.
Native fixtures validate the record's provider source and executable bytes
and never compile a missing prerequisite. They copy the immutable helper
into private directories with spaces, keeping response files and PATH private.

The canonical runner retains an explicit failed prerequisite record when
provider preparation fails. It blocks native tests, runs already-built
independent targets and doctests, and still exits unsuccessfully. Missing test
executables remain an invocation-wide blocker.

Without an explicit record, the runner cleans up its private artifacts after
tests exit. With an explicit record, retain the record and its `ownedDirectory`
until every consumer finishes, then remove those exact owned paths. A new
preparation uses new owned outputs and cannot invalidate an active consumer.
No cross-run build cache or runner-specific process isolation is required.

Run tests for a single skill:

```powershell
just test-skill ss-cargo-deps-tidy
pwsh -NoProfile -File ./scripts/Invoke-Tests.ps1 -Path ./tests/AgentInstallation.Tests.ps1
```

Filter by `Describe` or `It` name:

```powershell
just test -FullNameFilter '*Get-SectionInfo*'
just test-skill ss-cargo-deps-tidy -FullNameFilter '*Get-SectionInfo*'
```

# Evaluations

Skills are evaluated end-to-end with [Waza](https://microsoft.github.io/waza/).
Eval specs live under `evals/<skill-name>/<lane>.eval.yaml` (one file per
cost lane, e.g. `trigger.eval.yaml`, `workflow.eval.yaml`) with task
files
under `evals/<skill-name>/tasks/**/*.yaml`.

## Prerequisites

- Waza CLI (use the latest release).
- Always invoke `waza run` from the repo root so relative paths in eval
  specs resolve correctly.
- `waza preflight <spec>` inspects configuration and task discovery offline;
  it does not run hooks, graders, or models. Model execution requires
  explicit authorization. Confirm model availability and supply `--model`
  for an authorized run rather than assuming a configured model remains available.
- `dev/ss-cargo-deps-tidy/evals/Generate-FoloFullTask.ps1` prepares the
  pinned Folo fixture and generated task. Run preparation in a disposable
  checkout when checking it independently; it downloads public source.

## Running an eval

Eval specs are split per cost tier so the cheap lane doesn't pay for
expensive `before_run` hooks. A skill exposes one spec file per lane
under `evals/<skill-name>/<lane>.eval.yaml` (e.g. `trigger.eval.yaml`,
`workflow.eval.yaml`).

All eval commands are `just` recipes that wrap `waza run` and write
results to `./eval-results`. Any extra arguments you pass after the
recipe name are forwarded verbatim to `waza run`, so every `waza run`
flag (`--model`, `--tags`, `--task`, etc.) works without editing the
`justfile`. Run `just --list` to enumerate recipes.

Run every task across every skill and every lane:

```powershell
just eval-all
```

Run a single lane across every skill:

```powershell
just eval-triggers
just eval-workflows
```

Filter by tag within a lane (e.g. skip `heavy` tasks in the workflow lane):

```powershell
just eval-workflows --tags workflow
```

Override the model used by the agent:

```powershell
just eval-triggers --model claude-opus-4.8
```

Run a single spec file (the escape hatch when you want one skill + one
lane, or to target a single task id):

```powershell
just eval evals/ss-cargo-deps-tidy/trigger.eval.yaml
just eval evals/ss-cargo-deps-tidy/trigger.eval.yaml --task trigger-negative-unrelated
```

## Tag reference

Eval task tags are kept deliberately small and orthogonal so CI and ad
hoc runs can target the right cost tier without ambiguity. Every task
that is run end-to-end **must** carry exactly one of `trigger` or
`workflow`; `heavy` is added on top when applicable.

| Tag | Meaning | When to apply |
| --- | --- | --- |
| `trigger` | The task scores skill activation/non-activation against a prompt. Inexpensive - exercises the heuristic, not the full workflow. | All tasks under `tasks/trigger/`. |
| `workflow` | The task actually exercises the skill workflow against a fixture (file mutations, validators, etc.). | Any task that asserts on outputs the skill produces. |
| `heavy` | The task is expensive (large fixtures, long timeouts, or high premium-request burn) and should not run on every PR. | Add alongside `workflow` when the cost warrants gating. |

Avoid inventing additional tags. Per-skill or per-fixture distinctions
(e.g. "folo", "cargo-only") belong in the task `id` and `name`, not in
tags - tags are a CI/cost selector, not a categorisation system.

CI uses [tests.yml](.github/workflows/tests.yml) for Windows Pester and
focused Ubuntu coverage, Windows/Ubuntu native Clippy and Cargo tests, and
macOS PowerShell-only Cargo-helper/linking coverage. Pull requests have no
narrow path filter that could skip installer-only changes. CI does not run
paid model evaluations; the documented Waza recipes are explicit local entry points.

The Rust and Pester CI jobs disable Cargo incremental compilation and use
line-table-only debug information for dev and test builds. These job-level
environment settings keep backtrace source locations while avoiding unused
incremental state and full debug information on disposable runners.
Local developer builds retain Cargo's default settings.

Installation tests use test-owned homes and isolated fixture launchers with
simulated Cargo results. Never run the real installer against the developer's profile as a test.
Provider fixtures are sanitized; tests must not contact live review services.
Report dependency setup is opt-in and documented in the skill's platform guide.

## Authoring eval specs

See [AGENTS.md](AGENTS.md) for the Waza grader and task YAML
conventions (path resolution, fixture layout, grader nesting). The
upstream guides are also required reading:

- <https://microsoft.github.io/waza/guides/eval-yaml/>
- <https://microsoft.github.io/waza/guides/graders/>
- <https://microsoft.github.io/waza/reference/schema/>
- <https://microsoft.github.io/waza/reference/cli/>
