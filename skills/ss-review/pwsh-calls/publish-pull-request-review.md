```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Publish-PullRequestReview.ps1" `
  -Url "{{PR_URL}}" `
  -ApprovedFindingsPath "{{SESSION_STATE}}/approved-findings.json" `
  -ContextPath "{{CONTEXT_PATH}}" `
  -DiagnosticsPath "{{SESSION_STATE}}/focus-area-cache-diagnostics.json" `
  -RepositoryPath "{{REPOSITORY_PATH}}" `
  -OutputDirectory "{{SESSION_STATE}}"
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

For Azure DevOps, append `-Tenant "{{TENANT}}"` when a tenant was supplied.

The script consumes the full refreshed capture programmatically. Any agent inspection of its metadata or discussion uses `query-pr-context.md`; do not open the capture or substitute query results for `-ContextPath`. Handle the publication status and errors as phase 9 describes, without blindly replaying a write.

The last preflight read checks both live commits and discussion before any remote write. Changed evidence refuses publication; return to phase 8, revalidate the intervening delta and renew human approval when required. Never replace the checked capture just to satisfy this guard.

When a merge-decision record exists, append `-MergeApprovalPath "{{SESSION_STATE}}\merge-approval.json"`. The command validates the record's schema and binding to the PR and approved source commit before deriving the merge verdict. A true decision submits a GitHub approval or an approving ADO vote. Omit this argument only when no merge decision exists; resolve malformed or stale records rather than bypassing the check.
