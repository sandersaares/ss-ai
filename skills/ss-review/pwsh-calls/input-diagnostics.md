# Stage 1: record and inspect measured input costs

Prepared assignment, shared-read, and query commands update each worker attempt's private `input-diagnostics.json` atomically. Its record contract is in `data-guides/worker-inputs.md`. Do not share one counter file across concurrent workers. Record a viewer or shell-parser failure that occurred outside those commands explicitly; do not double-count a failure already recorded by a command.

```powershell
pwsh -NoProfile -NonInteractive -File '{{SKILL_ROOT}}\scripts\Get-ReviewInputDiagnostics.ps1' `
  -Path '{{DIAGNOSTICS_PATH}}'
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `DIAGNOSTICS_PATH` | Absolute diagnostics file path, or the current run's session-state directory to aggregate its `input-diagnostics.json` records. Keep measured baseline files outside that directory or under a different filename. |

To record an observed event, add `-Record '{{METRIC}}' -Count {{COUNT}}`.

| Placeholder | Description |
| --- | --- |
| `METRIC` | `firstReadFailures`, `malformedQueryFailures`, `cursorRestarts`, `setupCalls`, `repeatedSetupCalls`, or `documentReadCalls`. |
| `COUNT` | Positive observed count, not an estimate. Defaults to one when omitted. |

On error apply [error recovery](../error-recovery.md); missing diagnostics are not zero measurements. On success retain the validated counts and include them in the workflow's execution diagnostics. Compare baseline and current runs separately, identifying their scopes; do not describe a fixture replay as a live-review performance result. Baseline totals may be recorded from confirmed logs with this same command into a separate file.

`setupCalls` counts assignment/request creation attempts; `repeatedSetupCalls` counts attempts to recreate an existing assignment/request. `documentReadCalls` counts bounded shared-reader calls. `firstReadFailures` counts failed offset-zero shared reads. `malformedQueryFailures` counts failed request preparation after acquiring its output lock. `cursorRestarts` counts explicit saved-query restart attempts. Failures before argument binding and failures from tools outside these commands require the explicit recording route.
