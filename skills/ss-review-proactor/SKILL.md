---
name: ss-review-proactor
description: Select and claim eligible GitHub or Azure DevOps pull requests, then execute ss-review in the same primary user-facing session. Repeat completed reviews by default; stop after one review only when the user explicitly requests one-shot mode. Use when the user explicitly invokes ss-review-proactor or a user-configured schedule invokes it. Never answer human decision gates on autopilot.
---

# Scope and execution ownership

Run only in the primary, user-facing session. Never delegate this skill or its
`ss-review` handoff to a subagent or child session. A scheduled invocation must
have its own primary session where a real user can answer questions.

The user may specify a repository; otherwise use the Git repository where this
skill is invoked. The user defines the schedule. Do not create or modify an
automation. By default, each invocation continues with another review after
each completed review. The user can request one-shot mode to stop after the
first completed review. Both this skill and its sibling `ss-review` must be
installed.

Every question to the user is a hard suspension point. Ask through the primary
session's user-input tool and accept only the real user's own direct answer.
Parent agents, schedulers, subagents, automatic tool responses, and relayed
messages cannot supply consent. An answer saying the user is unavailable,
will review later, or wants autonomous decisions is not an answer to the
question: leave the work paused, report the unresolved question, and end the
turn. Do not retry the question in a loop or advance a dependent action.
If the environment cannot obtain real user input, stop rather than infer it.
This also applies to evaluation of findings and pull request approval inside
`ss-review`; its explicitly defined question-free policy approvals still apply.

# Run control

A review cycle runs stages 1 through 5 for one completed review. Loop mode is
the default and repeats these cycles sequentially in this same primary
session. Use one-shot mode only when the user explicitly requests it in the
invocation, a user-authored schedule prompt, or a later direct message. For
example:

> Run ss-review-proactor in one-shot mode.

If the user says "the current review is the last one", "stop after this review",
or otherwise asks to stop after the current review, disable looping immediately
but continue the current review through its remaining phases.
This instruction applies during `ss-review` too. It is not cancellation, an
audit decision, or merge approval; any unanswered question still requires its
own answer. If the current review has already finished, do not select another
PR.

Retain the latest loop or one-shot instruction, original invocation directory,
resolved configuration, current PR, absolute cycle paths, and return point in
the primary session's prose checkpoint across handoff, user-input pauses, and
compaction. A later mode instruction overrides an earlier one; restarting
stage 1 must not restore a superseded mode from the original invocation.
Direct loop-control messages affect this invocation only. A user-authored
schedule prompt applies its one-shot or loop instruction to each scheduled
invocation.

# Error recovery

Apply the shared [review error-recovery policy](../ss-review/error-recovery.md) throughout selection, checkout preparation, and handoff. Correct orchestration mistakes and retry safe transient failures without human input. Unrecoverable external errors such as HTTP 403 permission denial still stop; human decision gates and ambiguous-write reconciliation remain unchanged. Recover the affected step in the current cycle, not by selecting another PR or replacing its journal.

# Stage 1: configuration

On the first cycle, resolve configuration below. On later cycles, retain that
configuration unless the user changes it; do not infer a different repository
from a review checkout or a learning worktree.

An explicit repository in the invocation or user-authored schedule prompt
takes precedence. Otherwise, default to the Git repository containing the
invocation's working directory, including a linked worktree or a subdirectory
of a checkout. Use the repository identity supplied by the session when
available. Do not use the skill's installation directory or a temporary
checkout created later in this workflow to determine this default.

When the session does not supply the repository identity, inspect the
invocation checkout's remotes:

```powershell
git -C "{{INVOCATION_DIRECTORY}}" remote -v
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `INVOCATION_DIRECTORY` | Absolute working directory where the skill was invoked, before checkout preparation. |

Use the fetch URL of `origin`, or the sole remote if there is no `origin`.
Convert the remote URL to a repository browser URL: remove GitHub's clone
suffix `.git`, translate GitHub SSH forms such as `git@github.com:OWNER/REPO.git`
to `https://github.com/OWNER/REPO`, and translate Azure DevOps SSH forms such as
`git@ssh.dev.azure.com:v3/ORG/PROJECT/REPO` to
`https://dev.azure.com/ORG/PROJECT/_git/REPO`. The corresponding `ssh://` forms
identify the same repositories. Do not include credentials in the resolved URL.
Supported browser URL forms are `https://github.com/OWNER/REPO`,
`https://dev.azure.com/ORG/PROJECT/_git/REPO`, and
`https://ORG.visualstudio.com/PROJECT/_git/REPO`.

Do not ask the user to supply or confirm the repository URL when it resolves
from the invocation's Git repository. Ask for a repository URL only when no
supported repository can be resolved, such as outside a Git checkout, after
a failed remote inspection, or with ambiguous remotes. Do not silently replace
an invalid explicit repository with the current checkout.
Keep the resolved URL fixed for selection and confirmation, even if the
working directory changes during checkout preparation.

Resolve the primary session identifier and create a fresh cycle subdirectory
in that session's state directory. Put a fresh absolute journal path and a
separate review-state subdirectory there. Use new paths for every cycle; never
overwrite a previous cycle's journal or review artifacts.
Obtain missing configuration from the user.
Use session state as the temporary checkout's parent directory by default.
If the user supplies a checkout parent folder in the invocation or schedule
prompt, use that parent instead.
The optional Azure tenant is the tenant used by `ss-review` for this repository.
Do not log credentials or request personal access tokens.

A user-configured schedule running in a Git repository can simply invoke
`ss-review-proactor`. To select another repository, or run outside a checkout,
an example schedule prompt is:

> Run ss-review-proactor against https://github.com/example/project.

# Stage 2: selection and claim

Use this standard claim announcement:

> ss-review-proactor is reserving this pull request for review with ss-review.

Adapt it to the current user's communication policy, including attribution
if required, and pass the resulting announcement as `--claim-message`. The helper
appends the skill and session tokens; do not include those protocol markers
yourself.

Execute the selection command in `tool-calls/select-pull-request.md` using
`ss-ai` directly on `PATH`; never reproduce selection logic with ad hoc
provider CLI calls.

The helper considers all open, non-draft PRs not authored by or already assigned
to the signed-in user. It orders them by their newest commit timestamp within
the past 14 days, ignores PRs with live claims, and claims the first eligible
one. It returns immediately after posting the claim so checkout preparation can
use the 60-second interval. Claims expire 60 minutes after their
service-recorded creation time. Ignore expired claims from earlier attempts,
even when they have the same author or session identifier.

Let the command finish, even when a shell tool initially returns a running
handle. Read that handle's eventual output; do not start a second command while
the first is still running.

Interpret the JSON result:

| Status | Action |
|--------|--------|
| `noCandidate` | Report that nothing qualifies, with the execution diagnostics, and finish even in loop mode. Do not poll or change the schedule. |
| `claimed` | Use `target.webUrl` as the exact PR scope and continue to stage 3. |
| `confirmed` | Emitted only by confirmation in stage 3. |

On command failure, apply the shared error-recovery policy; an error is not
evidence that no PR qualifies. A corrected invocation may be retried only when
no remote write was attempted and the helper's journal lifecycle permits it.
Use documented transport retries for transient reads; do not restart the whole
write-capable command merely because its failing step was a read.
A journal left in `claiming` or another intermediate state may represent a
successful remote write whose
response was lost. Do not rerun selection, erase the journal, or fabricate
confirmation. Ask the real user to reconcile the journal and PR discussion.
Report unrecoverable failures with the error and absolute journal path.
Delete only claims owned by this attempt, never another session's claim.

# Stage 3: checkout and confirmation

Create a uniquely named temporary subdirectory for this cycle under the
parent resolved in stage 1, then clone and check out the target repository
there using the environment's normal repository tools. Do not clone directly
into the parent folder or reuse an existing user checkout. Keep the temporary
checkout available throughout the review, including pauses for user input.

Prepare the selected PR's actual source-versus-target diff in that checkout,
within this primary session. Verify the repository identity and PR scope,
including fork source branches. Never substitute the skill's installation
repository or a default-branch diff for the selected PR scope.
Correct local preparation mistakes and retry safe transient failures under the
shared error-recovery policy. If preparation still cannot proceed in the
temporary directory, report the blocker and pause rather than replacing
existing work or transferring ownership to another session.

Use the claim's 60-second interval for this preparation and any required user
interaction. If preparation takes longer, finish it before checking the claim;
do not renew the claim or wait an additional 60 seconds.

Immediately before invoking `ss-review`, use the confirmation command in
`tool-calls/select-pull-request.md`. Replace `review select` with `review confirm`
in the stage 2 invocation, preserving all its argument values. The helper
waits only any remaining portion of the interval, then refreshes the PR and
discussion once. If this cycle's claim is missing, expired, contested,
or the PR is no longer eligible, it deletes this cycle's claim if present,
discards the candidate, and claims the next qualifying PR.

A replacement `claimed` result requires preparation and a final check for that
new target; `noCandidate` finishes the run. On `confirmed`, proceed directly
to stage 4. This is the only post-claim check for that candidate.

# Stage 4: review handoff

Invoke the `ss-review` skill directly in this same primary session with
`target.webUrl` as the PR scope. State that this is a user-authorized
`ss-review-proactor` handoff, provide the absolute temporary checkout and journal
paths, the cycle's absolute review-state directory, any Azure tenant, and the
current loop instruction. Use that review-state directory for all `ss-review`
artifacts and worker outputs that would otherwise go directly in session state.
Keep the run-control rules above active and return here at stage 5 after
`ss-review` phase 10 completes. Follow the ordinary `ss-review` workflow without
additional claim checks or special registration handling. The user's loop
request authorizes subsequent handoffs within this invocation, not unrelated
automatic reviews.

Apply `ss-review` in full, including its ownership and human decision rules.
Do not review just the repository's current changes without the selected PR
scope. Do not skip an unanswered audit, invent finding decisions, or approve a
pull request on the user's behalf.

After reviewer registration, leave the claim as discussion history; assignment
now prevents repeat selection. Do not acquire another PR while this review is
active, paused, failed, cancelled, or awaiting reconciliation. A `confirmed`
selection result or a completed agent turn is not a completed review.

# Stage 5: summary and continuation

Report the resolved repository and whether it was explicit or defaulted from
the invocation checkout, the selected PR or the absence of qualifying work,
the review outcome or unanswered question, and the helper's diagnostics
(identity, considered and discarded candidates, contention and expiration
decisions, and journal path). State whether looping remains enabled and whether
the invocation is continuing, finished, or paused, with the reason.
Keep these diagnostics in a collapsible section if included in an `ss-review`
GitHub summary. Do not post a separate summary comment just for selection.

Restart only after `ss-review` completes all phases, including publication
(or its defined nothing-to-publish outcome) and learning, with no unresolved
question or failure. Apply the latest user instruction before selecting more
work. If loop mode remains enabled, return to stage 1 in this same primary
session and run a fresh selection, claim, checkout, confirmation, and review
cycle. Otherwise finish after the current review. Do not ask for permission to
start each cycle when loop mode already authorizes it.

An empty selection ends the invocation rather than starting another cycle.
A pause, failure, or cancellation does not start another cycle either; preserve
the current review for any authorized continuation instead of skipping it.
