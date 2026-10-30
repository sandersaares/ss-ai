```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$cacheDirectory = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)) '.copilot/cache/ss-review/focus-areas/v1'
ss-ai workflow restore-focus-area-cache --session-state "{{SESSION_STATE}}" `
  --input-directory "{{SOURCE_DIRECTORY}}" --snapshot-identity "{{SNAPSHOT_IDENTITY}}" --skill-root "{{SKILL_ROOT}}" `
  --recipe-path "{{SKILL_ROOT}}/prepare-focus-areas.md" --cache-directory $cacheDirectory `
  --generator-profile "gpt-6-luna" --freshness-days 7
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `SESSION_STATE` | Absolute review session directory with a source snapshot. |
| `SOURCE_DIRECTORY` | `sourceDirectory` returned by this invocation's instruction snapshot. |
| `SNAPSHOT_IDENTITY` | Exact `snapshotIdentity` returned by that snapshot. |

Read `status`, `reason`, `userInstructionFileCount`, and `preparationIdentity`
from the JSON result. On a miss, retain the identity for beginning generation.
Never replace a rejected identity with one read from session files.
