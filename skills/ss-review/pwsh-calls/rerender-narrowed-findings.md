```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/ConvertTo-FindingMarkdown.ps1" `
  -Path "{{SESSION_STATE}}/findings.json" `
  -OutputDirectory "{{SESSION_STATE}}/rendered" `
  -FindingId "{{FINDING_IDS}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `FINDING_IDS` | Comma-separated identifiers of the findings to re-render, as one argument. Naming them as separate arguments binds the second one to another parameter instead, which re-renders only the first finding and reports no error. |
