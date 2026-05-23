# Agent guide for `ss-ai`

Skills live under `skills/`, Waza evals under `evals/`, Pester tests
under `tests/`. This document is a thin reference; deep-dives live in
[`docs/`](docs/).

## Repository layout

- `skills/<skill-name>/SKILL.md` — skill entrypoint; helpers and
  reference docs sit alongside.
- `evals/<skill-name>/`
  - `<lane>.eval.yaml` — one spec per cost lane (e.g.
    `trigger.eval.yaml`, `workflow.eval.yaml`); lanes isolate
    expensive `before_run` hooks.
  - `tasks/**/*.yaml` — individual tasks. Tags are a separate,
    deliberately small vocabulary for CI cost tiers
    ([tag reference](DEVELOPMENT.md#tag-reference)).
  - `fixtures/` — files copied into the per-task workspace (default
    `./fixtures` relative to the eval YAML).
- `tests/<skill-name>/` — Pester tests for skill helpers (see
  [DEVELOPMENT.md](DEVELOPMENT.md)).

## Waza essentials

- Local CLI: `c:\Users\Sander\My Drive\Tools\Waza\waza.exe` —
  currently **v0.31.0**. Use `waza --help` / `waza run --help`.
- **Always run from `c:\Source\ss-ai`.** Several paths (`skill_path`,
  per-task `context_dir`, the `diff` grader's `context_dir`,
  `--context-dir`) resolve against the CWD, not the YAML file — see
  [docs/waza-graders.md](docs/waza-graders.md#paths-are-resolved-against-cwd-not-the-yaml-file).

```powershell
cd c:\Source\ss-ai
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
  buffering, silent hangs, sub-agent permission deadlock) →
  [docs/waza-caveats.md](docs/waza-caveats.md).
- **Validate YAML locally** (no Python/Node on this machine) →
  [docs/yaml-tooling.md](docs/yaml-tooling.md).
- **Write or run Pester tests for skill helpers** →
  [DEVELOPMENT.md](DEVELOPMENT.md).
