```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Remove-Finding.ps1" `
  -ArtifactPath "{{SESSION_STATE}}/findings.json" `
  -PlanPath "{{PLAN_PATH}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `PLAN_PATH` | Versioned `removals` plan built with `write-workflow-data.md`; each item names `findingId` and `reason`. |
