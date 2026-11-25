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

Use each `results` entry's `matches` as exact citation candidates. Each match contains `evidenceHandle`, `path`, `line`, and `excerpt`; submit the handle rather than reconstructing a citation tuple. Read only the bounded excerpts needed for the assertion. If an entry's `truncated` field is true, refine that query rather than assuming the first page is complete. Do not submit until every required evidence query has at least the intended literal match.

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

Read the output file only after the command succeeds. Its `completed` and `remaining` fields describe durable classifications before this batch. The command also returns `displayPath`; read that compact text for normal review. The top-level `nextAction` is `continue` for a nonempty batch and `report` for an empty batch; `finalResponseAllowed` is always false. The JSON shape is flat: `assertions[]` contains assertion fields directly, with no `.assertion` wrapper. An empty `assertions` array means the final report is next, not that a final response is allowed.

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

Use this command when recovering an already-recorded result or inspecting one explicit transition. For ordinary progress, prefer the compact `submit` command below; use `advance` only when a canonical low-level classification record already exists.

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

The command atomically records the classification journal and then exports the next batch from the same validated state load. `record` contains the durable completed and remaining counts. The top-level `nextAction` is authoritative for continuation: `continue` forbids a final response and requires processing the next batch, while `report` requires the final report immediately. `finalResponseAllowed` is always false for mutation transitions. `nextBatch.batchPath` is retained for a subsequent submission, while `nextBatch.displayPath` is the compact file to read for judgment. `nextBatch.batchCount` and `nextBatch.remaining` describe it. After a failed export, replaying the identical classification is safe because record insertion is idempotent.

After a successful advance, continue immediately according to `record.remaining`. A greater-than-zero value means read `nextBatch.displayPath`; zero means run `report`. This low-level recovery path is not completion.

## Submit a compact row batch

Write one file following [compact-submission.schema.json](schemas/compact-submission.schema.json). Do not create per-session PowerShell serializers for this record; write its small JSON rows directly and let native code resolve all evidence handles and build the canonical journal record.

Fresh rows:

```json
{
  "schemaVersion": 1,
  "rows": [
    {
      "assertionId": "A000001",
      "classification": "Documented",
      "evidenceStatus": "Confirmed",
      "actualOutcome": "The checked call returns the documented value.",
      "rationale": "The cited API documentation promises this outcome.",
      "citationHandles": ["E0123456789ab-000042"],
      "callingTestIds": ["T000001"]
    }
  ]
}
```

Reuse rows must explicitly confirm equivalence and replace both evidence and caller attribution:

```json
{
  "schemaVersion": 1,
  "rows": [
    {
      "assertionId": "A000002",
      "reuseFrom": "A000001",
      "equivalenceConfirmed": true,
      "citationHandles": ["E0123456789ab-000084"],
      "callingTestIds": ["T000002"]
    }
  ]
}
```

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai test-audit submit `
  --state-directory "{{STATE_DIRECTORY}}" `
  --batch-path "{{BATCH_PATH}}" `
  --submission-path "{{SUBMISSION_PATH}}" `
  --output-path "{{NEXT_BATCH_PATH}}" `
  --count 25
```

| Placeholder | Description |
| --- | --- |
| `STATE_DIRECTORY` | Exact initialized audit state directory. |
| `BATCH_PATH` | Current exported batch from `next-batch` or the prior `nextBatch.batchPath`. |
| `SUBMISSION_PATH` | Compact agent-authored row file following `schemas/compact-submission.schema.json`. |
| `NEXT_BATCH_PATH` | Machine-readable path for the next batch; the result also returns `displayPath`. |
| `25` | Maximum next-batch size. |

The command binds rows to the current batch and snapshot, resolves every evidence handle, validates every assertion and citation field, records the canonical classifications, and exports the next batch. Multiple citations belong in `citationHandles` as separate values. Reuse copies semantic outcome fields only after `equivalenceConfirmed: true`; citations and caller IDs are explicit replacements. Empty reuse citation arrays are rejected without fallback. Read `nextBatch.displayPath` for the next cycle while retaining `nextBatch.batchPath` for the next submission. On failure, correct the named assertion ID, field, and evidence reference, then retry the same compact submission.

After every submit, obey the top-level directive exactly. `nextAction: "continue"` means emit only an informational progress update and immediately process the returned batch; a final response is forbidden. `nextAction: "report"` means run the report next; a final response is still forbidden. Both transitions return `finalResponseAllowed: false`.

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

On success, require `nextAction: "complete"` and `finalResponseAllowed: true` before returning a final response. Use the returned `tracePath`, `testInventoryPath`, and `summaryPath`; do not assume a state-root report location. Retain `status`, test count, assertion count, module count, `noCountedAssertionCount`, `incidentalOnlyCount`, `unlinkedAssertionCount`, `fallibleValueCheckCount`, `conditionalAssertionCount`, and `diagnosticCount`. The trace JSON shape is `rows[]`; each row contains `assertion` and `classification`. Run this after the prior mutation returned `nextAction: "report"`. The command performs the authoritative exact-coverage validation. An incomplete classification journal is an error and emits no success report.
