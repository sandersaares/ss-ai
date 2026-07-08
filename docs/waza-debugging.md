# Debugging Waza runs

## Re-grade without re-executing the agent

`waza grade` re-runs the graders against a previous run's output
without invoking the model. Useful when iterating on grader config
after a successful (but mis-graded) agent run:

```powershell
waza grade evals/<skill>/<lane>.eval.yaml `
    --results .\eval-results\<timestamp>\claude-opus-4.8.json `
    --workspace 'C:\Users\Sander\AppData\Local\Temp\waza-<random>' `
    --task <task-id> -o regrade.json -v
```

The agent's per-task workspace must still exist (run the original
with `--keep-workspace` to preserve it). `--workspace` is required
because `waza grade` does not auto-derive it from the results JSON.

## Watch a run live

`waza run -v` only prints the prompt and a one-line status. For
event-by-event visibility (tool calls, reasoning, permissions,
turns), use the **global** `--debug` flag and redirect to a file you
can tail:

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

## Log surfaces

| File | What it contains | Best use |
|------|------------------|----------|
| `stdout.log` (from `--debug`) | Verbose event stream incl. tool args + results | Live tail to see what the agent is doing |
| `<timestamp>-session.jsonl` (from `--session-log`) | Sparse milestones (session/task start, task complete) | Programmatic progress polling |
| `<task>-transcript.json` (from `--transcript-dir`) | Full transcript per task on completion | Post-mortem debugging |

## Monitor the temp workspace

Watching file activity in the per-task temp workspace also helps
confirm forward progress:

```powershell
$tmp = (Get-ChildItem $env:TEMP -Directory | Where-Object Name -match 'waza-' |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
Get-ChildItem $tmp -Recurse -File |
    Group-Object { $_.LastWriteTime.ToString('HH:mm') } |
    Sort-Object Name | Format-Table Name, Count -AutoSize
```
