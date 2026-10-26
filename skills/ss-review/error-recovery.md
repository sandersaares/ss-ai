# Review workflow error recovery

This policy applies to `ss-review`, its bounded workers, and
`ss-review-proactor`. An unsuccessful command blocks dependent work, not
automatic recovery. Correct orchestration mistakes and retry safe transient
failures without asking the user. Never use an error as evidence of completion,
empty work, or approval.

# Stage 1: diagnose the failed operation

Check the exit status and diagnostic before using output. Retain the failed
command, absolute artifact paths, current phase, and any journal or receipt.
Distinguish these cases:

| Failure | Action |
| --- | --- |
| Orchestration mistake, such as a wrong input path, omitted argument, shell quoting error, or invalid agent-authored draft | Correct the diagnosed cause using the assignment, command recipe, and existing artifacts, then retry the affected operation when safe under stage 2. Do not ask the user to repair the agent's invocation. |
| Transient failure, such as throttling, a temporary connection or service failure, or lock contention | Use the command's documented retry handling first. Retry safe reads or local operations within the bounded budget below, honoring service retry guidance and waiting for a known competing command to finish. |
| Unrecoverable external failure, such as HTTP 403 permission denial or authentication requiring user action | Stop the affected workflow and report the diagnostic and required action. Do not repeatedly retry, change identity, bypass permissions, or install tools without the required consent. A documented command-specific exception still applies. |
| Unknown outcome of a remote write, inconsistent durable state, or a required human decision | Apply stage 2 before any retry. Preserve the state and pause when reconciliation or a real user's answer is required. |

For an orchestration mistake, verify input/output roles before retrying. A
missing intended output is not a missing prerequisite. If an actual prerequisite
is missing, resume its authorized producer when safe rather than fabricate an
empty artifact. Repair invalid drafts through the maintained writers and
validators; do not weaken schemas, erase progress, or reconstruct journals from
memory.

For transient failures without a command-specific budget, allow at most three
total attempts at the affected safe operation. This allows temporary disruption
to clear without creating an indefinite retry loop. Do not reset the budget by
replacing a worker. For orchestration mistakes, retry only after a concrete
correction; an unchanged repeated error requires a new diagnosis, not blind
repetition. If no safe recovery remains, report what was attempted and the
remaining blocker rather than claim success.

# Stage 2: establish whether a retry is safe

Local reads and PR-context queries may be retried after correction. Keep all
evidence access within `pwsh-calls/query-pr-context.md`; an error never permits a
raw capture read, alternate provider fetch, or unbounded page accumulation.
Use the saved reader's replay or explicit restart rules, not a guessed cursor.
Local writes use their documented idempotent retry or resume procedure and
preserve findings, classifications, human feedback, and coverage.

Before repeating a command capable of external writes, establish from the
failure stage and maintained journal or receipt whether any write was attempted.
A timeout, lost response, absent output, or missing receipt does not prove
that nothing reached the service. A read-only step inside a write-capable
command does not make the whole command safe to restart.

Retry a corrected pre-write invocation only when evidence establishes that no
remote write was attempted and the command's lifecycle permits re-entry.
Honor documented transport retries for definite refusals, such as explicit
rate-limit responses. Never blindly replay a possibly successful claim,
reviewer registration, overview publication, review submission, or tracker
creation. Preserve claim journals and publication records; do not delete, move
aside, or replace them to enable a retry. Where the command requires human
reconciliation after an attempted or ambiguous write, retain that requirement.
Successful publication is not replayed to repair a later cleanup failure.

# Stage 3: resume only the affected work

After recovery, verify the required output and continue from the failed step,
not from selection or the start of the review. Keep unrelated preparation and
review workers running unless their own inputs are invalid or stopping them is
necessary for safe repair. A bounded worker that cannot recover reports the
diagnostic, attempted corrections, paths, and unresolved work to the primary
orchestrator; it does not ask the user or cancel other workers. The primary
orchestrator applies this same policy before escalating.

Human approval gates remain hard suspension points. Recovery cannot supply
audit decisions, merge approval, installation consent, or publication
adjudication. Preserve explicit best-effort outcomes, such as overview
publication limitations and Markdown rendering fallback, instead of turning
them into a failed review.
