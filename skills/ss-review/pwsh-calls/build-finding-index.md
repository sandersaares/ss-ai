```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Get-FindingIndex.ps1" `
  -ReviewerArtifactDirectory "{{SESSION_STATE}}/findings" `
  -OutputPath "{{SESSION_STATE}}/finding-index.json"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
