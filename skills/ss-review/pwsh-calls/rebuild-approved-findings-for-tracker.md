```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/New-ApprovedFindings.ps1" `
  -Path "{{SESSION_STATE}}/findings.json" `
  -DecisionsPath "{{SESSION_STATE}}/audit-decisions.json" `
  -OutputPath "{{SESSION_STATE}}/approved-findings.json"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `CONTEXT_PATH` | Absolute path to the latest checked capture, when reviewing a pull request. |

For a pull request, append `-ContextPath "{{CONTEXT_PATH}}"` to retain the checked evidence binding and follow-up validation. Omit only `-RepositoryPath`: tracker findings do not need anchors in the PR diff. With no pull request, omit both parameters. Inspect the emitted approval diagnostics and stop on validation errors.
