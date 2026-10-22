# Inventory and file-group data

Paths in this guide are relative to the skill root.

The `inventory` schema is a nonempty array of records containing exactly `path`,
`sizeBytes`, and `change`. Paths are absolute; sizes are nonnegative integers.
Changes are `new`, `modified`, or `deleted`. Deleted files retain their
pre-change size and path; renames have a deleted old entry and a new entry.

The `fileGroups` schema is a nonempty array of objects containing exactly
`groupId` and `files`. Group identifiers are unique, nonempty strings of letters,
digits, dashes, and underscores. Each `files` array is nonempty and contains
original inventory records, preserving their paths, sizes, and change kinds.
Both schemas are arrays, without a versioned object envelope.

Publish inventory and grouping drafts through `pwsh-calls/write-review-input.md`
before consumers run. That command checks exact coverage, unchanged metadata and
group bounds, writes atomically, and refuses accidental replacement. Selection
and grouping remain agent judgments; assembling or validating their JSON does not.

Include every inventory entry exactly once across groups. Use the grouping
bounds and directory-based selection rules in `prepare-file-groups.md`; do not
filter the inventory by predicted review relevance.
