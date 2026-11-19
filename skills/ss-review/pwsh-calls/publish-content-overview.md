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
It freezes the overview and inspected commits, then posts the Markdown directly as GitHub
top-level pull request comments or resolved, unanchored ADO comments. Each UTF-8 comment body
is at most 64 KiB. A larger overview is divided into consecutive parts with part labels and
attribution on every body. It does not submit a review or cast a vote.

The JSON result records `status`, `stage`, `error`, `snapshot`, `comments`, `operations`, and an
absolute `resultPath`. Each operation identifies its `part` and `partCount`; `comments` retains
the matching receipts in that order.
`succeeded` means every comment was verified. `skipped` means the PR is closed. Every other status is an explicit
publication limitation, not a failed code review.

On failure, notify the user of the failed stage and error, any known remote URL, and the absolute
diagnostic path, then continue the review without asking them to repair publication. An error
before JSON is emitted follows the same non-blocking rule: report the error and continue.

Never blindly repeat a write. `needsReconciliation` indicates an earlier attempt may have created
remote state; keep reviewing and leave investigation to the user. Include the publication outcome
in the final session diagnostics without calling the review itself failed.

The initial report and comments remain unchanged, including when a resumed invocation supplies
a different prefix. Do not rerun this step after an overview is
regenerated for freshness or audit. Resuming a successful run reuses its existing receipts.
Do not publish `audit-findings.md`, audit HTML, reviewer artifacts, or feedback instead of the overview.
