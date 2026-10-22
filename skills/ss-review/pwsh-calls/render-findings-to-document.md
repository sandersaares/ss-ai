```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/ConvertTo-FindingMarkdown.ps1" `
  -Path "{{SESSION_STATE}}/approved-findings.json" `
  -OutputDirectory "{{SESSION_STATE}}/rendered" `
  -CombinedPath "{{SESSION_STATE}}/findings.md"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
