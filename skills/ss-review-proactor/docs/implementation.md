# ss-review-proactor implementation

## Boundaries

The primary agent owns configuration, selection-mode resolution, local checkout
preparation, the skill handoff, and human interaction. It may execute the
handoff directly or delegate the `ss-review` invocation. The shared `ss-ai`
executable owns repository enumeration, eligibility, ordering, claim writes,
the waiting period, and revalidation.
It emits one JSON result and keeps a local journal in session state.

A delegated review returns a complete user-gate handoff rather than question
text alone. The primary agent presents the exact finding or document material,
links, pull request reminder, question, and ordered choices before asking the
user. It then relays the user's exact answer and resumes the same review.

Loop control belongs to the primary agent, not the selection executable.
The agent repeats stages 1 through 5 by default and runs only one review under
the user's explicit one-shot instruction. It retains the latest instruction and
the stage-5 return point in its prose checkpoint through `ss-review`, pauses,
and compaction. A request to make the current review the last one disables the
next cycle without interrupting that review or answering its human gates.

The continuation boundary follows the complete `ss-review` workflow, including
learning. Selection confirmation, worker completion, publication alone, and
turn completion do not authorize the next cycle. An empty selection advances
to the next repository and ends the invocation only after the complete list is
exhausted; unresolved questions, failures, and cancellation block continuation.
The agent checks the latest loop instruction before restarting configuration
and does not reload an earlier opt-in over a later stop instruction.

The primary agent resolves the ordered repository scope before calling the
selection executable. One or more explicit invocation or schedule repositories
override the one-entry default from the invocation's Git checkout. The default
comes from session repository metadata or, when absent, the checkout's fetch
remote as specified in the design's repository scope. The agent validates the
complete list, preserves its order, and removes exact duplicate URLs. It converts
clone URLs to browser URLs and passes only the active repository as
`--repository-url`; the executable remains a single-repository operation and
validates the provider coordinates. Confirmation receives that same URL rather
than inferring scope again from a temporary checkout. Later cycles retain the
original invocation directory, ordered list, active position, exhausted
repositories, and resolved per-repository configuration unless the user changes
them, including after learning uses another worktree.

The same invocation-level selection mode is passed to selection and
confirmation for every candidate and repository. A replacement candidate uses
the mode persisted in that cycle's selection journal; a mismatched confirmation
fails before provider writes.

The active repository remains selected across completed reviews. A
`noCandidate` selection or confirmation marks it exhausted and advances the
cursor. The agent creates and confirms a fresh cycle for the next repository;
it never revisits an exhausted repository during the same invocation. A failure
or unanswered user gate does not advance the cursor. One-shot mode still checks
later repositories when necessary to find the first candidate, then stops after
that review completes.

For each cycle, the primary agent creates a fresh subdirectory in session state
containing the selection journal and a separate review-state directory.
The handoff supplies the review-state directory as the root for all `ss-review`
artifacts and worker outputs. Fixed artifact filenames therefore remain local
to one review, including decision logs and publication records. The selection
executable's journal schema and lifecycle remain unchanged: a new cycle uses a
fresh journal, while confirmation reuses its own cycle's journal.

The stage-5 aggregate remains in primary-session prose and must not become
shared `ss-review` publication data. Before an `ss-review` GitHub summary is
published, filter proactor diagnostics to the selected pull request's exact
repository. Do not include other repository URLs, identities, candidates,
claims, journal paths, exhaustion state, or ordered-list position.

Confirmation journals the activation intent before updating the remote marker
from reserved to active. The review invokes the read-only `review check`
operation immediately before publication. After phase 9, the proactor journals
the completion intent and changes the marker to completed. Interruption in an
activation or completion update requires human reconciliation.

The primary agent creates a unique temporary checkout subdirectory per cycle under
session state, or under the checkout parent folder supplied in the user's
prompt. It passes that checkout's absolute path to `ss-review` and keeps it
available through user-input pauses. Checkout placement does not change the
journal's session-state location.

Both skills call the same executable on `PATH`; neither imports another skill's
private libraries. The proactor still requires `ss-review` for its handoff.
Shared native transports
provide GitHub CLI execution, Azure OAuth acquisition, UTF-8 REST, structured
errors, and bounded read retries. Claim creation and deletion disable retries.
Azure DevOps comment deletion explicitly permits an empty successful response.

The primary agent applies `../ss-review/error-recovery.md` to command failures.
It corrects invocation and checkout mistakes and uses bounded retries for safe
transient operations. Restarting selection or confirmation requires evidence
that no remote write was attempted and that the journal lifecycle permits
re-entry; a transient read failure inside either command is not sufficient.
Intermediate claim states remain subject to human reconciliation. Recovery
does not erase journals, choose another PR to bypass a failed review, cancel
independent preparation workers, or answer human gates. Permission denials and
other unrecoverable external failures still stop the affected workflow.

## Platform reads

GitHub lists open pull requests through paginated REST and reads their commits
through the GraphQL commits connection. The REST pull-request commits endpoint
is not used because it caps the result at 250 commits. Requested reviewers,
submitted reviews, and issue comments are paginated independently. Selection
ignores pending and dismissed submissions when deriving reviewer assignments.

Azure DevOps lists active pull requests with `$top` and `$skip`, continuing
until an empty page so a service-side page-size limit cannot truncate selection.
Commit lists use the service continuation token. The shared transport exposes
that token only to callers explicitly requesting a page envelope; ordinary
capture calls still reject unexpected continuation. Repeated pages or tokens
are errors rather than infinite loops. Thread lists include resolved threads
and ignore deleted threads and comments.

Adapters reduce these payloads to target coordinates, assignment and submitted
review participation, eligibility, the newest non-future committer timestamp,
and discussion comments. Identity comparison
uses the shared utility's platform identity rules rather than display names.

Azure DevOps eligibility excludes declined reviewers
for both selection and confirmation. The native reader filters `hasDeclined`
before reducing entries to assignment evidence. A nonzero vote supplies
submitted-review evidence only for an active assignment. Registration still
includes declined entries so ordinary `ss-review` registration preserves
existing reviewer state.

## Claim lifecycle

The claim marker is a versioned HTML comment:

```text
<!-- ss-review-proactor:v3 session=SESSION_TOKEN claim=CLAIM_UUID state=STATE revision=SOURCE_COMMIT review=PARTICIPATION_FINGERPRINT -->
```

The session token comes from the primary session. A fresh UUID distinguishes
each attempt, including attempts within the same session. Expiration is derived
from the service timestamp, not a timestamp claimed by the comment body.

The primary agent supplies the claim announcement through `--claim-message`,
applying the current user's communication policy. The helper preserves that
text and appends the protocol marker; it supplies no attribution of its own.

The journal records the session, repository, selection mode, authenticated
identity, attempted pull request numbers, claim token and remote identifiers, and outcome
diagnostics. It is written before claim creation, then after the remote reply,
along with the earliest time the final check may start.
A file left mid-operation is evidence for human reconciliation, not authority
to replay the write.

Journal updates use the shared atomic output writer: serialize to a unique
temporary file beside the journal, then rename it over the destination. The
previous complete record remains available if the temporary write fails.
A retained sibling lock covers the complete selection or confirmation operation,
including its remote effects. Concurrent commands using the same journal wait
through temporary contention, then recheck the committed lifecycle state.
Unrelated I/O errors and persistent contention remain explicit failures.

Ownership checks use the remote comment identifier, author, session token, and
claim token. Surrounding prose and line-ending normalization do not affect
ownership. The original body remains in the journal for diagnostics.
The final check requires the live claim recorded by this cycle's journal,
not any comment sharing its author or session identifier. Expired reserved
claims from earlier attempts are ignored. Active claims never expire based on
their creation time, and completed markers are evidence rather than live
reservations.

Selection returns `claimed` immediately after posting, without waiting or
checking the claim. The agent uses the interval to prepare the local PR
checkout and complete any user interaction, then runs confirmation.
Confirmation waits only until the recorded earliest check time, if necessary,
and performs the sole post-claim check. It returns `confirmed` while the same
claim is uniquely live and the PR remains eligible. Otherwise it deletes the
owned comment if present and claims another candidate, excluding all attempted
candidates. Each replacement gets its own preparation interval and final check.

The journal is not a distributed lock; remote discussion is authoritative.
Reserved claims are not renewed while waiting for a person. Confirmation
changes a valid reserved claim to active before handoff. The publication
check verifies active ownership, and completion changes the marker only after
the review finishes. A completed claim remains in the discussion as diagnostics.

## Verification

Instruction contract tests cover default looping, explicit one-shot control,
the latest-user-instruction boundary, completion-only continuation,
same-repository continuation, ordered `noCandidate` advancement,
all-repositories-empty termination, and per-cycle state isolation across the
proactor and review handoff. Recovery
contracts cover corrected invocations, bounded transient retries, preserved
human gates, and reconciliation before replaying ambiguous external writes.

The clock and waiting operation are injected. Offline tests advance a fake
clock and mutate fake discussion state during the wait, never sleeping or
waiting for a real-time timeout. Platform tests cover actual request shapes,
paging, timestamp selection, identity comparisons, and claim cleanup.

Cargo integration tests run the real mimalloc-backed executable from outside
the skill directory, using test-owned provider launchers and HTTP servers.
They cover platform identity dispatch, Azure tenant forwarding, selection,
confirmation, retained file locks, atomic replacement, and the single-JSON-output
contract without live service access. Isolated Cargo installation tests verify
PATH discovery and same-version local-source upgrades on Windows and Linux.
