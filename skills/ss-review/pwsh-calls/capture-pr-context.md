```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Get-PullRequestContext.ps1" `
  -Url "{{PR_URL}}" `
  -OutputDirectory "{{SESSION_STATE}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. It resolves to the installed skill directory when the skill is installed and to the repository checkout during development; the installed directory is a link to the source, so both resolve to the same scripts. |
| `PR_URL` | Browser URL of the pull request under review. |
| `SESSION_STATE` | Session state directory for this review. |
| `TENANT` | Optional Azure tenant from the invocation or proactor handoff. |

For Azure DevOps, append `-Tenant "{{TENANT}}"` when a tenant was supplied.

The command writes the full immutable `pr-context.json` and separate `pr-context.json.authorship.json`, refusing existing outputs. It emits only a JSON summary: `platform`, `pullRequestUrl`, `repositoryUrl`, `contextPath`, `fetchedUtc`, `reviewedSourceCommit`, `reviewedMergeBaseCommit`, `threadCount`, `commentCount`, `reviewerCount`, `indeterminateCommentCount`, `reusedClassificationCount`, and `delta`. Without a comparison, no decisions are reused and `delta` is null. No comment-ID list or comment bodies are printed.

On error, apply [error recovery](../error-recovery.md) before continuing. Otherwise retain the path and counts, not a serialized capture. Every agent read of the capture must use `query-pr-context.md`. If `indeterminateCommentCount` is nonzero, use its `comments -Authorship indeterminate` variant and complete `classify-comment-authorship.md` before dispatching review work.
