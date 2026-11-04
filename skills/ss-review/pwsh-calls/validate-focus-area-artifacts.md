```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai workflow validate-focus-area-artifacts --session-state "{{SESSION_STATE}}" `
  --preparation-identity "{{PREPARATION_IDENTITY}}"
```

| Placeholder | Value |
| --- | --- |
| `SESSION_STATE` | Absolute review session directory containing the artifacts. |
| `PREPARATION_IDENTITY` | Exact `preparationIdentity` returned by this invocation's cache restore. |

The command reports `status: "Valid"` for a complete set. On failure, follow
the recovery path for the current preparation stage. A stale identity must not
be replaced with one obtained from another invocation.
