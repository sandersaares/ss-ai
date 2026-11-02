Write the required attribution to a UTF-8 file in session state, including an
empty file when no prefix is required. Then run:

```text
ss-ai review publish-overview --context-path "{{SESSION_STATE}}/pr-context.json" --overview-path "{{SESSION_STATE}}/content-overview.md" --output-directory "{{SESSION_STATE}}" --skill-root "{{SKILL_ROOT}}" --comment-prefix-path "{{COMMENT_PREFIX_PATH}}"
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `COMMENT_PREFIX_PATH` | Absolute path to the file containing attribution text required by active repository or account instructions, for example `[Copilot speaking]`. |
| `TENANT` | Optional Azure tenant from the invocation or proactor handoff. |

For Azure DevOps, append `--tenant "{{TENANT}}"` when a tenant was supplied.

The command consumes the full initial capture programmatically. Any agent lookup of its URL or commit metadata must use `query-pr-context.md`; do not open the capture or replace this input with a projection.

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
