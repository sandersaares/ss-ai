```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Save-FocusAreaCache.ps1" `
  -SessionStateDirectory "{{SESSION_STATE}}" `
  -InputDirectory "{{SESSION_STATE}}/instruction-set-source"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `SESSION_STATE` | Absolute session-state directory with a finalized expansion manifest. |

The command derives completeness from the manifest. Stored/Existing means a
complete set is cached; Bypassed reports why storage was skipped. On validation
failure, repair the manifest or source preparation before retrying.
