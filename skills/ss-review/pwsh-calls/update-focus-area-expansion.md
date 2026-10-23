```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Update-FocusAreaExpansion.ps1" `
  -SessionStateDirectory "{{SESSION_STATE}}" -Operation "{{OPERATION}}" `
  -InputPath "{{INPUT_PATH}}" -ReferenceId "{{REFERENCE_ID}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `SESSION_STATE` | Absolute session-state directory containing both source roots. |
| `OPERATION` | `Initialize`, `Register`, `Complete`, `Fail`, `Read`, or `Finalize`. |
| `INPUT_PATH` | Absolute `referenceInput` draft for Register or `referenceResult` draft for Complete/Fail; otherwise omit this argument. |
| `REFERENCE_ID` | Reference to query with Read; omit for other operations or an aggregate-only Read. |

Read the result shape defined in `data-guides/focus-areas.md`. Pass queried contexts to
categorization workers. On failure, correct the draft or unresolved reference
before continuing; never replace the manifest manually. Only the preparation
agent writes the manifest; expansion workers return drafts.
