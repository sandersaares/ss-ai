# Audit records

Paths in this guide are relative to the skill root. Audit and merge-approval
envelopes use `schemaVersion: 1`.

Preserve each verbatim feedback fragment in a text file, then use
`pwsh-calls/record-audit-feedback.md`. The tool appends an `auditFeedback` record
containing `responseId`, `findingId`, and `text`. Reuse a response ID only when
retrying the same human response; a new response needs a new ID. The pair of
response and finding IDs is unique. Unknown finding IDs are rejected.
The decision command loads feedback with `-AuditFeedbackPath`, preserving arrival
order without inline shell interpolation. Clarification feedback is not a verdict.

Use `pwsh-calls/record-merge-approval.md` for a user's merge decision. The
`mergeApproval` record contains `pullRequestUrl`, boolean `approveMerge`, and
`approvedSourceCommit`, selected directly from the supplied capture. Publication
takes `-MergeApprovalPath`, validates the record and its PR/commit binding, and
derives the approval flag itself. Omit the path only when there is no applicable
merge decision; do not use omission to recover from a malformed or stale approval.
A false decision continues to mean no approval when commits move; only true
approval requires an exact source-commit match.

Carry-forward takes both old and refreshed captures. It preserves only an
existing true decision bound to the old capture; it never creates an approval
from missing or false state. The orchestrator must first establish that freshness
revalidation leaves `auditFindingIds` empty, after recording any findings that
need renewed audit as required by `SKILL.md`. Commit movement alone does not
require another user decision. A nonempty audit list requires a fresh merge
decision instead of carry-forward.
