```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/New-ApprovedFindings.ps1" `
  -Path "{{SESSION_STATE}}/findings.json" `
  -DecisionsPath "{{SESSION_STATE}}/audit-decisions.json" `
  -OutputPath "{{SESSION_STATE}}/approved-findings.json" `
  -ContextPath "{{CONTEXT_PATH}}" `
  -RepositoryPath "{{REPOSITORY_PATH}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `CONTEXT_PATH` | Absolute path to the latest capture whose evidence has completed revalidation. |
| `REPOSITORY_PATH` | Local clone holding the reviewed commits. |

Keep `-ContextPath` pointed at the full refreshed capture so commit, anchor, and follow-up validation use complete evidence. Any agent investigation of a reported target or metadata value uses `query-pr-context.md`, never a direct capture read or a query result substituted for this input. Handle the validation summary and any excluded anchors as phase 8 describes; report errors before continuing to publication.
