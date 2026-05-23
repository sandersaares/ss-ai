# Waza caveats observed in practice

## Agent reads from outside the per-task workspace

If the per-task temp workspace looks empty, the agent will sometimes
look outside it. For prompts that reference "this Rust workspace"
without any seeded files, the agent has been observed to read files
from `C:\Source\folo` directly. It has not modified those files, but
the wasted compute is real. Seed a minimal fixture to keep the agent
in-sandbox.

## PowerShell terminal output is compressed and buffered

Terminal output in PowerShell often gets compressed mid-line, and
`Tee-Object` only flushes after the run finishes. Rely on the JSON
result file under `eval-results\<timestamp>\` for the authoritative
outcome of each task — not on terminal echo.

## Runs can silently hang

The agent sometimes stops emitting events while waza keeps waiting
(no API-side timeout). If a long task's per-task temp workspace
(`%TEMP%\waza-<random>\`) shows no file modifications for many
minutes, the run is almost certainly dead — kill the terminal and
restart.

## Sub-agent permission deadlock (upstream copilot-cli bug)

Waza installs an `allowAllTools` permission handler on the parent
session (`internal/execution/copilot.go`). When the agent invokes
the built-in `task` tool to spawn a sub-agent, the sub-agent's tool
calls arrive at the SDK with a child session ID that the SDK's
session map does not know — so the handler is never called and
`permission.requested` events accumulate without matching
`permission.completed` events. The run hangs indefinitely.

- Tracked in [copilot-sdk#947 / #959](https://github.com/github/copilot-sdk/issues/947)
  ("Subagents don't inherit session-defined external tools").
- Runtime fix landed in **copilot-cli v1.0.49-0** ("Hooks ... now
  fire correctly for sub-agent tool calls", 2026-05-13).
- Waza 0.31.0 still bundles **copilot_1.0.2** (see
  `internal/embedded/zcopilot_*.zst`), which predates the fix. There
  is no override env var — the embedded loader hard-locks the
  version.
- Until microsoft/waza ships a release with an updated bundled CLI,
  any task whose agent invokes the `task` tool will hang.

**Diagnostic:** in a `--debug` log, count `permission.requested` vs
`permission.completed` — if the gap grows monotonically while
`subagent.started` has fired, it is this bug.
