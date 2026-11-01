```text
ss-ai review publish-final-review --url "{{PR_URL}}" --approved-findings-path "{{SESSION_STATE}}/approved-findings.json" --context-path "{{CONTEXT_PATH}}" --diagnostics-path "{{SESSION_STATE}}/focus-area-cache-diagnostics.json" --repository-path "{{REPOSITORY_PATH}}" --output-directory "{{SESSION_STATE}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `PR_URL` | Browser URL of the pull request under review. |
| `SESSION_STATE` | Session state directory for this review. |
| `CONTEXT_PATH` | Absolute path to the latest checked capture bound to `approved-findings.json`. |
| `REPOSITORY_PATH` | Local checkout of the repository under review. The reviewed commits must be present in it, because the diff the findings are anchored against is computed locally. |
| `TENANT` | Optional Azure tenant from the invocation or proactor handoff. |

For Azure DevOps, append `--tenant "{{TENANT}}"` when a tenant was supplied.

The command accepts `--discard-pending-review`, `--publish-to-closed-pull-request`,
`--merge-approval-path`, `--tenant`, and `--now-utc`.
Use the switches only under the authority described in phase 9.

The command consumes the full refreshed capture programmatically. Any agent inspection of its metadata or discussion uses `query-pr-context.md`; do not open the capture or substitute query results for `--context-path`. Handle the publication status and errors as phase 9 describes, without blindly replaying a write.

The last preflight read checks both live commits and discussion before any remote write. Changed evidence refuses publication; return to phase 8, revalidate the intervening delta and renew human approval when required. Never replace the checked capture just to satisfy this guard.

When a merge-decision record exists, append `--merge-approval-path "{{SESSION_STATE}}/merge-approval.json"`. The command validates the record's schema and binding to the PR and approved source commit before deriving the merge verdict. A true decision submits a GitHub approval or an approving ADO vote. Omit this argument only when no merge decision exists; resolve malformed or stale records rather than bypassing the check.
