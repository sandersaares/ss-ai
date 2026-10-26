Use the common rules in `workflow-data.md` and the merge-approval contract in
`data-guides/audit.md`. After a direct human decision:

```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Set-ReviewMergeApproval.ps1" `
  -OutputDirectory "{{SESSION_STATE}}" -ContextPath "{{CONTEXT_PATH}}" -Decision "{{DECISION}}"
```

When freshness revalidation leaves `auditFindingIds` empty and the source commit
moved, carry an existing approval without asking the user again. Record all
findings needing renewed audit before checking that list. A nonempty list
requires a fresh human merge decision instead.

```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Set-ReviewMergeApproval.ps1" `
  -OutputDirectory "{{SESSION_STATE}}" -ContextPath "{{CONTEXT_PATH}}" `
  -CarryForward -PreviousContextPath "{{PREVIOUS_CONTEXT_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `SESSION_STATE` | Absolute session-state directory. |
| `CONTEXT_PATH` | Capture being audited, or refreshed capture for carry-forward. |
| `DECISION` | `Approve` for an explicit merge approval; `DoNotApprove` for findings-only approval. |
| `PREVIOUS_CONTEXT_PATH` | Capture to which the existing decision is bound. |

Success emits `status: "Recorded"` or `NoApproval`, plus `sourceCommit`.
`NoApproval` means carry-forward found no true approval; preserve the absence of
approval rather than asking again solely because the PR moved. If the source
commit is unchanged and no new audit is required, leave the existing record
unchanged. On error apply [error recovery](../error-recovery.md) and resolve the inconsistent state; do not supply a
publication flag manually.
