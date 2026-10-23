# Comment authorship

Paths in this guide are relative to the skill root.

`schemas/comment-authorship.schema.json` defines each capture's semantic sidecar.
`Get-PullRequestContext.ps1` creates it, `Set-CommentAuthorship.ps1` records
validated decision batches, and all capture readers validate and consume it.
Use `pwsh-calls/classify-comment-authorship.md`; never manually edit the sidecar.
The required fields are `schemaVersion`, `policyVersion`, `captureHash`, and
`decisions` (an explicit array, empty when nothing is classified). Decisions
carry nonempty `threadId`, `commentId`, `inputHash`, `kind` (`agent` or `human`),
and `origin` (`classified` or `reused`). Hashes are uppercase SHA-256 hex.
Identifiers are opaque and case-sensitive. Completion is derived from the
remaining indeterminate comments, not an agent-supplied completion flag.

Capture classifies a GitHub review containing only a vote (no body text or inline
comments) as human unless its account establishes agent authorship. This includes
whitespace-only bodies and dismissed votes. Such reviews do not need semantic
classification or a request for human guidance.

The classification draft is an array of objects with only `threadId`,
`commentId`, and `kind`. Every field is required; an empty array explicitly
records no new decisions. Submit the query's `snapshotId` separately with the
batch so a changed reading snapshot cannot accept stale judgments.
