# Native command recipes

Run commands directly through the installed executable. Expand named
placeholders rather than assembling JSON or writing state-management scripts.
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
ss-ai autoreapprove assessment --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --after "{{AFTER_CHANGE}}"
ss-ai autoreapprove read --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --reference "{{REFERENCE}}" --offset "{{OFFSET}}"
ss-ai autoreapprove source-context --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --change-id "{{SOURCE_CHANGE}}" --side "{{SIDE}}" --start-line "{{START_LINE}}" --line-count "{{LINE_COUNT}}"
```

Preparation returns the selected context and the first bounded metadata page.
Follow `nextOffset` through the assessment inventory, even when a page contains
fewer changes than requested. Native metadata and evidence responses fit within
16 KiB of pretty-printed UTF-8 JSON, including the final newline. The read
character limit is an upper bound; JSON escaping and multibyte text can shorten
a page. Consume one response per tool call rather than printing concatenated
pages or the retained run. If a tool truncates delivery, lower `--limit` and
replay the same offset before continuing.

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

Repeating `prepare` on the same snapshot preserves recorded reads and judgments.
A replacement worker follows the inventory from the first page, checking each
`decision` and `decisionDigest` and the `cumulative` judgment. Reread evidence needed for your own judgment
from offset zero and follow its returned offsets; overlapping reads are allowed.
Native read coverage does not establish that a replacement worker has seen the evidence.
Finish missing work and retain existing judgments unless deliberately replacing them
with their current digests. Never edit the run JSON or recreate the assessment
to resume it.
For a planned source-unit handoff, the next worker starts at `nextChangeOffset`
and reads only the missing changes it will judge. The final cumulative worker
reads recorded judgments through every bounded page and acquires its own policy,
discussion, and any source evidence needed for interactions. Native `results`
exposes `assessmentProgress` with `changeCount`, `judgedChangeCount`, and nullable
`nextChangeOffset`, without source content or change rationales, for orchestration
to verify forward progress.

# Stage 3: judgments and completion

```powershell
ss-ai autoreapprove judge --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --change-id "{{CHANGE}}" --disposition "{{DISPOSITION}}" --rationale "{{RATIONALE}}" --evidence "{{REFERENCE}}" --previous-digest "{{PREVIOUS}}"
ss-ai autoreapprove complete --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}"
```

Repeat `--evidence` for additional returned references. A change judgment must
cite its `CHANGE_ID:patch`. The cumulative judgment must cite both `policy` and
`discussion`. Every change judgment requires complete policy, required patch,
and applicable requested context reads, regardless of disposition. Cited optional
evidence also requires complete delivery. A cumulative judgment requires
complete policy, discussion, and all requested context reads. Opaque source can retain uncertainty, but
unexamined readable content cannot become a completed semantic refusal.
Omit `--previous-digest` for a new judgment; replacement requires
the current digest from `assessment` and rejects conflicting updates.
Missing work is not an empty successful assessment. Completion
requires all readable evidence for every disposition: mandatory patches,
requested ranges, policy, and discussion. Delivery, context, and
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
worker, read `results`, and call `refresh`. It accepts only stale PRs without an
attempted approval or notice effect and performs no remote writes. It consumes
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

An attempted or unknown effect blocks refresh, including accepted-but-stale
publication. Reconcile only through the original `publish` or `notify` command.
Keep the original run and snapshot for the reconciliation below;
do not treat a null publication or review ID alone as proof of no remote effect.
Only stale evidence qualifies for this retry path; unrelated failures do not.

GitHub publication creates a source-bound submitted approval and audit body.
ADO publication preserves the account's positive vote strength using
`isReapprove`, verifies a unique new structured vote on the assessed iteration,
then creates a separate authenticated audit carrying the original anchor.
ADO has no atomic source-SHA vote precondition; changed evidence produces an
explicit stale result rather than rollback. Clean preflight staleness uses
bounded refresh; accepted-but-stale results retain their original effects.
Never substitute a direct vote write.
Read the outcome and any publication error, including accepted-but-stale
effects. Do not interpret a `failed` run as no remote change.
Publication or notification can also return no item when preflight finds a
draft; do not report or perform further work on that PR.

The same publication command reconciles an interrupted attempt using its
original run and assessment. It never repeats an effect that might have
succeeded. Recovery may finish an unattempted ADO audit or retry an explicitly
refused audit after fresh vote verification, but never repeats an uncertain effect.
Unknown effects block later runs on the same publishing host.
Do not alter journals, delete retained locks, or issue manual retry writes.

`notify` accepts only a completed significant or uncertain live assessment. It refreshes
source and decision evidence under the publication lock, posts an attributed
standalone needs-review comment with a hidden marker, and verifies the owned
comment. Repeating the original command reconciles the same intent without
posting again. An unknown comment effect blocks later approval writes on this
host; preserve the original run for reconciliation. Never call `notify` for
a dry run, an existing active marker, missing approval authority, or a tooling
failure without a completed assessment. Uncertain assessments retain their
failure diagnostic and partial run status after notification.

Follow every final `results` page. Each in-range item supplies its title and
URL for the linked summary, `assessmentReason` for a completed judgment,
`reviewNotice` (`commentId`, `createdAt`, `rationale`) for a current or retained
needs-review comment, and `automaticApproval` (`approvalEventId`, `approvedAt`)
only for valid automatic approval standing. Null automatic metadata with
`alreadyValid` means an external approval, not an automatic one.
