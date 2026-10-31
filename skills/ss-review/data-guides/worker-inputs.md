# Worker inputs and saved queries

Paths in this guide are relative to the skill root.

`workerAssignment` binds a bounded phase, its resume flag, optional canonical
capture path (null when absent), diagnostics path, and `sharedDocument` records.
Document IDs name the rendered prompt, scope, focus area, overview, references,
workflow-data guidance, or query recipe. Each record has an absolute path, byte
hash, UTF-8 byte count, and line count (including a terminal empty line).
Assignment, saved-query, and diagnostics envelopes use `schemaVersion: 1`.

The assignment builder supplies `workflowData` only to phases that produce
structured drafts. Its document contains `workflow-data.md` followed by the
phase's canonical data guide, with LF line endings and no summaries or unrelated
guides. References and overview workers produce prose and receive no
`workflowData` document.
The prepared reader returns `sharedReadPage` slices with UTF-16 code-unit
offsets and lengths. `complete: true` requires `nextOffset: null`; otherwise
the next offset continues immediately after the returned text. Only assigned
Markdown documents belong to shared reading; PR captures never do. Use
`pwsh-calls/prepare-worker-assignment.md` for creation and consumption.

`queryRequest` stores absolute capture paths, canonical snapshot hashes,
normalized `queryOptions`, their combined `requestId`, a diagnostics path, and
`lastPage` and `lastPageCursor`. Comparison path and baseline identity are null
outside a changes query. Both page fields are null before any page is delivered.
A stored `queryPage` has explicit `complete` and nullable `nextCursor`;
`lastPageCursor` is its starting cursor, empty for the first page.
Before replay or continuation, the reader regenerates that page from the bound
capture and selectors and requires exact agreement, including item structure,
content, and continuation. The default read replays the validated page;
`-Next` advances, and `-Restart` explicitly rebinds snapshots.
Use `pwsh-calls/query-pr-context.md`, whose PowerShell entry points call the
native context commands. Do not edit these records or substitute their paths
for canonical capture paths at validation/publication boundaries.

`inputDiagnostics` contains required nonnegative counters, including explicit
zeros. The prepared writers serialize observations under a lock, including
concurrent reads within one assignment. Temporary diagnostic lock contention
uses bounded retries; persistent contention and other I/O failures remain
errors. The reporting command also validates the record. Use
`pwsh-calls/input-diagnostics.md` to record externally observed failures and
compare separately stored measured baseline and current-run totals.
