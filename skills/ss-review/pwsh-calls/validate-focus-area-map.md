```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai workflow validate-focus-area-map --session-state "{{PREPARATION_STATE}}" --map-path "{{MAP_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `PREPARATION_STATE` | Generation-specific `sessionState` returned by begin. |
| `MAP_PATH` | Absolute path to the worker's categorization JSON in the preparation directory. |

Success emits one JSON object with `status: "Valid"`, `categoryCount`, and
`chapterCount`. On failure, have the worker repair its JSON and repeat validation;
do not accept the partial map.
