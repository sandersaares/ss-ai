# Automatic reapproval

## Purpose

Reapproval preserves the authenticated user's approval across later source
changes without conducting an initial review. The installed
[significance policy](../../../agents/.github/skills/ss-autoreapprove/significance.md)
permits renewal unless there are significant design changes, likely performance
regressions, or likely correctness regressions. Insufficient evidence is uncertain,
not permission. Native operations validate evidence and publication; the agent
supplies the semantic judgments.

## External approval and renewal recognition

An **external approval** is an approval by the authenticated account whose review
body does not contain the reserved `<!-- ss-autoreapprove` marker. It can come
from the UI or another review tool and can predate installation. Its service
review identity and approved source commit form the **anchor**.

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
only if the current standing approval has a service-proven change-caused dismissal
and the marker names exactly its existing external anchor. It changes the standing
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

Publication refreshes identity, authority, source, and discussion, and submits the
assessed source SHA explicitly. A retained same-host lock encloses intent,
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

Minimum-reviewer evaluations identify applicable policy configurations. Their
settings must agree with the captured configurations. Inspection combines
`requireVoteOnLastIteration` and `requireVoteOnEachIteration` across applicable
policies. A retained positive approval on an older source iteration requires
assessment under either rule, even if another reviewer supplies a fresh vote.
The account's approval and satisfaction of the aggregate merge policy are
different facts. The original approving thread and its source iteration bind
the comparison; an approval on the current iteration is already valid.

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

The [controlled-experiment fixture](../tests/fixtures/ado-no-approval-policy.json)
establishes retained approval and reviewer removal without approval policies.
Synthetic regression cases cover policy combinations and iteration freshness
without claiming a live reset experiment.

ADO inspection and source assessment are read-only. TODO: enable ADO publication
only with a provider-specific, source-bound vote/audit transaction and recovery
contract that preserves the original external anchor across renewals.

The maintained tooling under `dev/ss-autoreapprove` captures authorized experiments
and exports sanitized fixtures. Live writes require explicit authorization in
addition to the tool's write flag. Initialization pins identity, branches, source
and target commits, empty reviewer state and disabled auto-complete. Each effect
has a persisted intent and verified readback; uncertain writes stop without replay.
Cleanup refuses intervening source, reviewer or policy changes.

Exports accept only completed verified experiments, replace private identities
consistently, shift timestamps together, omit private prose/content, and reject
unfamiliar properties. Raw captures remain outside the repository. The
[GitHub contract fixture](../tests/fixtures/github-approval.json) records verified
read-only provider shapes; null-cause dismissal cases are conservative synthetic
counterexamples, not claims of a live manual-dismissal experiment.

## Implementation responsibilities

`discovery.rs` owns the schema-5 run and applies shared recency before dispatching
provider inspection. `github_approval.rs` joins complete REST reviews to the
cursor-paged GraphQL history; filtered timeline totals are not pagination limits.
`ado_approval.rs` validates structured actors, source iterations, and policy
pages. Both fence repeated complete observations before deriving authority.

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

`publication.rs` acquires the stable account/repository/PR lock before the run
lock. `renewal.rs` defines the audit marker. The write transport never retries
publication, including rate-limit refusals. Recovery matches account, source,
body, and a unique review; verification removes only that owned effect before
comparing the remaining evidence. Receipts retain the absolute host-journal path.
Every attempt to write, including resumed preparation, checks for other runs'
unresolved effects under the host lock. The host record is saved before the run
record. If execution stops between those saves, recovery copies the validated
terminal receipt into its original run without another service request; the
original immutable assessment and scope must still match.

Shared `git.rs` and `git_diff.rs` operations also serve full-review consumers.
They compare exact immutable endpoints, decode quoted paths, and reject truncated
hunks while disabling replacement objects, external diffs, and text conversion.
The PowerShell adapter retains finding lookup and hunk membership; it does not
maintain another parser or change full-review publication decisions.
