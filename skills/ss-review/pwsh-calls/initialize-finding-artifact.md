```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/New-FindingArtifact.ps1" `
  -OutputDirectory "{{SESSION_STATE}}/findings" `
  -ReviewerId "{{REVIEWER_ID}}" `
  -FileGroupQueuePath "{{FILE_GROUP_QUEUE}}" `
  -FocusArea "{{FOCUS_AREA}}" {{RESUME_FLAG}}
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `REVIEWER_ID` | Identifier of your focus-area worker, supplied by the orchestrator. |
| `FOCUS_AREA` | Path of the focus area file you are reviewing against. |
| `FILE_GROUP_QUEUE` | This worker's absolute queue/journal path from the manifest, also supplied before first-time initialization. |
| `RESUME_FLAG` | `-Resume` for both first dispatch and replacement: phase initialization prepares the artifact and exclusions. |

The JSON result names the artifact, reports its finding count, and identifies whether it reused existing state with `resumedExisting`. `-Resume` reopens the matching artifact without removing or changing any finding, including findings from partially processed groups. It creates missing initial state, but refuses a missing artifact if the journal already contains a claim or completion. A missing journal despite recorded findings is also an error. Identity mismatches and malformed findings are errors, not reasons to replace the artifact. Stop and report errors. Never use `-Force` for recovery.
