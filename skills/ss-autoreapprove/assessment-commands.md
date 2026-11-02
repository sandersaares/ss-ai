# Worker command recipes

The assignment supplies `RUN`, `PR`, and `SNAPSHOT`. Use named arguments and
replace placeholders in the examples with the native values. Each example
uses PowerShell:

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$scope = @('--state-path', '{{RUN}}', '--pull-request-id', '{{PR}}', '--snapshot-digest', '{{SNAPSHOT}}')
```

| Placeholder | Value |
| --- | --- |
| `RUN`, `PR`, `SNAPSHOT` | Absolute run path, PR ID, and immutable snapshot digest supplied by the orchestrator. |
| `AFTER`, `COUNT` | Returned window or pagination offset and count. Start an inventory or plan at offset zero. |
| `REFERENCE`, `OFFSET` | Native evidence reference and returned Unicode scalar offset. |
| `READ_TOKEN`, `STATE_TOKEN` | Latest native read or assessment token. |
| `CHANGE`, `SIDE`, `START_LINE`, `LINE_COUNT` | Change ID, `old` or `new`, and captured-side line range. |
| `BATCH_JSON` | JSON request assembled from native references and tokens. |
| `DISPOSITION`, `RATIONALE`, `EVIDENCE` | Semantic disposition, explanation, and exact supporting reference. |
| `OBSERVATIONS`, `INTERFACES`, `CHECK` | Discussion observation, relevant interfaces, and returned check ID. |

# Stage 1: evidence

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$scope = @('--state-path', '{{RUN}}', '--pull-request-id', '{{PR}}', '--snapshot-digest', '{{SNAPSHOT}}')

ss-ai autoreapprove assessment @scope --after "{{AFTER}}" --compact --limit 100
ss-ai autoreapprove read-batch @scope --batch-json '{{BATCH_JSON}}'
ss-ai autoreapprove unit-read-plan @scope --reference discussion --after "{{AFTER}}" --limit 100
ss-ai autoreapprove read-units @scope --reference discussion --after "{{AFTER}}" --count "{{COUNT}}" --read-token "{{READ_TOKEN}}"
ss-ai autoreapprove read @scope --reference "{{REFERENCE}}" --offset "{{OFFSET}}" --limit 16384
ss-ai autoreapprove source-context @scope --change-id "{{CHANGE}}" --side "{{SIDE}}" --start-line "{{START_LINE}}" --line-count "{{LINE_COUNT}}" --limit 16384
ss-ai autoreapprove source-context-batch @scope --batch-json '{{BATCH_JSON}}'
```

Follow `nextOffset` for every inventory and evidence page. A `read-batch`
request has the shape
`{"readToken":"TOKEN","items":[{"reference":"policy","offset":0,"limit":16384}]}`.
Use distinct references. A mutable-record continuation also supplies its
previous `contentDigest`. A plan returns independent unit windows and a read
token; `read-units` returns their text and bindings. Read missing bindings and
continue partial units. Do not combine separately bounded tool outputs.

For several known context ranges, the batch registration request is an array
such as `[{"changeId":"CHANGE","side":"new","startLine":1,"lineCount":20}]`.
It returns each range's `reference` and `totalCharacters` and the final
`readToken`; it does not deliver the text. Read every returned reference using
`read-batch` and continue partial pages. Scope and tokens in a `read-batch`
response apply to all its items. If registration reports `batchTooLarge`,
retry the same ranges in smaller batches; no range from that request was registered.

Use the captured side's line count to bound context requests. Registering
context changes the read token and invalidates dependent judgments and
observations. Read the returned range completely, then rebuild only affected
records. A stale local read token requires a fresh `assessment` token and a
retry at the same offsets, not a new worker or snapshot. A native command
failure is not evidence or a completed assessment.

# Stage 2: judgments and completion

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$scope = @('--state-path', '{{RUN}}', '--pull-request-id', '{{PR}}', '--snapshot-digest', '{{SNAPSHOT}}')

ss-ai autoreapprove judge-batch @scope --batch-json '{{BATCH_JSON}}'
ss-ai autoreapprove note @scope --unit-id discussion --observations "{{OBSERVATIONS}}" --interfaces "{{INTERFACES}}" --evidence discussion
ss-ai autoreapprove checks @scope --after "{{AFTER}}" --limit 100
ss-ai autoreapprove read-batch @scope --batch-json '{{BATCH_JSON}}'
ss-ai autoreapprove resolve @scope --check-id "{{CHECK}}" --disposition "{{DISPOSITION}}" --rationale "{{RATIONALE}}" --evidence "{{EVIDENCE}}"
ss-ai autoreapprove judge @scope --change-id cumulative --disposition "{{DISPOSITION}}" --rationale "{{RATIONALE}}" --evidence policy --evidence discussion
ss-ai autoreapprove complete @scope
```

A judgment batch has the shape
`{"stateToken":"TOKEN","items":[{"id":"CHANGE_ID","judgment":{"disposition":"insignificant","rationale":"Explanation","evidence":["CHANGE_ID:patch"],"concerns":[],"interactions":[]},"previousDigest":null}]}`.
Each item names a different path. Replacements use the current decision digest;
new judgments use null. To replace a note, resolution, or cumulative judgment,
pass its current `--previous-digest`; omit that option for a new record.

Repeat `--evidence` for each supporting reference, and repeat optional
`--concern` and `--interaction` for concrete obligations. Read a check and its
originating record before resolving it. Follow the assignment's completion
requirements; a successful `complete` returns the assessment digest and
decision. Do not publish, notify, or infer success from a failed command.
