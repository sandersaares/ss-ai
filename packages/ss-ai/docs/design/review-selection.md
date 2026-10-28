# Review selection and confirmation

## Purpose

Selection reserves eligible review work for a session. Confirmation verifies
that reservation after checkout preparation and human interaction. Neither
operation makes a review judgment or approves a pull request.

These operations follow the utility-wide [design](../design.md). Their command
inputs and journal records are defined in the [package reference](../../README.md).

## Eligibility and ranking

Selection considers all open pull requests in the supplied repository. A
candidate must not be a draft, self-authored, or already assigned to the
authenticated user. GitHub assignment includes requested users and submitted
review authors, but not team membership or pending reviews.
Azure DevOps entries with `hasDeclined: true` do not count as assignments.
Entries with a false or absent flag still count, regardless of vote.

Commit recency uses every commit page and the latest non-future committer
timestamp, normalized to UTC. The fixed 14-day cutoff is inclusive. Older
commits and entirely future-dated commits do not qualify. Missing or malformed
timestamp evidence is an error. This recency operation stands independently
of the proactor's reviewer exclusions so other skills can reuse it.

Candidates rank by descending latest commit time, then ascending pull request
number. Selection rechecks eligibility and discussion before claiming. Any
live claim excludes the candidate, including one from the same session.

## Reservations and confirmation

A claim is a discussion comment with a session token and fresh UUID. It lives
for 60 minutes from the service creation timestamp, regardless of edits.
Future-dated claims remain reservations under clock skew. Deleted comments
do not reserve a pull request; resolved Azure DevOps threads do.

Selection returns immediately after creating a claim. Checkout preparation
and human interaction count toward its 60-second preparation interval.
Confirmation waits only the remaining interval, rounded up to whole seconds,
and rechecks once. It requires this journal's ownership-checked claim to be
the sole live claim and the pull request to remain eligible.

An invalid reservation is discarded. The utility deletes only its verified
own comment, never another claim or an Azure DevOps thread. It then selects
again without revisiting attempted candidates. Each new claim receives a new
preparation interval. A successful confirmation authorizes only the existing
skill handoff, not a review verdict.

The remote protocol is cooperative rather than an atomic service-side lock.
Discussion visibility and the interval between confirmation and registration
remain its boundaries.
