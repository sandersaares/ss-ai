```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai workflow snapshot-instructions --session-state "{{SESSION_STATE}}" `
  --skill-root "{{SKILL_ROOT}}" --repository-root "{{REPOSITORY_ROOT}}" `
  --repository-instruction "{{REPOSITORY_INSTRUCTION}}"
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Directory containing this skill's `SKILL.md`. |
| `SESSION_STATE` | Absolute directory containing this review's artifacts. |
| `REPOSITORY_ROOT` | Absolute root of the reviewed repository. Omit this option unless repository instructions are authorized and selected. |
| `REPOSITORY_INSTRUCTION` | Selected repository-relative instruction path. Repeat this option for every selected path. Omit it together with `--repository-root` when no repository instructions are selected. |

The command emits one JSON object with `schemaVersion: 1`, `sourceDirectory`,
`snapshotIdentity`, `externalFileCount`, and `roots`. Each root has its original absolute `path`,
snapshot-relative `prefix`, and `fileCount`. When repository instructions are
selected, `repositoryRoot` provides the same mapping for their root and prefix.
Keep these mappings for reference
expansion and learning, and pass the exact `snapshotIdentity` to cache restore.
`prefix/relative-path` resolves to `path/relative-path`
in the original instruction tree. On failure, apply the error recovery policy;
do not continue using an old snapshot as if discovery succeeded.
