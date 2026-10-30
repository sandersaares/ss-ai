```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai workflow publish-focus-area-generation --session-state "{{SESSION_STATE}}" `
  --generation-id "{{GENERATION_ID}}"
```

| Placeholder | Value |
| --- | --- |
| `SESSION_STATE` | Absolute review session directory used to begin generation. |
| `GENERATION_ID` | Exact `generationId` returned by begin. |

Success returns `status: "Published"` and the generation ID. Continue with
real-session artifact validation and cache storage. On failure, stop; do not
copy partial artifacts or reuse another generation's receipt.
