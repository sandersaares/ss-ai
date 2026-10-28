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
| `REFERENCE` | `policy`, `discussion`, or a returned change ID followed by `:old` or `:new`. |
| `OFFSET` | `nextOffset` from the previous evidence read, initially zero. |
| `CHANGE` | Exact returned change ID, or `cumulative`. |
| `DISPOSITION` | `insignificant`, `significant`, or `uncertain`. |
| `RATIONALE` | Concise semantic judgment grounded in the returned evidence. |
| `PREVIOUS` | Current `decisionDigest` or `cumulativeDigest` when deliberately replacing a judgment; omit the argument for a new judgment. |
| `ATTRIBUTION` | The user's required agent attribution, such as `[Copilot speaking]`. |

# Stage 1: discovery and history

```powershell
ss-skills-util autoreapprove discover --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --policy-path "{{POLICY}}" --tenant "{{TENANT}}" --dry-run
ss-skills-util autoreapprove results --state-path "{{RUN}}" --after "{{AFTER_PR}}"
ss-skills-util autoreapprove inspect --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --pull-request-id "{{PR}}" --tenant "{{TENANT}}"
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
ss-skills-util autoreapprove prepare --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}"
ss-skills-util autoreapprove assessment --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --after "{{AFTER_CHANGE}}"
ss-skills-util autoreapprove read --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --reference "{{REFERENCE}}" --offset "{{OFFSET}}"
```

Preparation returns the selected context and the first bounded metadata page.
Follow `nextOffset` through the assessment inventory. Read the captured policy
and discussion plus both readable sides of each changed source. Evidence
responses contain `text`, `complete`, and `nextOffset`; consume contiguous
pages until complete. A side without readable text cannot support an
insignificant decision. Do not substitute an arbitrary checkout or target tip.

# Stage 3: judgments and completion

```powershell
ss-skills-util autoreapprove judge --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --change-id "{{CHANGE}}" --disposition "{{DISPOSITION}}" --rationale "{{RATIONALE}}" --evidence "{{REFERENCE}}" --previous-digest "{{PREVIOUS}}"
ss-skills-util autoreapprove complete --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}"
```

Repeat `--evidence` for additional returned references. A change judgment must
cite that change. The cumulative judgment must cite both `policy` and
`discussion`. Omit `--previous-digest` for a new judgment; replacement requires
the current digest from `assessment` and rejects conflicting updates.
Missing work is not an empty successful assessment. Completion
returns `needsReview`, `failed`, `wouldReapprove`, or `readyToReapprove`;
only the last permits publication.
Completed significant or uncertain assessments in a live run require the orchestrator's native `notify` command;
workers stop after completion and never publish. Dry runs do not notify.
An uncertain evaluation returns `failed` with a diagnostic and keeps the run
partial; it is not a completed no-work result.

# Stage 4: publication and recovery

```powershell
ss-skills-util autoreapprove refresh --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --pull-request-id "{{PR}}" --tenant "{{TENANT}}"
ss-skills-util autoreapprove publish --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --attribution "{{ATTRIBUTION}}" --tenant "{{TENANT}}"
ss-skills-util autoreapprove notify --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --attribution "{{ATTRIBUTION}}" --tenant "{{TENANT}}"
ss-skills-util autoreapprove results --state-path "{{RUN}}"
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
