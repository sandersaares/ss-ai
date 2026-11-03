# Automatic reapproval

## Purpose

Reapproval preserves the authenticated user's approval across later source
changes without conducting an initial review. The installed
[significance policy](../../../skills/ss-autoreapprove/significance.md)
permits renewal unless there are significant design changes, likely performance
regressions, or likely correctness regressions. Insufficient evidence is uncertain,
not permission. Native operations validate evidence and publication; the agent
supplies the semantic judgments.

## Approval recognition

The provider's applicable account-owned approval event and its approved source
commit form the **anchor** for the current assessment. The same rule applies
regardless of how the approval was submitted. Complete provider history
establishes standing and chronology; discussion comments do not establish
approval authority. Negative decisions, reviewer removal, and dismissals with
an unknown cause cannot authorize renewal. A pending owned review blocks
publication. Ambiguous ordering is an explicit service-evidence failure.

GitHub's `ReviewDismissedEvent.pullRequestCommit` supplies the dismissal cause;
it is distinct from the approved review's `commit_id`. A null value produces
`ineligible` with reason `approvalDismissed`: the dismissal removes approval
authority, and only proven source-change invalidation qualifies for renewal.
This does not establish that a human dismissed the review. New commits alone do
not prove invalidation. Source comparisons use the applicable approval source and
current source, including removals after force-pushes. Historical target tips,
merge bases, instrumented approvals, and warm-up observations are not prerequisites.

GitHub's last-push requirement deliberately retains stale `APPROVED` reviews rather
than dismissing them. Inspection combines classic branch protection with the
effective organization and repository rules returned by `rules/branches/{branch}`.
If any applicable rule requires last-push approval, a retained approval on a
different source revision produces `assessmentRequired`. The current PR head and
the review's approved commit supply the source binding; commit dates and aggregate
`reviewDecision` or merge blockage do not supply expiry authority.
This conservatively reassesses changed source under that requirement, without
claiming that every different revision is a reviewable push or that the account
can satisfy all merge requirements. An approval bound to the current source
remains `alreadyValid`. With no last-push requirement, an undismissed approval
remains `alreadyValid` despite a different source commit.
See [GitHub's review requirement semantics](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches#require-pull-request-reviews-before-merging).

Last-push requirements cannot restore a dismissed approval with an unknown cause,
an initial approval, a withdrawal, a negative decision, or pending human review.
Policy changes participate in freshness checks before publication.
Approval snapshots require complete policy evidence.

## Evidence and publication

Discovery applies the shared inclusive 14-day UTC committer-time rule before
history, discussion, or Git content work. It reads complete pages and reports
excluded PRs as counts. The window limits candidates, not approval history.

Isolated Git-object access supplies exact paths, modes and content without executing
reviewed code. Typed, snapshot-bound judgments cover every change and the cumulative
result. Every judgment requires the policy, its complete operation/patch document,
and all requested context for its change; the cumulative judgment requires
complete policy, discussion, and requested-context reads.
Every full synthesis requires all readable evidence, not just favorable outcomes.
An examined significant change can instead complete an explicitly partial
refusal; it never grants approval or claims examination of unrelated changes.
The discussion document contains complete semantic discussion rather than the
retained approval record. It preserves description and review bodies, comments,
authorship, reply relationships, available thread state, and source locations.
GitHub inline comments reference a shared table of exact excerpt text. Workers
read every shared excerpt as part of the complete document; sharing does not
permit skipping historical context or dropping repeated comments.
Opaque source still prevents an insignificant outcome;
absent sides represent additions or deletions, not missing evidence. Structural or
behavioral edits are not automatically significant. Mixed significant or uncertain
judgments prevent approval. See the package command contract for schemas and bounds.

Patches retain all changed text with three surrounding lines. Complete captured
readable sides remain available for deliberate context reads. Assessors request
exact line ranges when hunks do not establish enclosing logic, interfaces,
invariants, interactions, or an objection's relevance. Requested ranges become
mandatory evidence. A newly registered range invalidates the affected change's
judgment, its dependents, any discussion observation and check dispositions,
and the cumulative judgment. Repeating an existing request leaves
records intact. Native coverage cannot prove
that the assessor chose enough context; needed evidence outside the captured
sides remains explicitly missing.
Workers can register several known ranges atomically and then read their
individual references; registration alone does not count as delivery.
There is no filename, size, or edit-category exemption.

The orchestrator prepares the comparison and reserves a GPT 6.1 Sol assessment
worker per eligible PR with explicit medium reasoning and regular context.
Preparation returns only the snapshot binding, protocol, state, and preflight
progress needed for scheduling. The worker obtains inventory pages, context
bindings, digests, and read/state tokens through assessment metadata.
The worker owns bounded evidence reads, judgments, and completion. It returns
scalar results and a concise reason, not source or discussion text, and cannot
publish. A host-required skill invocation routes an assigned worker directly
to its assessment rather than starting another orchestrator.

Evidence and metadata responses have a serialized UTF-8 byte budget, including
JSON escaping and pretty formatting, so a character-count bound cannot hide
tool truncation. Metadata pages shrink at complete change boundaries and return
their actual next offset; a single oversized item fails explicitly. Evidence
pages advance only through returned characters and expose total length.
Page sizing includes the exact serialized context binding, including its path;
metadata that leaves no room for the next character fails explicitly.
Native read progress distinguishes unread empty text from an explicitly read
empty document and distinguishes opaque content from pending text delivery.
Read receipts track native delivery, not model comprehension; a truncated tool
response must be replayed with a smaller limit.

An immutable evidence-unit manifest partitions every required patch and the
complete discussion into bounded exact ranges. It prefers complete hunks,
threads, and review bodies, groups interleaved replies by their retained parent,
and splits oversized records or single lines without omission. Each unit retains
its original reference, scalar ranges, content digests, and source/thread
bindings. Sharing excerpt text never merges its referencing threads. The direct
protocol delivers independently addressable pages in bounded atomic batches.
`read-next` derives grouping and contiguous offsets from native receipts and
actual serialized response sizes. Its change references match the short
snapshot-scoped references in the inventory. A worker can select changes for
targeted delivery without reading unrelated documents; native code still owns
their continuation offsets. Transport fragments need no semantic notes.
Workers submit concise grouped conclusions using native short change references.
The snapshot and state token bind those references to exact inventory records.
Native code derives member patch citations, requested-context dependencies,
coverage, and conditional replacements rather than asking workers to copy them.

The worker independently examines combined effects and relevant exact evidence.
One final synthesis records the independent combined-change outcome, material
discussion and its disposition, and unresolved substantive concerns.
Native code binds all final member conclusions and exact evidence, validates
coverage, and completes atomically. Significant or uncertain members and
unresolved concerns prevent a favorable label from authorizing renewal.
Replacements invalidate dependent records through their bound digests.

Completion requires all required raw evidence and requested context, final
change conclusions and an independent final synthesis, including for an empty
source inventory. Discussion notes, check enumeration, empty checks, and
per-path essays are not required.
Native receipts prove delivery, not comprehension. A cumulative set that cannot
be examined with its necessary evidence remains explicitly incomplete.

Tool-output, context, and worker limits are incomplete evidence, not substantive
uncertainty. They neither freeze a semantic refusal nor authorize a notice.
Judgment recording rejects unread readable source for every disposition, and
completion independently checks the same boundary. Partial significant
completion requires an evidence-backed significant change and every requested
context range. Its native progress retains unexamined evidence and missing
change conclusions; they cannot be interpreted as full coverage.
Actual semantic uncertainty
and intrinsically opaque source remain fail-closed review outcomes.

A replacement worker uses the original run and snapshot and acquires the
evidence needed for its own decisions. Repeating preparation preserves progress;
judgments use conditional digests for deliberate replacement. A persistent
per-PR budget reserves time for publication and bounds native work commands and
worker dispatch reservations across interruption and stale recovery. Exhaustion
leaves explicit unfinished work. Legacy assessments cannot continue; a fresh
assessment is required. Earlier run schemas require a fresh run and assessment;
existing files are left untouched.

Timeouts diagnose slow assessment work, not delayed observation by the parent.
Native full synthesis, full completion, and partial significant completion
check elapsed time before persisting a completion timestamp. Timely completion
freezes the assessment clock; later metadata inspection and publication do not
consume that budget or retroactively label the assessor slow. An unfinished
or genuinely over-budget assessment remains incomplete.

The budget retains active elapsed time across stale recovery while excluding
idle time after native completion. Publication and notification each start a
separate window bounded by the reserved publication duration and still require
current provider evidence and guarded publication. A preflight that exceeds
that window returns `publicationTimeout` before any new effect, without
changing the assessor's elapsed time or exhaustion diagnostic.
Finalization rejects a start earlier than the retained assessment observation;
it measures its window from the actual invocation timestamp rather than moving
a regressed clock forward.
Registering source context changes the local read token, not the snapshot; the
same worker can obtain a new token and retry undelivered pages.

## Material evidence

Freshness checks compare potentially material changes, not complete provider JSON.
Capture enumerates complete service collections, validates their authority
semantics, and retains focused records. Approval publication, needs-review
publication, and discussion-only readback share those projections.

The retained evidence includes stable repository and account identities, source
commits and refs, PR content and state, individual decisions and pending review,
human discussion and its source context, and applicable reviewer-policy settings.
Comment bodies, authors, deletion, and content edit times remain material.
GitHub inline comments retain their `diff_hunk` excerpts so objections about
intermediate or outdated code remain understandable alongside source locations.
Profile names, avatars, links, reactions, repository statistics, project revision
counters, and merge jobs or queue progress do not affect freshness.

ADO validates all returned policies before omitting disabled and known merge-only
checks from new snapshots. It retains enabled reviewer policies and their
applicability, not policy names, creator profiles, revision counters, or evaluation
attempt identifiers in freshness comparisons. Unknown policy types, settings, and
history semantics remain explicit failures. Pure routine system notifications
are redundant with current source, PR state, and policy reads; they do not enter
new evidence. Threads containing human replies, pending review, or deleted history
remain guarded.
Structured event properties establish vote and reviewer authority; generated
system-comment prose and its edit clock do not enter retained evidence.

ADO's content-specific comment update time governs edit detection. The general
update time serves as a fallback only when the content-specific field is absent.
Metadata updates such as likes cannot invalidate an unchanged notice.

Comparisons project richer retained snapshots in memory. Stored snapshots,
digests and assessment bindings remain exact and unchanged.

## Bounded stale-evidence recovery

Source or decision evidence can change during inspection or between assessment
and publication. `staleEvidence` and `staleDuringExecution` report that movement,
not a terminal semantic refusal. The orchestrator stops the old worker and calls
native `refresh` in the same run, then follows the returned current authority
decision. An eligible refreshed snapshot requires a new worker's complete
assessment; prior judgments never authorize the new snapshot.

Each PR has a persisted budget of two recovery attempts after the initial
attempt. Refresh consumes an attempt before capturing current evidence. A failed
stale capture can consume the next attempt; another stale refresh request at the
limit records `retryExhausted` and leaves explicit partial work. An interrupted
capture resumes through `inspect`. Independent PRs keep their own budgets.
Refresh applies to staleness before a remote approval request; an approval request
error is reported without automatic resend in that attempt.

`discovery.rs` owns the schema-17 run, recovery record, and result metadata.
Earlier run schemas require a fresh run and assessment. Discovery discards draft PRs
before recency checks, and inspection, refresh, or publication preflight removes
a PR found to have become a draft. Notification staleness is recorded separately
from an uncertain assessment's diagnostic.

## Persistent needs-review decisions

A completed significant or uncertain assessment produces a standalone attributed comment
through native `notify`. Its `<!-- ss-autoreapprove-needs-review {JSON} -->`
marker binds the account, repository, PR, approval anchor, assessed source,
policy, and assessment. The comment states that a separate new review is required.
It does not cast a negative vote or modify existing discussion. Uncertainty keeps
its explicit diagnostic and partial run status even after the notice is posted.

Inspection recognizes only service-authenticated comments by the account and
uses their service creation times. An active notice returns `needsReview`
without preparing source or repeating semantic assessment. A strictly later
approval by the account expires every older notice; another account's approval
does not. A notice posted after a newer approval still applies. Edited,
malformed, duplicate, or scope-mismatched owned
markers fail explicitly.

Deleted ADO threads and comments cannot supply notice evidence, and their
removed payloads are not required.

Notice publication uses its own journal and run locks. It refreshes source and decision evidence before
persisting intent and posting once. Readback identifies a unique owned effect;
recovery never repeats an uncertain POST. Dry runs have
no remote effects. Tooling failures before a completed assessment and missing approval authority do not produce
semantic refusal markers.

Recovery verifies the original operation's authenticated comment independently
of current approval eligibility, including after closure or draft conversion.
It captures PR identity and complete discussion without requiring source refs
or approval history. It ignores unrelated expired notices but
retains the intended comment's scope, timestamp, exact-body, and uniqueness
checks. Recovery uses the same utility version and supported run schema.
Incompatible schemas fail before changing stored records or performing provider
effects; read-only readback does not bypass the version policy.

The final summary lists each in-range candidate as a linked PR number and title.
It identifies current and retained needs-review decisions.

## Approval publication

Publication refreshes identity, authority, source, and discussion before the
write. GitHub submits the assessed source SHA explicitly; ADO submits the
retained-strength positive vote. The single provider approval request is the
final remote action. A successful response finishes the operation; an error is
reported without automatically resending the request in that attempt. Dry runs
have no remote effects. There is no approval ledger, post-approval readback,
reconciliation, or exactly-once guarantee. ADO's vote API has no source
precondition, so a concurrent source change can race with submission.

## Azure DevOps evidence and policy interpretation

ADO inspection combines current individual reviewer votes with structured
service events and complete source iterations. An individual's `votedFor`
entries describe contributions to groups; they do not turn the individual's
own vote into an unsupported group vote. Routine policy, auto-complete, and
draft activity does not change approval authority.
System notifications with null or omitted property bags do not supply vote,
reset, or reviewer authority. Inspection does not interpret their prose.

Reviewer-decline events use the typed `CodeReviewReviewersUpdatedNumDeclined`
count, `CodeReviewReviewersUpdatedChangedIdentity` identity mapping, and
`CodeReviewReviewersUpdatedChangedDecline` string state. Inspection retains
these fields and replays the events in service-time order alongside votes.
A decline invalidates preceding approval authority and reports `ineligible`
with reason `pendingHumanReview` until a later approval. Undoing a decline neither
grants approval authority nor erases withdrawals and negative votes.
A later direct account-owned positive vote establishes a new approval event
and source anchor; a proxy vote does not clear a decline.
Current decline and pending-review flags still veto renewal. Malformed decline
fields, incomplete bulk identity evidence, and ambiguous decision ordering
remain explicit failures.

Deleted threads and comments do not contribute events, pending reviews, or
renewal authority. Their payloads may be absent; complete observations still retain
the returned tombstones and fence changes to them. A current positive vote alone
cannot replace a deleted approving event or identify its source. With no surviving
applicable approval, inspection reports `noPriorApproval`. A deletion-related
current-vote mismatch is `ineligible` with reason `approvalEvidenceDeleted`,
not permission to renew.
Surviving withdrawals, negative decisions, removals, and pending reviews remain
authoritative.

Minimum-reviewer evaluations identify applicable policy configurations. Their
settings must agree with the captured configurations. Inspection combines
`requireVoteOnLastIteration` and `requireVoteOnEachIteration` across applicable
policies. A retained positive approval on an older source iteration requires
assessment under either rule, even if another reviewer supplies a fresh vote.
The account's approval and satisfaction of the aggregate merge policy are
different facts. The applicable approving event and its source iteration bind
the comparison; an approval on the current iteration is already valid.

Inspection uses the [evaluation endpoint's complete-list request](https://learn.microsoft.com/en-us/rest/api/azure/devops/policy/evaluations/list?view=azure-devops-rest-7.1)
without optional `$top` or `$skip` parameters and includes not-applicable
evaluations. ADO can omit underlying records from offset-page results, so the
number of returned evaluations does not reliably identify the next offset.
Unexpected continuations and duplicate evaluation identifiers remain service
failures. Configuration reads independently follow their continuation tokens.

Build, status, comment-resolution, merge-strategy, required-reviewer, secrets
scanning, and proof-of-presence checks do not establish individual approval
freshness. Repository vote settings and unknown policy types still require
explicit interpretation. Unrecognized types or settings produce service
failures naming the missing interpretation, not completed unresolved results.
The [provider's branch-policy documentation](https://learn.microsoft.com/en-us/azure/devops/repos/git/branch-policies)
defines reviewer reset and iteration settings.

Explicit withdrawals, negative decisions, and reviewer removals invalidate old
approvals. A current zero vote that contradicts a historical approval requires
authoritative reset-cause evidence. Current `resetOnSourcePush` and
`resetRejectionsOnSourcePush` settings alone do not establish historical
causality; missing evidence is an actionable failure. Neither generated English
nor `isReapprove` proves automatic invalidation.

### ADO publication

ADO renewals preserve the existing positive vote strength. The native transaction
uses the documented [reviewer vote API](https://learn.microsoft.com/en-us/rest/api/azure/devops/git/pull-request-reviewers/create-pull-request-reviewer?view=azure-devops-rest-7.1)
with `isReapprove: true`, so a retained vote can count on the assessed iteration.
It never clears a vote first, changes reviewer requirements, or withdraws an
approval as rollback. A request for the account's attention (`isFlagged: true`
on its ADO reviewer record), a declined review, or an account-authored
pending thread returned by the server blocks publication. The utility checks
returned threads containing the account's comment; it does not claim comprehensive
draft detection or establish whether unpublished server-saved drafts appear.

The reviewer API accepts no source SHA precondition. Preflight checks current
authority and source, but a concurrent push can race with the vote. The response
finishes the operation without reading back the vote or creating an audit comment.
The request-only `isReapprove` flag does not itself establish standing authority.

## Implementation responsibilities

`discovery.rs` owns the persisted run and applies shared recency before dispatching
provider inspection. `github_approval.rs` joins complete REST reviews to the
cursor-paged GraphQL history; filtered timeline totals are not pagination limits.
`ado_approval.rs` validates structured actors, source iterations, and policy
pages. Both fence repeated complete observations before deriving authority.
ADO system comments without a property bag may remain untyped notifications
only when they have no nonempty identity mappings. Present property bags require
a typed `CodeReviewThreadType`; retained identity evidence without that discriminator
is incomplete structured history. Required event properties remain strict, and
diagnostics identify the thread and missing typed evidence. Notifications remain
in the captured observations, and neither excuse current-vote/history mismatches
nor bypass system-event timestamp validation.

REST `COMMENTED` reviews may lack a GraphQL timeline review node. They remain
in the captured evidence and receive normal identity, submission-time, body,
and dismissal-consistency validation, but do not change approval standing.
Present review nodes must agree with REST states; decision-bearing reviews and
dismissals still require matching history. Pending owned reviews still block
publication. Reconciliation failures identify the review node, REST review ID
and state, related GraphQL review/dismissal IDs and states, endpoints, pagination
methods, and collection sizes without including discussion bodies.

`objects.rs` prepares isolated exact source inventories and compares captured
blob pairs using fixed Myers behavior, no indent heuristic, and unified context.
Blob comparison avoids repository attributes and preserves the raw inventory's
rename/type pairing. An opaque side contributes no invented text; all text from
the readable side remains required and the document identifies both kinds.
Capture budgets and full retained blobs remain unchanged.

The shared Git parser validates exact old-to-new reconstruction from each patch
using forward-only captured-line iterators, including unchanged gaps, CRLF, and
missing-final-newline markers. It retains
ordered ranges without changing the existing full-review range output.
`source.rs` binds operation metadata and ranges to the snapshot and inventory.
Its context records bind line, byte, and Unicode-scalar bounds to a captured
object and content digest, without copying the range's body into state.
`assessment.rs` owns
context selection, bounded contiguous reads, conditional judgment replacement,
coverage, and completion digests. `units.rs` supplies exact source/discussion
partitions; `checkpoint.rs` binds discussion observations, judgments, explicit
check dispositions, and monotonic handoff progress. Producers and consumers use the same validation,
so missing data cannot become an empty successful result. Timestamps use the shared
Jiff parser and injectable clock; the reapproval workflow has no independent
clock or retry timer.

The required patch manifest has its own digest; completion also binds all
requested ranges. Every command validates these against retained source.
New context removes the affected change's judgment, its dependents, the discussion
observation, check dispositions, and the cumulative judgment. Unrelated change
judgments remain intact. Failed context acquisition stays
in the run as incomplete evidence, with no approval or notice authority.
Earlier run schemas require a fresh assessment rather than migrating receipts
or judgments; existing files are left untouched.

`discussion.rs` projects captured history into compact, provider-tagged semantic
documents. It does not participate in authority or freshness comparisons.
Provider-shaped locations retain missing and null distinctions, including
outdated inline positions. Exact hunk text shares a SHA-256 reference while
comments retain their own paths, authors, revisions, and reply relationships.
ADO discussion retains a compact iteration-to-source-commit map so earlier
inline coordinates remain interpretable. Deleted comment tombstones do not
require removed identifiers or bodies. Human replies in service threads remain present.
Native validation and work
dispatch use the same renderer, and the assessment binds its exact document
bytes and projection version through `discussionDigest`. Incompatible read
offsets and judgments are not migrated to another projection.

Large individual source documents and discussion still require complete worker reads. Context
exhaustion remains incomplete work. Checkpointed source/discussion summaries
are a separate design decision tracked in the repository [TODO](../../../TODO.md);
read receipts alone cannot establish that a summary preserves every objection.

Approval publication performs provider preflight followed by one approval
request. The write transport does not retry that request, including rate-limit
refusals. The provider response determines the reported result; the attempt
does not issue subsequent remote reads or writes.

`review_notice.rs` owns the distinct refusal marker, authenticated creation-time
expiry, and schema-1 notice-intent journal. The shared approval evidence
boundary applies notices only after provider authority interpretation. A
discussion marker never becomes approval authority.

Shared `git.rs` and `git_diff.rs` operations also serve full-review consumers.
They compare exact immutable endpoints, decode quoted paths, and reject truncated
hunks while disabling replacement objects, external diffs, and text conversion.
The PowerShell adapter retains finding lookup and hunk membership; it does not
maintain another parser or change full-review publication decisions.

## Integration coverage and resource ownership

`tests/reapproval.rs` exercises command dispatch, assessment authority,
publication, and notice recovery without contacting live review services.
Common assessment setup uses the command's default read size and follows exact
continuation offsets. It derives page-count bounds from captured content, so a
nonterminating or truncated response fails deterministically. Shared paging checks
the total character count; only exact-content comparisons collect the complete
text in a caller-owned buffer. Explicit helper page limits are nonzero. Focused
pagination coverage checks small Unicode pages, replay, overlap, gaps, incomplete evidence,
stale snapshots, and the hard response cap through the same persisted commands.
Controlled contention registers distinct captured-context ranges under the retained
run lock and verifies neither range nor its progress is overwritten. Synthetic
sparse/dense fixtures measure every serialized page separately from source
characters and requested context. Sparse edits require at least 90% fewer
mandatory source characters; dense replacements have no reduction guarantee.
The bounded `ss-ai-assessment/fuzz` target exercises exact patch parsing and
read/context/judgment operation ordering without contacting a provider.

Publication-only matrices share an immutable editorial source repository and
its captured commit IDs. Their run and provider state remain
private. Preparation records successful real Git responses separately for GitHub
and ADO; publication matrices replay only those exact ordered commands. Unknown
commands or changed source IDs fail explicitly without falling back to native
Git. These responses replace repeated source transport, not assessment reads,
judgments, authority checks, or run persistence.

Complete representative GitHub and ADO publication scenarios retain
real Git. Notice-expiry scenarios that reverse source endpoints,
force-push/path/mode/opaque-content cases, equal-tree cases, and Unicode paging
use real Git. Scenarios that mutate repositories own independent temporary
repositories. The suite checks the shared source's status and tip after use;
configuration and OAuth isolation retain their native transport coverage.

The binary has one serialized test entry point because process creation can
inherit another thread's retained file locks. Test correctness does not depend
on runner-specific isolation or scheduling. Run with `--nocapture` to see counts
of source-setup Git commands, native assessment Git commands, recorded replies,
and attempted assessment reads. These counts include fixture preparation and
negative cases; elapsed time remains an observation rather than an assertion.
