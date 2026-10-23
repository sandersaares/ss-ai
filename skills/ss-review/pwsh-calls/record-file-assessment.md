```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Set-ReviewFileAssessment.ps1" `
  -QueuePath "{{FILE_GROUP_QUEUE}}" -InventoryPath "{{FILE_INVENTORY}}" `
  -FileGroupsPath "{{FILE_GROUPS}}" -AssessmentPath "{{ASSESSMENT_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `FILE_GROUP_QUEUE` | Worker's private journal path. |
| `FILE_INVENTORY` | Immutable inventory path supplied by the orchestrator. |
| `FILE_GROUPS` | Immutable grouped inventory path. |
| `ASSESSMENT_PATH` | Absolute draft path following the `assessment` contract in `data-guides/review-coverage.md`, included in the assignment's `workflowData` document. |

Use this command for explicit targeted repair, not a loop over normal review
files. Normal group completion uses `record-assessment-batch.md`.

Success emits `status: "Recorded"`, `path`, and `disposition`. On failure, repair
the assessment and retry before completing the group. Do not alter the journal.
