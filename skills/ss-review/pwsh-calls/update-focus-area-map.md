Append a validated one-chapter draft or worker map:

```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Update-FocusAreaMap.ps1" `
  -SessionStateDirectory "{{SESSION_STATE}}" -Path "{{MAP_PATH}}" -Operation Append -InputPath "{{INPUT_PATH}}"
```

Use `-Operation Replace` with the same arguments to apply corrected drafts for
existing chapter IDs. Regroup a category without rewriting its chapter metadata:

```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Update-FocusAreaMap.ps1" `
  -SessionStateDirectory "{{SESSION_STATE}}" -Path "{{MAP_PATH}}" -Operation Regroup `
  -CategoryId "{{CATEGORY_ID}}" -NewCategoryId "{{NEW_CATEGORY_ID}}" -Title "{{TITLE}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `SESSION_STATE` | Absolute session-state directory. |
| `MAP_PATH` | Absolute destination map path; Append creates it if absent. |
| `INPUT_PATH` | Absolute valid map draft to append. |
| `CATEGORY_ID` | Existing category to move. |
| `NEW_CATEGORY_ID` | Destination category, existing or new. |
| `TITLE` | Destination category's title. |

Success emits `status: "Recorded"`, `categoryCount`, and `chapterCount`. Repair
conflicting drafts on failure; do not invent merge scripts or patch persisted maps.
