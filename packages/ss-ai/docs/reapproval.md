# Automatic reapproval

## Purpose

Reapproval preserves the authenticated user's approval across later source
changes without conducting an initial review. The installed
[significance policy](../../../skills/ss-autoreapprove/significance.md)
permits renewal unless there are significant design changes, likely performance
regressions, or likely correctness regressions. Insufficient evidence is uncertain,
not permission. Native operations validate evidence and publication; the agent
supplies the semantic judgments.

## External approval and renewal recognition

An **external approval** is an approval by the authenticated account that this
skill did not issue. It can come from the UI or another review tool and can
predate installation. Its service approval-event identity and approved source
commit form the **anchor**. GitHub recognizes external approvals by the absence
of the reserved `<!-- ss-autoreapprove` marker in the review body. ADO joins
structured vote events to separate authenticated audit threads, as described below.

A **renewal** is an approval with exactly one complete, valid schema-1
`<!-- ss-autoreapprove {JSON} -->` audit marker. The marker carries the operation
UUID, repository/account/PR identities, original anchor review and source,
assessed source, policy and assessment digests, and `state: submitted`.
Recognition joins the REST review's stable node ID to the GraphQL review and
checks the service author, scope and review `commit_id` against the marker.
`submitted` describes creation of the review, not successful postflight verification.

The marker is attribution data on a service-authenticated review, not a signature
or independent grant of authority. Recognition does not trust prose, a display
name, another account's marker, or a marker pasted into discussion. A missing
marker identifies an external approval; a present but malformed, duplicate,
unsupported, or mismatched marker is unresolved renewal evidence, never a fresh
external approval.

The interpreter processes complete history chronologically. A renewal is valid
only when its marker names exactly the existing external anchor and either the
standing approval has a service-proven change-caused dismissal or an effective
last-push approval requirement calls for assessment of a different source
revision. It changes the standing
review but does not move that anchor. A later external approval establishes a new
anchor. A renewal cannot restore authority after a negative decision, reviewer
removal, or dismissal with an unknown cause. A pending owned review blocks
publication. Ambiguous ordering is an explicit service-evidence failure.

GitHub's `ReviewDismissedEvent.pullRequestCommit` supplies the dismissal cause;
it is distinct from the approved review's `commit_id`. A null value produces
`ineligible` with reason `approvalDismissed`: the dismissal removes approval
authority, and only proven source-change invalidation qualifies for renewal.
This does not establish that a human dismissed the review. New commits alone do
not prove invalidation. Source comparisons always use the original anchor source and
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
Policy changes participate in freshness checks before and after publication.
Approval snapshots require complete policy evidence.

## Evidence and publication

Discovery applies the shared inclusive 14-day UTC committer-time rule before
history, discussion, or Git content work. It reads complete pages and reports
excluded PRs as counts. The window limits candidates, not approval history.

Isolated Git-object access supplies exact paths, modes and content without executing
reviewed code. Typed, snapshot-bound judgments cover every change and the cumulative
result. Present sides must be readable and fully read for an insignificant outcome;
absent sides represent additions or deletions, not missing evidence. Structural or
behavioral edits are not automatically significant. Mixed significant or uncertain
judgments prevent approval. See the package command contract for schemas and bounds.

The orchestrator dispatches a fresh GPT 6.1 Sol assessment worker per eligible PR
with explicit medium reasoning and regular context. That worker owns preparation,
bounded evidence reads, judgments, and completion. It returns scalar results and
a concise reason, not source or discussion text, and cannot publish.

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
new evidence. Threads containing human replies, pending review, deleted history,
or audit markers remain guarded.
Structured event properties establish vote and reviewer authority; generated
system-comment prose and its edit clock do not enter retained evidence.

ADO's content-specific comment update time governs edit detection. The general
update time serves as a fallback only when the content-specific field is absent.
Metadata updates such as likes cannot invalidate an unchanged audit or notice.

Comparisons project richer retained snapshots in memory. Stored snapshots,
digests, assessment bindings, and recovery-journal integrity checks remain exact
and unchanged.

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
capture resumes through `inspect`. Independent PRs keep their own budgets and
publication identities.

Refresh takes the provider publication lock, notice lock, and run lock in that
order. It permits replacement only for stale evidence without an attempted remote
effect. Host journals, not a null receipt or review identifier, establish that
boundary. Accepted-but-stale and unknown writes retain the original snapshot
and use only original-command reconciliation. Refresh never rolls back, repeats,
or forgets such effects.
An existing publication receipt also requires retained host evidence of a
preflight-only operation; missing host history is not proof that no write occurred.

`discovery.rs` owns the schema-8 recovery record and result metadata. Schema-7
runs migrate with no consumed attempts and unchanged initial host identities.
Discovery discards draft PRs before recency checks. Valid older records drop
draft entries when loaded, and inspection, refresh, or publication preflight removes
a PR found to have become a draft without consuming the stale-recovery budget.
An attempted remote effect still requires original-command
reconciliation so that a potentially accepted write is not repeated.
Each fresh attempt adds a UUID used as that PR's host-journal run identity;
the enclosing run ID stays fixed. The snapshot digest also binds this UUID,
so equal evidence and timestamps cannot reuse an earlier snapshot. This preserves
canceled preflight records and prevents a new assessment from colliding with an
old immutable journal operation. Existing schema-8 snapshots remain readable.
Notification staleness is recorded separately from an uncertain assessment's
diagnostic, so neither condition conceals the other.

## Persistent needs-review decisions

A completed significant or uncertain assessment produces a standalone attributed comment
through native `notify`. Its `<!-- ss-autoreapprove-needs-review {JSON} -->`
marker binds the account, repository, PR, original anchor, assessed source,
policy, and assessment. The comment states that a separate new review is required.
It does not cast a negative vote or modify existing discussion. Uncertainty keeps
its explicit diagnostic and partial run status even after the notice is posted.

Inspection recognizes only service-authenticated comments by the account and
uses their service creation times. An active notice returns `needsReview`
without preparing source or repeating semantic assessment. A strictly later
external approval by the account expires every older notice. Renewals and other
accounts' approvals cannot expire notices. A notice posted after a new external
approval still applies. Edited, malformed, duplicate, or scope-mismatched owned
markers fail explicitly.

Deleted ADO threads and comments cannot supply notice evidence, and their
removed payloads are not required.

Notice publication holds the provider's retained publication lock before the
notice journal and run locks. It refreshes source and decision evidence before
persisting intent and posting once. Readback identifies a unique owned effect;
recovery never repeats an uncertain POST. Unresolved notices block later approval
publication, and unresolved approval effects block new notices. Dry runs have
no remote effects. Tooling failures before a completed assessment and missing approval authority do not produce
semantic refusal markers.

Recovery verifies the original operation's authenticated comment independently
of current approval eligibility, including after closure or draft conversion.
It captures PR identity and complete discussion without requiring source refs
or approval history. It ignores unrelated expired notices but
retains the intended comment's scope, timestamp, exact-body, and uniqueness
checks. A run-wide publication failure records its originating PR; reconciling
another PR cannot clear that failure.

The final summary lists each in-range candidate as a linked PR number and title.
It identifies current and retained needs-review decisions and distinguishes
valid automatic approval standing from external approvals. Automatic metadata
uses the review submission time on GitHub and structured vote time on ADO,
including when a previous invocation created the automatic approval.

## Approval publication

Publication refreshes identity, authority, source, and discussion. GitHub submits
the assessed source SHA explicitly; ADO binds the source through its structured
iteration history and separate audit. A retained same-host lock encloses intent,
acknowledgement, verification, and atomic run updates. Recovery reconciles only a
unique owned effect and never blindly repeats a write. Dry runs have no remote
effects. Accepted-but-stale and unknown outcomes remain explicit, without rollback.
Postflight checks do not prevent an existing auto-merge race; local locks do not
provide distributed exactly-once guarantees.

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
with reason `pendingHumanReview` until superseded. Undoing a decline neither
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
current-vote mismatch or a renewal without its preceding external authority is
`ineligible` with reason `approvalEvidenceDeleted`, not permission to renew.
Surviving withdrawals, negative decisions, removals, and pending reviews remain
authoritative.

Retained account-owned renewal markers in deleted records conservatively make the
inspection `ineligible` with reason `approvalEvidenceDeleted`. Their bytes can veto
authority but cannot establish an anchor or turn a surviving automatic vote into
an external approval. A surviving audit for an explicitly deleted vote supplies no
vote authority. Malformed surviving evidence and genuine service, authentication,
or transport failures remain explicit failures.

Minimum-reviewer evaluations identify applicable policy configurations. Their
settings must agree with the captured configurations. Inspection combines
`requireVoteOnLastIteration` and `requireVoteOnEachIteration` across applicable
policies. A retained positive approval on an older source iteration requires
assessment under either rule, even if another reviewer supplies a fresh vote.
The account's approval and satisfaction of the aggregate merge policy are
different facts. The original approving thread and its source iteration bind
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

Explicit withdrawals, negative decisions, and reviewer removals supersede old
approvals. A current zero vote that contradicts a historical approval requires
authoritative reset-cause evidence. Current `resetOnSourcePush` and
`resetRejectionsOnSourcePush` settings alone do not establish historical
causality; missing evidence is an actionable failure. Neither generated English
nor `isReapprove` proves automatic invalidation.

The [controlled-experiment fixture](../../ss-ai-review/tests/fixtures/ado-no-approval-policy.json)
establishes retained approval and reviewer removal without approval policies.
Synthetic regression cases cover policy combinations and iteration freshness
without claiming a live reset experiment.

### ADO publication and recovery

ADO renewals preserve the existing positive vote strength. The native transaction
uses the documented [reviewer vote API](https://learn.microsoft.com/en-us/rest/api/azure/devops/git/pull-request-reviewers/create-pull-request-reviewer?view=azure-devops-rest-7.1)
with `isReapprove: true`, so a retained vote can count on the assessed iteration.
It never clears a vote first, changes reviewer requirements, or withdraws an
approval as rollback. A request for the account's attention (`isFlagged: true`
on its ADO reviewer record), a declined review, or an account-authored
pending thread returned by the server blocks publication. The utility checks
returned threads containing the account's comment; it does not claim comprehensive
draft detection or establish whether unpublished server-saved drafts appear.

The reviewer API accepts neither a source SHA precondition nor an operation ID.
The host journal therefore captures complete authority and source evidence before
the single vote request. Readback must identify exactly one new positive
`VoteUpdate` by the account and its own initiator. A retained reviewer value,
an acknowledgement alone, or absence of an event cannot establish completion.
Missing or competing events leave the write unknown and block later host runs.
The service cannot distinguish an identical concurrent human vote by the same
account; schedules for the same scope must use one publishing host, and the
workflow does not promise distributed ownership or an atomic human-decision fence.

After identifying the vote, the transaction creates a standalone closed
[audit thread](https://learn.microsoft.com/en-us/rest/api/azure/devops/git/pull-request-threads/create?view=azure-devops-rest-7.1).
It changes no existing discussion. Its `<!-- ss-autoreapprove-ado {JSON} -->`
marker contains `audit` (the shared schema-1 scope, operation, policy, assessment,
source and original anchor), `voteEventId`, and positive `iterationId`.
The marker joins the service-authenticated comment author to the structured vote
actor, initiator and source iteration. Duplicate, edited, mismatched,
or malformed owned markers fail explicitly. Recognition processes the joined
vote chronologically and preserves the existing external anchor; later human
approvals establish new anchors, while withdrawals, negatives and removals
still supersede automatic renewals.

Each remote effect has its own persisted intent and readback. Recovery never
repeats an uncertain vote or thread creation. It may finish an unattempted audit,
or retry a definitively refused audit only after identifying the same vote in
fresh complete evidence. A vote without a verified audit remains unresolved and
blocks other host publications. An inspection during that interval is not a
new publishing authority: later publication must acquire the host lock and pass
fresh evidence checks.

Postflight strips only the unique owned vote and audit, then compares all
remaining evidence with the original snapshot. It also requires current valid
approval standing on the assessed source. A source, policy, discussion or human
decision race produces an explicit accepted-but-stale result, never a successful
renewal. The audit retains the assessed source even for stale effects. ADO's
mutable vote can affect a newer source or auto-merge before postflight; these
checks detect that race rather than prevent it.

Freshness comparisons ignore evaluation progress and context, which can change
as the renewed vote satisfies aggregate policy. They compare effective reviewer
configuration and applicability under the shared material-evidence rules above.
The complete reviewer endpoint owns reviewer evidence rather than the PR's
duplicated reviewer summary; the request-only `isReapprove` flag does not
represent standing authority. Source, discussion, attention and decision
evidence remain fenced.

The maintained tooling under `dev/ss-autoreapprove` captures authorized experiments
and exports sanitized fixtures. Live writes require explicit authorization in
addition to the tool's write flag. Initialization pins identity, branches, source
and target commits, empty reviewer state and disabled auto-complete. Each effect
has a persisted intent and verified readback; uncertain writes stop without replay.
Cleanup refuses intervening source, reviewer or policy changes.

Exports accept only completed verified experiments, replace private identities
consistently, shift timestamps together, omit private prose/content, and reject
unfamiliar properties. Raw captures remain outside the repository. The
[GitHub contract fixture](../../ss-ai-review/tests/fixtures/github-approval.json) records verified
read-only provider shapes; null-cause dismissal cases are conservative synthetic
counterexamples, not claims of a live manual-dismissal experiment.

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

`objects.rs` prepares isolated exact source inventories. `assessment.rs` owns
context selection, bounded contiguous reads, conditional judgment replacement,
coverage, and completion digests. Producers and consumers use the same validation,
so missing data cannot become an empty successful result. Timestamps use the shared
Jiff parser and injectable clock; the reapproval workflow has no independent
clock or retry timer.

`publication.rs` dispatches the provider transaction and acquires the stable
account/repository/PR lock before the run lock. `ado_publication.rs` owns ADO's
separate vote and audit effects; `ado_approval.rs` joins their remote provenance.
`renewal.rs` defines the shared audit data. The write transport never retries
publication, including rate-limit refusals. Recovery matches account, source,
body, and a unique review; verification removes only that owned effect before
comparing the remaining evidence. Receipts retain the absolute host-journal path.
ADO synthetic provider fixtures exercise publication, refusal, uncertainty,
source and human-decision races, and cumulative anchor preservation without
writing to live pull requests. They use the documented write contracts and
sanitized service history shapes, not a claim of a live renewal experiment.
Every attempt to write, including resumed preparation, checks for other runs'
unresolved effects under the host lock. The host record is saved before the run
record. If execution stops between those saves, recovery copies the validated
terminal receipt into its original run without another service request; the
original immutable assessment and scope must still match.
Replaying a successfully persisted publication reuses its receipt and reporting
metadata without refreshing service standing. A new run captures later approval
or withdrawal events.

`review_notice.rs` owns the distinct refusal marker, authenticated creation-time
expiry, automatic-approval summary metadata, and schema-1 notice-intent journal.
The shared approval evidence boundary applies notices only after provider
authority interpretation. Notice parsing remains separate from renewal audits;
a discussion marker never becomes approval authority.

Shared `git.rs` and `git_diff.rs` operations also serve full-review consumers.
They compare exact immutable endpoints, decode quoted paths, and reject truncated
hunks while disabling replacement objects, external diffs, and text conversion.
The PowerShell adapter retains finding lookup and hunk membership; it does not
maintain another parser or change full-review publication decisions.

## Integration coverage and resource ownership

`tests/reapproval.rs` exercises command dispatch, durable journals, assessment
authority, publication, and recovery without contacting live review services.
Common assessment setup uses the command's default read size and follows exact
continuation offsets. It derives page-count bounds from captured content, so a
nonterminating or truncated response fails deterministically. Shared paging checks
the total character count; only exact-content comparisons collect the complete
text in a caller-owned buffer. Explicit helper page limits are nonzero. Focused
pagination coverage checks small Unicode pages, replay, overlap, gaps, incomplete evidence,
stale snapshots, and the hard response cap through the same persisted commands.

Publication-only matrices share an immutable editorial source repository and
its captured commit IDs. Their journals, host stores, and provider state remain
private. Preparation records successful real Git responses separately for GitHub
and ADO; publication matrices replay only those exact ordered commands. Unknown
commands or changed source IDs fail explicitly without falling back to native
Git. These responses replace repeated source transport, not assessment reads,
judgments, authority checks, or journal persistence.

Complete representative GitHub and ADO publication/recovery lifecycles retain
real Git. ADO cumulative-anchor scenarios also retain the real failed fetch of a
later unavailable source. Notice-expiry scenarios that reverse source endpoints,
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
