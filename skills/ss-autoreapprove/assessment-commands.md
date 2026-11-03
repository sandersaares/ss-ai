# Worker command recipes

Use named arguments and native values. Each example uses PowerShell.

| Placeholder | Value |
| --- | --- |
| `RUN`, `PR`, `SNAPSHOT` | Absolute run path, PR ID, and immutable snapshot digest supplied by the orchestrator. |
| `AFTER` | Returned inventory offset; initially zero. |
| `READ_TOKEN`, `STATE_TOKEN` | Latest native evidence or semantic-state token. |
| `CHANGE` | Native short change reference from inventory, bound to this snapshot. |
| `CONCLUSIONS_JSON`, `SYNTHESIS_JSON` | Semantic inputs with native references, as illustrated below. |
| `CONTEXT_JSON` | Specific captured-source ranges as illustrated below. |
| `RATIONALE` | Evidence-backed synthesis explaining a significant partial refusal. |

# Stage 1: evidence

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$scope = @('--state-path', '{{RUN}}', '--pull-request-id', '{{PR}}', '--snapshot-digest', '{{SNAPSHOT}}')

ss-ai autoreapprove assessment @scope --after "{{AFTER}}" --compact --limit 100
ss-ai autoreapprove read-next @scope --read-token "{{READ_TOKEN}}"
ss-ai autoreapprove read-next @scope --read-token "{{READ_TOKEN}}" --change-id "{{CHANGE}}"
ss-ai autoreapprove unit-read-plan @scope --reference discussion --after "{{AFTER}}" --limit 100
ss-ai autoreapprove read-units @scope --reference discussion --after "{{AFTER}}" --count "{{COUNT}}" --read-token "{{READ_TOKEN}}"
ss-ai autoreapprove source-context-batch @scope --batch-json '{{CONTEXT_JSON}}'
```

Follow inventory `nextOffset`. `read-next` derives bounded batches and offsets,
returning exact text, short change references, content digests, receipts, and
continuation metadata.
Repeat `--change-id` to select specific changes and their requested context.
Native code selects offsets even for targeted delivery. With a selection,
`items: []` means that selection is delivered; without one, it means no required
text remains undelivered. Neither means semantic work is complete.
Do not combine separately bounded tool outputs.

A context batch is an array such as
`[{"changeId":"c1","side":"new","startLine":1,"lineCount":20}]`.
Choose actual necessary ranges within the captured side's line count.
Registration returns references and a new read token without delivering text.
Use that token with `read-next` to deliver all registered ranges. A stale local
token requires current assessment metadata and a retry, not a new snapshot.

For a long discussion, follow each plan page and issue the returned independent
`read-units` windows together as separately bounded calls when they fit context.
Read any missing bindings and continue partial units using their returned
references. For deliberate rereads, use `read-batch` as described in
[native commands](commands.md#stage-2-preparation-and-evidence).
A partial mutable-record continuation supplies its previous content digest.
Handle nonzero command exits explicitly; a failed command supplies no evidence.

# Stage 2: conclusions and completion

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$scope = @('--state-path', '{{RUN}}', '--pull-request-id', '{{PR}}', '--snapshot-digest', '{{SNAPSHOT}}')

ss-ai autoreapprove conclude @scope --conclusions-json '{{CONCLUSIONS_JSON}}'
ss-ai autoreapprove synthesize @scope --synthesis-json '{{SYNTHESIS_JSON}}'
ss-ai autoreapprove complete-partial @scope --change-id "{{CHANGE}}" --rationale "{{RATIONALE}}"
```

A grouped input has the shape
`{"stateToken":"TOKEN","groups":[{"changes":["c1","c2"],"disposition":"insignificant","rationale":"Shared behavioral effect and significance."}]}`.
Use only changes that actually share the explanation. Native code derives
member evidence and conditional replacements. Optional `evidence` names exact
additional native references, including `c1:patch` and returned context references.
The response returns coverage and the next state token, not per-path paperwork.

Final synthesis has the shape
`{"stateToken":"TOKEN","disposition":"insignificant","rationale":"Independent combined-change reasoning.","discussion":"Material objections and their disposition."}`.
Optional `unresolvedConcerns` retains substantive concerns; any nonempty list
prevents approval. Optional `evidence` cites additional exact evidence.
Native validation and completion are atomic: missing delivery or change coverage
leaves the synthesis unrecorded. No separate note, check, or complete call is required.

Use `complete-partial` instead of final full synthesis only for a supported
significant change. Its completion explicitly retains partial coverage and can
never authorize renewal. Missing evidence is not significance. Return native
status and digest; never publish, notify, or infer success from a failed command.
