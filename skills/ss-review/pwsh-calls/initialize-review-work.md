```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\New-ReviewWork.ps1" `
  -InventoryPath "{{SESSION_STATE}}\file-inventory.json" `
  -FileGroupsPath "{{SESSION_STATE}}\file-inventory-grouped.json" `
  -FocusAreaDirectory "{{SESSION_STATE}}\focus-areas" `
  -RepositoryRoot "{{REPOSITORY_ROOT}}" -ArtifactDirectory "{{SESSION_STATE}}\findings" `
  -OutputDirectory "{{SESSION_STATE}}\review-work"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `REPOSITORY_ROOT` | Absolute root of the repository under review, used for declared repository-relative path scopes. |

The JSON result names `manifestPath`, the snapshotted `inventoryPath` and `fileGroupsPath`, and `focusAreaQueuePath`, and reports file, group, reviewer, `dispatchCount`, and `excludedReviewerCount` counts. Read the manifest's `reviewers` array as the complete expected coverage. It assigns each focus area a `reviewerId`, `focusArea` path, private `fileGroupQueuePath`, `artifactPath`, and `dispatch` (`review` or `excluded`). Pass these paths to workers rather than constructing or editing queue state manually.

The command initializes artifacts and journals, expanding provable path exclusions into ordinary per-file records. Entirely excluded groups are already journaled; entirely excluded focus areas have validated complete, empty artifacts and are absent from the dispatch queue. Missing path metadata, semantic conditions, and uncertainty retain review work. Set `RESUME_FLAG` to `-Resume` even for first dispatch to preserve prepared state. Never remove excluded assignments from the manifest.

Run at the start of phase 2. Existing output, empty inputs, malformed inventory, or groups that omit, duplicate, alter, or exceed the allowed bounds produce an error. Group validation runs before creating the output directory: if that validation fails and the directory does not exist, regenerate the groups with the reported errors and retry this command. Stop and report other errors. Resume active work using the existing manifest and queues, not by initializing them again. If initialization left a partial directory without a manifest, report it and do not dispatch workers from it.
