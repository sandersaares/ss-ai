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
outcome of each task - not on terminal echo.

## Runs can silently hang

The agent sometimes stops emitting events while waza keeps waiting
(no API-side timeout). If a long task's per-task temp workspace
(`%TEMP%\waza-<random>\`) shows no file modifications for many
minutes, the run is almost certainly dead - kill the terminal and
restart.
