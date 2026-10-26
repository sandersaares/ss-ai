```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Get-PullRequestContext.ps1" `
  -Url "{{PR_URL}}" `
  -OutputDirectory "{{SESSION_STATE}}" `
  -OutputPrefix "{{OUTPUT_PREFIX}}" `
  -ComparePath "{{BASELINE_PATH}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. It resolves to the installed skill directory when the skill is installed and to the repository checkout during development; the installed directory is a link to the source, so both resolve to the same scripts. |
| `PR_URL` | Browser URL of the pull request under review. |
| `SESSION_STATE` | Session state directory for this review. |
| `OUTPUT_PREFIX` | New prefix for this pass, such as `audit-` before policy classification or `publish-1-` after audit. Never reuse a capture path. |
| `BASELINE_PATH` | Absolute path to the last checked capture, initially `pr-context.json`. Preserve both it and the original capture. |
| `TENANT` | Optional Azure tenant from the invocation or proactor handoff. |

For Azure DevOps, append `-Tenant "{{TENANT}}"` when a tenant was supplied.

The command preserves existing captures and writes a new prefixed `pr-context.json` plus its `.authorship.json` sidecar. Stdout contains the scalar fields and counts described in `capture-pr-context.md`, including `indeterminateCommentCount` and `reusedClassificationCount`, plus a fixed-size `delta` summary:

| Properties | Meaning |
| --- | --- |
| `threadsAddedCount`, `threadsRemovedCount` | Counts of added and removed threads. |
| `commentsAddedCount`, `commentsRemovedCount`, `commentsEditedCount` | Counts of comment changes. |
| `resolutionChangedCount` | Count of threads whose resolution changed. |
| `hasChanges` | Whether discussion or either reviewed commit changed. |
| `evidenceChanged` | Whether review evidence changed, including exact bodies, attribution, PR intent, and thread context even when provider edit timestamps are unchanged. |
| `reviewedSourceCommitChanged`, `previousReviewedSourceCommit`, `reviewedSourceCommit` | Source movement flag and compared commits. |
| `reviewedMergeBaseCommitChanged`, `previousReviewedMergeBaseCommit`, `reviewedMergeBaseCommit` | Merge-base movement flag and compared commits. |

The summary has no changed-ID or indeterminate-comment lists. On error, apply [error recovery](../error-recovery.md) before continuing. Otherwise classify any indeterminate comments with `classify-comment-authorship.md`, using the refreshed path, before beginning a cursor-based freshness pass.

Use `query-pr-context.md` for every agent read: `changes` with `BASELINE_PATH` as `-ComparePath` and the returned `contextPath` as `-Path` discovers the intervening delta in bounded pages. Targeted metadata or conversation queries provide evidence; the `reviewEvidence` entry also requires checking relevant metadata and thread context. Exhaust delta discovery in bounded batches, including removals and resolution changes; do not dump either capture or concatenate the pages into a replacement capture. Advance the checked capture only after revalidation, and preserve the audit capture across a human wait.
