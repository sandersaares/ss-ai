```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Add-FindingDecision.ps1" `
  -OutputDirectory "{{SESSION_STATE}}" `
  -FindingId "{{FINDING_ID}}" `
  -Decision "{{DECISION}}" `
  -Reason "{{REASON}}" `
  -PolicyClassification
```

Records one policy classification. `auto-approved` carries the finding into the approved set;
`needs-audit` advances the classification loop but leaves the finding undecided for publication.
`-PolicyClassification` rejects user verdicts and feedback. The command emits a single JSON object with `decisionsPath` and
`recorded`; verify that `recorded` contains the finding identifier, then poll for the next finding.
On an error, stop and report it to the caller. The primary orchestrator also uses `needs-audit`
when a substantive revision requires renewed user approval; it then returns directly to the audit.

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `FINDING_ID` | Identifier returned by the polling command, or identified by the primary orchestrator as needing renewed user approval. |
| `DECISION` | `auto-approved` or `needs-audit`, as determined by `auto-approval-rules.md`. |
| `REASON` | The matched auto-approval category, or why the finding needs the user's judgment. |
