```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Copy-ReviewInstructionSources.ps1" `
  -SessionStateDirectory "{{SESSION_STATE}}"
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Directory containing this skill's `SKILL.md`. |
| `SESSION_STATE` | Absolute directory containing this review's artifacts. |

The command emits one JSON object with `schemaVersion: 1`, `sourceDirectory`,
`externalFileCount`, and `roots`. Each root has its original absolute `path`,
snapshot-relative `prefix`, and `fileCount`. Keep this mapping for reference
expansion and learning: `prefix/relative-path` resolves to `path/relative-path`
in the original instruction tree. On failure, apply the error recovery policy;
do not continue using an old snapshot as if discovery succeeded.
