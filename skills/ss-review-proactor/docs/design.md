# ss-review-proactor design

## Purpose

`ss-review-proactor` finds a pull request for the current user to review across
one or more repositories and hands it to `ss-review` directly or through
delegated execution. The user configures the schedule outside this skill.
By default, each invocation continues with reviews in the active repository
until it has no qualifying pull request, then advances to the next repository.
The user can explicitly request one-shot mode to stop after the first completed
review. An invocation finishes without requesting work or changing the schedule
when no configured repository has a qualifying pull request, including in loop
mode.

GitHub and Azure DevOps Services repositories are supported. Authentication is
the same as in `ss-review`: the signed-in GitHub CLI account or the Azure CLI
OAuth identity, respectively.

The target repository is reviewed in a temporary checkout under session state
by default. A checkout parent folder supplied in the user's prompt overrides
that location. Each review gets its own subdirectory under the chosen
parent rather than using the parent itself or an existing user checkout.

## Loop mode

A review cycle includes configuration, selection and claim, checkout and
confirmation, the full `ss-review` workflow, and the outcome summary. Loop mode
repeats that cycle sequentially in the owning session by default. One-shot
mode requires an explicit user request, whether in an invocation, a
user-authored schedule prompt, or a direct message during the session.

The user can say "the current review is the last one" to disable looping while
letting the current review finish. This remains available during `ss-review`
and its user-input pauses. The instruction controls future selection, not
finding decisions, merge approval, or cancellation. The latest instruction survives workflow handoffs and compaction and overrides
an earlier loop-mode request.
Direct messages do not change the schedule or future invocations. A
user-authored schedule prompt can specify one-shot mode for its scheduled
invocations.

The next cycle begins only after all review phases, including publication and
learning, finish. A review with nothing to publish records a durable phase-8
completion checkpoint and then completes normally.
Unanswered questions, failures, and cancelled reviews do not advance the loop.
An active repository with no qualifying pull request advances the repository
cursor. Exhausting every repository ends the invocation without polling.

Successive cycles retain the ordered repositories and user configuration
unless the user changes them. A completed review returns to the same active
repository. A `noCandidate` result advances the repository cursor and an
exhausted repository is not revisited during the invocation. Each cycle has
separate review artifacts, a fresh claim journal, and a unique checkout, so
earlier reviews remain available and no audit decisions or publication state
carry into the next review or repository. Exactly one cycle is active in the
owning session. Stages 1 through 3 may establish a pending identity from the
repository and fresh artifact paths, then populate it from selection and
confirmation. Once stage 4 starts, the pull request, journal, review-state
directory, and checkout form the established identity that the agent verifies
after interruption or compaction. Completed cycles become immutable history
before selection begins another cycle, and a stale result cannot reopen or
advance work for a completed cycle. Pre-publication resumption verifies target
availability and active ownership without reapplying selection eligibility;
phase-9 resumption preserves that identity through its durable completion
evidence.

## Selection mode

The default `available` mode finds pull requests that are not already assigned
to the signed-in user. The explicit `assigned` mode instead requires the pull
request to list the signed-in user as a reviewer and excludes it after that
user has submitted an active review. One resolved mode applies to every
configured repository in the invocation and remains fixed through selection,
replacement, and confirmation.

## Repository scope

One or more explicit repositories in the invocation or schedule prompt take
precedence. The skill preserves their order and processes exact duplicate URLs
once. Otherwise, the skill uses the Git repository containing the invocation's
working directory without asking the user to provide or confirm its URL.
Linked worktrees and checkout subdirectories identify their containing
repository, not the skill's installation repository.

The session's repository identity supplies the default when available.
Otherwise, the skill uses the checkout's `origin` fetch URL, or its sole remote
when there is no `origin`. HTTPS and SSH clone URLs are converted to supported
repository browser URLs. Missing, unsupported, or ambiguous repository
configuration requires user input.

Every resolved repository remains fixed throughout its selection and
confirmation cycles. An Azure tenant can be associated with each repository
independently. When no tenant override exists, the skill discovers the active
tenant from the authenticated Azure CLI account. It treats a missing discovery
result as an optional absent override and does not request a tenant from the
user. Defaulting the repository does not authorize reuse or modification of the
invocation checkout; review preparation still uses a separate temporary
checkout.

The primary session retains the aggregate list and diagnostics for operator
visibility. Remote review summaries never contain cross-repository data: when a
summary belongs to a pull request, it can publish diagnostics only from that
pull request's repository. This prevents a later public review from exposing
the identity or selection details of an earlier private repository.

## Selection

In `available` mode, a qualifying pull request is open, is not a draft, was not
authored by the current user, is not already assigned to that user, contains a
commit made in the past 14 days, and has no unexpired proactor claim. In
`assigned` mode, the assignment condition is reversed: the user must be assigned
and must not already have submitted an active review.

In either mode, a pull request remains ineligible for 48 hours after the current
user's most recent completed proactor review when that review no longer matches
the latest source or reviewer participation. This short suppression window
spreads review attention across pull requests instead of immediately
re-reviewing a pull request changed shortly after a completed review. A
completed review whose source and participation still match remains ineligible
after the window.

On GitHub, assignment includes requested users and authors of active submitted
reviews because submitting a review removes the pending request. Pending and
dismissed reviews do not count as active submitted reviews. Team assignments do
not imply an individual assignment.

On Azure DevOps, a reviewer entry with `hasDeclined: true` does not count as
an assignment. The user can use the GUI's decline action to make a pull request
ineligible in `assigned` mode or eligible in `available` mode, subject to the
other selection rules. Entries with a false or absent `hasDeclined` flag count
as assignments. A nonzero vote is a submitted review; a neutral or absent vote
is not.

Commit recency means the committer timestamp of a commit in the pull request,
not the pull request's creation time, discussion activity, or push time. The
most recent timestamp not in the future determines ordering. The 14-day
boundary is inclusive. Ties use the pull request number in ascending order.
All pages are considered before choosing a candidate.

## Review claims

A claim is a PR discussion comment carrying the skill identifier and tokens
identifying the primary session and the individual claim. Its announcement
follows the current user's communication policy; the skill prescribes no
attribution prefix. An Azure DevOps claim is posted in a resolved thread.
Resolution does not invalidate a claim.

A reserved claim is live for 60 minutes from the service's comment creation
timestamp; editing it does not extend that interval. Confirmation changes it
to an active claim that remains live across review work and user pauses.
Completion changes the same comment to evidence used by later selection in
either mode. Selection uses that completed comment's last content update for
the 48-hour window. Deleted comments do not hold claims. All sessions respect live
reserved and active claims, including claims from another identity. Expired
reserved claims from earlier attempts are ignored, including those posted by
the current user or from the same session.

After posting a claim, the primary agent prepares the checkout or clone and
completes any required user interaction. This work counts toward the 60-second
interval; preparation that takes longer extends the interval, not the claim's
lifetime. Once preparation is complete, the primary agent waits only any remaining
portion of the interval and checks the claim and eligibility once, immediately
before invoking `ss-review`.

If it finds competing live claims, it deletes only its own claim,
discards that pull request for this cycle, and tries the next qualifying
pull request. There is no earliest-claim tie-breaker. If the claim created by
this cycle is missing or has expired, or the pull request is no longer
eligible, the candidate is also discarded.

If the claim holds, the primary agent hands off to the ordinary `ss-review`
workflow, either directly or through delegated execution. Reviewer
registration consumes the reservation in `available` mode through the
resulting assignment. Submitting an active review does not release the active
marker: the skill verifies the marker immediately before publication and marks
it completed after the full review, including learning. Clean reviews and
reviews that publish only ADO comments therefore also produce completion
evidence without inventing an approval vote.

When a delegated review reaches a user gate, it returns the exact question,
ordered choices, and complete presentation required before the question. The
proactor displays that material before asking the user and relays only the
user's exact answer. Audit handoffs retain the full finding text or document
links and the pull request reminder so delegation does not reduce the user's
decision context.

This is a cooperative comment protocol, not an atomic service-side lock.
It relies on discussion reads exposing competing claims and accepts the small
window between its final check and reviewer registration.

## Human authority and failures

The user's explicit invocation or configured schedule authorizes selection,
claim comments, and the handoff to `ss-review`. It does not authorize an agent
to answer a question posed to the user, evaluate findings on their behalf, or
approve a pull request. Only the real user's direct response in the owning
session can resume a question-dependent action. The defined question-free
policy approvals in `ss-review` remain unchanged.

The workflow corrects orchestration mistakes and retries safe transient failures
without human input, preserving the current cycle and unrelated work.
Unrecoverable external failures, such as permission denial or authentication
requiring user action, stop the run with an explicit diagnostic. Exhausted or
unsafe recovery is reported as a blocker. No error is treated as an empty
candidate list.
Skipped, dismissed, cancelled, timed-out, or unrecognized user prompts remain
unanswered. They stop dependent work and do not authorize a state transition.
Ambiguous writes are not retried automatically. A claim left by an interrupted
run expires naturally; another invocation cannot resume its ownership merely
by reusing a session token.

Overlapping commands for the same local journal preserve its lifecycle state
and do not duplicate claim operations. This local coordination is separate from
the remote claim protocol used by independent sessions.
