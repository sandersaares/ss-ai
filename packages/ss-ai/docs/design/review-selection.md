# Review selection and confirmation

## Purpose

Selection reserves eligible review work for a session. Confirmation verifies
that reservation after checkout preparation and human interaction. Neither
operation makes a review judgment or approves a pull request.

These operations follow the utility-wide [design](../design.md). Their command
inputs and journal records are defined in the [package reference](../../README.md).

## Eligibility and ranking

The `available` mode considers all open pull requests in the supplied
repository and excludes drafts, self-authored pull requests, and pull requests
assigned to the authenticated user. The `assigned` mode first narrows provider
enumeration to outstanding requests for that user. It then reverses the
assignment filter and additionally excludes a pull request where that user has
submitted an active review.

Provider-side narrowing discovers candidates but never proves eligibility.
Selection validates every candidate's current identity, participation, commits,
and claim discussion before reserving it. Active submitted reviews are
ineligible in `assigned` mode, so narrowing discovery to outstanding requests
does not remove any eligible assignment.

The selected source revision comes from the GitHub pull request head or the
highest-numbered Azure DevOps iteration's `sourceRefCommit`. This matches the
revision evidence used by capture and publication rather than Azure merge-state
metadata.

GitHub assignment includes requested users and authors of active submitted
reviews, but not team membership, pending reviews, or dismissed reviews.
Azure DevOps entries with `hasDeclined: true` do not count as assignments.
Entries with a false or absent flag count as assignments. A nonzero Azure
DevOps vote represents a submitted review; a neutral or absent vote does not.

The mode is fixed for the entire reservation cycle. Confirmation uses the
persisted mode and rejects a command that attempts to select with another mode.

Commit recency uses every commit page and the latest non-future committer
timestamp, normalized to UTC. The fixed 14-day cutoff is inclusive. Older
commits and entirely future-dated commits do not qualify. Missing or malformed
timestamp evidence is an error. This recency operation stands independently
of the proactor's reviewer exclusions so other skills can reuse it.

Candidates rank by descending latest commit time, then ascending pull request
number. Selection rechecks eligibility and discussion before claiming. Any
live claim excludes the candidate, including one from the same session.

## Reservations and confirmation

A claim is a discussion comment with a session token, fresh UUID, and state.
A reserved claim lives for 60 minutes from the service creation timestamp,
regardless of edits. Future-dated reserved claims remain reservations under
clock skew. Active claims do not use the reserved-claim deadline, while
completed markers do not reserve a pull request. Deleted comments reserve
nothing; resolved Azure DevOps threads still contribute claim evidence.

Selection returns immediately after creating a reserved claim. Checkout
preparation and human interaction count toward its 60-second preparation
interval. Confirmation waits only the remaining interval, rounded up to whole
seconds, rechecks once, and changes the owned marker to active. It requires
this journal's ownership-checked claim to be the sole live claim and the pull
request to remain eligible.

An active claim remains observable through review work and user pauses. The
review workflow verifies target availability and the active claim immediately
before publication. This check does not reapply candidate selection criteria:
review assignment, submitted reviews, and commit recency cannot invalidate an
established reservation. The closed-target check may proceed only with
explicit closed-pull-request publication authorization.

The workflow changes the active claim to completed only after the full workflow
succeeds. Completion verifies the active reservation's ownership without
reapplying candidate eligibility, including commit recency. Completed markers
block later selection in either mode by the same reviewer without creating an
approval vote only while the source commit and reviewer participation
fingerprint still match. Completion preserves the revision that the claim
originally bound, not a later live head, while refreshing participation
evidence from the completed review. A new source revision or a changed
assignment/review state makes the completed marker historical rather than
current.

An invalid reserved claim is discarded. The utility deletes only its verified
own comment, never another claim or an Azure DevOps thread. It then selects
again without revisiting attempted candidates. Each new claim receives a new
preparation interval. A successful confirmation authorizes only the existing
skill handoff, not a review verdict.

The remote protocol is cooperative rather than an atomic service-side lock.
Discussion visibility and the interval between the publication-time check and
the remote publication effect remain its boundaries.
