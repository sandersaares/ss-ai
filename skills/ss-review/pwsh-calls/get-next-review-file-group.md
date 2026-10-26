```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Get-NextReviewItem.ps1" `
  -QueuePath "{{FILE_GROUP_QUEUE}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `FILE_GROUP_QUEUE` | Absolute path of this worker's private group queue. |

The JSON result has `exhausted`, `item`, and `remainingCount`. A non-exhausted result also carries `snapshot`; copy it unchanged into the assessment batch. When `exhausted` is false, `item` is a group carrying `groupId` and a `files` array of inventory records (`path`, `sizeBytes`, and `change`). The queue atomically retains it as `inProgress` before returning; subsequent calls return that same group until `record-assessment-batch.md` journals assessments and completion together. This includes recovery after the claim's output was lost. A zero `remainingCount` does not mean the returned group is already reviewed.

Assess every file's scope as defined in `review.md`, analyze the requested scope in every non-excluded file using its diff-first and targeted-inspection policy, persist findings and per-file exclusion reasons, and journal completion before requesting the next group. Preserve earlier findings and exclusion records if replaying an unfinished group; later deduplication handles overlapping findings. Only `exhausted: true` with `item: null` means no pending or in-progress groups remain. On error, apply [error recovery](../error-recovery.md); missing, corrupt, or locked state is not exhaustion.
