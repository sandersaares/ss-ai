# Skills utility design

## Purpose and boundaries

`ss-ai` supplies operations shared by the repository's skills through
one native executable on `PATH`. It owns deterministic decisions and complete
service/file transactions. Agents own repository selection, checkout preparation,
review judgments, and human interaction.

The first consumers are `ss-review-proactor` for selection and confirmation
and `ss-review` for reviewer registration and exact Git comparisons. The
proactor's intentional handoff to `ss-review` remains in the primary user-facing
session. Registration does
not approve a pull request. The `ss-review` workflow owns review capture,
judgments, report rendering, and publication.

## Version compatibility

Sessions and persisted workflow state belong to the utility version that creates
them. Use the same utility version throughout a session. Do not add or retain
cross-version readers, migrations, or recovery paths. Read-only recovery,
including readback of an uncertain remote effect, is not an exception.

Readers reject incompatible schemas without changing the stored state or
performing provider effects. Same-version interruption recovery retains its
normal validation and authority boundaries; it does not authorize loading an
incompatible record format.

## Reapproval discovery

Discovery ignores draft PRs and narrows the supplied repository's other open
PRs using the shared commit-recency rule before reading approval history,
discussions, or Git content. It does not reuse the proactor's exclusion of already-reviewed PRs.
Old PRs require no per-PR agent work; output exposes aggregate exclusion counts
and bounded pages of recent candidates or explicit failures.

A run binds the repository, authenticated account, trusted significance policy,
and intended dry-run mode. Interrupted discovery resumes only unfinished
recency work. Existing evidence stays intact; a fresh scan uses a new run.
Drafts do not enter the inventory, counts, or results. A PR found to be a draft
during inspection, refresh, or publication preflight is removed from the run. Pending reads, per-PR
failures, and scope failures remain distinct from complete no-work results.

Discovery never changes remote state, with or without the dry-run flag.
Recent candidates are `assessmentRequired`, not approved or complete.
Assessment and publication must establish their own complete, fresh evidence.

## ADO approval inspection

Inspection consumes only recent, nondraft discovery entries. It binds
immutable provider observations to the run's account, repository, and policy.
Two complete service observations fence potentially material source, decision,
discussion, iteration, and reviewer-policy evidence. Material movement is an
explicit recoverable stale condition. Captures retain focused evidence rather
than provider presentation or bookkeeping. Stable scope, account attribution,
human discussion, and effective approval authority remain guarded; project
revision counters, profile changes, reactions, and merge-job progress do not
cause staleness. Needs-review notice recovery can use discussion-only readback.

Structured vote events, current reviewer state, source iterations, and applicable
reviewer policies establish approval standing. Ordinary build, status, required
reviewer, and other merge policies do not invalidate an individual's vote.
An individual's contribution to a reviewer group remains an individual vote.
Retained positive votes can require assessment when a policy requires fresh
iteration approval. The approving event and its source iteration form the
assessment anchor. This classification does not claim that aggregate merge
requirements are satisfied.

Reviewer removal, declines, withdrawals, and negative decisions prevent using old
authority. Inspection replays participation and vote events chronologically:
a later direct approval establishes new authority after a decline; undoing the
decline alone does not. Missing reset causality, unknown policy settings, and
uninterpretable history are explicit tooling failures, never successful unresolved outcomes.
Current reset settings and a later push alone do not prove why a vote is zero.
Routine untyped system notifications do not change approval standing or supply
vote-reset authority.

Deleted ADO threads and comments supply no approval, withdrawal, or pending-review
authority. Inspection ignores their removed payloads and retains the returned
records in its snapshot. If no applicable approval survives, inspection reports
`noPriorApproval`; deletion-related gaps in otherwise present authority produce
`ineligible` with reason `approvalEvidenceDeleted`. Neither outcome permits renewal.
Surviving explicit decisions and pending reviews still prevent automatic renewal.

Inspection does not create or renew approvals. It reports authority outcomes
and leaves failed evidence explicit. Stale evidence permits bounded refresh
within the run rather than terminating the PR's work.

## GitHub approval inspection

Complete submitted reviews, service dismissal events, and effective branch review
requirements establish current standing. A dismissal cause identifies the invalidating commit, not the
approved baseline. Later account-owned approvals replace earlier authority;
comment-only reviews leave it unchanged, including when GitHub omits them from
the timeline. Negative decisions and removal of the account's review request
prevent reuse of the old approval.

Dismissals remove approval authority. Only service-proven source-change
invalidation permits renewal assessment; other dismissals are ineligible.

GitHub can retain an `APPROVED` review while requiring approval of the most recent
reviewable push. When effective classic protection or an active organization or
repository ruleset requires last-push approval, an approval bound to an older
source revision requires cumulative source assessment. A review on the current
source remains valid. Different source commits without that requirement do not
establish expiry. This classification preserves the account's existing authority;
it does not assert that aggregate merge requirements are met or identify the last pusher.

Inspection preserves pending human reviews and captures current discussion.
Proven commit-caused invalidation permits source assessment, not immediate renewal.
The provider's applicable approval supplies the source baseline, regardless of
how that approval was submitted. The [reapproval contract](reapproval.md)
describes the provider evidence.

## Reapproval assessment and publication

An isolated bare Git store supplies exact provider-approved/current source
trees, including force-push removals, paths, modes, and opaque content.
No reviewed code executes. Target-tip advancement alone does not affect
eligibility or assessment.

Native commands select the installed policy, assessment contract, and
discussion, expose bounded source reads, and require exact per-change and
cumulative judgments. Required source documents contain every changed hunk and
operation, with exact endpoint bindings and initial surrounding lines. Complete
captured sides remain available; an assessor requests exact line ranges when
the initial patch does not establish behavior or interactions. Every requested
range becomes required evidence. This reduces unchanged input without removing
paths, merge-derived changes, or force-push removals from the comparison.
Selecting enough context remains the assessor's responsibility; native coverage
proves complete delivery, not semantic sufficiency. Model-visible discussion retains descriptions, review and
comment bodies, authorship, threading, and source locations. Identical inline
excerpts share one text entry without merging their comments or revision bindings.
Approval history reconstruction stays in native evidence rather than consuming
the assessor's context. The skill assigns each PR's semantic assessment to a fresh
GPT 6.1 Sol worker with medium reasoning and regular context. The orchestrator
receives decisions rather than source or discussion evidence.
Workers receive bounded batches of independently addressable evidence pages.
Transport fragments do not require semantic notes. Workers group changes with
shared substantive conclusions using native-issued snapshot-bound short references.
Native code derives exact per-change coverage, evidence bindings, and replacement
bookkeeping. Workers provide one final synthesis of independently reasoned combined
effects, material discussion and its disposition, and unresolved concerns.
Native code validates and completes that synthesis atomically. Discussion notes,
check enumeration, and per-path essays are not prerequisites.
Any significant or uncertain change or unresolved concern prevents renewal.
Digest-bound dependencies invalidate affected judgments when their inputs change.
Serialized evidence and metadata pages have a byte budget as well as item and
character limits.
Full synthesis requires complete readable evidence. Delivery or context
failures remain unfinished work and cannot authorize a needs-review notice;
genuine uncertainty after examination still prevents approval.
Complete delivery cannot prove comprehension. A cumulative set that exceeds
regular-context examination remains explicitly incomplete. The retained per-PR
budget bounds elapsed time, native commands, and worker reservations across
interruptions and stale recovery, leaving time for freshness and publication.
Timeouts identify slow assessment work so maintainers can improve workers or
tooling. Native full or partial completion freezes the assessment's elapsed
time. A parent noticing a timely result late does not make the assessor slow,
invalidate its completed judgment, or produce an assessment timeout.
Completed idle time does not consume a later stale-recovery assessment's
remaining budget. Each finalization attempt has its own bounded freshness
window; a slow finalizer reports a publication timeout, not an assessor timeout.
An examined significant change can complete a partial refusal with explicit
partial coverage and all requested context delivered. Unexamined evidence
cannot itself become significance, and partial completion never permits approval.
Only complete insignificant work can become a live
publication; dry runs end without remote writes. Unknown content and
insufficient evidence cannot become success-shaped empty work. The policy permits
renewal unless changes introduce significant design changes, likely performance
regressions, or likely correctness regressions; it does not restrict edit categories.

A significant or uncertain live assessment produces an attributed standalone needs-review
comment with a hidden marker. The comment contains a readable list of reasons
for withholding renewal, not a PR summary, coverage caveats, or workflow
diagnostics. The utility then clears the account's captured positive approval:
GitHub dismisses its retained approving reviews; ADO resets its positive vote
to zero. Already-cleared approvals need no write. The utility does not cast
negative votes or change another account's decisions. Independent later
approvals supersede the refusal and remain intact. ADO cannot atomically
condition a reset on the prior vote, so a concurrent human vote can race with
the request. Reset failures remain explicit even when the comment exists.
Future runs retain that decision without another
source assessment or duplicate comment. A strictly later approval by the
authenticated account expires older markers; another account cannot reset them.
Uncertainty retains its explicit failure diagnostic;
posting a notice does not turn the assessment into success. Dry runs never post notices
or clear approvals.

Every in-range candidate appears in the skill's final linked summary with its
decision. Separate new reviews remain visible across invocations.

The utility checks source and authority before submitting an approval. GitHub
submits an explicit-commit approval; ADO submits the current positive vote.
A successful provider response finishes the operation without later provider
calls. Errors are reported without automatically repeating the request.
ADO's vote API does not bind the write to a source commit, so a concurrent
source change can race with submission.
Needs-review notice readback remains available after PR closure or draft conversion.

Staleness before any attempted remote effect permits two recovery attempts per
PR after the initial attempt. Native refresh reacquires complete current evidence
and authority, discards obsolete judgments, and requires a fresh isolated
assessment when eligible. Repeated movement produces explicit retry exhaustion.
The assessment budget survives interruption and applies to inspection and assessment.
Approval and notice preflight still require fresh authority and source evidence
within their finalization window. A failed approval request does not trigger an
automatic retry in the same attempt.

## Git comparisons

Shared Git commands compare full immutable commit IDs already present in a
local repository. They compare the exact endpoint trees, not a three-dot
source range, and leave the checkout unchanged. Missing objects and incomplete
diff evidence are explicit failures.

The comparison reports changed paths and complete unified hunk ranges,
including hosting-service context lines. File names preserve case, Unicode,
spaces, and literal backslashes. Git's external diff and text conversion
programs cannot replace source evidence. The review workflow retains ownership
of deciding whether a finding falls within those ranges.

## Local review context

Local context commands read captured pull request evidence without contacting
the provider. They select bounded pages of metadata, discussion, search
results, or changes between captures. Saved requests bind their selectors and
capture identities, replay the last page by default, and require an explicit
continuation or restart. Authorship decisions remain separate from captured
evidence. Invalid or changed inputs produce errors rather than incomplete
results.

## Local finding artifacts

Local finding commands maintain reviewer artifacts, their finding history, and
consolidation plans without contacting a provider. Artifact completion remains
explicit, including for a review with no findings. Identifiers remain stable
through updates and are not reused after removal. The review workflow keeps
coverage validation and audit decisions separate from these local operations.

Local audit commands maintain append-only finding decisions and verbatim
feedback, and bind merge decisions to the reviewed pull request and source.
Approved projections require settled decisions and retain the evidence binding
needed for freshness checks. Shared Markdown rendering supplies the same finding
prose to audit and publication without granting approval or contacting a provider.

## Worker inputs

Local worker commands prepare phase-specific assignments from the active skill
assets without granting orchestration or human decision authority. Workers read
registered Markdown through bounded, integrity-checked pages with explicit
continuation. Canonical pull request captures remain selective query inputs.

## Functional areas

Skills invoke named commands and consume complete JSON results. The command
and record contracts live in the [package reference](../README.md).
The Cargo inventory reads local workspace manifests without provider
authentication. It reports dependency declarations and can atomically publish
the complete inventory for later skill processing. Internal libraries are not
a supported SDK.

- [Review selection and confirmation](design/review-selection.md) reserves
  eligible work and verifies ownership before the calling skill proceeds.
- [Reviewer registration](design/reviewer-registration.md) records participation
  without changing review judgments or existing reviewer state.
- [Automatic reapproval](reapproval.md) assesses source changes from the
  provider-selected approval baseline.

Each area owns its complete operation rather than exposing a sequence of
service requests for an agent to assemble. Provider differences stay within
the utility; callers use the same command and result conventions.

## Persistence and failures

Operations use explicit repository coordinates and the authenticated account,
not the current checkout or a display name. Required service evidence must
be complete before it can justify a decision.

Each skill run uses one installed version of the utility. Different versions
are not required to share records or locks, even when runs share a session or
host. An upgrade can change commands, record formats, and storage locations.

Valid schema-2 proactor journals remain readable. A retained sibling lock
covers the whole transaction, including validation, waiting, and remote
effects. Readers can retain a complete opened version during atomic
replacement. Temporary contention receives bounded, contention-specific
retries; semantic conflicts and unrelated I/O failures remain explicit.

The utility records intent before claim creation/deletion and outcome after
the service reply. An interrupted or ambiguous write is not replayable.
Unknown service evidence never becomes an empty successful result. Credentials
do not enter journals or diagnostics. Authentication and permission errors
remain visible to the calling skill and its human decision gates.

## Installation and platforms

Windows and Linux use the same commands and records, with native paths and
UTF-8 content. Windows and WSL have separate native installations and
authentication. The binary uses mimalloc on both platforms.

`Install.ps1` installs from the local package with Cargo and `--locked`,
using the repository toolchain. It verifies invocation through `PATH` before
updating skill links. Cargo owns the installation root and conflict detection.
Re-running setup upgrades unchanged package versions from local source.
There are no skill-specific versions or bundled binaries.
Local installations optimize for the installing machine's CPU rather than
producing binaries intended for transfer to other machines.
