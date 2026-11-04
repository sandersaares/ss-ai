Append a validated one-chapter draft or worker map:

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai workflow update-focus-area-map --session-state "{{PREPARATION_STATE}}" `
  --path "{{MAP_PATH}}" --operation append --input-path "{{INPUT_PATH}}"
```

Use `--operation replace` with the same arguments to apply corrected drafts for
existing chapter IDs. Regroup a category without rewriting its chapter metadata:

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai workflow update-focus-area-map --session-state "{{PREPARATION_STATE}}" `
  --path "{{MAP_PATH}}" --operation regroup --category-id "{{CATEGORY_ID}}" `
  --new-category-id "{{NEW_CATEGORY_ID}}" --title "{{TITLE}}"
```

| Placeholder | Value |
| --- | --- |
| `PREPARATION_STATE` | Generation-specific `sessionState` returned by begin. |
| `MAP_PATH` | Absolute destination map path within the preparation directory; Append creates it if absent. |
| `INPUT_PATH` | Absolute valid map draft in the preparation directory to append. |
| `CATEGORY_ID` | Existing category to move. |
| `NEW_CATEGORY_ID` | Destination category, existing or new. |
| `TITLE` | Destination category's title. |

Success emits `status: "Recorded"`, `categoryCount`, and `chapterCount`. Repair
conflicting drafts on failure; do not invent merge scripts or patch persisted maps.
