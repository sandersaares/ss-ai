```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai workflow begin-focus-area-generation --session-state "{{SESSION_STATE}}" `
  --preparation-identity "{{PREPARATION_IDENTITY}}"
```

| Placeholder | Value |
| --- | --- |
| `SESSION_STATE` | Absolute review session directory used for cache restore. |
| `PREPARATION_IDENTITY` | Exact `preparationIdentity` returned by that restore. |

Retain `generationId` and `sessionState` from the JSON result. Use that
`sessionState` as `PREPARATION_STATE` for all preparation workers and commands.
On failure, stop preparation and apply the recovery policy; never adopt another
generation's identity or work directory.
