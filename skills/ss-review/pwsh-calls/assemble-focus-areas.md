```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai workflow assemble-focus-areas --session-state "{{PREPARATION_STATE}}" --map-path "{{MAP_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `PREPARATION_STATE` | Generation-specific `sessionState` returned by begin. |
| `MAP_PATH` | Absolute path to the merged `focus-area-map.json` in the preparation directory. |

Success emits one JSON object with `status: "Assembled"`, `categoryCount`, and
`chapterCount`. Continue to semantic scope verification. On failure, repair the
map or source preparation and retry; do not review or cache partial artifacts.
