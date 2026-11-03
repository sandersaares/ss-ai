# Native command recipes

Run commands directly through the installed executable. Expand named
placeholders rather than writing state-management scripts. JSON supplied to
batch commands is a request, not the retained workflow state.
All commands return JSON; nonzero exits and stderr diagnostics are explicit
failures, not permission to stop at recoverable staleness. Let an in-flight
command finish rather than starting a duplicate.

| Placeholder | Value |
| --- | --- |
| `REPOSITORY` | Supported HTTPS repository browser URL resolved in skill stage 1 from the explicit repository or invocation checkout. Keep it fixed throughout the run. |
| `RUN` | Absolute session-state path for this invocation's run JSON. |
| `POLICY` | Absolute path to this installed skill's `significance.md`. |
| `TENANT` | User-selected Azure tenant; omit the argument when not supplied. |
| `PR` | Positive PR identifier returned by discovery. |
| `SNAPSHOT` | Exact `snapshotDigest` returned by inspection. |
| `AFTER_PR` | `nextAfter` from the previous results page. |
| `AFTER_CHANGE` | `nextOffset` from the previous assessment page. |
| `REFERENCE` | `policy`, `discussion`, a returned `CHANGE_ID:patch`, optional full-side `CHANGE_ID:old` / `CHANGE_ID:new`, or an exact context reference returned by `source-context`. |
| `SOURCE_CHANGE` | Exact changed-path ID; never `cumulative`. |
| `SIDE` | `old` or `new`, selecting a captured readable side. |
| `START_LINE` | Positive one-based first line in that side. |
| `LINE_COUNT` | Positive count of complete existing lines to examine. |
| `OFFSET` | `nextOffset` from the previous evidence read, initially zero. |
| `CHANGE` | Exact returned change ID, or `cumulative`. |
| `DISPOSITION` | `insignificant`, `significant`, or `uncertain`. |
| `RATIONALE` | Concise semantic judgment grounded in the returned evidence. |
| `PREVIOUS` | Current `decisionDigest` or `cumulativeDigest` when deliberately replacing a judgment; omit the argument for a new judgment. |
| `ATTRIBUTION` | The user's required agent attribution, such as `[Copilot speaking]`. |
| `BATCH_JSON` | Compact JSON request described below, quoted as one argument. For the PowerShell single-quoted recipes, double each apostrophe in the JSON when inserting it into the command. |

PowerShell turns each doubled apostrophe in a single-quoted argument back into
one apostrophe before passing the JSON to the executable. For example, the
JSON value `{"rationale":"user's change"}` becomes the PowerShell argument
`'{"rationale":"user''s change"}'`. Apply this rule to both batch commands;
do not alter the JSON content itself.

# Stage 1: discovery and history

```powershell
ss-ai autoreapprove discover --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --policy-path "{{POLICY}}" --tenant "{{TENANT}}" --dry-run
ss-ai autoreapprove results --state-path "{{RUN}}" --after "{{AFTER_PR}}"
ss-ai autoreapprove inspect --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --pull-request-id "{{PR}}" --tenant "{{TENANT}}"
```

Omit `--dry-run` only for an authorized publishing invocation. Omit `--tenant`
when not supplied. The mode and policy are immutable for this run. Read
`items`, `counts`, `state`, `error`, and `nextAfter`; follow all results pages.
Omit `--after` for the first page; later cursors are positive PR IDs.
Inspect only recent nondraft entries requiring assessment. Retain each
inspection's `snapshotDigest`. Scope failures stop the run; independent PR
failures remain visible while other eligible PRs can proceed.
Inspection or refresh can return no item when a PR has become a draft; do not
assess or report that PR.

# Stage 2: preparation and evidence

For an inspected snapshot, prepare and reserve in one tool call. This keeps
orchestration delay within the same overall budget while avoiding an extra
model turn before the worker starts. The first command must succeed and return
the expected in-progress snapshot before the reservation is attempted.

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$scope = @('--state-path', '{{RUN}}', '--pull-request-id', '{{PR}}', '--snapshot-digest', '{{SNAPSHOT}}')
$prepared = ss-ai autoreapprove prepare @scope | ConvertFrom-Json
if ($prepared.snapshotDigest -ne '{{SNAPSHOT}}' -or
    $prepared.assessmentProtocol -ne 'directV1' -or
    $prepared.state -ne 'in-progress') {
    throw 'Preparation did not return the expected in-progress assessment.'
}
$reservation = ss-ai autoreapprove start-worker @scope | ConvertFrom-Json
$item = @($reservation.items)
if ($item.Count -ne 1 -or $item[0].pullRequestId -ne [int]'{{PR}}' -or
    $item[0].snapshotDigest -ne '{{SNAPSHOT}}' -or
    $item[0].status -ne 'assessmentRequired') {
    throw 'Worker reservation did not return the expected assessment.'
}
Write-Output "Snapshot: $($prepared.snapshotDigest)"
$prepared.progress | Format-List
$item[0].assessmentBudget | Format-List
```

Inspect the returned progress and budget before dispatch. A failed command
or unexpected response stops dispatch and requires reporting or recovery.

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai autoreapprove assessment --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --after "{{AFTER_CHANGE}}" --compact --limit 100
ss-ai autoreapprove read-batch --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --batch-json '{{BATCH_JSON}}'
ss-ai autoreapprove unit-read-plan --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --reference discussion --after "{{AFTER_UNIT}}" --limit 100
ss-ai autoreapprove read-units --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --reference discussion --after "{{AFTER_UNIT}}" --count "{{UNIT_COUNT}}" --read-token "{{READ_TOKEN}}"
ss-ai autoreapprove read --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --reference "{{REFERENCE}}" --offset "{{OFFSET}}"
ss-ai autoreapprove source-context --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --change-id "{{SOURCE_CHANGE}}" --side "{{SIDE}}" --start-line "{{START_LINE}}" --line-count "{{LINE_COUNT}}" --limit 16384
ss-ai autoreapprove source-context-batch --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --batch-json '{{BATCH_JSON}}'
```

Preparation returns only `snapshotDigest`, `assessmentProtocol`, `state`, and
`progress` with preflight counts. Do not print the complete reservation or
retained run. Worker inventory, context bindings, digests, document reads, and
tokens belong to `assessment`, not preparation.
In assessment metadata, `readToken` identifies immutable
evidence and registered context; `stateToken` identifies the current semantic
assessment state. Independent evidence reads do not change it, but a judgment
still requires the current evidence receipts. Reserve each worker with `start-worker` before
dispatch; its returned `assessmentBudget` shows reservations and limits.
Follow `nextOffset` through the assessment inventory, even when a page contains
fewer changes than requested. Use each change's `oldLineCount` and
`newLineCount` to keep context requests within the captured source. Native metadata and evidence responses fit within
16 KiB of pretty-printed UTF-8 JSON, including the final newline. The read
character limit is an upper bound; JSON escaping and multibyte text can shorten
a page. Do not print the retained run. If a tool truncates delivery, reduce
the requested batch and replay the same offsets before continuing.

A batch request has the shape
`{"readToken":"TOKEN","items":[{"reference":"policy","offset":0,"limit":1024},{"reference":"discussion","offset":0,"limit":1024}]}`.
Use distinct references and positive limits. The response has `items`, a
`stateToken`, and the `readToken`. Obtain progress from `assessment` when needed.
Each item carries its own `nextOffset` and `contentDigest`. Multiple batches
may use the same read token
for independent references without waiting for receipt updates. A continuation
of a mutable semantic record must also supply the preceding `contentDigest`;
ordinary `read` accepts only its first page, so use `read-batch` for further
pages. Registering source context changes the read token; use the new token
returned by `source-context`. A batch
whose combined response exceeds the native limit fails without committing any
receipts; retry smaller batches. If a batch rejects a token after context was
registered, use `assessment` to get the current token and retry undelivered
pages at the same offsets in the same worker. This is not snapshot staleness.
If the host truncates a successful response,
reacquire a fresh `readToken` with `assessment` and repeat those pages before
proceeding. Repeated pages do not prove that the worker understood them.

`unit-read-plan` returns windows selected from actual serialized unit responses,
including escaped text and metadata. Follow `nextOffset` to enumerate the plan;
the returned `after` and `count` values select independent windows for
`read-units`. The plan records no evidence receipts. A unit that does not fit
entirely in its window reports `nextOffset`; continue it with `read-batch`.
Read any binding not included inline. If context registration changes the read
token, acquire the new token and retry undelivered windows.

`documentReads` reports `reference`, `totalCharacters`, nullable `readOffset`,
and `complete` for policy and discussion. Each present source side exposes its
`content.kind`; text includes the same `content.progress` fields. Null read
offset means no receipt, including for an empty document. Use this progress
to find missing work, not to infer another worker's knowledge.
`pendingChangeCount` and `nextChangeOffset` locate unjudged source.
Reading assessment metadata after completion does not consume the assessment
budget. Native completion freezes assessment time even if the parent observes
the result late. Publication still checks freshness within its own bounded
window; `publicationTimeout` diagnoses a slow finalizer, not a slow assessor.
The compact inventory represents `old` and `new` as paths or null rather than
side metadata. Required patch documents still contain complete source bindings
and content classifications. Omit `--compact` when full side metadata is needed.
Each change exposes `requiredPatch` with the same progress fields and `hunkCount`.
For approval, read the captured policy, discussion, and every required patch document. Evidence
responses contain `text`, `totalCharacters`, `complete`, and `nextOffset`; consume contiguous
pages until complete. A side without readable text cannot support an
insignificant decision. Do not substitute an arbitrary checkout or target tip.

Patch documents bind the snapshot, change ID, endpoint objects, paths, modes,
and content classifications to complete exact hunks. Additions/deletions include
all available text; rename/mode-only changes still have required documents.
The comparison includes merge-derived content and force-push removals, never
a three-dot PR diff. `sourceEvidenceDigest` binds the manifest.

Use `source-context` when surrounding captured code is necessary. It registers
the range and returns its first bounded page plus a stable `reference` and
exact `context` binding. Continue through `read` using that reference and
`nextOffset`. Repeated identical requests are idempotent. Inspect
`requestedContextCount` and `nextContextReference` at assessment and change level
to resume required context. Full `:old` / `:new` reads remain available as
optional context. All offsets count Unicode scalar values, not UTF-8 bytes.
Registering a new range removes affected conclusions and their dependents.
After reading the range, rebuild the missing records.
Invalid or unavailable requested context is a retained incomplete-evidence
failure, never a reason to publish a needs-review notice. Finalized assessments
allow replay of existing evidence but no new ranges.

Discussion is a compact semantic document, not approval bookkeeping. Preserve
the distinction between review bodies, comments, and replies. GitHub comments
use `hunkReference` to locate exact historical excerpts in the document's
`hunks` table; read every shared excerpt and interpret it using each comment's
own path and revision. Identical excerpt text does not merge comments.
The native `discussionDigest` binds the complete document. Do not reconstruct
discussion from the retained run or skip outdated or repeated objections.

Repeating `prepare` preserves reads and judgments. A replacement worker follows
the inventory and reacquires the evidence it needs; earlier receipts do not
establish that worker's knowledge. Retain judgments unless deliberately
replacing them with their current digests. Never edit the run JSON or recreate
the assessment to resume it. Direct `assessmentProgress` reports required
document and context delivery separately from changed-path judgments.
Transport fragment counts describe the manifest, not unfinished unit work.

```powershell
ss-ai autoreapprove units --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --after "{{AFTER}}" --limit "{{LIMIT}}" --reference "{{DOCUMENT}}"
ss-ai autoreapprove read-next --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --read-token "{{READ_TOKEN}}"
ss-ai autoreapprove read-next --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --read-token "{{READ_TOKEN}}" --change-id "{{CHANGE}}"
ss-ai autoreapprove checkpoint --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}"
```

Additional placeholders:

| Placeholder | Value |
| --- | --- |
| `DOCUMENT` | Optional original document filter, `discussion` or `CHANGE_ID:patch`; omit the option to enumerate every unit. Offsets apply to the selected list. |

Other placeholders retain their definitions above/below. Follow each returned
`nextOffset`. Unit metadata gives exact ranges and bindings, but direct
assessments need no unit or discussion notes. `read-next` derives bounded batches
and continuation offsets for policy, discussion, patches, and requested context.
Repeat `--change-id` to deliver policy and the selected changes' patches and
context without waiting for unrelated documents. An empty selected batch means
that selection is delivered, not that the whole assessment has coverage.
Read every returned page; complete delivery does not prove comprehension.
Replacements invalidate dependent records. `checkpoint`
is for a context-pressure handoff after new semantic work, not for each batch.

For a compact unit list, add `--compact`; each item retains its unit ID,
character count, evidence reference, binding reference, and read offset.
Read units by their returned `unit:` references and any separately returned
`binding:` references. Each unit starts at offset zero independently, so batches
using the same current `readToken` can be issued together. All units and their
bindings must be delivered to establish complete coverage of the original
document. A source-context registration changes the token; finish outstanding
batches before registering context or retry with the new token.

# Stage 3: judgments and completion

```powershell
ss-ai autoreapprove conclude --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --conclusions-json '{{CONCLUSIONS_JSON}}'
ss-ai autoreapprove synthesize --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --synthesis-json '{{SYNTHESIS_JSON}}'
ss-ai autoreapprove complete-partial --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --change-id "{{CHANGE}}" --rationale "{{RATIONALE}}"
```

| Placeholder | Value |
| --- | --- |
| `CONCLUSIONS_JSON` | Grouped semantic conclusions using native short references and the latest state token. |
| `SYNTHESIS_JSON` | Independent final outcome, combined reasoning, material discussion, and optional unresolved concerns or additional evidence. |
| `CHANGE`, `RATIONALE` | Examined significant change reference and evidence-backed partial-refusal synthesis. |

Use the input examples in [worker recipes](assessment-commands.md#stage-2-conclusions-and-completion).
Native code expands each group's members into exact evidence-bound coverage and
conditional replacements; it returns progress and the current state token.
One final synthesis binds all final conclusions, complete discussion, policy,
and requested context, validates coverage, and completes atomically. No separate
note, check enumeration, empty checks, or per-path essays are required.
Significant or uncertain members and unresolved concerns prevent renewal.
A replacement worker must reacquire evidence needed for its own reasoning.

Full synthesis requires all mandatory patches and requested ranges, policy,
discussion, and every change conclusion. `complete-partial` instead permits
only an examined significant refusal and explicitly retains incomplete coverage.
It cannot disguise missing evidence as significance or authorize approval.
Delivery, context, and worker limits remain incomplete work, not uncertainty.
Completion returns `needsReview`, `failed`, `wouldReapprove`, or
`readyToReapprove`; only the last permits publication.
Completed significant or uncertain assessments in a live run require the orchestrator's native `notify` command;
workers stop after completion and never publish. Dry runs do not notify.
An uncertain evaluation returns `failed` with a diagnostic and keeps the run
partial; it is not a completed no-work result.

# Stage 4: publication and recovery

```powershell
ss-ai autoreapprove refresh --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --pull-request-id "{{PR}}" --tenant "{{TENANT}}"
ss-ai autoreapprove publish --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --attribution "{{ATTRIBUTION}}" --tenant "{{TENANT}}"
ss-ai autoreapprove notify --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --attribution "{{ATTRIBUTION}}" --tenant "{{TENANT}}"
ss-ai autoreapprove results --state-path "{{RUN}}"
```

For `staleEvidence` or `staleDuringExecution` from any stage, stop the old PR
worker, read `results`, and call `refresh`. It accepts stale assessments
without a completed publication and performs no remote writes. It consumes
one of two persisted recovery attempts before reacquiring evidence; interrupted
capture resumes with ordinary `inspect` rather than consuming another attempt.
Follow stale refresh results with another `refresh`. Once the budget is used,
the next stale refresh request records `retryExhausted`, leaves the run partial,
and does not capture or write again. `staleRecovery` in each result item reports
`attempts`, `limit`, and `exhausted`. Never start another run to reset this budget.

Use the refreshed `snapshotDigest` and decision, not the original assessment.
If assessment is still required, dispatch a fresh worker to prepare, read, judge,
and complete from the new snapshot, then publish or notify that new assessment.
Old snapshot commands are rejected. Current approval standing, active notices,
and lost authority instead follow their returned nonassessment outcomes.
An uncertain assessment's stale notification remains recoverable and does not
erase its uncertainty diagnostic.

GitHub publication submits an approval bound to the assessed source SHA.
ADO preserves the account's positive vote strength using `isReapprove`;
the API has no atomic source-SHA precondition. A successful provider response
finishes the operation without later provider calls. Never substitute a direct
vote write. Report a request error or unknown outcome and do not automatically
resend it in the same attempt. Do not interpret a `failed` run as no remote change.
Publication or notification can also return no item when preflight finds a
draft; do not report or perform further work on that PR.

`notify` accepts only a completed significant or uncertain live assessment. It refreshes
source and decision evidence, posts an attributed
standalone needs-review comment with a hidden marker, and verifies the owned
comment. Repeating the original command reconciles the same intent without
posting again, including for a previous schema-14 or schema-15 run. This recovery does not
resume the previous assessment or change its run file. For that run, use the
`recoveryOnly` response to report the recovered notice; do not call `results`
on the previous run. Preserve an unknown comment attempt for reconciliation. Never call `notify` for
a dry run, an existing active marker, missing approval authority, or a tooling
failure without a completed assessment. Uncertain assessments retain their
failure diagnostic and partial run status after notification.

Follow every final `results` page. Each in-range item supplies its title and
URL for the linked summary, `assessmentReason` for a completed judgment,
`reviewNotice` (`commentId`, `createdAt`, `rationale`) for a current or retained
needs-review comment. `alreadyValid` does not identify who or what submitted the
account's approval.
