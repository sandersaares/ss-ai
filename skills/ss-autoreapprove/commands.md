# Native command recipes

Run commands directly through the installed executable. Expand named
placeholders rather than assembling JSON or writing state-management scripts.
All commands return JSON; nonzero exits and stderr diagnostics are explicit
failures. Let an in-flight command finish rather than starting a duplicate.

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
An uncertain evaluation returns `failed` with a diagnostic and keeps the run
partial; it is not a completed no-work result.

# Stage 4: publication and recovery

```powershell
ss-skills-util autoreapprove publish --repository-url "{{REPOSITORY}}" --state-path "{{RUN}}" --pull-request-id "{{PR}}" --snapshot-digest "{{SNAPSHOT}}" --attribution "{{ATTRIBUTION}}"
ss-skills-util autoreapprove results --state-path "{{RUN}}"
```

ADO supports read-only inspection and source assessment, not publication.
Report the unsupported publication transaction rather than using a direct vote write.
GitHub publication creates a source-bound submitted approval and audit body.
Read the outcome and any publication error, including accepted-but-stale
effects. Do not interpret a `failed` run as no remote change.

The same publication command reconciles an interrupted attempt using its
original run and assessment. It never repeats an effect that might have
succeeded. Unknown effects block later runs on the same publishing host.
Do not alter journals, delete retained locks, or issue manual retry writes.
