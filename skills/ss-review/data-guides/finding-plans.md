# Finding plans

Paths in this guide are relative to the skill root. Plan envelopes use
`schemaVersion: 1`.

`Get-FindingIndex.ps1` produces the `findingIndex` schema: `schemaVersion: 1` and
a required `findings` array of `findingIndexEntry` records. Every entry has a
unique `id`, `reviewerId`, `focusArea`, `title`, `description`, `recommendedAction`,
`anchorPath`, and `impactedPaths` array. Only `followUpThreadId` is optional.
An explicit empty findings array is valid. Both generation and candidate
validation enforce the schema, including when there are no candidate clusters.

Use `pwsh-calls/write-workflow-data.md` to initialize and append records:

| Kind | Array field | Item definition |
| --- | --- | --- |
| `mergeCandidates` | `clusters` | `cluster`: `findingIds` array with distinct identifiers. |
| `mergePlan` | `groups` | `merge`: `sourceFindingIds` and `findingPath`. |
| `updates` | `updates` | `update`: `findingId` and `findingPath`. |
| `removals` | `removals` | `removal`: `findingId` and `reason`. |
| `decisions` | `decisions` | `decision`: `findingId`, `decision`, optional `reason` and `feedback`. |

Candidate clusters and merge groups need at least two distinct IDs. An explicit
empty candidate or merge plan is valid; a missing named plan or missing array is
not. Update, removal, and decision consumers require a nonempty plan. Finding
paths are resolved relative to the item draft when appended and stored absolute.
Overlapping candidate clusters are permitted hypotheses; actual merge groups
must not share findings. Before adjudication, execute
`pwsh-calls/validate-finding-candidates.md` to check IDs against the generated index.

Writers wait through temporary sibling-lock contention and publish complete
records atomically. Exact append retries do not
duplicate records; conflicting repeated finding IDs are rejected. A plan is
owned by one orchestrator; workers submit typed item drafts, not edits to the
shared file. Correct accepted items with the writer's explicit `-ReplaceIndex`;
it preserves array order and validates the whole resulting document. Finding
content itself follows `review.md` and is persisted by the existing finding commands.
