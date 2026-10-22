```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/New-ConsolidatedFindings.ps1" `
  -ReviewerArtifactDirectory "{{SESSION_STATE}}/findings" `
  -MergePlanPath "{{SESSION_STATE}}/merge-plan.json" `
  -OutputPath "{{SESSION_STATE}}/findings.json"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
