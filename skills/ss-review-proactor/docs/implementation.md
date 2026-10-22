# ss-review-proactor implementation

## Boundaries

The primary agent owns configuration, local checkout preparation, the skill
handoff, and human interaction. The shared `ss-skills-util` executable owns repository enumeration,
eligibility, ordering, claim writes, the waiting period, and revalidation.
It emits one JSON result and keeps a local journal in session state.

Loop control belongs to the primary agent, not the selection executable.
The agent defaults to one review and repeats stages 1 through 5 only under
the user's explicit loop instruction. It retains the latest instruction and
the stage-5 return point in its prose checkpoint through `ss-review`, pauses,
and compaction. A request to make the current review the last one disables the
next cycle without interrupting that review or answering its human gates.

The continuation boundary follows the complete `ss-review` workflow, including
learning. Selection confirmation, worker completion, publication alone, and
turn completion do not authorize the next cycle. Empty selection ends the
invocation; unresolved questions, failures, and cancellation block continuation.
The agent checks the latest loop instruction before restarting configuration
and does not reload an earlier opt-in over a later stop instruction.

The primary agent resolves repository scope before calling the selection
executable. An explicit invocation or schedule repository overrides the
default from the invocation's Git checkout. The default comes from session
repository metadata or, when absent, the checkout's fetch remote as specified
in the design's repository scope. The agent converts clone URLs to browser
URLs and passes the resolved value as `--repository-url`; the executable
validates the provider coordinates. Confirmation receives that
same URL rather than inferring scope again from a temporary checkout. Later
cycles retain the original invocation directory and resolved configuration
unless the user changes them, including after learning uses another worktree.

For each cycle, the primary agent creates a fresh subdirectory in session state
containing the selection journal and a separate review-state directory.
The handoff supplies the review-state directory as the root for all `ss-review`
artifacts and worker outputs. Fixed artifact filenames therefore remain local
to one review, including decision logs and publication records. The selection
executable's journal schema and lifecycle remain unchanged: a new cycle uses a
fresh journal, while confirmation reuses its own cycle's journal.

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

## Platform reads

GitHub lists open pull requests through paginated REST and reads their commits
through the GraphQL commits connection. The REST pull-request commits endpoint
is not used because it caps the result at 250 commits. Requested reviewers,
submitted reviews, and issue comments are paginated independently.

Azure DevOps lists active pull requests with `$top` and `$skip`, continuing
until an empty page so a service-side page-size limit cannot truncate selection.
Commit lists use the service continuation token. The shared transport exposes
that token only to callers explicitly requesting a page envelope; ordinary
capture calls still reject unexpected continuation. Repeated pages or tokens
are errors rather than infinite loops. Thread lists include resolved threads
and ignore deleted threads and comments.

Adapters reduce these payloads to target coordinates, eligibility, the newest
non-future committer timestamp, and discussion comments. Identity comparison
uses the shared utility's platform identity rules rather than display names.

Azure DevOps eligibility excludes declined reviewers
for both selection and confirmation. The native reader filters `hasDeclined`
before reducing entries to identities, because identity records do not carry
reviewer state. Registration still includes declined entries so ordinary
`ss-review` registration preserves existing reviewer state.

## Claim lifecycle

The claim marker is a versioned HTML comment:

```text
<!-- ss-review-proactor:v1 session=SESSION_TOKEN claim=CLAIM_UUID -->
```

The session token comes from the primary session. A fresh UUID distinguishes
each attempt, including attempts within the same session. Expiration is derived
from the service timestamp, not a timestamp claimed by the comment body.

The primary agent supplies the claim announcement through `--claim-message`,
applying the current user's communication policy. The helper preserves that
text and appends the protocol marker; it supplies no attribution of its own.

The journal records the session, repository, authenticated identity, attempted
pull request numbers, claim token and remote identifiers, and outcome
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
not any comment sharing its author or session identifier. Expired claims from
earlier attempts are ignored.

Selection returns `claimed` immediately after posting, without waiting or
checking the claim. The agent uses the interval to prepare the local PR
checkout and complete any user interaction, then runs confirmation.
Confirmation waits only until the recorded earliest check time, if necessary,
and performs the sole post-claim check. It returns `confirmed` while the same
claim is uniquely live and the PR remains eligible. Otherwise it deletes the
owned comment if present and claims another candidate, excluding all attempted
candidates. Each replacement gets its own preparation interval and final check.

The journal is not a distributed lock; remote discussion is authoritative.
Claims are not renewed while waiting for a person. A `confirmed` result is
handed directly to `ss-review`, without repeated confirmation or changes to
that skill's registration behavior. A consumed claim may remain in the
discussion as diagnostics.

## Verification

Instruction contract tests cover opt-in control, the latest-user-instruction
boundary, completion-only continuation, empty-selection termination, and
per-cycle state isolation across the proactor and review handoff.

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
