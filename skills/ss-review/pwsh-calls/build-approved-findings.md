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
| `CONTEXT_PATH` | Absolute path to the capture presented for audit, when reviewing a pull request. |
| `REPOSITORY_PATH` | Local review clone holding the commits named by the audited capture. |

For a pull request, supply both `-ContextPath` and `-RepositoryPath` to bind this approved set to the audited evidence and validate anchors against the reviewed diff. Use the existing review clone, including when the audited capture comes from the pre-audit refresh or a return to audit after revalidation. With no pull request, omit both parameters. The command rejects undecided findings and emits approval diagnostics; resolve errors before continuing. Handle any excluded anchors as phase 8 describes. Phase 8 rebuilds against the latest checked capture after final revalidation.
