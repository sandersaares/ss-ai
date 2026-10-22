```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Add-Finding.ps1" `
  -ArtifactPath "{{SESSION_STATE}}/findings/{{REVIEWER_ID}}.json" `
  -FindingPath "{{FINDING_PATH}}" `
  -ContextPath "{{SESSION_STATE}}/pr-context.json"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `REVIEWER_ID` | Identifier of your focus-area worker. |
| `FINDING_PATH` | Absolute path of the UTF-8 JSON file you just wrote under your assigned staging directory. Follow `review.md`, "Finding structure", including multiline Markdown encoded with `\n` and `\n\n`. |

Pass the file path, not its contents or an encoded payload. The command emits one JSON object with `findingId` and `artifactPath`; retain the assigned identifier for the group journal. On a validation error, correct the finding file and retry before continuing.

Drop the `-ContextPath` argument if there is no `pr-context.json`. Supply it whenever there is one, so that a mistyped follow-up target fails now, while you can still correct it.

This script validates against the full capture programmatically. To inspect a prospective follow-up yourself, use `query-pr-context.md`, not a direct capture read; pass the canonical path unchanged to this command.
