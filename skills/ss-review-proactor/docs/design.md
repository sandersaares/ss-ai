# ss-review-proactor design

## Purpose

`ss-review-proactor` finds a pull request for the current user to review and
hands it to `ss-review` in the same primary, user-facing session. The user
configures the schedule outside this skill. By default, each invocation starts
at most one review. The user can explicitly request loop mode to start another
review after each completion. An invocation with no qualifying pull
request finishes without requesting work or changing the schedule, including
in loop mode.

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
repeats that cycle sequentially in the owning session. Only an explicit user
request enables it, whether in an invocation, a user-authored schedule prompt,
or a direct message during the session.

The user can say "the current review is the last one" to disable looping while
letting the current review finish. This remains available during `ss-review`
and its user-input pauses. The instruction controls future selection, not
finding decisions, merge approval, or cancellation. The latest instruction
survives workflow handoffs and compaction and overrides an earlier loop request.
It does not change the schedule or future invocations.

The next cycle begins only after all review phases, including publication and
learning, finish. A review with nothing to publish still completes normally.
Unanswered questions, failures, and cancelled reviews do not advance the loop.
No qualifying pull request ends the invocation without polling.

Successive cycles retain the repository and user configuration unless the
user changes them. Each cycle has separate review artifacts, a fresh claim
journal, and a unique checkout, so earlier reviews remain available and no
audit decisions or publication state carry into the next review.

## Repository scope

An explicit repository in the invocation or schedule prompt takes precedence.
Otherwise, the skill uses the Git repository containing the invocation's
working directory without asking the user to provide or confirm its URL.
Linked worktrees and checkout subdirectories identify their containing
repository, not the skill's installation repository.

The session's repository identity supplies the default when available.
Otherwise, the skill uses the checkout's `origin` fetch URL, or its sole remote
when there is no `origin`. HTTPS and SSH clone URLs are converted to supported
repository browser URLs. Missing, unsupported, or ambiguous repository
configuration requires user input.

The resolved repository remains fixed throughout selection and confirmation.
Defaulting the repository does not authorize reuse or modification of the
invocation checkout; review preparation still uses a separate temporary checkout.

## Selection

A qualifying pull request is open, is not a draft, was not authored by the
current user, does not already list that user as a reviewer, contains a commit
made in the past 14 days, and has no unexpired proactor claim. On GitHub,
reviewers include both requested reviewers and authors of submitted reviews,
because submitting a review removes the pending request. Team assignments do
not imply an individual assignment.

On Azure DevOps, a reviewer entry with `hasDeclined: true` does not count as
an assignment. The user can use the GUI's decline action to make a pull request
eligible again, subject to the other selection rules. Entries with a false or
absent `hasDeclined` flag still count as assignments, regardless of vote.

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

A claim is live for 60 minutes from the service's comment creation timestamp;
editing it does not extend its lifetime. Deleted comments do not hold claims.
All sessions respect all live claims, including claims from another identity.
Expired claims from earlier attempts are ignored, including those posted
by the current user or from the same session.

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

If the claim holds, the primary agent hands off directly to the ordinary
`ss-review` workflow. Its reviewer registration consumes the reservation:
the reviewer assignment, rather than the comment, then prevents another review.
There are no additional proactor checks inside `ss-review`.

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

Unexpected authentication, permission, service, or local-state failures stop
the run for human input. No error is treated as an empty candidate list.
Ambiguous writes are not retried automatically. A claim left by an interrupted
run expires naturally; another invocation cannot resume its ownership merely
by reusing a session token.

Overlapping commands for the same local journal preserve its lifecycle state
and do not duplicate claim operations. This local coordination is separate from
the remote claim protocol used by independent sessions.
