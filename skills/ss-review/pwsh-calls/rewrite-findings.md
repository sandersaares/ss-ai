```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Update-Finding.ps1" `
  -ArtifactPath "{{SESSION_STATE}}/findings.json" `
  -PlanPath "{{PLAN_PATH}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `PLAN_PATH` | Versioned `updates` plan built with `write-workflow-data.md`; each item names `findingId` and a rewritten `findingPath`. |

Each `findingPath` names a complete UTF-8 finding JSON file following `review.md`, "Finding structure". Preserve multiline Markdown using `\n` for line feeds and `\n\n` for paragraph breaks; omit the tooling-owned fields even when copying a stored finding. Keep content in files rather than command-line arguments or encoded payloads.

The command emits one JSON object with `artifactPath` and the `updated` identifiers. On error, apply [error recovery](../error-recovery.md); a rejected batch leaves the artifact unchanged.
