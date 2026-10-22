```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Set-ReviewWorkerFailure.ps1" `
  -QueuePath "{{FILE_GROUP_QUEUE}}" -Reason "{{FAILURE_REASON}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `FILE_GROUP_QUEUE` | Failed worker's private journal from the manifest. |
| `FAILURE_REASON` | Concrete failure reported by the worker or runtime; quote as a PowerShell string. |

The orchestrator runs this after confirming that a failed worker has stopped.
It preserves findings, claims, assessments, and completed groups. Identical
retries do not rewrite the journal. Success emits `status: "Failed"`, `queuePath`,
and `reason`. Replacement dispatch uses the ordinary resume commands; validated
resume clears the failure marker, but never the retained work.

On a missing or malformed journal, stop and report the inconsistent state.
Do not create a successful empty result or reset state to record the failure.
