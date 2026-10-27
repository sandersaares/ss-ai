# Self-review delivery

Use this procedure only after self-review completes phase 6. It delivers analysis to the local caller, not an approved review for publication.

# Stage 1: verify classification

Use the phase 6 result from `pwsh-calls/get-next-auto-approval-finding.md` and require `isComplete: true`. Retain `auditFindingIds` as the findings whose audit is skipped. Resume incomplete classification through phase 6; never infer that a missing decision means approval.

Keep those findings and their `needs-audit` records unchanged. Do not build an approved set: it intentionally cannot include undecided findings, whereas this delivery must include them.

# Stage 2: render local findings

Execute the command in [pwsh-calls/render-self-review.md](pwsh-calls/render-self-review.md). It renders the complete consolidated artifact, including findings requiring audit, without consulting approval state or applying PR publication-anchor filtering.

On failure, apply `error-recovery.md` without asking questions or performing remote writes. Report an unrecoverable blocker and the paths of any available artifacts; do not claim successful delivery.

# Stage 3: deliver and finish

Use the renderer's emitted `findings` entries as the authoritative mapping of finding identifiers to absolute Markdown filesystem paths. Verify that every current finding has an emitted file, and that each file exists and contains its rendered finding. Do not enumerate stale files from earlier runs.

Deliver those paths directly in the current session, grouped as **Policy-approved findings** and **Audit skipped (not approved)** according to `auditFindingIds`. Include each finding identifier with its path. The second group is informational: the caller may choose to act on it, but the workflow has neither audited nor approved it. Do not omit that group or present it as rejected.

Report the absolute `combinedPath` as the complete Markdown report, explicitly noting that it also contains any unaudited findings. Include the local content-overview path. When there are no findings, deliver the empty combined report and explicitly state that the review found none; when all findings require audit, state that none were policy-approved rather than calling the review clean.

Include concise diagnostics: self-review mode, reviewed scope, policy-approved count, skipped-audit count, and that no audit or remote writes occurred. Retain applicable phase 5 freshness diagnostics. Finish here without merge approval, publication, or learning.
