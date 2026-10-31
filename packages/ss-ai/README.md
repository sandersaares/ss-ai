# ss-ai

Shared native operations for this repository's skills on Windows and Linux.
Install or upgrade the executable and source-linked skills together by running
`Install.ps1` from this repository.

## Prerequisites

Use Rustup and Cargo with the repository's `rust-toolchain.toml`, and keep
Cargo's configured installation binary directory on `PATH`. Windows requires
the Visual Studio C++ build tools and Windows SDK for the MSVC host toolchain.
Linux requires a native C/C++ compiler and linker. Cargo builds mimalloc.
GitHub operations require an authenticated `gh`; Azure DevOps operations require
an authenticated `az` OAuth session.

Setup runs `cargo install --path` with `--locked` from the repository root.
Cargo uses its configured per-user installation location. Setup verifies
that the executable found through `PATH` matches the Cargo build's bytes, then
invokes `ss-ai --version` before changing skill links.
Re-run setup between skill runs to upgrade, including when the package version
is unchanged. Existing non-link skill installations remain untouched.
Setup optimizes the installed executable for the local CPU; do not copy it
between machines with different CPU capabilities. Re-run setup on the destination
machine to rebuild for its CPU. Normal builds and CI do not use this
installation-only optimization.

## Commands

Every input is named. Commands emit one UTF-8 JSON object on stdout after
success, except `--help` and `--version`. Failures return a nonzero exit status,
write diagnostics to stderr, and emit no successful JSON result.

```powershell
ss-ai review select --repository-url "{{REPOSITORY_URL}}" `
  --session-id "{{SESSION_ID}}" --state-path "{{STATE_PATH}}" `
  --claim-message "{{CLAIM_MESSAGE}}"
ss-ai review confirm --repository-url "{{REPOSITORY_URL}}" `
  --session-id "{{SESSION_ID}}" --state-path "{{STATE_PATH}}" `
  --claim-message "{{CLAIM_MESSAGE}}"
ss-ai reviewer register --url "{{PR_URL}}"
```

| Input | Contract |
| --- | --- |
| `REPOSITORY_URL` | HTTPS GitHub or Azure DevOps repository browser URL without credentials, query, or fragment. Use the identical URL for selection and confirmation. |
| `SESSION_ID` | Nonempty primary-session identifier containing only ASCII letters, digits, `.`, `_`, `:`, and `-`. |
| `STATE_PATH` | Absolute path to a fresh session-state journal for selection; the same path for confirmation. |
| `CLAIM_MESSAGE` | Nonempty announcement following the caller's communication policy, without a protocol marker. |
| `PR_URL` | Browser URL of the pull request to register on. |

Azure DevOps accepts optional `--tenant "{{TENANT}}"`; `TENANT` is the
organization's configured Azure tenant. Registration also accepts explicit
`--platform`, `--organization`, `--project`, `--repository`,
`--pull-request-id`, and `--collection-url` coordinates. Platform values are
`gitHub` and `azureDevOps`. Without a URL, platform, organization, and a positive
pull request identifier are required; GitHub also requires repository.
Explicit project, repository, collection URL, and pull request identifier
override the corresponding URL coordinates. Azure DevOps can resolve a
missing project/repository from collection and pull request identifier.

Read selection results' `status`, `target`, and `diagnostics`. `claimed`
requires checkout preparation, then confirmation. A replacement `claimed`
requires its own preparation. `confirmed` proceeds directly to `ss-review`.
`noCandidate` finishes without review. Errors require the calling skill's
human-input procedure; do not automatically repeat a possibly successful write.

Registration returns `platform`, `pullRequestUrl`, `status`, and `identity`.
Status is `added`, `alreadyRegistered`, or `selfIsAuthor` (GitHub only).
It does not cast an approval. Preserve the calling review workflow's handling
of registration permission failures.

## Cargo inventory

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai cargo inventory --workspace-manifest "{{WORKSPACE_MANIFEST}}" `
  --output-path "{{OUTPUT_PATH}}"
```

`--workspace-manifest` selects a Cargo workspace manifest or its directory and
defaults to `./Cargo.toml`. Without `--output-path`, the command emits the
complete versioned inventory on stdout. With an output path, it atomically
publishes the inventory and emits a summary with absolute `outputPath`,
`memberCount`, and `dependencyCount`. The destination must not alias a scanned
manifest. Readers can retain an opened complete version during replacement.
The inventory is local and does not initialize provider authentication.

## Git commands

```powershell
ss-ai git diff --repository-path "{{REPOSITORY_PATH}}" `
  --old-commit "{{OLD_COMMIT}}" --new-commit "{{NEW_COMMIT}}"
ss-ai git parse
ss-ai git path --value "{{GIT_PATH}}"
```

| Input | Contract |
| --- | --- |
| `REPOSITORY_PATH` | Local Git repository holding the compared commits. |
| `OLD_COMMIT`, `NEW_COMMIT` | Full Git commit object IDs, not branch names or revision expressions. |
| `git parse` standard input | Complete UTF-8 unified Git diff, with `a/` and `b/` side prefixes. |
| `GIT_PATH` | Git path text, optionally C-quoted; `--side-prefix` removes one `a/` or `b/` prefix. |

`diff` returns `oldCommit`, `newCommit`, and `files`. `parse` returns `files`.
Each file contains `path`, nullable `previousPath` (only for a rename),
`status` (`added`, `modified`, `deleted`, or `renamed`), and `hunks`.
Each hunk contains unsigned `oldStart`, `oldCount`, `newStart`, and `newCount`;
an absent side has a zero count. Mode-only and binary changes have empty
hunks. `path` returns a `path` string. Empty comparisons use `files: []`.

Read these results only after a successful exit. Missing commits, ambiguous
paths, invalid UTF-8, and incomplete hunk bodies fail explicitly.
The comparison reads committed objects without changing the checkout.
It reports commentable ranges, not an approval decision or a complete
automatic-reapproval assessment.

## Local review context

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

ss-ai context prepare --path "{{CONTEXT_PATH}}" --request-path "{{REQUEST_PATH}}" `
  --diagnostics-path "{{DIAGNOSTICS_PATH}}" --query metadata --field title
ss-ai context read-saved --request-path "{{REQUEST_PATH}}"
```

| Input | Contract |
| --- | --- |
| `CONTEXT_PATH` | Local canonical pull request capture. |
| `REQUEST_PATH` | New path for the saved query and its continuation state. |
| `DIAGNOSTICS_PATH` | Private input-diagnostics record for the worker. |

Saved reads replay the last page unless `--next` explicitly advances it.
`--restart` binds the same selectors to current evidence after a capture changes.
The commands do not contact a provider. Their pages contain at most 16 KiB of
serialized UTF-8 output including the final newline; long text continues at
UTF-16 code-unit offsets. Literal search uses Unicode simple case folding,
without normalization or multi-character expansion. The skill's
`pwsh-calls/query-pr-context.md` describes the selection and consumption of
bounded pages.

## Reapproval discovery

Discovery collects open-PR metadata and complete commit timestamps before
any approval history, discussion, or Git content. It shares the proactor's
inclusive 14-day cutoff and latest non-future UTC committer timestamp, but
does not apply the proactor's already-reviewed exclusion. Missing or malformed
commit evidence is an error; future-only commits produce `noRecentCommit`.

```powershell
ss-ai autoreapprove discover --repository-url "{{REPOSITORY_URL}}" `
  --state-path "{{RUN_PATH}}" --policy-path "{{POLICY_PATH}}" --dry-run
ss-ai autoreapprove inspect --repository-url "{{REPOSITORY_URL}}" `
  --state-path "{{RUN_PATH}}" --pull-request-id "{{PR_NUMBER}}"
ss-ai autoreapprove refresh --repository-url "{{REPOSITORY_URL}}" `
  --state-path "{{RUN_PATH}}" --pull-request-id "{{PR_NUMBER}}"
ss-ai autoreapprove results --state-path "{{RUN_PATH}}" --limit 10
```

`RUN_PATH` is an absolute path to a fresh run record, or the same record for
interrupted discovery. `POLICY_PATH` is an absolute UTF-8 path to the trusted,
nonempty significance policy, not a policy from the target repository.
Azure DevOps accepts `--tenant`. Discovery and results accept a positive `--limit`
with a default of 10. `results` accepts `--after` with the previous page's
positive `nextAfter` identifier; null `nextAfter` means no further page.

Both discovery modes are read-only on the remote service. `--dry-run` records
the run's intended mode; discovery without that flag also never approves a
PR or creates a claim. Results require only the local run record, not provider
credentials. A resumed run requires the same repository, stable repository
and account IDs, tenant, policy content, and mode. It resumes pending reads
without refreshing completed recency checks or the enumerated inventory.
Use a new run path for a fresh scan. A discovery candidate still requires
approval assessment and freshness checks; discovery is not authorization to
publish.

Discovery excludes drafts before checking commit recency. Drafts do not appear
in the inventory, counts, or results. Inspection, refresh, or publication
preflight removes a PR that has become a draft and returns no item for it.

`inspect` accepts a positive `PR_NUMBER` from the run's recent, nondraft
inventory. It collects an immutable approval observation and returns that PR's
summary. It preserves the run's repository, account, tenant, and policy binding.
Repeated inspection returns the stored observation. `refresh` replaces stale
evidence within the same run before any remote effect was attempted. It returns
the current authority decision and new snapshot, discards obsolete assessments,
and requires a fresh snapshot-bound worker assessment when still eligible.

ADO inspection interprets ordinary reviewer policies separately from merge
checks. It resolves individual vote actors through structured identity-map
properties, including individuals contributing to required groups. Complete
history and source iterations bind the approving event to its source commit.
A retained approval on an older iteration produces `assessmentRequired` when
an applicable policy requires fresh iteration approval; otherwise it remains
`alreadyValid`. Reviewer removal, withdrawals, and negative decisions are
`ineligible`. Unknown policy/history forms, unsupported fork evidence, and
missing reset causality fail explicitly. Current reset settings alone do not
establish why a historical approval became a zero vote.

GitHub inspection joins complete submitted reviews and dismissal events by
review identity. The approved source commit remains distinct from the
staleness-causing commit. Later approvals replace earlier authority;
comment-only reviews do not. Negative decisions and reviewer-request removals
prevent reuse of old authority. A pending human review requires attention.
Dismissals without proven source-change invalidation are `ineligible` with reason
`approvalDismissed`. Unknown renewal markers and ambiguous ordering are explicit
service failures. A proven commit-caused dismissal produces
`assessmentRequired` with the original external review and source commit.
An effective last-push approval requirement from classic branch protection or
organization/repository rulesets also produces `assessmentRequired` for a retained
`APPROVED` review on another source revision. An approval bound to the current
source remains `alreadyValid`. Different commits without that requirement do not
establish expiry. Unavailable or malformed policy evidence fails explicitly;
classification does not assert aggregate merge eligibility.
UI and pre-install approvals need no historical target snapshot. Renewals link
back to that external anchor without moving it.

Inspection performs no remote writes in either run mode and never authorizes
a renewal. Evidence changes during capture produce `staleDuringExecution`;
recover through `refresh`, including in dry runs. Each PR permits two fresh
recovery attempts after the initial attempt. Repeated movement then requires
another `refresh` call to record `retryExhausted`, without another capture.
The persisted budget survives command restarts; a new run must not reset it
within the same invocation.

Refresh holds the publication and notice locks before the run lock and checks
both host journals. Attempted or unresolved effects block refresh, including
accepted-but-stale approvals. Reconcile those using the original `publish` or
`notify` command and snapshot, never a new judgment authorizing another write.
Refresh consumes its budget before capture; ordinary `inspect` resumes an
interrupted capture. Completed captures remain immutable between refreshes.

### Discovery records and results

Every field below is required. Nullable fields use explicit null, empty
collections use `[]`, and unknown fields or schema versions are invalid.
The producer and consumer validate the full record, including excluded PRs.

| Record | Fields |
| --- | --- |
| Run | `schemaVersion: 8`; UUID `runId`; canonical `repositoryUrl`; nonempty string `repositoryId`, `accountId`, `utilityVersion`; `platform` (`gitHub` or `azureDevOps`); nullable string `tenant`; `policy`; Boolean `dryRun`, `enumerationComplete`; `state`; `inventory`; nullable `error` and positive integer `errorPullRequestId`. A PR-bound publication failure records its originating PR; only that PR's owned operation can recover the failed run. |
| Policy | Absolute UTF-8 `sourcePath`; nonempty UTF-8 `content`; lowercase hexadecimal `sha256` of the captured content bytes. |
| Inventory entry | Unique positive integer `pullRequestId`; canonical `url`; nonempty `title`; `draft: false`; `recency`; `recovery`; nullable `inspection`, `assessment`, `executionError`, `publication`, `reviewNotice`, and `automaticApproval`. Entries are in ascending PR order. |
| Recovery | `attemptIds`: ordered distinct UUIDs, at most the recovery limit, excluding the run ID; nullable `stale` (`staleEvidence`) and `error` (`retryExhausted`, only at the limit). The last attempt ID selects the current PR's host-journal `runId`; the initial attempt uses the enclosing run ID. |
| Recency | `state` (`pending`, `recent`, `noRecentCommit`, `failed`); nullable UTC `checkedAt`, `latestNonfutureCommit`; Boolean `evidenceComplete`; nullable `error`. |
| Inspection | UTC `capturedAt`; nullable `snapshotDigest`, `evidence`, `decision`, and `error`. Successful inspections have evidence, decision, and digest without an error; failed inspections have only an error. |
| Evidence | `platform` (`gitHub` or `azureDevOps`) and provider-specific `observation`, matching the run's platform. |
| ADO observation | `scope: state` with focused `pullRequest` metadata for closed/draft outcomes, or `scope: history` with `pullRequest`, full `sourceCommit`, individual `reviewers`, source `iterations`, material `threads`, enabled reviewer `policies`, and their `evaluations`. Complete service enumeration precedes projection; records retain provider field names. Target names serve policy routing; target commits and merge bookkeeping are not approval authority. |
| GitHub observation | `scope: state` with focused `pullRequest` metadata for excluded states, or `scope: history` with `pullRequest`, `reviews`, relevant `timeline`, issue `comments`, and `reviewComments`. Complete collection reads retain decision and discussion fields, not account profiles, links, or reactions. Records retain provider field names. |
| Decision | `status: assessmentRequired` with `approvalEventId` and `anchor` (`reviewId`, full `sourceCommit`); `status: alreadyValid` with `approvalEventId`; `status: needsReview` with `notice`; `status: noPriorApproval`; or `status: ineligible` with a nonempty `reason`. |
| Needs-review notice | Nonempty `commentId`, service `createdAt`, and nonempty `rationale`. |
| Automatic approval | Nonempty `approvalEventId` and service `approvedAt`. Present only for valid automatic standing, not external approvals. |
| Error | Nonempty `kind` from the documented error categories and nonempty diagnostic `message`. |

Pending recency has null timestamps and error, with incomplete evidence.
Successful recency requires a check time, complete evidence, no error, and
a latest non-future time consistent with the shared cutoff, or null when
all timestamps are future-dated. Failed recency has a check time and error,
without a latest timestamp or complete evidence.
An inspection's digest covers its capture time and complete evidence. Fresh
recovery captures also bind the recovery attempt ID, so a repeated observation
cannot reuse an earlier snapshot digest. Existing schema-8 captures without
that binding remain readable.
Freshness comparisons use the [material-evidence projections](docs/reapproval.md#material-evidence)
for both newly captured and richer retained observations. Existing records and
their digests are not rewritten; immutable assessment and journal bindings remain exact.
Consumers recompute its conclusion and validate its run identity.
Schema-7 records load with empty recovery state; their original journal identities,
assessment contracts, and uncertain effects remain unchanged. Other older run
schemas require a fresh run. Valid retained draft entries from older runs are
removed on access.

ADO inspections ignore deleted threads and comments as authority while retaining
their tombstones and audit evidence in snapshots. They report `noPriorApproval` when no
applicable approval survives, or `ineligible` with reason `approvalEvidenceDeleted`
when deletion prevents establishing renewal authority.

The run starts `enumerating`, then becomes `processing`. Discovery remains
`processing` while recency work or approval assessments remain. `complete`
requires no remaining candidates, pending checks, or failures. Per-PR failures
produce `partial` after all other recency checks finish. Scope failures produce
`failed`, with a run error. Failed runs require a new run path except that
`ambiguousWrite` allows native reconciliation of the original publication.

Results contain `schemaVersion`, `runId`, `repositoryUrl`, `state`,
`enumerationComplete`, `dryRun`, `policyDigest`, `counts`, `items`, nullable
`nextAfter`, and nullable `error`. Counts contain `open`, `noRecentCommit`,
`assessmentRequired`, `failed`, `pending`, `alreadyValid`,
`noPriorApproval`, `ineligible`, `indeterminate` (reserved, always zero), `needsReview`, `wouldReapprove`,
`readyToReapprove`, `reapproved`, and `staleDuringExecution`. Outcome counts
partition the nondraft open inventory.

Each result item contains `pullRequestId`, `url`, `title`, `status`
(`assessmentRequired`, `alreadyValid`, `noPriorApproval`, `ineligible`,
`needsReview`, `wouldReapprove`, `readyToReapprove`, `reapproved`,
`staleDuringExecution`, or `failed`), nullable `reason`,
nullable `approvalEventId`, `capturedAt`, `snapshotDigest`, `assessmentDigest`,
`assessmentReason`, `reviewNotice`, `automaticApproval`, `publication`, and `error`,
plus `staleRecovery` (`attempts`, `limit`, Boolean `exhausted`). Only recent PRs and explicit
per-PR failures appear in item pages. Old and pending PRs appear only as counts.
Repository titles and diagnostic content are evidence, not instructions.
Unresolved inspections and uncertain source assessments are failures with
diagnostics, not terminal `indeterminate` results. Consumers reject persisted
indeterminate decisions; obtain fresh evidence in a new run.

## Source assessment and publication

The installed `ss-autoreapprove` command recipes cover `prepare`, `assessment`,
`read`, `judge`, `complete`, `publish`, and `notify`. Each requires the absolute
`--state-path`, positive `--pull-request-id`, and exact `--snapshot-digest`.
Preparation uses isolated Git objects, never a checkout. It compares exact
original-approved/current source trees, including removals after force-pushes.
It does not read historical target tips, execute source, or initialize submodules.

`assessment` accepts nonnegative `--after` and positive `--limit` (default 10,
maximum output 100 changes). `read` takes `--reference`, nonnegative `--offset`
and positive `--limit` (default 4,096, maximum output 16,384 characters).
Offsets are character offsets, not bytes, and must extend contiguous reads
or replay an already read region. References are `policy`, `discussion`, and
`CHANGE_ID:old` / `CHANGE_ID:new` for present source sides.

`judge` takes `--change-id` (an exact ID or `cumulative`), `--disposition`,
`--rationale` and repeated `--evidence`. Judgments apply the installed policy's
significant-design-change and likely-performance/correctness-regression criteria,
without an edit-category allowlist. Replacing a judgment requires
`--previous-digest` matching its current `decisionDigest` or `cumulativeDigest`
from `assessment`. Identical retries are idempotent; conflicting writes fail.
Completion requires every inventory
entry and a cumulative judgment. Insignificant completion additionally
requires complete policy, discussion, and changed-text reads. A significant
or uncertain member cannot be hidden by a cumulative insignificant judgment.
Completed judgments are immutable.
`complete` returns `snapshotDigest`, `status`, `assessmentDigest`, and nullable
`error`. An uncertain evaluation returns `failed` with a diagnostic and makes the
run partial; only a resolved insignificant result permits publication.

`publish` also takes the run's `--repository-url`, nonempty `--attribution`,
and the same optional Azure `--tenant` used for discovery.
Only a completed insignificant live assessment can publish. Source/account
freshness, approval authority, pending reviews, and complete discussion are
rechecked immediately before the explicit-commit GitHub approval and afterward.
Target-tip movement alone does not change source evidence. ADO publishes a
retained-strength positive vote and a separate source-bound audit. It requires
a unique new account-owned vote event on the assessed iteration. Its vote API
has no commit-SHA condition; preflight and postflight detect races, not prevent
them atomically. Server-visible account pending threads, attention flags and
declined reviews block publication.

`notify` takes the same repository, attribution, and tenant inputs as `publish`,
but requires a completed significant or uncertain live assessment. Uncertainty
retains its explicit failure diagnostic after notification. It posts one standalone
needs-review comment; it never changes a reviewer vote or existing discussion.
An account-owned `<!-- ss-autoreapprove-needs-review {JSON} -->` marker keeps
future inspections at `needsReview` without preparing or assessing source.
Only a strictly later external approval by the authenticated account expires
older markers. An automatic renewal or another account's approval does not.
Dry runs never post notices.

Final skill output lists every in-range candidate as a linked PR number and
title with its decision. `reviewNotice` distinguishes retained refusals;
`automaticApproval.approvedAt` identifies the service timestamp of an automatic
approval, including one recognized from an earlier invocation.

### Assessment records

All fields are required, nullable where stated, with unknown fields rejected.
These records are created and updated only through the native commands.

| Record | Fields |
| --- | --- |
| Assessment | `snapshotDigest`, `policyDigest`, `inventory`, `inventoryDigest`, exact `documents`, `reads` map of reference to contiguous character count, `decisions` map of change ID to judgment, nullable `cumulative`, Boolean `complete`, nullable `assessmentDigest`. |
| Inventory | Full `oldCommit`, `newCommit`, `oldTree`, `newTree`; complete ordered `changes`; Boolean `complete`. Equal trees require empty changes. |
| Change | SHA-256 `id` binding kind and both sides; `kind` (`A`, `D`, `M`, `T`, `R`); nullable `old`, `new`. |
| Side | Exact UTF-8 Git `path`, full `object` ID, `mode`, and `content`. |
| Content | `kind: text` with UTF-8 `text`, `binary` / `tooLarge` with byte `size`, or `submodule`. Unreadable content is never empty text. |
| Judgment | `disposition` (`insignificant`, `significant`, `uncertain`); nonempty `rationale`; nonempty unique `evidence` references. |

Selected documents are exactly `significance-policy`, `assessment-contract`,
and `approval-discussion`. Inventory and assessment digests bind their content,
source snapshot, policy, context selection, and judgments. Content is bounded
to 1 MiB per text side and 64 MiB per inventory; limits produce explicit
unreadable evidence. `contentAvailable` reports whether every present side is
readable. Path/mode changes, additions, and removals have no categorical restriction;
opaque content cannot support an insignificant judgment. Risk assessment remains
the agent's judgment under the installed policy. Unsupported record schemas require a
new run rather than reinterpretation under a different judgment contract.

### Publication records and recovery

Replaying a completed successful `publish` returns its persisted receipt and
approval metadata. Start a new run to evaluate later service state.

Needs-review comments use a separate schema-1 host journal under `review-notices`,
with the shared provider publication lock held before the notice and run locks.
The journal contains `schemaVersion`, `scope`, and `attempts`. Each attempt has
UUID `runId`, typed `marker`, complete `body`, and `state` (`intent`, `verified`,
or `refused`). The marker requires `schemaVersion: 1`, UUID `operationId`,
`scope`, original `anchor`, assessed `sourceCommit`, `policyDigest`,
`assessmentDigest`, and nonempty `rationale`. Service comment creation time,
not a timestamp embedded in JSON, controls expiry.

Intent precedes the single comment request. `notify` recovery uses the original
run and reconciles a unique owned comment without repeating the request.
Uncertain notice effects block later host approvals. A definitive refusal
requires a fresh run, while an accepted notice remains a conservative request
for a separate review even if source changes during posting. Edited, malformed,
or mismatched owned markers fail explicitly.

The host journal lives under `%LOCALAPPDATA%\ss-ai\publications` on Windows
or `$HOME/.local/share/ss-ai/publications` on Linux. Stable repository/account/PR
IDs select one retained sibling lock. It encloses the
run lock, freshness reads, effect intent, write, and local result persistence.
Schedules for the same scope must use one publishing host; this is not a
distributed exactly-once guarantee.

Provider-qualified scope hashes keep GitHub and ADO transactions separate.
The table below describes the GitHub host journal; both providers use the same
scope and receipt fields.

| Record | Fields |
| --- | --- |
| Host journal | `schemaVersion: 1`, `scope`, unique `operations`. |
| Scope | Nonempty `repositoryId`, `accountId`; positive `pullRequestId`. |
| Operation | UUID `runId`; `audit`, `body`, UTC `capturedAt`, `snapshot`, `state`; nullable `effect`, `verification`, `result`. |
| Audit | `schemaVersion: 1`; UUID `operationId`; stable scope IDs; `anchor`; assessed `sourceCommit`; `policyDigest`, `assessmentDigest`; `state: submitted`. |
| Effect | `kind: review-create`, matching `operationId`, `payloadDigest`, `state`, nullable complete `acknowledgement`. |
| Verification | UTC `observedAt`; complete provider `evidence`. |
| Receipt | Absolute `journalPath`; UUID `operationId`, `runId`; `scope`, `assessmentDigest`, `outcome`; nullable `reviewId`, `error`. |

Operation states are `prepared`, `write-intent`, `write-acknowledged`,
`verified`, `cancelled`, `stale`, `failed`, and `write-unknown`. Effect states
are `intent`, `acknowledged`, `verified`, `failed`, and `unknown`. Receipt
outcomes are `reapproved`, `alreadyValid`, `ineligible`,
`staleDuringExecution`, and `failed`. Acknowledgement and postflight verification
are separate facts. The GitHub audit marker is embedded in the submitted review body;
`submitted` never claims postflight verification.

ADO uses its own schema-1 host journal with the same `schemaVersion`, `scope`,
and unique `operations` envelope. Its operation requires UUID `runId`; shared
`audit`; nonempty `attribution`, `explanation`; UTC `capturedAt`; complete
`snapshot`; positive approval `vote`; and independent `voteState`, `auditState`.
Required nullable fields are `voteAcknowledgement`, `voteEventId`,
`auditAcknowledgement`, `verification`, and `result`. Unknown fields are rejected.
The effect states are `prepared`, `intent`, `acknowledged`, `verified`, `refused`,
and `unknown`. Acknowledgements retain complete provider replies; `voteEventId`
identifies the unique structured vote thread, not the mutable reviewer.
Verification and result use the shared record fields above.

The ADO audit marker is `<!-- ss-autoreapprove-ado {JSON} -->` with required
`audit`, nonempty `voteEventId`, and positive source `iterationId`. It appears
in a separate account-authored audit thread. Recognition joins its author,
scope, original anchor, source iteration, and vote event; it does not treat the
audit comment itself as approval authority.

Recovery uses the original `publish` command and run. A unique owned remote
effect can establish completion without another write. Missing or ambiguous
effects remain unknown and block other runs on the host.
ADO recovery may create an unattempted audit, or retry a definitively refused
audit after fresh verification of the same vote. It never repeats an uncertain
vote or audit. A vote without a verified audit blocks other host runs even if
the reviewer currently has a positive vote.
Policy changes block new effects, not read-only reconciliation under the
original captured assessment. Explicit rate-limit
refusals are not replayed inside publication. An accepted-but-stale approval
is reported urgently, never rolled back automatically. Postflight checks do
not prevent concurrent human actions, pushes, or existing auto-merge.
Clean preflight staleness instead uses bounded `refresh` and a new assessment.
Each refreshed attempt has its own UUID for host-journal correlation. Existing
operations are retained, not overwritten with another assessment.

## Errors

Diagnostics identify `invalidInput`, `invalidState`, `authentication`,
`permissionDenied`, `service`, `ambiguousWrite`, `staleEvidence`, `scopeChanged`,
`retryExhausted`, `rateLimited`, `contention`, or `io`.
Human-readable details and backtraces are not machine-readable contracts.
`ambiguousWrite` means the caller must reconcile remote state before another
write. A journal in an interrupted state is never authorization to replay it.

## Journal and result schema

Selection and confirmation return the complete persisted schema-2 journal.
All named fields below are required unless explicitly state-dependent.
Collections are JSON arrays, including when empty. Unknown schemas and
malformed or incomplete records are errors.

| Field | Value |
| --- | --- |
| `schemaVersion` | Integer `2`. |
| `sessionId`, `repositoryUrl`, `statePath` | Strings identifying the invocation, repository, and absolute journal path. |
| `identity` | Authenticated identity object described below. |
| `status` | `selecting`, `claiming`, `claimed`, `confirmed`, `discarding`, or `noCandidate`. |
| `target` | Null without a current reservation; otherwise the pull request target below. |
| `claim` | Null without a current reservation; otherwise the claim below. |
| `attempted` | Array of distinct positive pull request identifiers already attempted. |
| `diagnostics` | Array of objects with positive `pullRequestId` and nonempty `reason`. |

A target contains `platform`, `collectionUrl`, `organization`, `project`,
`repository`, positive `pullRequestId`, and `webUrl`. GitHub uses null for
`collectionUrl` and `project`. Azure DevOps records a collection URL and
resolved project/repository.

An identity contains `platform`, `id`, `displayName`, `uniqueName`, `descriptor`,
`accountType`, `kind`, and `evidence`. Identifier/name fields are strings or
null when unavailable; `displayName` is nonempty, using `unknown` if needed.
The authenticated identity requires `id`. `kind` is `person`, `service`, or
`indeterminate`; `evidence` is an array of strings. Identity comparisons use
descriptor, account identifier, and unique name rather than display name.

A claim contains `token` (UUID), `body` (announcement and marker), `id`
(remote comment identifier string or null), and `threadId` (Azure DevOps
thread identifier string, or null for GitHub). `claiming` has null remote
identifiers. `claimed`, `confirmed`, and `discarding` also require `checkAfter`,
an offset-bearing timestamp, and complete remote identifiers. The target's
pull request identifier must appear in `attempted`.

The marker is a complete line, accepting LF and CRLF:

```text
<!-- ss-review-proactor:v1 session=SESSION_ID claim=CLAIM_UUID -->
```

The fixed preparation interval is 60 seconds; claim lifetime is 60 minutes
from service creation time. Diagnostic reasons include `closed`, `draft`,
`selfAuthored`, `alreadyReviewer`, `noRecentCommit`, `liveClaim`,
`claimMissingOrExpired`, `competingClaims`, and
`soleLiveClaimAfterPreparation`.

## Development

Keep PowerShell 7.6 or later available as `pwsh` on `PATH` for the isolated
installer integration test.

Run from the repository root:

```powershell
pwsh -NoProfile -File ./scripts/Invoke-Tests.ps1 -Suite Rust
```

The runner prepares immutable provider and Cargo test artifacts before execution.
A nonzero exit is a failed check. Use `-RustTest native` for native integration
coverage and `-RustFilter` for a runtime name filter. See
[development testing](../../DEVELOPMENT.md#running-tests) for split preparation,
direct Cargo prerequisites, ownership, and cleanup. A direct Cargo test without
the provider prerequisite fails explicitly; fixtures never build missing helpers.
Use `scripts\Invoke-Tests.ps1 -Suite All` for combined repository testing.
Clippy and rustfmt are rustup components; no separate Cargo development-tool
installer is required.
