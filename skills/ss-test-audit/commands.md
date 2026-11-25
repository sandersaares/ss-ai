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
  --query "{{QUERY_ONE}}" `
  --query "{{QUERY_TWO}}" `
  --kind "{{EVIDENCE_KIND}}" `
  --limit {{LIMIT}}
```

| Placeholder | Description |
| --- | --- |
| `STATE_DIRECTORY` | Exact initialized audit state directory. |
| `QUERY_ONE`, `QUERY_TWO` | Independent case-insensitive term groups. Terms within each query must all occur in a path or one line excerpt. Repeat `--query` up to 10 times; a single query is valid. |
| `EVIDENCE_KIND` | Optional `documentation`, `publicDeclaration`, `traitImplementation`, `test`, or `source`; omit `--kind` to search all indexed lines. |
| `LIMIT` | Result bound from 1 through 200. Use 50 unless a narrower or broader page is justified. |

Use each `results` entry's `matches` as exact citations. Read only the bounded excerpts needed for the assertion. If an entry's `truncated` field is true, refine that query rather than assuming the first page is complete.

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

The command validates the audit token, snapshot freshness, unique assertion IDs, outcome explanation, category-specific evidence, exact citation lines, known calling tests, and shared-site linkage. Identical replays report `unchanged`; different repeats fail. Use the returned `completed` and `remaining` values in the cumulative progress update. That update is informational only. If `remaining` is greater than zero, immediately invoke `next-batch` again in the same skill invocation. If `remaining` is zero, immediately invoke `report`, whose exact-coverage validation is authoritative. Do not treat a successful record or `remaining: 0` alone as completion.

Use this command when recovering an already-recorded result or inspecting one explicit transition. For ordinary progress, prefer the combined `advance` command below.

## Record and export the next batch

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai test-audit advance `
  --state-directory "{{STATE_DIRECTORY}}" `
  --batch-path "{{CLASSIFICATION_BATCH_PATH}}" `
  --output-path "{{NEXT_BATCH_PATH}}" `
  --count 25
```

| Placeholder | Description |
| --- | --- |
| `STATE_DIRECTORY` | Exact initialized audit state directory. |
| `CLASSIFICATION_BATCH_PATH` | Strict agent-authored record following `schemas/classification-batch.schema.json`. |
| `NEXT_BATCH_PATH` | File receiving the next bounded assertion batch. It may contain zero assertions when the audit is complete. |
| `25` | Maximum next-batch size. |

The command atomically records the classification journal and then exports the next batch from the same validated state load. `record` contains the durable completed and remaining counts. `nextBatch.batchPath` identifies the file to classify next; `nextBatch.batchCount` and `nextBatch.remaining` describe it. After a failed export, replaying the identical classification is safe because record insertion is idempotent.

After a successful advance, continue immediately according to `record.remaining`. A greater-than-zero value means read `nextBatch.batchPath`; zero means run `report`. The advance result is not completion.

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

On success, retain `tracePath`, `testInventoryPath`, `summaryPath`, `status`, test count, assertion count, and module count. Run this after `advance.record.remaining` is zero. The command performs the authoritative exact-coverage validation. An incomplete classification journal is an error and emits no success report.
