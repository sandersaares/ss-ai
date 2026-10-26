```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/ConvertTo-FindingMarkdown.ps1" `
  -Path "{{SESSION_STATE}}/approved-findings.json" `
  -OutputDirectory "{{SESSION_STATE}}/rendered" `
  -Destination "tracker" `
  -ContextPath "{{CONTEXT_PATH}}"
```

Omit `-ContextPath` when the review has no captured pull request.

Placeholders to replace:

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `CONTEXT_PATH` | Absolute path to the latest checked capture, when reviewing a pull request. |

The renderer consumes the full capture programmatically; agent inspection of its metadata or discussion must use `query-pr-context.md` instead. Do not replace `-ContextPath` with a query projection. Check the emitted JSON for the rendered outputs and use them for the destination-change handling in phase 9. On an error, apply [error recovery](../error-recovery.md) before publication.
