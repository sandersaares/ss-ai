# Native command recipes

Run these installed commands directly. Every state write uses the same `--state-directory`.

## Initialize

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai test-audit initialize `
  --repository-path "{{REPOSITORY_PATH}}" `
  --scope-path "{{SCOPE_PATH}}" `
  --state-directory "{{STATE_DIRECTORY}}"
```

| Placeholder | Description |
| --- | --- |
| `REPOSITORY_PATH` | Absolute path to the repository root or a path inside a Git working tree. |
| `SCOPE_PATH` | Existing repository-relative or absolute file/directory scope. Repeat `--scope-path` for multiple scopes; omit it for the complete repository. |
| `STATE_DIRECTORY` | New empty directory outside the audited source scope. |

On success, retain the returned state paths, `auditId`, `snapshotSha256`, source revision, test count, assertion count, file count, and diagnostic count. On any failure, report the native diagnostic and do not create an alternate state schema.

## Search evidence

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai test-audit search `
  --state-directory "{{STATE_DIRECTORY}}" `
  --query "{{QUERY}}" `
  --kind "{{EVIDENCE_KIND}}" `
  --limit {{LIMIT}}
```

| Placeholder | Description |
| --- | --- |
| `STATE_DIRECTORY` | Exact initialized audit state directory. |
| `QUERY` | Case-insensitive terms that must all occur in a path or one line excerpt. |
| `EVIDENCE_KIND` | Optional `documentation`, `publicDeclaration`, `traitImplementation`, `test`, or `source`; omit `--kind` to search all indexed lines. |
| `LIMIT` | Result bound from 1 through 200. Use 50 unless a narrower or broader page is justified. |

Use returned `matches` as exact citations. Read only the bounded excerpts needed for the assertion. If `truncated` is true, refine the query rather than assuming the first page is complete.

## Export the next assertion batch

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai test-audit next-batch `
  --state-directory "{{STATE_DIRECTORY}}" `
  --output-path "{{BATCH_PATH}}" `
  --count 25
```

| Placeholder | Description |
| --- | --- |
| `STATE_DIRECTORY` | Exact initialized audit state directory. |
| `BATCH_PATH` | New or atomically replaceable file receiving at most the requested assertions. |
| `25` | Bounded batch size. Use 25 so cumulative conversation progress remains predictable. |

Read the output file only after the command succeeds. Its `completed` and `remaining` fields describe durable classifications before this batch. An empty `assertions` array means all inventoried sites are classified.

## Record classification results

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai test-audit record-batch `
  --state-directory "{{STATE_DIRECTORY}}" `
  --batch-path "{{CLASSIFICATION_BATCH_PATH}}"
```

| Placeholder | Description |
| --- | --- |
| `STATE_DIRECTORY` | Exact initialized audit state directory. |
| `CLASSIFICATION_BATCH_PATH` | Strict agent-authored record following `schemas/classification-batch.schema.json`. |

The command validates the audit token, snapshot freshness, unique assertion IDs, outcome explanation, category-specific evidence, exact citation lines, known calling tests, and shared-site linkage. Identical replays report `unchanged`; different repeats fail. Use the returned `completed` and `remaining` values in the cumulative progress update.

## Emit final reports

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai test-audit report `
  --state-directory "{{STATE_DIRECTORY}}"
```

| Placeholder | Description |
| --- | --- |
| `STATE_DIRECTORY` | Exact initialized audit state directory. |

On success, retain `tracePath`, `testInventoryPath`, `summaryPath`, `status`, test count, assertion count, and module count. An incomplete classification journal is an error and emits no success report.
