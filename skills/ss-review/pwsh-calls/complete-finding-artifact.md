```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Complete-FindingArtifact.ps1" `
  -ArtifactPath "{{SESSION_STATE}}/findings/{{REVIEWER_ID}}.json" `
  -FileGroupQueuePath "{{FILE_GROUP_QUEUE}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `REVIEWER_ID` | Identifier of your focus-area worker. |
| `FILE_GROUP_QUEUE` | Absolute path of your private group queue. |

Call only after every file has analysis of the requested scope with enough context or a journaled explicit scope exclusion as defined in `review.md`, and every group has a completion journal entry. Diffs, targeted reads, and structured-data queries can satisfy coverage without a whole-file read. Complete any targeted coverage repair assigned by the orchestrator too. The command reports the artifact path, finding count, and completion status as JSON. It refuses pending or in-progress groups but cannot prove sufficient analysis or correct scope judgments. The final coverage gate validates all file assessments against the immutable inventory. On error, leave the artifact open and report it.
