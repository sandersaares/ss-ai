# Skills utility design

## Purpose and boundaries

`ss-skills-util` supplies operations shared by the repository's skills through
one native executable on `PATH`. It owns deterministic decisions and complete
service/file transactions. Agents own repository selection, checkout preparation,
review judgments, and human interaction.

The first consumers are `ss-review-proactor` for selection and confirmation
and `ss-review` for reviewer registration and exact Git comparisons. The
proactor's intentional handoff to `ss-review` remains in the primary user-facing
session. Registration does
not approve a pull request. Review capture, findings, report rendering, and
publication remain in `ss-review`.

## Reapproval discovery

Discovery narrows the supplied repository's open PRs using the shared
commit-recency rule before reading approval history, discussions, or Git
content. It does not reuse the proactor's exclusion of already-reviewed PRs.
Old PRs require no per-PR agent work; output exposes aggregate exclusion counts
and bounded pages of recent candidates or explicit failures.

A run binds the repository, authenticated account, trusted significance policy,
and intended dry-run mode. Interrupted discovery resumes only unfinished
recency work. Existing evidence stays intact; a fresh scan uses a new run.
Drafts cannot become assessment candidates. Pending reads, per-PR failures,
and scope failures remain distinct from complete no-work results.

Discovery never changes remote state, with or without the dry-run flag.
Recent candidates are `assessmentRequired`, not approved or complete.
Assessment and publication must establish their own complete, fresh evidence.

## ADO approval inspection

Inspection consumes only recent, nondraft discovery entries. It binds
immutable provider observations to the run's account, repository, and policy.
Two complete observations fence revision, decision, discussion, iteration,
and policy evidence; observed movement is an explicit failure.

Structured vote events, current reviewer state, source iterations, and applicable
reviewer policies establish approval standing. Ordinary build, status, required
reviewer, and other merge policies do not invalidate an individual's vote.
An individual's contribution to a reviewer group remains an individual vote.
Retained positive votes can require assessment when a policy requires fresh
iteration approval. The approving event and its source iteration form the
assessment anchor. This classification does not claim that aggregate merge
requirements are satisfied.

Reviewer removal, withdrawals, and negative decisions prevent using old
authority. Missing reset causality, unknown policy settings, and uninterpretable
history are explicit tooling failures, never successful unresolved outcomes.
Current reset settings and a later push alone do not prove why a vote is zero.

Inspection does not create or renew approvals. It emits bounded terminal
outcomes and leaves failed evidence explicit. A new run obtains
new evidence rather than replacing an existing immutable observation.

## GitHub approval inspection

Complete submitted reviews and service dismissal events establish current
standing. A dismissal cause identifies the invalidating commit, not the
approved baseline. Later manual approvals replace earlier authority;
comment-only reviews leave it unchanged, including when GitHub omits them from
the timeline. Negative decisions and removal of the account's review request
prevent reuse of the old approval.

Dismissals remove approval authority. Only service-proven source-change
invalidation permits renewal assessment; other dismissals are ineligible.

Inspection preserves pending human reviews and treats unknown renewal markers
as unreconciled evidence, not new external approvals. It captures current
discussion and fences complete observations. Proven commit-caused invalidation
permits source assessment, not immediate renewal. UI and pre-install approvals
can qualify without historical target context. Renewals preserve the original
external approval as their cumulative source anchor. The
[reapproval authority contract](reapproval.md) defines recognition and provenance.

## Reapproval assessment and publication

An isolated bare Git store supplies exact original-approved/current source
trees, including force-push removals, paths, modes, and opaque content.
No reviewed code executes. Target-tip advancement alone does not affect
eligibility or assessment.

Native commands select the installed policy, assessment contract, and
discussion, expose bounded source reads, and require exact per-change and
cumulative judgments. Only complete insignificant work can become a live
publication; dry runs end without remote writes. Unknown content and
insufficient evidence cannot become success-shaped empty work. The policy permits
renewal unless changes introduce significant design changes, likely performance
regressions, or likely correctness regressions; it does not restrict edit categories.

A stable same-host scope lock encloses publication and its run update.
Intent precedes the explicit-commit approval. Acknowledgement and postflight
verification remain distinct. Unknown writes are reconciled read-only, never
blindly replayed; they block later runs. A concurrent push or human decision
can make an accepted effect stale. The utility reports it without rollback
and does not claim that detection prevents existing auto-merge.

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

## Functional areas

Skills invoke named commands and consume complete JSON results. The command
and record contracts live in the [package reference](../README.md).
The library target connects the executable and tests; it is not a supported SDK.

- [Review selection and confirmation](design/review-selection.md) reserves
  eligible work and verifies ownership before the calling skill proceeds.
- [Reviewer registration](design/reviewer-registration.md) records participation
  without changing review judgments or existing reviewer state.
- [Automatic reapproval](reapproval.md) preserves an external approval across
  qualifying source changes without moving its anchor.

Each area owns its complete operation rather than exposing a sequence of
service requests for an agent to assemble. Provider differences stay within
the utility; callers use the same command and result conventions.

## Persistence and failures

Operations use explicit repository coordinates and the authenticated account,
not the current checkout or a display name. Required service evidence must
be complete before it can justify a decision.

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
