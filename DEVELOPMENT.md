# Development

This document covers how to develop and test the skills in this repo. For end-user usage, see [README.md](README.md). For agent-facing conventions (skill layout, Waza usage, repo idioms), see [AGENTS.md](AGENTS.md).

## Development prerequisites

Required regardless of which area you work in:

- PowerShell 7+ (`pwsh`).
- Git.
- Rust (latest stable toolchain via [rustup](https://rustup.rs/)). Some
  skills (e.g. `ss-cargo-deps-tidy`) shell out to `cargo` during
  evaluation, and a clean rustup install is the expected way to provide
  it.
- [`just`](https://github.com/casey/just) command runner. The repo's
  `justfile` is the canonical entry point for running tests and evals;
  every workflow in the rest of this document is a `just` recipe.
  Installs via `cargo` once the Rust toolchain above is in place:

  ```powershell
  cargo install just --locked
  ```

Required for skill-helper unit tests:

- [Pester](https://pester.dev/) 5.5.0 or newer:

  ```powershell
  Install-Module Pester -Scope CurrentUser -MinimumVersion 5.5.0 -Force
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

All test commands are `just` recipes that run from the repo root. Use
`just --list` to enumerate everything available. Any extra arguments
you pass after the recipe name are forwarded verbatim to
`Invoke-Pester`.

Run every test in the repo:

```powershell
just test
```

Run tests for a single skill:

```powershell
just test-skill ss-cargo-deps-tidy
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

CI mapping (see [.github/workflows](.github/workflows)):

- `eval.yml` (push to `main`, same-repo PRs) - runs every
  `evals/*/trigger.eval.yaml`.
- `eval-full.yml` (`workflow_dispatch`) - runs every
  `evals/*/*.eval.yaml` by default; the operator can pass
  `tags: workflow` to skip the heavy bucket or `tags: heavy` to run
  only the expensive scenarios.

## Authoring eval specs

See [AGENTS.md](AGENTS.md) for the Waza grader and task YAML
conventions (path resolution, fixture layout, grader nesting). The
upstream guides are also required reading:

- <https://microsoft.github.io/waza/guides/eval-yaml/>
- <https://microsoft.github.io/waza/guides/graders/>
- <https://microsoft.github.io/waza/reference/schema/>
- <https://microsoft.github.io/waza/reference/cli/>
