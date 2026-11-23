---
name: ss-review-proactor
description: Select and claim eligible GitHub or Azure DevOps pull requests across one or more repositories, then invoke ss-review directly or through delegated execution. Repeat completed reviews by default; stop after one review only when the user explicitly requests one-shot mode. Use when the user explicitly invokes ss-review-proactor or a user-configured schedule invokes it. Never answer human decision gates on autopilot.
---

# Scope and execution ownership

Run this skill in the primary, user-facing session so a real user can answer
questions. The review handoff may invoke `ss-review` in this session or delegate
it to a subagent or child session while this session retains user interaction
and loop control.

The user may specify one or more repositories in the invocation or schedule
prompt; otherwise use the Git repository where this skill is invoked. Process
the explicitly ordered repository list sequentially. The user may also select
the `assigned` selection mode when they want only pull requests where the
signed-in user is assigned as a reviewer and has not submitted a review. The
default `available` mode finds pull requests not already assigned to that user.
The selection mode applies to every configured repository for the invocation.
The user defines the schedule. Do not create or modify an automation. By
default, each invocation
continues reviewing the current repository until selection reports no candidate,
then advances to the next repository. The user can request one-shot mode to stop
after the first completed review. Both this skill and its sibling `ss-review`
must be installed.

Every question to the user is a hard suspension point. Ask through the primary
session's user-input tool and accept only the real user's own direct answer.
A caller may relay that exact answer from a delegated `ss-review` invocation;
no parent agent, scheduler, subagent, automatic tool response, or other
intermediary may supply consent. An answer saying the user is unavailable,
will review later, or wants autonomous decisions is not an answer to the
question: leave the work paused, report the unresolved question, and end the
turn. Do not retry the question in a loop or advance a dependent action.
If the environment cannot obtain real user input, stop rather than infer it.
Treat a skipped, dismissed, cancelled, timed-out, or unrecognized response,
including an explicit `[Skipped by user]` result, as no answer. After asking
any question, do not start another cycle or perform a dependent review,
publication, or state-transition action until the real user supplies an exact
recognized answer.
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
ordered repository list, current repository, exhausted repositories, resolved
configuration, selection mode, current PR, absolute cycle paths, and return
point in the primary session's prose checkpoint across handoff, user-input
pauses, and compaction. Represent one **active review cycle** in the checkpoint.
Stages 1 through 3 may construct and populate a pending identity: stage 1 binds
the repository URL and fresh cycle paths, stage 2 adds the selected pull
request URL and number, and stage 3 adds the checkout and confirmation result.
The fully established identity also contains the selection journal,
review-state directory, and current stage. Keep completed cycle records as
immutable history, never as the active cycle.

Stages 1 through 3 may start when no active cycle exists, but they must first
create the pending identity and update that same record from selection and
confirmation results. From stage 4 onward, require an existing, matching
identity before resuming, dispatching work, or running a command. Verify that
every pull request identity and artifact path belongs to that single cycle. Do
not combine paths or queued actions from different cycles in one command,
worker batch, or turn. If the checkpoint and established cycle journal disagree,
or more than one cycle could be current, stop; perform only read-only
diagnostics and ask the user to resolve the identity if needed. A skipped or
unanswered identity question blocks all review work.

A later loop-mode instruction overrides an earlier one; restarting stage 1
must not restore a superseded mode from the original invocation.
Direct loop-control messages affect this invocation only. A user-authored
schedule prompt applies its one-shot or loop instruction to each scheduled
invocation.

# Error recovery

Apply the shared [review error-recovery policy](../ss-review/error-recovery.md) throughout selection, checkout preparation, and handoff. Correct orchestration mistakes and retry safe transient failures without human input. Unrecoverable external errors such as HTTP 403 permission denial still stop; human decision gates and ambiguous-write reconciliation remain unchanged. Recover the affected step in the current cycle, not by selecting another PR or replacing its journal.

# Stage 1: configuration

On the first cycle, resolve the configuration below. Resolve every explicitly
supplied repository before starting selection so an invalid later entry cannot
leave an unintended partial run. Preserve the user's repository order and ignore
an exact duplicate URL after its first occurrence. On later cycles, retain that
ordered repository list and the current position unless the user changes them;
do not infer a different repository from a review checkout or a learning
worktree.

One or more explicit repositories in the invocation or user-authored schedule
prompt take precedence. Otherwise, form a one-entry repository list from the
Git repository containing the invocation's working directory, including a
linked worktree or a subdirectory of a checkout. Use the repository identity
supplied by the session when available. Do not use the skill's installation
directory or a temporary checkout created later in this workflow to determine
this default.

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

Do not ask the user to supply or confirm the repository URL when the one-entry
list resolves from the invocation's Git repository. Ask for repository URLs only
when no supported repository can be resolved, such as outside a Git checkout,
after a failed remote inspection, or with ambiguous remotes. Do not silently
replace an invalid explicit repository with the current checkout. Keep every
resolved URL and the active URL fixed for selection and confirmation, even if
the working directory changes during checkout preparation.

Resolve the primary session identifier and create a fresh cycle subdirectory
in that session's state directory. Put a fresh absolute journal path and a
separate review-state subdirectory there. Use new paths for every cycle; never
overwrite a previous cycle's journal or review artifacts. If no active cycle
exists, record this repository and these fresh paths as the pending cycle
identity before issuing any selection command. An interrupted stage 1 may
establish this record before it retries its remaining preparation.
Resolve an Azure tenant for each Azure DevOps repository. A tenant explicitly
associated with one repository applies only to that repository; a clearly
stated shared tenant applies to repositories that have no specific value.
Obtain missing configuration from the user.
Use session state as the temporary checkout's parent directory by default.
If the user supplies a checkout parent folder in the invocation or schedule
prompt, use that parent instead.
Resolve the selection mode from the invocation or user-authored schedule prompt.
Use `assigned` only when the user explicitly asks to review pull requests
assigned to the signed-in reviewer. Otherwise use `available`. Preserve this
mode across configuration and every cycle in the invocation.
Pass the active repository's optional tenant to selection and `ss-review`.
Do not log credentials or request personal access tokens.

A user-configured schedule running in a Git repository can simply invoke
`ss-review-proactor`. To select another repository, run outside a checkout, or
select multiple repositories in priority order, example schedule prompts are:

> Run ss-review-proactor against https://github.com/example/project.

> Run ss-review-proactor against https://github.com/example/project, then https://dev.azure.com/example/team/_git/other.

To select only pull requests already assigned to the signed-in user, use a
prompt such as:

> Run ss-review-proactor in assigned mode.

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

In `available` mode, the helper considers open, non-draft PRs not authored by
or already assigned to the signed-in user. In `assigned` mode, it instead
requires an assignment to the signed-in user and excludes an existing active
submitted review. It orders qualifying PRs by their newest commit timestamp
within the past 14 days, ignores PRs with live claims, and claims the first
eligible one. It returns immediately after posting the claim so checkout
preparation can use the 60-second interval. Reserved claims expire 60 minutes
after their service-recorded creation time. Ignore expired reserved claims from
earlier attempts, even when they have the same author or session identifier.

Let the command finish, even when a shell tool initially returns a running
handle. Read that handle's eventual output; do not start a second command while
the first is still running.

Interpret the JSON result:

| Status | Action |
|--------|--------|
| `noCandidate` | Mark the active repository exhausted. If another repository remains, retain the execution diagnostics and repeat selection for that next repository. After the list is exhausted, report that no repository has qualifying work and finish, even in loop mode. Do not poll or change the schedule. |
| `claimed` | Use `target.webUrl` as the exact PR scope and continue to stage 3. |
| `confirmed` | Emitted only by confirmation in stage 3. |

On `claimed`, update the pending cycle identity with the result's exact pull
request URL and number before preparing its checkout. Do not replace that
identity while confirming a replacement candidate; update the same pending
cycle from the replacement result, then refresh its checkout and retained
candidate-specific artifact paths before using the replacement target.

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
On success, confirmation changes the reserved marker to an active reservation.
The active marker remains visible across checkout work, human-input pauses, and
the complete `ss-review` handoff.

A replacement `claimed` result requires preparation and a final check for that
new target. `noCandidate` exhausts the active repository and advances to the
next repository; it finishes the invocation only after every repository is
exhausted. On `confirmed`, proceed directly to stage 4. This is the only
post-claim check for that candidate. Add the confirmation result and checkout
path to the pending cycle identity, making it the established active review
identity for stage 4.

# Stage 4: review handoff

Invoke the `ss-review` skill with `target.webUrl` as the PR scope, either in
this session or through delegated execution. State that this is a
user-authorized `ss-review-proactor` handoff, provide the absolute temporary
checkout and journal paths, the cycle's absolute review-state directory, any
Azure tenant, the resolved repository URL, session identifier, claim message,
selection mode, and current loop instruction. Use that review-state directory
for all `ss-review` artifacts and worker outputs that would otherwise go
directly in session state. Keep the run-control rules above active and return
here at stage 5 after `ss-review` phase 9 completes. The review workflow
verifies the active claim immediately before remote publication. When
delegating, require
`ss-review` to return the exact question, ordered choices, and complete
ordinary-session presentation, including complete audit text or document links
and the PR reminder where applicable. Display that presentation verbatim and in
order before collecting the user's answer, then relay the exact answer before
resuming the review. All other behavior follows the ordinary `ss-review`
workflow. The user's loop
request authorizes subsequent handoffs within this invocation, not unrelated
automatic reviews.

Apply `ss-review` in full, including its ownership and human decision rules.
Do not review just the repository's current changes without the selected PR
scope. Do not skip an unanswered audit, invent finding decisions, or approve a
pull request on the user's behalf.

When resuming this stage after interruption or compaction before final review
publication, run the read-only active-reservation `review check` command from
the active cycle before using any retained review artifact or dispatching a
worker. Require `confirmed` and require its `target.webUrl` to match the pull
request in the active-cycle record. A refusal or mismatch means the cycle is
not safely resumable; stop instead of substituting another cycle's artifacts.

After phase 8 has completed and the review is in phase 9, do not rerun
`review check`: a submitted GitHub review correctly produces `alreadyReviewed`.
Validate the exact cycle identity against the handoff's journal, review-state
paths, and durable phase-8 outcome instead. The stage-5 `review complete`
command performs the remaining remote ownership and active-reservation
validation after learning completes. A phase-8 failure or ambiguous outcome
still follows publication recovery before learning can start.

After reviewer registration, leave the claim as discussion history. A resulting
assignment, an active submitted review, or a matching completed marker prevents
repeat selection according to the mode. Do not acquire another
PR while this review is active, paused, failed, cancelled, or awaiting
reconciliation. A `confirmed`
selection result or a completed agent turn is not a completed review.

# Stage 5: summary and continuation

Report the ordered resolved repositories and whether the list was explicit or
defaulted from the invocation checkout, the active and exhausted repositories,
the selection mode, the selected PR or the absence of qualifying work, the
review outcome or unanswered question, and the helper's diagnostics (identity,
considered and discarded candidates, contention and expiration decisions, and journal path)
from every repository processed so far.
For a completed review, run the stage-5 completion command in
`tool-calls/select-pull-request.md` before considering the next cycle. It
changes the owned active marker to observable completion evidence bound to the
exact final revision retained by `ss-review`'s maintained artifacts. Never
derive that revision from the pull request's live head. If this command fails
or becomes ambiguous, preserve the journal and stop for reconciliation instead
of selecting more work.
State whether looping remains enabled and whether the invocation is continuing,
finished, or paused, with the reason.
Keep the cross-repository aggregate only in the primary-session report. If an
`ss-review` GitHub summary includes selection diagnostics, include only
information from that summary's destination repository and the selected PR;
exclude other repository names, identities, candidates, claims, journals,
and repository-list progress. Keep destination-repository diagnostics in a
collapsible section. Do not post a separate summary comment just for selection.

Restart only after `ss-review` completes all phases, including publication
(or its defined nothing-to-publish outcome) and learning, with no unresolved
question or failure. Apply the latest user instruction before selecting more
work. Mark the previous cycle completed and immutable in the checkpoint before
calling stage 2 again. Do not leave its capture, journal, findings, or audit
artifacts as the active paths. If loop mode remains enabled, return to stage 1
in this same primary session.
While the active repository has not returned `noCandidate`, run another fresh
selection, claim, checkout, confirmation, and review cycle against that same
repository. On `noCandidate`, switch to the next repository and continue there.
Do not cycle back to an earlier repository during the same invocation. Otherwise
finish after the current review. Do not ask for permission to start each cycle
when loop mode already authorizes it.

An empty selection from the final repository ends the invocation rather than
starting another cycle. An empty selection from another repository advances to
the next repository.
A pause, failure, or cancellation does not start another cycle either; preserve
the current review for any authorized continuation instead of skipping it.
