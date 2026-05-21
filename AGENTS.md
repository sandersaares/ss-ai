# Agent guide for `ss-ai`

This repository hosts Copilot **skills** (under `skills/`) and **Waza evals**
that verify those skills (under `evals/`). This document captures the
conventions an automated agent needs to be productive here.

## Repository layout

- `skills/<skill-name>/` — Skill definition. `SKILL.md` is the entrypoint;
  helper scripts and reference docs live alongside it.
- `evals/<skill-name>/` — Waza evaluation suite for that skill.
  - `<lane>.eval.yaml` — top-level eval spec, one per cost lane (e.g.
    `trigger.eval.yaml`, `workflow.eval.yaml`). Lanes exist so cheap
    runs don't pay for expensive `before_run` hooks tied to heavier
    lanes.
  - `tasks/**/*.yaml` — individual tasks (organize by directory, e.g.
    `tasks/trigger/`, `tasks/folo/`). Task **tags** are a separate,
    deliberately small vocabulary used by CI to select cost tiers; see
    the [Tag reference in DEVELOPMENT.md](DEVELOPMENT.md#tag-reference).
  - `fixtures/` — files that get copied into the per-task workspace
    (resolved relative to the eval YAML, default `./fixtures`)
- `tests/<skill-name>/` — Pester tests for skill helper scripts (see
  [DEVELOPMENT.md](DEVELOPMENT.md))

## Working with Waza

Waza is the eval runner used here. Authoritative docs:

- **Writing eval specs**: <https://microsoft.github.io/waza/guides/eval-yaml/>
- **Validators & graders**: <https://microsoft.github.io/waza/guides/graders/>
- **YAML schema reference**: <https://microsoft.github.io/waza/reference/schema/>
- **CLI reference**: <https://microsoft.github.io/waza/reference/cli/>
- **Token limits**: <https://microsoft.github.io/waza/guides/token-limits/>

Local CLI: `c:\Users\Sander\My Drive\Tools\Waza\waza.exe` (currently
v0.31.0). Run `waza --help` and `waza run --help` for the full surface.

### Always run from the repo root

Waza resolves several paths relative to the **current working directory**,
not relative to the eval YAML or the task YAML. In particular:

- The `trigger` grader's `skill_path` is resolved against the CWD.
- `--context-dir` defaults to `./fixtures` relative to the eval spec, but
  references inside the spec are easier to reason about when CWD is fixed.

Always invoke Waza from `c:\Source\ss-ai`:

```powershell
cd c:\Source\ss-ai
waza run evals/<skill>/<lane>.eval.yaml --tags <tag> --output-dir .\eval-results
```

### Stable conventions (verified against Waza 0.31.0)

- **Grader `skill_path`** — use a path relative to the repo root, e.g.
  `skills/ss-cargo-deps-tidy/SKILL.md`. Do **not** try to traverse with
  `../..` from the task YAML; the path is not resolved against the task
  file's directory.
- **`inputs.files[].path`** — both source (under the active fixtures
  dir) AND destination (under the per-task workspace dir). To seed a
  Cargo workspace manifest at the workspace root, the fixture file
  must live directly under the active fixtures dir (not under a
  sub-folder like `before/`). When two tasks both need to seed the
  same workspace-relative path (e.g. each wants its own root
  `Cargo.toml`), use a per-task `context_dir` to give each task its
  own private fixture root — see below.
- **Per-task `context_dir`** — task YAML files accept a top-level
  `context_dir:` that overrides the eval-level fixtures dir for that
  one task ([waza/internal/models/testcase.go](https://github.com/microsoft/waza/blob/main/internal/models/testcase.go),
  field `TestCase.ContextRoot`). Used directly with `filepath.Join`
  by the runner, so it is resolved relative to the **CWD of
  `waza run`** (not the task YAML or eval spec). Always express it
  as a repo-root-relative path, e.g.
  `context_dir: evals/<skill>/fixtures/trigger/<task-id>`.
- **`inputs.files[].content`** — alternative to `path`, supplies the
  file body inline. Use `path` + a per-task `context_dir` for real
  fixture files; reserve inline `content` for one-off cases where
  carving out a fixture directory adds more clutter than it removes.
- **`diff` grader `snapshot`** — path relative to the grader's
  `context_dir`. Store expected files under a clearly named subdir
  (e.g. `fixtures/<scenario>-expected/...`) so they don't collide
  with workspace-seeding fixtures.
- **`diff` grader `context_dir`** — IS a real field (per
  [docs/graders/diff.md](https://github.com/microsoft/waza/blob/main/docs/graders/diff.md)).
  Resolved relative to the CWD of `waza run` (NOT the eval spec).
  The CLI default behaviour (`--context-dir ./fixtures` relative to
  spec) is unreliable in practice (silently misresolves to CWD when
  filtering with `--tags`/`--task`). Always set this inline, e.g.
  `context_dir: evals/<skill>/fixtures`.
- **Verifying skill invocation.** Waza's bundled copilot-sdk *does*
  emit `skill.invoked` events when the agent invokes a skill, and they
  surface in three places in the results JSON:
  - In each task's `runs[].transcript` as events with `type:
    "skill.invoked"` (the `content` field carries the full SKILL.md
    text).
  - Aggregated into `runs[].skill_invocations` (omitted entirely if no
    skill was invoked in that run).
  - As a `skill` entry in `runs[].session_digest.tools_used` /
    `tool_calls`.

  The simplest grader to verify invocation is the `behavior` grader
  with `required_tools: [skill]` (positive) or `forbidden_tools:
  [skill]` (negative). The `skill_invocation` grader also works but
  requires at least one `required_skills` entry, so it can't directly
  express "no skill invocation" for negative tasks.

  **Use this signal only for trigger-style evals.** In trigger evals
  the whole point is "did the agent decide to invoke the skill", so
  `skill` tool presence/absence is the metric. In workflow-style
  evals, even when the prompt names the skill explicitly ("apply the
  `ss-cargo-deps-tidy` skill"), the agent sometimes reads the skill
  body from its system-prompt context and proceeds directly with
  `view` / `str_replace` / `create` calls *without* invoking the
  `skill` tool. Observed empirically: the `folo-3e1f241-cargo-only`
  run at `eval-results/folo-debug/...20260520-083815.json` completed
  79 tool calls with zero `skill.invoked` events. For workflow evals,
  measure the *outcome* (`diff` graders against expected Cargo.toml
  files) rather than tool-call shape.
- **Grader config nesting** — `type:` and `name:` go at the top of the
  grader entry; type-specific keys go under `config:`. Example:

  ```yaml
  graders:
    - type: behavior
      name: skill-invoked
      config:
        required_tools:
          - skill
  ```

### Re-grading without re-executing the agent

`waza grade` re-runs the graders against a previous run's output
without invoking the model. Useful when iterating on grader config
after a successful (but mis-graded) agent run:

```powershell
waza grade evals/<skill>/<lane>.eval.yaml `
    --results .\eval-results\<timestamp>\claude-opus-4.6.json `
    --workspace 'C:\Users\Sander\AppData\Local\Temp\waza-<random>' `
    --task <task-id> -o regrade.json -v
```

The agent's per-task workspace must still exist (run the original
with `--keep-workspace` to preserve it). `--workspace` is required
because `waza grade` does not auto-derive it from the results JSON.

### Avoid the `trigger` grader

The `trigger` grader scores the *prompt* heuristically against the
skill's keyword set (no stop-word filtering, no session inspection),
so false positives on negative cases are easy and the agent's actual
behavior is invisible to it. Prefer `behavior` / `skill_invocation`
/ `diff` graders that observe what the agent actually did.

### Caveats observed in practice

- The agent will sometimes look outside the per-task temp workspace if it
  finds the workspace empty. For prompts that reference "this Rust
  workspace" without any seeded files, the agent has been observed to
  read files from `C:\Source\folo` directly. It has not modified those
  files, but the wasted compute is real.
- Terminal output in PowerShell often gets compressed mid-line and
  Tee-Object only flushes after the run finishes — rely on the JSON
  result file under `eval-results\<timestamp>\` for the authoritative
  outcome of each task.
- Runs can silently hang: the agent stops emitting events but waza keeps
  waiting (no API-side timeout). If a long task's per-task temp workspace
  (`%TEMP%\waza-<random>\`) shows no file modifications for many minutes,
  the run is almost certainly dead — kill the terminal and restart.
- **Sub-agent permission deadlock — upstream copilot-cli bug.** Waza
  installs an `allowAllTools` permission handler on the parent session
  (`internal/execution/copilot.go`), but when the agent invokes the
  built-in `task` tool to spawn a sub-agent, the sub-agent's tool calls
  arrive at the SDK with a child session ID that the SDK's session map
  does not know — so the handler is never called and
  `permission.requested` events accumulate without matching
  `permission.completed` events. The run hangs indefinitely. This is
  tracked in [copilot-sdk#947 / #959](https://github.com/github/copilot-sdk/issues/947)
  ("Subagents don't inherit session-defined external tools") and the
  copilot-cli runtime fix landed in **copilot-cli v1.0.49-0**
  ("Hooks ... now fire correctly for sub-agent tool calls", 2026-05-13).
  Waza 0.31.0 still bundles **copilot_1.0.2** (see
  `internal/embedded/zcopilot_*.zst`), which predates the fix and has
  no override env var — the embedded loader hard-locks the version.
  Until microsoft/waza ships a release with an updated bundled CLI, any
  task whose agent invokes the `task` tool will hang. Diagnostic: in a
  `--debug` log, count `permission.requested` vs `permission.completed`
  — if the gap grows monotonically while `subagent.started` has fired,
  it is this bug.

### Watching a run live

`waza run` with `-v` only prints the prompt and a one-line status. For
event-by-event visibility (tool calls, reasoning, permissions, turns),
use the **global** `--debug` flag and redirect to a file you can tail:

```powershell
cd c:\Source\ss-ai
$dbg = ".\eval-results\folo-debug"
New-Item -ItemType Directory -Force $dbg | Out-Null
waza --debug run evals/<skill>/<lane>.eval.yaml --tags <tag> `
    --output-dir .\eval-results --keep-workspace -v `
    --session-log --session-dir $dbg --transcript-dir $dbg `
    *>&1 > "$dbg\stdout.log"
```

Then in a separate terminal, tail it:

```powershell
Get-Content .\eval-results\folo-debug\stdout.log -Wait -Tail 20
# Or filter to "interesting" events only:
Get-Content .\eval-results\folo-debug\stdout.log -Wait |
    Select-String 'tool.execution_(start|complete)|turn_start|turn_end|assistant.message'
```

Three log surfaces, each useful for a different purpose:

| File | What it contains | Best use |
|------|------------------|----------|
| `stdout.log` (from `--debug`) | Verbose event stream incl. tool args + results | Live tail to see what the agent is doing |
| `<timestamp>-session.jsonl` (from `--session-log`) | Sparse milestones (session/task start, task complete) | Programmatic progress polling |
| `<task>-transcript.json` (from `--transcript-dir`) | Full transcript per task on completion | Post-mortem debugging |

Watching the temp workspace also helps confirm forward progress:

```powershell
$tmp = (Get-ChildItem $env:TEMP -Directory | Where-Object Name -match 'waza-' |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
Get-ChildItem $tmp -Recurse -File |
    Group-Object { $_.LastWriteTime.ToString('HH:mm') } |
    Sort-Object Name | Format-Table Name, Count -AutoSize
```

## YAML validation without Python or Node

This machine has no native Python or Node.js. The `powershell-yaml`
module (PSGallery, v0.4.12+) is the most reliable local YAML validator:

```powershell
Install-Module powershell-yaml -Scope CurrentUser
Import-Module powershell-yaml
ConvertFrom-Yaml (Get-Content -Raw path/to/file.yaml)
```

## Skill helper-script testing

See [DEVELOPMENT.md](DEVELOPMENT.md) for the Pester conventions used by
helper scripts that ship with skills (e.g.
`skills/ss-cargo-deps-tidy/Generate-Inventory.ps1`).
