# Development

This document covers how to develop and test the skills in this repo. For end-user usage, see [README.md](README.md). For agent-facing conventions (skill layout, Waza usage, repo idioms), see [AGENTS.md](AGENTS.md).

## Development prerequisites

Required regardless of which area you work in:

- PowerShell 7+ (`pwsh`).
- Git.

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

## Prerequisites

See "Development prerequisites" above for Pester setup.

## Running tests

Run every test in the repo:

```powershell
Invoke-Pester ./tests/
```

Run tests for a single skill:

```powershell
Invoke-Pester ./tests/ss-cargo-deps-tidy/
```

Filter by `Describe` or `It` name:

```powershell
Invoke-Pester ./tests/ -FullNameFilter '*Get-SectionInfo*'
```

# Evaluations

Skills are evaluated end-to-end with [Waza](https://microsoft.github.io/waza/).
Eval specs live under `evals/<skill-name>/<lane>.eval.yaml` (one file per
cost lane, e.g. `trigger.eval.yaml`, `workflow.eval.yaml`) with task
files
under `evals/<skill-name>/tasks/**/*.yaml`.

## Prerequisites

- Waza CLI (`waza --version` should report 0.31.0 or newer).
- Always invoke `waza run` from the repo root so relative paths in eval
  specs resolve correctly.

## Running an eval

Eval specs are split per cost tier so the cheap lane doesn't pay for
expensive `before_run` hooks. A skill exposes one spec file per lane
under `evals/<skill-name>/<lane>.eval.yaml` (e.g. `trigger.eval.yaml`,
`workflow.eval.yaml`).

Run every task across every skill and every lane:

```powershell
Get-ChildItem evals/*/*.eval.yaml | ForEach-Object {
    waza run $_.FullName --output-dir .\eval-results
}
```

Run a single lane for a single skill:

```powershell
waza run evals/ss-cargo-deps-tidy/trigger.eval.yaml --output-dir .\eval-results
waza run evals/ss-cargo-deps-tidy/workflow.eval.yaml --output-dir .\eval-results
```

Filter by tag within a lane (e.g. skip `heavy` tasks in the workflow lane):

```powershell
waza run evals/ss-cargo-deps-tidy/workflow.eval.yaml --tags workflow --output-dir .\eval-results
```

Run a single task by id:

```powershell
waza run evals/ss-cargo-deps-tidy/trigger.eval.yaml --task trigger-negative-unrelated --output-dir .\eval-results
```

## Tag reference

Eval task tags are kept deliberately small and orthogonal so CI and ad
hoc runs can target the right cost tier without ambiguity. Every task
that is run end-to-end **must** carry exactly one of `trigger` or
`workflow`; `heavy` is added on top when applicable.

| Tag | Meaning | When to apply |
| --- | --- | --- |
| `trigger` | The task scores skill activation/non-activation against a prompt. Inexpensive — exercises the heuristic, not the full workflow. | All tasks under `tasks/trigger/`. |
| `workflow` | The task actually exercises the skill workflow against a fixture (file mutations, validators, etc.). | Any task that asserts on outputs the skill produces. |
| `heavy` | The task is expensive (large fixtures, long timeouts, or high premium-request burn) and should not run on every PR. | Add alongside `workflow` when the cost warrants gating. |

Avoid inventing additional tags. Per-skill or per-fixture distinctions
(e.g. "folo", "cargo-only") belong in the task `id` and `name`, not in
tags — tags are a CI/cost selector, not a categorisation system.

CI mapping (see [.github/workflows](.github/workflows)):

- `eval.yml` (push to `main`, same-repo PRs) — runs every
  `evals/*/trigger.eval.yaml`.
- `eval-full.yml` (`workflow_dispatch`) — runs every
  `evals/*/*.eval.yaml` by default; the operator can pass
  `tags: workflow` to skip the heavy bucket or `tags: heavy` to run
  only the expensive scenarios.

## Authoring eval specs

See [AGENTS.md](AGENTS.md) for the verified-against-Waza-0.31.0
conventions (path resolution, fixture layout, grader nesting). The
upstream guides are also required reading:

- <https://microsoft.github.io/waza/guides/eval-yaml/>
- <https://microsoft.github.io/waza/guides/graders/>
- <https://microsoft.github.io/waza/reference/schema/>
- <https://microsoft.github.io/waza/reference/cli/>
