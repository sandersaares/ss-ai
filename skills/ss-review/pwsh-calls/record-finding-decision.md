```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Add-FindingDecision.ps1" `
  -OutputDirectory "{{SESSION_STATE}}" -FindingId "{{FINDING_ID}}" -Decision "{{DECISION}}" `
  -AuditFeedbackPath "{{FEEDBACK_PATH}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `FINDING_ID` | Identifier of the finding the user decided on. |
| `DECISION` | One of `approved`, `rejected` or `dropped`. Policy classification uses `record-auto-approval-decision.md` instead. |
| `FEEDBACK_PATH` | Absolute `audit-feedback.json` path. Omit the argument only if no feedback log exists. |

Success reports `recorded` identifiers. The command validates and joins feedback;
stop on errors rather than writing a decision without its recorded human feedback.
