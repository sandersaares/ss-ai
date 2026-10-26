```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Get-NextAutoApprovalFinding.ps1" `
  -Path "{{SESSION_STATE}}\findings.json" `
  -DecisionsPath "{{SESSION_STATE}}\audit-decisions.json"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |

The command emits a single JSON object with `isComplete`, `remainingCount`, `finding`, and `auditFindingIds`. `finding` is the next full finding with no recorded classification or audit verdict, in artifact order, or null when none remain. `remainingCount` includes the returned finding. The command is read-only: it returns the same finding until a decision is recorded.

`auditFindingIds` lists live findings with no decision or with `needs-audit`, not rejected or dropped findings. After phase 6 verifies `isComplete`, phase 7 uses these identifiers for human presentation. On a direct return to phase 7, unclassified new findings in this list require the user's decision too; do not rerun policy classification.

A missing decision log means nothing has been classified yet. Missing findings or invalid input are errors, not completion. On an error, apply [error recovery](../error-recovery.md) before continuing.
