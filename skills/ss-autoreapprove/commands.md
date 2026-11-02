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
Inspect only recent nondraft entries requiring assessment. Retain each
inspection's `snapshotDigest`. Scope failures stop the run; independent PR
failures remain visible while other eligible PRs can proceed.
Inspection or refresh can return no item when a PR has become a draft; do not
assess or report that PR.

# Stage 2: preparation and evidence

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai autoreapprove prepare --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}"
ss-ai autoreapprove start-worker --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}"
ss-ai autoreapprove assessment --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --after "{{AFTER_CHANGE}}"
ss-ai autoreapprove read-batch --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --batch-json '{{BATCH_JSON}}'
ss-ai autoreapprove read --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --reference "{{REFERENCE}}" --offset "{{OFFSET}}"
ss-ai autoreapprove source-context --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --change-id "{{SOURCE_CHANGE}}" --side "{{SIDE}}" --start-line "{{START_LINE}}" --line-count "{{LINE_COUNT}}"
```

Preparation returns the selected context and the first bounded metadata page.
Its `progress` provides preflight counts. `readToken` identifies immutable
evidence and registered context; `stateToken` identifies the current assessment
state. Reserve each worker with `start-worker` before
dispatch; its returned `assessmentBudget` shows reservations and limits.
Follow `nextOffset` through the assessment inventory, even when a page contains
fewer changes than requested. Native metadata and evidence responses fit within
16 KiB of pretty-printed UTF-8 JSON, including the final newline. The read
character limit is an upper bound; JSON escaping and multibyte text can shorten
a page. Do not print the retained run. If a tool truncates delivery, reduce
the requested batch and replay the same offsets before continuing.

A batch request has the shape
`{"readToken":"TOKEN","items":[{"reference":"policy","offset":0,"limit":1024},{"reference":"discussion","offset":0,"limit":1024}]}`.
Use distinct references and positive limits. The response has `items`, a new
`stateToken`, the `readToken`, and `progress`. Each item carries its own
`nextOffset` and `contentDigest`. Multiple batches may use the same read token
for independent references without waiting for receipt updates. A continuation
of a mutable semantic record must also supply the preceding `contentDigest`;
ordinary `read` accepts only its first page, so use `read-batch` for further
pages. Registering source context changes the read token. A batch
whose combined response exceeds the native limit fails without committing any
receipts; retry smaller batches. If the host truncates a successful response,
reacquire a fresh `readToken` with `assessment` and repeat those pages before
proceeding. Repeated pages do not prove that the worker understood them.

`documentReads` reports `reference`, `totalCharacters`, nullable `readOffset`,
and `complete` for policy and discussion. Each present source side exposes its
`content.kind`; text includes the same `content.progress` fields. Null read
offset means no receipt, including for an empty document. Use this progress
to find missing work, not to infer another worker's knowledge.
`pendingChangeCount` and `nextChangeOffset` locate unjudged source.
Each change exposes `requiredPatch` with the same progress fields and `hunkCount`.
Read the captured policy, discussion, and every required patch document. Evidence
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
Registering a new range removes that change's judgment, its dependents, the
discussion observation, check dispositions, and the cumulative judgment. After
reading the range, rebuild the missing records.
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
ss-ai autoreapprove note --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --unit-id "{{UNIT}}" --observations "{{OBSERVATIONS}}" --interfaces "{{INTERFACES}}" --evidence "{{REFERENCE}}" --concern "{{CONCERN}}" --interaction "{{INTERACTION}}" --previous-digest "{{PREVIOUS}}"
ss-ai autoreapprove checks --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --after "{{AFTER}}" --limit "{{LIMIT}}"
ss-ai autoreapprove resolve --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --check-id "{{CHECK}}" --disposition "{{DISPOSITION}}" --rationale "{{RATIONALE}}" --evidence "{{REFERENCE}}" --previous-digest "{{PREVIOUS}}"
ss-ai autoreapprove checkpoint --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}"
```

Additional placeholders:

| Placeholder | Value |
| --- | --- |
| `DOCUMENT` | Optional original document filter, `discussion` or `CHANGE_ID:patch`; omit the option to enumerate every unit. Offsets apply to the selected list. |
| `UNIT` | Native unit ID, or `discussion` for the final discussion aggregate. |
| `OBSERVATIONS` | Concise behavioral observations, preserving relevant objections. |
| `INTERFACES` | Relevant interfaces/invariants and their relationship to the evidence. |
| `CONCERN`, `INTERACTION` | Explicit obligation to examine; repeat each option as needed, or omit it when there are none. |
| `CHECK` | Native concern or interaction ID returned by `checks`. |

Other placeholders retain their definitions above/below. Follow each returned
`nextOffset`. Unit metadata gives exact ranges and bindings, but direct
assessments need no unit notes. Record one `discussion` observation from the
complete discussion and policy; include material concerns and interactions.
All check resolutions require exact evidence, the originating semantic record,
and the check itself. Replacements invalidate dependent records. `checkpoint`
is for a context-pressure handoff after new semantic work, not for each batch.

# Stage 3: judgments and completion

```powershell
ss-ai autoreapprove judge --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --change-id "{{CHANGE}}" --disposition "{{DISPOSITION}}" --rationale "{{RATIONALE}}" --evidence "{{REFERENCE}}" --previous-digest "{{PREVIOUS}}"
ss-ai autoreapprove judge-batch --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --batch-json '{{BATCH_JSON}}'
ss-ai autoreapprove complete --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}"
```

A judgment batch has the shape
`{"stateToken":"TOKEN","items":[{"id":"CHANGE_ID","judgment":{"disposition":"insignificant","rationale":"Evidence-based explanation","evidence":["CHANGE_ID:patch"],"concerns":[],"interactions":[]},"previousDigest":null}]}`.
Each item names a distinct changed path; `cumulative` is not allowed in a
batch. Use the latest `stateToken`. Each replacement needs its current
`decisionDigest`; use null for a new judgment. The utility validates and
commits the entire batch atomically and returns each digest, a new token, and
progress. On a stale token or any invalid item no judgments are committed.
Use the ordinary `judge` command for the independently reasoned cumulative
judgment.

Repeat `--evidence` for additional returned references. A change judgment must
cite its `CHANGE_ID:patch`. The cumulative judgment must cite both `policy` and
`discussion`. Repeat optional `--concern` and `--interaction` on a change judgment
for explicit cumulative obligations. The cumulative judgment cannot introduce
unchecked obligations; record them on the relevant unit or change first.
Every change judgment requires complete policy, required patch,
and applicable requested context reads, regardless of disposition. Cited optional
evidence also requires complete delivery. A cumulative judgment binds all final
path judgments, the discussion observation, every check and disposition, and
all requested context. A replacement worker must read what it needs to reason
independently rather than treating those bindings as comprehension.
Opaque source can retain uncertainty, but
unexamined readable content cannot become a completed semantic refusal.
Omit `--previous-digest` for a new judgment; replacement requires
the current digest from `assessment` and rejects conflicting updates.
Missing work is not an empty successful assessment. Completion
requires all readable evidence for every disposition: mandatory patches,
requested ranges, policy, and discussion. Delivery, context, and
worker limits cannot be hidden behind notes. Final source
judgments, discussion observation, check dispositions, and an independent
cumulative judgment are mandatory. Native delivery does not prove comprehension
or that notes captured every implication. Delivery, context, and
worker limits leave incomplete work; do not loop over the inventory to invent
uncertain judgments. An `evidenceDelivery` diagnostic is a tooling failure,
not a completed nonqualifying assessment.
Completion
returns `needsReview`, `failed`, `wouldReapprove`, or `readyToReapprove`;
only the last permits publication.
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
posting again, including for a previous schema-14 run. This recovery does not
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
