```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Set-ReviewAssessmentBatch.ps1" `
  -QueuePath "{{FILE_GROUP_QUEUE}}" -InventoryPath "{{FILE_INVENTORY}}" `
  -FileGroupsPath "{{FILE_GROUPS}}" -ArtifactPath "{{ARTIFACT_PATH}}" `
  -BatchPath "{{BATCH_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `FILE_GROUP_QUEUE` | Worker's private journal path. |
| `FILE_INVENTORY` | Immutable inventory snapshot supplied by the orchestrator. |
| `FILE_GROUPS` | Immutable grouped inventory snapshot supplied by the orchestrator. |
| `ARTIFACT_PATH` | This worker's finding artifact returned by initialization. |
| `BATCH_PATH` | Absolute draft path following the `assessmentBatch` contract in `data-guides/review-coverage.md`, included in the assignment's `workflowData` document. |

Run once after finishing the claimed group and persisting its findings. Copy the
claim's `snapshot` object and exact `groupId` into the draft. Supply assessments
for every file not already assessed in the journal, and the identifiers of all
findings emitted for this group, including findings retained from an interrupted
attempt. Use `findingIds: []` when there are none. Do not repeat automatically
prepared exclusions or change their evidence.
Initial completion rejects every already-recorded path, even if the supplied
assessment is identical. Use `record-file-assessment.md` for assigned repairs.

Success emits `groupId`, `alreadyComplete`, `assessmentCount`, and
`remainingCount`. All assessments and completion share one atomic journal write;
there is no separate group-completion call. Update the checkpoint and request
the next group. An identical retry is read-only, even after another claim.
Partial batches, duplicate or already-recorded paths, conflicting retries, missing findings, and
stale snapshot identities are errors. Repair the draft and retry; never reset
the journal or substitute an error for exhaustion. The command checks persisted
coverage, not the truth of the reviewer's analysis.
