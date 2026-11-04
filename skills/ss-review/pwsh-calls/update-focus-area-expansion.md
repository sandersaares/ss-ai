```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai workflow update-focus-area-expansion --session-state "{{PREPARATION_STATE}}" `
  --operation "{{OPERATION}}" --input-path "{{INPUT_PATH}}" --reference-id "{{REFERENCE_ID}}"
```

| Placeholder | Value |
| --- | --- |
| `PREPARATION_STATE` | Generation-specific `sessionState` returned by begin, containing both source roots. |
| `OPERATION` | `initialize`, `register`, `complete`, `fail`, `read`, or `finalize`. |
| `INPUT_PATH` | Absolute `referenceInput` draft for Register or `referenceResult` draft for Complete/Fail; otherwise omit this argument. |
| `REFERENCE_ID` | Reference to query with `read`; omit for other operations or an aggregate-only read. |

Read the result shape defined in `data-guides/focus-areas.md`. Pass queried contexts to
categorization workers. On failure, correct the draft or unresolved reference
before continuing; never replace the manifest manually. Only the preparation
agent writes the manifest; expansion workers return drafts.
