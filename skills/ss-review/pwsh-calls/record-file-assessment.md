```text
ss-ai workflow set-review-assessment --queue-path "{{FILE_GROUP_QUEUE}}" --inventory-path "{{FILE_INVENTORY}}" --file-groups-path "{{FILE_GROUPS}}" --assessment-path "{{ASSESSMENT_PATH}}"
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
