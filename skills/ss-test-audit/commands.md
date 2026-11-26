# Native command recipes

Run these installed commands directly. Every state write uses the same `--state-directory`. For continuation and completion, [follow the current directive](SKILL.md#follow-the-current-directive).

## Initialize

```powershell
#Requires -Version 7.6

if ($PSVersionTable.PSVersion -lt [version]'7.6') { throw 'PowerShell 7.6 or later is required.' }
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
| `SCOPE_PATH` | Existing repository-relative or absolute file or directory for test and assertion inventory. Repeat `--scope-path` for multiple scopes; omit it for the repository-wide inventory. The evidence index covers the ordinary repository walk and any explicitly selected source outside it. |
| `STATE_DIRECTORY` | New, empty directory outside the repository, even when `--scope-path` limits inventory. |

On success, retain the returned state paths, `auditId`, `snapshotSha256`, source revision, test count, assertion count, file count, and diagnostic count. On any failure, report the native diagnostic and do not create an alternate state schema.

Source discovery indexes text files up to and including 4 MiB. Larger files
produce an `oversizedFile` diagnostic and do not enter the inventory or index.

## Search evidence

```powershell
#Requires -Version 7.6

if ($PSVersionTable.PSVersion -lt [version]'7.6') { throw 'PowerShell 7.6 or later is required.' }
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai test-audit search `
  --state-directory "{{STATE_DIRECTORY}}" `
  --query '{{QUERY_ONE}}' `
  --query '{{QUERY_TWO}}' `
  --kind "{{EVIDENCE_KIND}}" `
  --limit {{LIMIT}}
```

| Placeholder | Description |
| --- | --- |
| `STATE_DIRECTORY` | Exact initialized audit state directory. |
| `QUERY_ONE`, `QUERY_TWO` | Independent queries split on whitespace. Each term may occur anywhere in the result's path or excerpt; matching folds ASCII case only. Double any apostrophe in the substituted text. Repeat `--query` up to 10 times; a single query is valid. |
| `EVIDENCE_KIND` | Optional `documentation`, `publicDeclaration`, `traitImplementation`, `test`, or `source`; omit `--kind` to search all indexed lines. |
| `LIMIT` | Result bound from 1 through 200. Use 50 unless a narrower or broader page is justified. |

Use each `results` entry's `matches` as citation candidates. Submit the returned `evidenceHandle` rather than reconstructing a citation from `path`, `line`, and `excerpt`. Refine truncated results. Find a matching citation for `Documented` and for the documented component of `Mixed`. A diagnostic search can end without a match after reasonable targeted queries and after ruling out truncation; this can support an `Implicit` or `Incidental` explanation. One empty query does not prove that no public promise exists.

## Export the next assertion batch

```powershell
#Requires -Version 7.6

if ($PSVersionTable.PSVersion -lt [version]'7.6') { throw 'PowerShell 7.6 or later is required.' }
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
| `BATCH_PATH` | Preferred output name for the machine batch. |
| `25` | Bounded batch size. Use 25 so cumulative conversation progress remains predictable. |

Batch exports preserve existing files; `--output-path` supplies a preferred name outside the report publication area. Open the returned `displayPath` to read the assertions and nearby source. Retain the JSON record at the returned `batchPath` unchanged for `submit --batch-path`.

## Record classification results

```powershell
#Requires -Version 7.6

if ($PSVersionTable.PSVersion -lt [version]'7.6') { throw 'PowerShell 7.6 or later is required.' }
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

The canonical classification input must fit in 1 MiB, including JSON whitespace.
The command checks that the audit ID (`auditId`) identifies the active audit and that the snapshot digest (`snapshotSha256`) identifies its source snapshot. It also checks source freshness, unique assertion IDs, outcome explanation, category-specific evidence, exact citation lines, known calling tests, and shared-site linkage. Identical replays report `unchanged`; different repeats fail. This record-only command returns no batch; if its directive is `continue`, use `next-batch` to export one.

Use this command when recovering an already-recorded result or inspecting one explicit transition. For ordinary progress, prefer the compact `submit` command below; use `advance` only when a canonical low-level classification record already exists.

## Record and export the next batch

```powershell
#Requires -Version 7.6

if ($PSVersionTable.PSVersion -lt [version]'7.6') { throw 'PowerShell 7.6 or later is required.' }
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
| `NEXT_BATCH_PATH` | Preferred output name for the next machine batch. |
| `25` | Maximum next-batch size. |

The command commits the classification journal before exporting the next batch from the same validated state load. `record` contains the durable counts. Use the returned `nextBatch.displayPath` for judgment and retain `nextBatch.batchPath` for the next submission. After a known post-record export failure, replay the identical classification input.

## Submit a compact row batch

Write one file following [compact-submission.schema.json](schemas/compact-submission.schema.json). Do not create per-session PowerShell serializers for this record; write its small JSON rows directly and let native code resolve all evidence handles and build the canonical journal record.

Fresh rows:

```json
{
  "schemaVersion": 1,
  "batchFingerprint": "COPY THE CURRENT BATCH FINGERPRINT",
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
  "batchFingerprint": "COPY THE CURRENT BATCH FINGERPRINT",
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

if ($PSVersionTable.PSVersion -lt [version]'7.6') { throw 'PowerShell 7.6 or later is required.' }
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
| `NEXT_BATCH_PATH` | Preferred output name for the next machine batch. |
| `25` | Maximum next-batch size. |

The command binds rows to the exported machine batch using `batchFingerprint`, resolves all evidence handles, and commits the validated classifications before exporting the next batch. Use separate `citationHandles` entries for multiple citations. Read the returned `nextBatch.displayPath` for judgment and retain `nextBatch.batchPath` for the next submission. Preserve the original inputs and [recover according to the point of failure](SKILL.md#recover-from-failure).

## Emit final reports

```powershell
#Requires -Version 7.6

if ($PSVersionTable.PSVersion -lt [version]'7.6') { throw 'PowerShell 7.6 or later is required.' }
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai test-audit report `
  --state-directory "{{STATE_DIRECTORY}}"
```

| Placeholder | Description |
| --- | --- |
| `STATE_DIRECTORY` | Exact initialized audit state directory. |

The successful report receipt provides `summaryJsonPath`, `summaryPath`,
`detailsJsonPath`, and `detailsPath`; do not assume a state-root report location.
`details.json` is the canonical machine-readable row detail, while `summary.json`
is the aggregate report.

The paired JSON artifacts conform to [summary.schema.json](schemas/summary.schema.json)
and [details.schema.json](schemas/details.schema.json). Their schema version is
part of each file and changes only with the native utility's record contract.

The command checks exact coverage and does not publish a success report for an
incomplete classification journal. On a nonzero result, preserve the exact
command and diagnostic and follow
[Recover from failure](SKILL.md#recover-from-failure). A concrete failure that
requires user correction leaves the audit incomplete; do not infer completion
from a prior `report` directive.
