# Agent guide for `ss-ai`

Reusable instructions live under `instructions/`, skills under `skills/`,
native tooling under `packages/`, Waza evaluations under `evals/`, and
Pester tests under `tests/`. Resolve the active checkout root, including
worktrees and paths containing spaces; never assume a user's installation path.

## PowerShell and script contracts

Target current stable PowerShell; scripts use the 7.6 release line. Every
advanced executable declares `[CmdletBinding(PositionalBinding = $false)]`.
Agent-facing skill scripts accept named arguments only and declare no
explicit parameter positions. Dot-sourced libraries belong under `lib/`.
`tests/LibraryLayout.Tests.ps1` covers root `Install.ps1`, repository scripts,
skill executables, and developer/evaluation tools.

Use root `Install.ps1` for instructions, skills, and runtime dependencies;
do not introduce a second installation command. Do not configure user-facing
Copilot preferences. `scripts/Invoke-Tests.ps1` is the test entry point.
Keep developer tools off the user PATH.

## Shared access and workflow data

Assume commands can overlap within one session. Readers permit concurrent
readers and atomic replacement. Publish complete files atomically and hold
retained sibling locks across read-modify-write transactions. Use bounded,
contention-specific retries, never retry unrelated failures, discard updates,
or delete active lock files. Test overlaps with controlled synchronization,
not real-time sleeps.

Bash setup acquires its private environment lock before the retained
`$HOME/.config/shell-startup.lock`, and releases them in reverse order.
The independent `dev-env` installer uses the same shared lock convention.

Machine-readable workflow records have explicit schemas and maintained
writers/readers. Validate producer and consumer boundaries; never invent
serialization tools during skill execution, coerce malformed records, or
silently treat missing input as completed work. Preserve the review and
autoreapproval authority boundaries and human decision gates.

## Context and commands

Load guidance by responsibility and active task. Keep common rules small
and canonical; read specialized procedures only when relevant. Assignment
construction, validation, recovery, and cache inputs share one context
contract. Test required inclusion and irrelevant-document exclusion.
Later review stages improve findings under the same eligibility rules.
Separate finding validity, explanation clarity, and remedy practicality.

When changing native code, read the package's `AGENTS.md` and relevant
instructions under `instructions/`. Waza guides are conditional evaluation
context, not prerequisites for native or review-helper work.

## Repository layout

- `skills/<skill-name>/SKILL.md` - skill entry point; runtime commands
  live under `scripts/`, libraries under `scripts/lib/`.
- `dev/<skill-name>/` - development-only commands. Cargo evaluation
  preparation and graders live under `dev/ss-cargo-deps-tidy/evals/`.
- `evals/<skill-name>/`
  - `<lane>.eval.yaml` - one spec per cost lane (e.g.
    `trigger.eval.yaml`, `workflow.eval.yaml`); lanes isolate
    expensive `before_run` hooks.
  - `tasks/**/*.yaml` - individual tasks. Tags are a separate,
    deliberately small vocabulary for CI cost tiers
    ([tag reference](DEVELOPMENT.md#tag-reference)).
  - `fixtures/` - files copied into the per-task workspace (default
    `./fixtures` relative to the eval YAML).
- `tests/<skill-name>/` - Pester tests for skill helpers (see
  [DEVELOPMENT.md](DEVELOPMENT.md)).

## Waza essentials

- Locate current Waza through PATH or an explicitly configured executable
  path. Use `waza --help` / `waza run --help`.
- **Run from the active checkout root.** Several paths (`skill_path`,
  per-task `context_dir`, the `diff` grader's `context_dir`,
  `--context-dir`) resolve against the CWD, not the YAML file - see
  [docs/waza-graders.md](docs/waza-graders.md#paths-are-resolved-against-cwd-not-the-yaml-file).

```powershell
waza run evals/<skill>/<lane>.eval.yaml --tags <tag> --output-dir .\eval-results
```

- Upstream Waza docs:
  - [Writing eval specs](https://microsoft.github.io/waza/guides/eval-yaml/)
  - [Validators & graders](https://microsoft.github.io/waza/guides/graders/)
  - [YAML schema reference](https://microsoft.github.io/waza/reference/schema/)
  - [CLI reference](https://microsoft.github.io/waza/reference/cli/)
  - [Token limits](https://microsoft.github.io/waza/guides/token-limits/)

## When you need to…

- **Write or change graders / task YAML** (paths, `context_dir`,
  `snapshot`, grader nesting, verifying skill invocation, avoiding
  the `trigger` grader, top-level `skill:` gotcha) →
  [docs/waza-graders.md](docs/waza-graders.md).
- **Re-grade a previous run, watch a live run, or read log surfaces**
  (`waza grade`, `--debug`, session/transcript dirs, workspace
  monitoring) → [docs/waza-debugging.md](docs/waza-debugging.md).
- **Diagnose a hung or misbehaving run** (workspace leak, terminal
  buffering, silent hangs) →
  [docs/waza-caveats.md](docs/waza-caveats.md).
- **Validate YAML locally** (no Python/Node on this machine) →
  [docs/yaml-tooling.md](docs/yaml-tooling.md).
- **Write or run Pester tests for skill helpers** →
  [DEVELOPMENT.md](DEVELOPMENT.md).
