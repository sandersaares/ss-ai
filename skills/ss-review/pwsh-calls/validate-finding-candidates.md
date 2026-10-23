```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Test-FindingCandidates.ps1" `
  -Path "{{SESSION_STATE}}\merge-candidates.json" -IndexPath "{{SESSION_STATE}}\finding-index.json"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `SESSION_STATE` | Absolute session-state directory. |

Success emits `status: "Valid"` and `clusterCount`. Repair candidate drafts and
rebuild the plan if validation fails; do not dispatch adjudicators on invalid IDs.
