# Review coverage records

Paths in this guide are relative to the skill root. The `fileGroupJournal`
schema uses version 3 and contains `assessment` records. Every reviewed file
has an `assessment` in its worker's file-group journal:

```json
{
  "groupId": "group-001",
  "path": "C:\\repo\\src\\example.rs",
  "disposition": "excluded",
  "scope": "The focus area's declared scope is published PowerShell scripts.",
  "reason": "This file is a Rust source file."
}
```

Use `reviewed` only after analysis of the requested scope with enough context.
Diffs, targeted reads, and structured-data queries can satisfy this declaration;
whole-file reads are optional. For reviewed files, `scope`
and `reason` may be empty strings. Excluded files require both strings to be
nonblank. The path must match an inventory member exactly, including a deleted
file's pre-change path. Normal processing uses one `assessmentBatch` per claimed
group through `pwsh-calls/record-assessment-batch.md`:

```json
{
  "schemaVersion": 1,
  "snapshot": {
    "inventorySha256": "COPY THE CLAIM'S INVENTORY DIGEST",
    "fileGroupsSha256": "COPY THE CLAIM'S GROUPED INVENTORY DIGEST"
  },
  "groupId": "group-001",
  "assessments": [],
  "findingIds": []
}
```

Replace the illustrative digest strings with the exact uppercase SHA-256 values
from the claim's `snapshot`. `assessments` is an array of the `assessment` records
above. It covers every group file not already assessed in the journal; an empty
array is valid only when all files already have records. `findingIds` contains
all findings emitted for the group, including retained findings from an
interrupted attempt, or `[]` for none. Each identifier must already be durable
in the supplied reviewer artifact. Finding content and persistence follow
`review.md` and `pwsh-calls/append-finding-during-review.md`.

On initial completion, the batch must omit every already-recorded path,
including identical assessments. Only the completed-group retry path accepts
repeated assessments; targeted repairs use the per-file command.

The batch validates inputs once and atomically commits assessments, completion,
and a `batchReceipt` containing the group ID, canonical batch digest, artifact
path, and finding IDs. Identical retries are read-only. Partial batches,
duplicate paths, conflicting retries, or stale inventory identities are errors.
Findings remain in their existing artifacts and are never rewritten by a batch.
The journal's optional `snapshot` and `batchReceipts` fields allow per-file
recovery of unbound journals; batch completion requires a bound snapshot.

`reviewWork` defines the immutable manifest and `reviewAssignment` its entries.
Predispatch preparation binds the repository root, inventory digests, and focus
contents, then expands shared path-exclusion proofs into ordinary assessments.
`dispatch: "excluded"` has a complete, empty artifact and a fully completed
journal but no dispatched worker. `dispatch: "review"` retains work even when
its eventual findings are empty. An open/missing artifact or outstanding claim
is incomplete; it is never completed-empty. A journal's non-null `failure`
records the reported reason for failed work, distinct from ordinary incomplete
work. The orchestrator writes it through `pwsh-calls/record-worker-failure.md`
only after the worker stops. Claims and completion are blocked until validated
resume clears the marker. Null or absent `failure` means no reported failure,
not completion.
The final gate rechecks excluded assignments against the bound focus metadata.

Group completion requires an assessment for every member. Resume and the final
coverage gate validate assessments against the immutable inventory. Use
`pwsh-calls/record-file-assessment.md` for targeted repair; it replaces only the
named file's assessment. Repair can
replace completed files' assessments without resetting the journal. Scripts can
verify recorded coverage and evidence presence, not the truth of an analysis
declaration or scope judgment. Keep freeform recovery notes in Markdown, with a
pointer to the journal rather than duplicated exclusion records.
