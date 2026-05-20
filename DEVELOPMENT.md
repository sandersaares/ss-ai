# Development

This document covers how to develop and test the skills in this repo. For end-user usage, see [README.md](README.md). For agent-facing conventions (skill layout, Waza usage, repo idioms), see [AGENTS.md](AGENTS.md).

# Testing

Some skills ship with executable helper scripts that have their own unit tests. Tests live under `tests/<skill-name>/` (parallel to `skills/`) and use [Pester](https://pester.dev/) 5.x.

## Prerequisites

- PowerShell 7+
- Pester 5.5.0 or newer:

  ```powershell
  Install-Module Pester -Scope CurrentUser -MinimumVersion 5.5.0 -Force
  ```

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
Eval specs live under `evals/<skill-name>/eval.yaml` with task files
under `evals/<skill-name>/tasks/**/*.yaml`.

## Prerequisites

- Waza CLI (`waza --version` should report 0.31.0 or newer).
- Always invoke `waza run` from the repo root so relative paths in eval
  specs resolve correctly.

## Running an eval

Run every task in a skill's suite, writing results into `eval-results/`:

```powershell
waza run evals/ss-cargo-deps-tidy/eval.yaml --output-dir .\eval-results
```

Filter by tag (e.g. trigger-precision tests only):

```powershell
waza run evals/ss-cargo-deps-tidy/eval.yaml --tags trigger --output-dir .\eval-results
```

Run a single task by id:

```powershell
waza run evals/ss-cargo-deps-tidy/eval.yaml --task trigger-negative-unrelated --output-dir .\eval-results
```

## Authoring eval specs

See [AGENTS.md](AGENTS.md) for the verified-against-Waza-0.31.0
conventions (path resolution, fixture layout, grader nesting). The
upstream guides are also required reading:

- <https://microsoft.github.io/waza/guides/eval-yaml/>
- <https://microsoft.github.io/waza/guides/graders/>
- <https://microsoft.github.io/waza/reference/schema/>
- <https://microsoft.github.io/waza/reference/cli/>
