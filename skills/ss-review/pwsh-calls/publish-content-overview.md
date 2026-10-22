```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Publish-ContentOverview.ps1" `
  -ContextPath "{{SESSION_STATE}}/pr-context.json" `
  -OverviewPath "{{SESSION_STATE}}/content-overview.md" `
  -OutputDirectory "{{SESSION_STATE}}" `
  -CommentPrefix "{{COMMENT_PREFIX}}"
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `COMMENT_PREFIX` | Attribution text required by active repository or account instructions, for example `[Copilot speaking]`. Use an empty string or omit `-CommentPrefix` when no prefix is required. |
| `TENANT` | Optional Azure tenant from the invocation or proactor handoff. |

For Azure DevOps, append `-Tenant "{{TENANT}}"` when a tenant was supplied.

The script consumes the full initial capture programmatically. Any agent lookup of its URL or commit metadata must use `query-pr-context.md`; do not open the capture or replace this input with a projection.

Run this once immediately after initial overview generation and validation, before review workers.
It freezes the overview and inspected commits, creates an unlisted Markdown Gist on GitHub or an
ZIP attachment containing one self-contained `report.html` on ADO, verifies it, and posts a separate top-level linking comment. The ADO
thread is created resolved. The comment contains the supplied prefix verbatim, a blank line,
and a link to the overview; without a prefix it contains only the link. It does not submit a review or cast a vote.

The JSON result records `status`, `stage`, `error`, `missingTools`, `snapshot`, `artifact`,
`comment`, `operations`, and an absolute `resultPath`. `succeeded` means both the artifact and
comment were verified. `skipped` means the PR is closed. Every other status is an explicit
publication limitation, not a failed code review.

On failure, notify the user of the failed stage and error, any known remote URL, and the absolute
diagnostic path, then continue the review without asking them to repair publication. An error
before JSON is emitted follows the same non-blocking rule: report the error and continue.
The exception is missing tools: follow `check-report-tools.md` if no installation decision exists
for this run, and retry after approved setup only when no remote write was attempted.

Never blindly repeat a write. `needsReconciliation` indicates an earlier attempt may have created
remote state; keep reviewing and leave investigation to the user. Include the publication outcome
in the final session diagnostics without calling the review itself failed.

The initial report and comment remain unchanged, including when a resumed invocation supplies
a different prefix. Do not rerun this step after an overview is
regenerated for freshness or audit. Resuming a successful run reuses its existing receipt.
Do not upload `audit-findings.md`, audit HTML, reviewer artifacts, or feedback instead of the overview.
