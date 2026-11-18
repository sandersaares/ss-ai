```text
ss-ai workflow initialize-review-queue --inventory-path "{{FILE_INVENTORY}}" --file-groups-path "{{FILE_GROUPS}}" --queue-path "{{FILE_GROUP_QUEUE}}" {{RESUME_FLAG}}
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `FILE_INVENTORY` | Absolute path of the immutable inventory snapshot supplied to every worker. |
| `FILE_GROUPS` | Absolute path of the immutable grouped inventory snapshot supplied to every worker. |
| `FILE_GROUP_QUEUE` | This worker's absolute `fileGroupQueuePath` from the manifest. |
| `RESUME_FLAG` | `--resume` for both first dispatch and replacement, preserving phase initialization's exclusions. |

The command verifies the groups against the original inventory and creates a private queue and completion journal in one file. `--resume` preserves existing state, checks its coverage against the immutable grouped inventory, and creates state only if initialization never wrote it. It does not requeue completed groups or discard the current claim.

The JSON result reports `queuePath`, `groupCount`, `fileCount`, `completedGroupCount`, `inProgressGroupId`, `snapshot`, and `remainingCount` (pending groups excluding the current claim). Verify the inventory totals. An in-progress group will be returned by the next claim, even when `remainingCount` is zero. On error, apply [error recovery](../error-recovery.md); do not reset or reconstruct a corrupt journal from conversation history.
