```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Complete-ReviewFileGroup.ps1" `
  -QueuePath "{{FILE_GROUP_QUEUE}}" `
  -GroupId "{{GROUP_ID}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `FILE_GROUP_QUEUE` | Absolute path of this worker's private queue and completion journal. |
| `GROUP_ID` | Exact `groupId` from the currently claimed group. |

This is the legacy per-file recovery path. Normal review uses `record-assessment-batch.md`, which persists assessments and completion together. Run only after every file in the group has either analysis of the requested scope with enough context or a journaled explicit scope exclusion as defined in `review.md`, and all findings have been appended successfully. Diffs, targeted reads, and structured-data queries can satisfy coverage without a whole-file read. Persist every disposition with `record-file-assessment.md` first. The command requires an assessment for every member, then atomically appends `completedGroupIds` and clears `inProgress`. Repeating the same completion after uncertain output is safe and does not clear a later claim.

The JSON result reports `groupId`, `alreadyComplete`, `completedGroupCount`, and `remainingCount`. On success, update the checkpoint and compaction notes and request another group. On error, stop and report it; do not advance or manually edit the journal. The command records your coverage declaration, not proof of sufficient analysis or correct scope exclusion.
