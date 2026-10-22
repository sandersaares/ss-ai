# Stage 1: group the complete inventory

Read `file-inventory.json` and group its files by directory structure into related groups of no more than 20 files or 100 000 bytes of content, whichever limit is reached first. A single-file group may exceed the byte limit. These bounds leave room for focus-area guidance and supporting context while bounding the work repeated if a worker is interrupted.

Keep related files together and combine small sibling directories where practical rather than defaulting to a group per file or directory. Prefer fewer bounded groups unless their supporting context calls for smaller groups; every group requires its own claim and completion record.

Preserve every inventory record exactly, including its absolute `path`, `sizeBytes`, and `change`. Include every file exactly once across the groups, including deleted files. Do not filter by predicted relevance or inspect only small files. Split groups further when their supporting context would otherwise be too large, but do not split or omit a file's content.

# Stage 2: write the grouped inventory

Write a grouping draft as a JSON array following the `fileGroups` contract from `data-guides/file-groups.md`, included in your assignment's `workflowData` document, then publish it as `file-inventory-grouped.json` with `pwsh-calls/write-review-input.md`. Use sequential identifiers such as `group-001`.

Example shape:

```json
[
  {
    "groupId": "group-001",
    "files": [
      { "path": "C:\\repo\\src\\example.rs", "sizeBytes": 123, "change": "modified" }
    ]
  }
]
```

No commentary or other metadata is needed in the JSON. Return its path and the group count to the orchestrator. Phase initialization validates exact inventory coverage, unchanged metadata, and group bounds before any review worker runs.
