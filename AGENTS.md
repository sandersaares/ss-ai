# Agent guide for `ss-ai`

This repository hosts Copilot **skills** (under `skills/`) and **Waza evals**
that verify those skills (under `evals/`). This document captures the
conventions an automated agent needs to be productive here.

## Repository layout

- `skills/<skill-name>/` — Skill definition. `SKILL.md` is the entrypoint;
  helper scripts and reference docs live alongside it.
- `evals/<skill-name>/` — Waza evaluation suite for that skill.
  - `eval.yaml` — top-level eval spec
  - `tasks/**/*.yaml` — individual tasks (organize by tag, e.g.
    `tasks/trigger/`, `tasks/folo/`)
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
waza run evals/<skill>/eval.yaml --tags <tag> --output-dir .\eval-results
```

### Stable conventions (verified against Waza 0.31.0)

- **Grader `skill_path`** — use a path relative to the repo root, e.g.
  `skills/ss-cargo-deps-tidy/SKILL.md`. Do **not** try to traverse with
  `../..` from the task YAML; the path is not resolved against the task
  file's directory.
- **`inputs.files[].path`** — both source (under fixtures dir) AND
  destination (under per-task workspace dir). To seed a Cargo workspace
  manifest at the workspace root, the fixture file must live directly
  under the fixtures dir (not under a sub-folder like `before/`).
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
- **`skill_invocation` grader DOES NOT WORK with waza's skill
  injection model.** Waza injects SKILL.md content directly into the
  agent's system prompt and never fires Copilot SDK `SkillInvoked`
  events. The grader always scores 0 in single-skill scenarios.
  Use `diff` (workspace file matching) or `trigger` (prompt
  heuristic) graders to verify skill effectiveness instead.
- **Grader config nesting** — `type:` and `name:` go at the top of the
  grader entry; type-specific keys go under `config:`. Example:

  ```yaml
  graders:
    - type: trigger
      name: trigger-positive
      config:
        skill_path: skills/ss-cargo-deps-tidy/SKILL.md
        mode: positive
        threshold: 0.6
  ```

### Re-grading without re-executing the agent

`waza grade` re-runs the graders against a previous run's output
without invoking the model. Useful when iterating on grader config
after a successful (but mis-graded) agent run:

```powershell
waza grade evals/<skill>/eval.yaml `
    --results .\eval-results\<timestamp>\claude-opus-4.6.json `
    --workspace 'C:\Users\Sander\AppData\Local\Temp\waza-<random>' `
    --task <task-id> -o regrade.json -v
```

The agent's per-task workspace must still exist (run the original
with `--keep-workspace` to preserve it). `--workspace` is required
because `waza grade` does not auto-derive it from the results JSON.

### Trigger graders execute the agent too

The `trigger` grader scores the *prompt* heuristically against the skill's
keyword set; it doesn't need the agent to run. Waza, however, still
executes the agent for every task before grading. For trigger tasks this
means each run consumes premium-request credit even though only the prompt
is being scored. Keep trigger prompts short and consider seeding a minimal
fixture so the agent doesn't escape its sandbox while hunting for context.

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
waza --debug run evals/<skill>/eval.yaml --tags <tag> `
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
