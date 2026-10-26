# ss-review implementation

## Purpose and abstraction level

This guide explains how the implementation upholds the tenets in `design.md`.
It describes architectural boundaries, information flow, state ownership, and
failure semantics. Phase order, agent prompts, command examples, and operational
recovery instructions belong in `SKILL.md` and the `pwsh-calls` documents.
Field-level schemas and service payload details belong beside the code and its
tests.

The implementation follows a functional-core, imperative-boundary shape:

```text
Analysis:
  primary orchestrator -> bounded workers -> invariant-enforcing commands
                                                |
                                                v
                                  deterministic finding models
                                                |
                                                v
                                     versioned artifacts

Evidence:
  remote service -> transport -> adapter -> canonical model -> artifacts
                                                               |
                                                               v
                                             bounded local query -> agent

Publication:
  artifacts -> renderer and planner -> adapter -> transport -> remote service
```

The inner layers reason about review concepts. The outer layers translate,
persist, and communicate. A service-specific concept must not cross inward
unless the review model genuinely needs it.

## Orchestration boundary

The primary session is the sole owner of workflow state and user interaction.
It decides which bounded work can run concurrently, validates the artifacts
returned by that work, and suspends at user decisions.

Proactor handoffs supply a per-cycle review-state directory for artifacts and
worker outputs, isolating fixed filenames from other reviews in the same
session. The primary agent keeps the proactor continuation context and latest
loop instruction in its prose checkpoint. It returns to the proactor only
after phase 10 completes; that workflow owns further selection.

Model, reasoning-effort, and context-tier choices in `SKILL.md` are passed as
explicit spawn arguments. Every subagent uses medium reasoning effort,
including nested workers and retries; delegated prompts carry that requirement
to nested dispatches. Review workers, including retries, and the auto-approval
worker receive the default context tier explicitly. Quality refinement and
synthesis receive the long-context tier. Unspecified context tiers remain
runtime preferences.

Workers own bounded analysis or policy classification, not orchestration. A
worker receives explicit evidence and one bounded concern, writes to its
assigned artifact, and reports unresolved
questions to the primary session. It cannot advance the review globally or
publish state remotely.

Scripts enforce mechanical invariants that should not depend on language-model
judgment: schema validation, state transitions, deterministic rendering,
comparison, target resolution, and publication. This allocation keeps judgment
where context is available and keeps safety properties in executable code.

## Worker input boundary

The installed skill resolves its filesystem link to the canonical checkout.
Preparation copies the sibling `instructions/` tree under
`instruction-set-source/ss-ai/instructions/`. The generation recipe and exact
source bytes participate in the cache key, so incompatible instruction
provenance produces a miss rather than requiring a legacy cache reader.
Runtime scripts, report templates, and pinned npm manifests stay together in
the skill tree so installed links and checkout-relative use share assets.

`New-ReviewWorkerAssignment.ps1` is the common builder for bounded preparation
and focus-area reviewer tasks. It renders phase instructions, states the
delegation authorization without granting orchestration or user authority, and
selects reviewer paths directly from the immutable work manifest. Prepared
assignments use resume semantics on first dispatch to retain their findings and
scope exclusions; entirely excluded assignments have no worker. A replacement
uses a new assignment with the same reviewer and resume semantics; assignment
creation never resets a finding artifact or journal.

`Get-ReviewWorkerDataGuidePath` selects a canonical guide by phase. Grouping,
focus-area preparation, and review receive `workflow-data.md` followed by their
respective guide from `data-guides/`. The builder snapshots that text with the
shared writer's LF normalization into one attempt-local `workflowData` document
under the assignment lock. Its schema and registered document role remain
unchanged. References and overview phases
have no structured output, so they omit the role. Assignment validation uses the
same phase selection and rejects missing or unrelated roles. Original and
replacement workers read the complete assigned snapshot through the bounded
reader, without separately loading its sources.

The shared-document manifest records absolute paths, byte hashes, UTF-8 byte
counts, and line counts. Only known Markdown roles enter it; canonical captures
remain separate query inputs. The prepared reader validates bytes on every read
and returns exact contiguous slices with explicit continuation and completion,
including for a single very long line. The dispatch prompt contains a generated,
quoted first-read command, not the shared document bodies or a default viewer
invocation. Readers reload through the same route after compaction.
The builder embeds canonical `error-recovery.md` text before the first-read
instructions so a reader failure cannot prevent the worker from learning how
to recover. An empty policy prevents assignment creation. Every preparation
phase and original or replacement reviewer receives the same policy without
adding unrelated shared-document roles. Workers retain the policy and reader
command through compaction and apply recovery before reporting unresolved
blockers to the primary session.
PR dispatch prompts reference the calling-shell contract in their registered
query recipe rather than repeating its instructions.

Assignments and query requests use the shared workflow schema validator and
atomic writers. Preparation uses the existing focus-area/group validators and
the reusable overview heading validator, which ignores fenced examples.
Design references, significant-change interpretation, and scope preservation
remain semantic judgments rather than ad hoc structural parsing.

Prepared input commands maintain per-attempt counters for setup attempts, repeated
setup, shared reads, first-read failures, malformed query preparation, and
explicit cursor restarts. Each counter file belongs to one worker attempt, but
independent input commands within that attempt may run concurrently. Diagnostic
updates hold the retained sibling lock across reading, validation, incrementing,
and atomic replacement using the shared-file policy below.
A prepared reporting/recording command aggregates
the run's validated records and handles
observations outside the command boundary and separately stored baseline
measurements. These counters expose costs and failures; they do not suppress
diagnostics or establish live-review performance from synthetic tests.

## Shared workflow files

Session state is shared unless a file is explicitly handed to one subagent.
Logical workflow ownership does not imply that commands run one at a time.
The common file helpers in `PullRequestModel.ps1` provide atomic publication,
replacement-compatible reads, and retained sibling locks. Readers keep the
complete version they opened and permit replacement of its path. Writers stage
complete content beside the destination, using replacement for existing files
and a non-overwriting move for creation. A competing creator is handled as
replacement, while command-level guards still enforce immutable records and
explicit regeneration.

Read-modify-write commands hold the resource lock from initialization and
validation through commit. This covers workflow plans, feedback, counters,
queues, finding artifacts, decision logs, and authorship decisions. Publication
and proactor operations hold their journal lock across preflight and remote
effects so a competing invocation rechecks durable state instead of duplicating
delivery. Immutable captures publish their authorship sidecar before making
the capture visible.

Only atomic lock, read-open, and publication operations retry recognized
sharing violations: Windows `ERROR_SHARING_VIOLATION` or Linux/macOS
`EWOULDBLOCK`. Polling uses 50 ms intervals with at most 30 seconds of total
delay, allowing bursts on a busy host while bounding persistent contention.
Non-contention I/O errors and invalid records propagate without retry.
Transactions and remote writes are never replayed by this mechanism, and lock
files are never deleted to recover ownership.

Focus-area assembly, validation, work preparation, and cache copies coordinate
on the session's `focus-areas.lock`. Cache operations acquire the cache-key lock
before the artifact-set lock when both are needed. Opportunistic maintenance
never waits for busy entries, avoiding a cycle with stores requesting cleanup.
Cache executables hold the session's `focus-area-cache-operation.lock` across
their operation and diagnostic update, before acquiring any cache-key or
artifact-set lock. Diagnostic field updates also read and merge within their
own transaction. HTML rendering holds its output lock through publication and
hashing so its returned identity describes the file it wrote.
Report dependency installation holds an exclusive user-level tools lock.
Discovery and rendering use shared ownership of that lock, allowing parallel
renders while preventing installation from replacing their dependencies.

## Analysis scope

The orchestrator passes the review boundary against duplicating compiler,
typechecker, or linter checks to every analysis worker. Content-overview
preparation does not seed those checks as completeness questions.

Review workers apply the boundary before investigating or recording a finding.
Quality refinement removes findings that duplicate compiler, typechecker, or
linter checks from the consolidated artifact before strengthening the remaining
claims. Synthesis and freshness revalidation preserve the same boundary. Mixed
findings retain only independently supported concerns beyond those checks.

This is a semantic scope decision in the prompts, not a keyword filter in the
finding model: mentioning a type or a tool does not establish that a finding
duplicates its diagnostics. Validation of the skill's own artifacts remains a
separate responsibility of its commands.

## Focus-area preparation

Preparation reads scope from materialized sources with stable paths and
unchanged contents, not from flattened injected instruction context. Each
chapter carries its source and heading, original frontmatter and scope prose,
and effective applicability through expansion, categorization, and assembly.
Reference expansion carries referring scopes recursively. The preparation agent
assigns one writer per expanded target and merges every referring context,
including later-discovered referrers and their effect on expanded descendants.
Shared references retain distinct applicability contexts even when fetched only once.

Categorization workers use the same versioned JSON map as final assembly.
The schema and format reference live beside the preparation prompt. Source
spans use named roots and inclusive line ranges, not heading lookup or copied
text fields. All fields are required, including empty arrays for absent scope
declarations, reference origins, and uncertainty. Unknown properties are errors.
Worker outputs are validated before acceptance; the preparation agent checks
each worker's assigned chapter IDs and owns merging and category regrouping.

The assembler validates references and non-overlapping chapter ranges, then
requires every nonblank raw-input line to appear in content or scope context.
It reads source text directly, normalizes only line endings, and renders both
the category catalog and focus-area files deterministically in map order.
Each chapter carries complete applicability alternatives and their declarations,
reference chains, and uncertainty evidence. Metadata follows ATX or setext
headings, or precedes unheaded excerpts. Source attribution binds copied
relative language to the original file. There is no automatic deduplication.
Rendering completes in staging before output replacement, with rollback if
installation fails; schema or coverage failures do not touch existing outputs.

Generated Markdown uses per-chapter metadata even for identical scopes.
Reference deduplication during expansion preserves every referring scope and
source; assembly retains separate raw chapter contributions.
Operational instructions for the reviewer remain orchestration concerns;
criteria for authored agent workflows remain review guidance.

The preparation agent verifies scope preservation against the raw and original
sources before returning or caching generated artifacts. This semantic check is
separate from the command's structural artifact validation. Cache hits reuse
verified artifacts under the same recipe and source digests without repeating
semantic generation work. Unresolved source meaning remains explicit and
cannot serve as an exclusion rule.

An optional `pathScope` declares a complete path-only effective scope per
applicability context. Null or absent metadata, semantic conditions, and
uncertainty retain review work. The assembler embeds the complete map subset
and a digest of the rendered guidance in each focus-area Markdown file. This
travels through the existing cache without a separate artifact or loss of
section boundaries. Preparation verifies semantic equivalence against sources;
dispatch never infers a filter from natural-language prose or category titles.

Initialization evaluates the union of every section's alternatives against
repository-relative inventory paths, retaining external paths as uncertain.
The deliberately restricted glob language has case-insensitive matching to
conservatively retain possible matches without assuming filesystem case rules.
Any possibly applicable section keeps the file. Shared exclusion evidence is
expanded deterministically into ordinary per-file journal records, preserving
path provenance without model-authored repetition. Wholly excluded groups are
completed before dispatch; a wholly excluded focus area also receives a complete
empty finding artifact. Its manifest assignment remains present with
`dispatch: excluded`, but it is absent from the worker queue.

Workers assess the remaining files against every section. Exclusions require
concrete scope evidence; the orchestrator checks these reasons as well as the
completion journal. Scripts validate structure and recorded coverage, not the
truth of a semantic analysis declaration.

The review prompt starts change analysis from the diff and permits targeted
searches, selected ranges, and optional whole-file reads. Workers choose the
additional context needed for the applicable guidelines; reviews without a diff
use the same targeted-inspection policy over the requested content. Read-only,
schema-aware queries select relevant records and fields from structured content
with a known schema. Truncated evidence needed for analysis is retrieved through
narrower reads or queries. Captured PR context still uses its dedicated bounded
query commands.

## Review work queues

Preparation writes a structured file inventory used by both overview preparation
and review dispatch, then groups related files for review. Before any dispatch,
initialization verifies that groups cover the inventory exactly once, preserve
its metadata, and satisfy the bounds in `design.md`, "Review coverage".
Phase initialization snapshots the inventory and groups and binds the complete
generated focus-area list to content digests. The immutable manifest assigns a stable
reviewer identifier to each focus area and remains the coverage authority even
after dispatch consumes the focus-area queue. The phase gate reads this manifest
directly and verifies every worker's artifact, focus-area attribution, and
completed-group journal against the immutable grouped inventory. It does not trust an identifier list transcribed from model
memory.

The orchestrator maintains a rolling pool of at most 16 workers, refilling a
vacancy without waiting for unrelated workers. This concurrency limit bounds
simultaneous model work while allowing small focus areas to finish independently.
Each worker receives a prepared private group queue from the same grouped inventory.
Its queue file also stores one current claim, the completed-group identifiers,
and per-file assessments under schema version 3. Assessment records contain a
group, exact inventory path, reviewed/excluded disposition, controlling scope,
and reason. Exclusions require nonempty evidence. Completion requires every
member to have a record; resume and the final gate match them to the immutable
grouped inventory. New journals also bind inventory and group text digests and
store batch completion receipts. Targeted repair updates only the named file.
Consuming one worker's queue never advances another worker's coverage.

Queue commands hold an exclusively opened, persistent sibling lock file for
each read-modify-write operation. They replace queue contents atomically rather
than truncating a live file. Lock contention and malformed or missing state are
errors, not exhaustion. Only a successfully read empty queue reports exhaustion.
The lock file is retained so concurrent callers always coordinate on the same
file, including across atomic queue replacement.

A group claim atomically moves a pending group into `inProgress` and returns the
bound snapshot identities. Repeated claims return that group until a batch
transaction persists every remaining assessment, adds its identifier to
`completedGroupIds`, and clears the claim. Immutable inputs are read and validated
once per batch from the same text that is hashed. Mutable journal validation and
replacement remain under the retained sibling lock. The batch explicitly lists
required finding IDs, which must already exist in the artifact. Completion
receipts retain those IDs for the final gate; findings are never moved or
rewritten by the transaction. Receipt validation recognizes both active findings
and findings preserved in removal history. Withdrawal does not change a receipt
or restore the finding; an ID absent from both collections remains an error.
Removal history must preserve a complete finding that passes the same shape
validation as an active finding; an identifier alone is not completion evidence.

Canonical field and inventory order make identical retries read-only even after
another group has been claimed. Conflicting retries require targeted repair.
Initial completion rejects already-recorded paths rather than replacing
prepared exclusions or assessments retained from an interrupted attempt.
Partial batches, stale snapshots, malformed journals, and missing findings fail
before the commit point. A failed staging write leaves the previous journal;
an interrupted successful replacement leaves the complete group. The per-file
writer and separate completion command remain available for targeted repair
and unbound-journal recovery, not normal worker loops. This single-document
journal avoids a crash window between queue removal and recording ownership.
Completion is declared only after assessing every file, analyzing the requested
scope of non-excluded files with enough context, and persisting every finding and
scope-exclusion reason. Diffs, targeted reads, and structured-data queries can
satisfy this declaration without whole-file reads. The scripts check bookkeeping,
not the truth of that declaration.

Workers keep a prose checkpoint with assignment rules,
unfinished reading progress, and exact reload paths. Exclusion records live in
the journal and survive checkpoint updates and worker replacement; compaction retains their reload path
rather than the complete list in context. At group boundaries workers update notes for
automatic compaction rather than attempting to invoke it. Compaction notes
prioritize discarding completed-work history and file contents, including loaded
focus-area guidance, while preserving instructions to re-read originals when
needed. The journal, not a compacted summary, decides which groups are complete.

Interrupted workers, including those stopped by inherent runtime token limits,
are replaced only after the previous instance has stopped. Optional subagent
token budgets are not set. A reported worker failure is persisted as a nonblank
`failure` reason in its journal; an ordinary interruption leaves this marker
null. The coverage gate reports `failedReviewers` separately from incomplete or
missing artifacts. Claims and completion are blocked while failure is recorded.
Resume clears the marker only after validating the existing snapshot and
coverage, and preserves every claim, assessment, and finding.
Resume validates and reuses the existing journal and
reopens the finding artifact without dropping or rewriting any finding. The
outstanding claim is replayed using the same targeted-inspection policy when prior
analysis progress is uncertain; completed groups are never requeued by normal
recovery. Findings from the unfinished group remain and may be duplicated by
replay, which deduplication handles later.
If initialization was interrupted, missing state can be created; an artifact
missing despite journaled work or malformed state instead requires investigation.
Preparation publishes the manifest only after all prepared artifacts and
journals are durable. A partial initialization without a manifest cannot
dispatch work. First dispatch uses the same resume commands as replacement.
The final gate validates excluded assignments from their bound scope metadata
and checks their empty artifacts and every per-file reason.
Artifact initialization precedes group initialization, and findings precede
group completion, so ordinary interruption never leaves completed groups without
their findings.

### Coverage persistence measurements

`dev/ss-review/Measure-ReviewCoverage.ps1` in the repository compares the
per-file recovery path and normal batch persistence using the inventory size
reported in issue #125. It generates a fresh fixture with deleted and rename-side
entries, uses bounded groups, and verifies complete coverage after each run.
Each persistence call launches a fresh PowerShell process; claims run in the
driver. Initialization is outside the measured interval.

The `coverageMeasurement` output separates persistence process launches,
committed journal writes (including claims), schema-validation time, and worker
elapsed time. Schema timing measures the actual `Assert-WorkflowData` calls;
elapsed time also includes startup, semantic checks, serialization, and I/O.
Do not interpret the difference as isolated process startup or as a prediction
of model-review time. Run without concurrent test workloads when comparing
elapsed values.

```powershell
.\dev\ss-review\Measure-ReviewCoverage.ps1 -StateDirectory "{{MEASUREMENT_DIRECTORY}}"
```

| Placeholder | Description |
| --- | --- |
| `MEASUREMENT_DIRECTORY` | Absolute path of a new directory reserved for this measurement. |

The directory must be new. The command retains its fixture for inspection and
emits the measurements only after both journals pass coverage validation.

Reference measurement on Windows with PowerShell 7.6.6, using a 121-file
inventory split at the permitted group bound:

| Persistence path | Process launches | Journal writes | Schema validation | Worker elapsed |
| --- | ---: | ---: | ---: | ---: |
| Per-file | 128 | 135 | 36.06 s | 133.88 s |
| Batch | 7 | 14 | 2.05 s | 8.08 s |

Elapsed values depend on the host and its load. The structural guarantee is
one persistence process and one journal replacement per completed batch, not
per assessed file; each group also has its own claim write.

## Machine-readable handoffs

`workflow-data.md` supplies common authoring rules. Task-specific contracts in
`data-guides/` and the named definitions in `workflow-data.schema.json` specify
the reference graph, expansion outcomes, per-file assessments, finding plans,
human feedback, and merge decisions. The primary workflow and command recipes
link producers and consumers to their relevant guide; workers do not load the
entire collection. Shared schema validation rejects
unknown fields and absent required arrays. Prepared commands own atomic writes
and lock read-modify-write operations; workers submit typed item drafts.
Prose remains Markdown where no mechanical consumer requires fields.

Validation uses PowerShell's bundled JSON Schema engine with flat diagnostic
output. This preserves invalid-field paths below schema references;
`Test-Json` in PowerShell 7.6 does not traverse every level of its hierarchical
error output. Schema parsing and evaluation failures remain terminating errors.

Expansion records connect each target to every referring context by parent ID
and immutable source spans. Resolving parent chains on demand incorporates late
referrers into descendants without bespoke metadata merging. Cycles and shared
output ownership are errors. Finalization derives aggregate completeness from
all outcomes, and assembly and cache storage validate that finalized record.

Candidate plans are validated against the finding index before adjudication.
Merge, rewrite, removal, and decision plans have versioned envelopes, prepared
append commands, and strict consumer reads. An explicit empty merge plan differs
from a missing named plan. Existing finding-content checks still govern claims,
locations, provenance, and merge coverage.

Feedback append commands preserve human text and reject unknown findings.
Response IDs make exact retries idempotent without treating a clarification as a
verdict. Merge-decision commands select PR and source commit from the audited
capture. Carry-forward checks the previous binding and cannot create a true
approval from absent or false state. Publication reads and validates the record
directly before any remote write. These checks enforce representation and
binding; the primary session still owns all human authority and semantic
revalidation decisions.

## Finding artifacts

Findings remain structured, versioned data for their entire lifetime. Markdown
is an output format, never an intermediate database.

Each review worker, defined in `design.md`, is the unit of concurrency, artifact
ownership, and replacement. One worker owns one artifact across attempts;
commands also serialize artifact mutations, including identifier allocation,
so overlapping calls preserve every accepted finding. Completion is explicit so an empty
successful review can be distinguished from a worker that stopped before
reporting. Its provenance names the worker and focus area, not a file partition.
The central finding model validates every write while the
originating worker still has enough context to repair it.

The shared finding-prose validator permits tab and line feed only in multiline
fields and rejects every other C0 control character. Carriage returns are
rejected rather than normalized because audit decisions bind exact finding
text. Authored findings and stored records pass through the same validator, so
additions, rewrites, consolidation, and approved-set construction enforce the
same prose invariant.

Authored findings use UTF-8 JSON files, with line feeds escaped as `\n` inside
strings. Commands accept paths rather than prose arguments or encoded payloads,
keeping Markdown out of shell parsing and command-length limits. Additions,
synthesis, and rewrites use the same content schema; the file's serialized lines
do not constrain the structure of the rendered prose.

Transitions that replace an artifact use write-then-rename semantics. Readers
therefore observe either the previous complete document or the next complete
document, not an interrupted serialization.

Reduction is conservative:

- a lightweight index proposes possible duplicate clusters without loading the
  whole review into one model context;
- cluster adjudication reads full findings only where duplication is plausible;
- merge plans name only findings that are actually replaced; and
- findings absent from a plan pass through unchanged.

Synthesis follows the same rule. A broader finding is added before fully
subsumed sources are removed, so a failed synthesis cannot erase the evidence it
was meant to summarize.

Audit decisions form an append-only log. The latest decision for a finding is
authoritative, which supports resumption and freshness-driven retirement without
rewriting history. The approved artifact is a complete projection of that log;
it cannot be built while a live finding remains undecided. Decision records
retain whether their authority came from the user or from the defined policy
approval, allowing diagnostics to report those paths separately.

Anchor validation can exclude a finding from the approved artifact without
changing its audit decision. The orchestrator repairs pure attribution changes
in the consolidated artifact, re-renders the finding, and rebuilds the approved
artifact under the existing decision. It neither asks for renewed approval nor
appends a policy approval that would replace the original approval source.

Merge approval is kept in a separate source-commit-bound record. After recording
revalidation decisions, the orchestrator queries `auditFindingIds`. An empty
list keeps the prior merge decision in effect without returning to the audit
or asking again, including when every finding was dropped. Source movement
uses the carry-forward command to bind an existing true approval to the refreshed
commit; an unchanged source commit needs no record update. A nonempty list
returns directly to the human audit and requires a fresh merge decision, even
when the source commit is unchanged. Settling that audit without explicit merge
approval records a false decision rather than retaining the earlier approval.

## Finding quality refinement

Quality refinement is a distinct transformation over the consolidated finding
artifact. It applies the principles in `design.md`, "Findings", before the
audit, using the finding model to preserve identity and provenance while
rewriting sound findings or recording why unsupported ones were removed.

The layer has no approval authority and does not render publication prose. This
keeps evidence correction independent of the later user decision about whether
a valid recommendation is worth publishing.

## Auto-approval

Auto-approval runs after synthesis and before the human audit. The policy lives
only in `../auto-approval-rules.md`; the worker prompt in `../auto-approve.md`
defines iteration and authority without duplicating its categories.

The consolidated finding artifact stays immutable during classification. One
worker owns decision-log writes and repeatedly polls for the first live finding
with no record. A poll returns one full finding, a remaining count, and the
identifiers needing human audit; it does not consume work or rewrite files.
The existing append-only log journals both outcomes: `auto-approved` is a final
policy verdict, while `needs-audit` is a completed classification with no final
approval or rejection. Polling skips both, as well as existing user verdicts.
The worker's recording command uses `-PolicyClassification` to reject other
verdicts and user feedback before writing the log.

Recording the outcome is the only progress operation. Decision-log writes stage
a sibling temporary file and atomically replace the log only after serialization
finishes. An interruption before replacement preserves the previous log and
replays the same finding; an interruption after it advances to the next.
Replacements start only after their predecessor stops, with the same
artifact and log paths. Work allocation remains with the designated classification
worker; shared decision-log updates are serialized independently. The orchestrator verifies exhaustion through
the polling command rather than trusting the worker's completion report.

Approved-set construction and audit diagnostics treat `needs-audit` as
undecided. The human audit uses the polling result's pending identifiers without
reclassifying findings. When freshness or destination changes require renewed
approval, the orchestrator records `needs-audit` for materially revised claims
and returns directly to the user; new findings need no prior classification.
Unchanged claims, strict narrowings, and location-only adjustments retain their
approval and its original source.

## Human audit

Phase 7 offers document approval and an optional in-session presentation.
Only the latter loads `../audit-in-session.md`. Both use the same rendering,
feedback, decision, and approved-set commands; no separate queue or decision
schema represents the walkthrough.

The in-session loop selects the first pending `auditFindingIds` entry, then reads
its destination-specific file from the renderer's `findings` path mapping. It
emits the complete Markdown in session text before asking `Approve/Other`.
Feedback and verdicts are persisted before the next iteration. Clarifications
keep the current finding open; amendments regenerate its canonical text before
another question. Polling the decision log on resumption skips settled findings
without treating policy-classification completion as audit completion.

The existing prose checkpoint retains presentation mode, absolute snapshot and
rendered paths, overview presentation, and a pending final merge question.
An eligible walkthrough marks that question pending before per-finding decisions
and clears it only after recording the user's final merge answer. This prevents
an interruption after the last finding from bypassing merge consent or carrying
a previous audit's approval. The ordinary approved-set and freshness gates still
control publication.

## Content overview

The content overview is a derived context artifact with lower authority than the
content it describes. It is produced from the reviewed files, surrounding
documentation, references, and repository history after those inputs are
available.

`prepare-content-overview.md` defines the ordered section headings, including
Design changes and Significant changes immediately after Substance. The overview
worker compares the before and after states against the relevant design
documents, including their pre-change versions when updated or removed. It cites
documented design changes even when the documents are unchanged, and separately
lists significant behavioral changes to existing logic whose previous behavior
was undocumented, without duplicating entries. That list carries the caveat that
the previous behavior may have been incorrect and an entry may be a fix rather
than a design change.

The orchestrator verifies the headings, applicable design references, and the
caveat when Significant changes lists entries.
Snapshot creation also enforces the section order before publication. Sections
without applicable entries remain present and explicitly say so; reviews of
content as it stands do not manufacture a before state.

For pull request reviews, the orchestrator supplies the current capture and
the shared `pwsh-calls/query-pr-context.md` recipe. The overview selects URL and
commit metadata as needed; the orchestrator verifies its link beneath the title
with the recipe's `metadata -Field pullRequestUrl` query. Neither reads the
canonical capture directly.

The overview distinguishes observed incompleteness from uncertainty and turns
uncertainty into review questions. Review workers consume those questions as
hypotheses, not findings. The overview never supplies instructions to workers,
which prevents untrusted repository prose from crossing the prompt boundary as
control input.

When the reviewed commits move, the overview is regenerated from refreshed
evidence. Focus-area generation is independent of that content snapshot and
does not need to be repeated.

## Cache inputs

The focus-area cache memoizes instruction expansion, not review results.

Its key represents every input capable of changing the generated guidance:
materialized instruction sources, their stable paths and contents, the
generation recipe, common and focus-area data guides, format, schema, rendering tools and profile, the cache
schema, and the external-reference freshness policy. Review files, discussion, and session identity are excluded
because they do not define the guidance corpus.

The default generator profile identifies the model that verifies the final
focus-area scope semantics. The generation recipe also records the preparatory models.

Input selection does not depend on predicted relevance to the reviewed content.
If it did, cache equivalence would inherit a nondeterministic judgment and could
silently reuse incomplete coverage.

## Cached artifacts

A cache entry is a complete generation result: the raw expanded guidance, its
categorization, the focus-area set, and a manifest binding inputs and outputs by
digest. Scope declarations and source provenance travel in those Markdown
artifacts without a separate cache representation. Worker maps and the merged
JSON are generation intermediates, not prerequisites for using a cache hit.
Generation recipe and tool digests bind reuse to the scope-preservation and
rendering requirements.

Partial restoration is forbidden. Any missing artifact, digest mismatch, schema
mismatch, or degraded source turns the entry into a miss. This makes the cache a
transparent performance layer rather than an alternate source of review policy.

## Publication and restore

Cache entries are assembled in staging and published atomically under a
content-derived key. Published entries are immutable. Per-key coordination
allows concurrent sessions to converge on one entry without exposing a partial
one.

Restoration is also staged. The candidate is verified inside session state
before it replaces active artifacts, and replacement retains enough prior state
to roll back a failed swap. Cache maintenance is best effort and coordinated
with readers; failure to store, restore, or prune never reduces review coverage.
Stores and restores wait through temporary cache-key contention. Assembly and
cache copies also hold the session artifact-set lock throughout replacement or
inspection, so another command cannot observe a partly swapped set.

Diagnostics record whether the cache was used and whether source expansion was
degraded. Publication consumes those diagnostics without coupling substantive
finding content to cache behavior.

## Pull request capture layout

Pull request integration is divided by responsibility rather than by workflow
phase:

| Concept | Responsibility |
| --- | --- |
| Target resolver | Produces validated, canonical coordinates without network access. |
| Platform dispatch | Selects the one adapter and transport family for the target. |
| Adapter | Maps a service vocabulary into review concepts. |
| Canonical model | Orders, compares, enriches, and serializes platform-neutral evidence. |
| Local query | Projects canonical evidence into bounded, snapshot-bound pages for agent reading. |
| Transport | Executes service requests and classifies communication failures. |
| Transport host | Owns process, credential, encoding, and HTTP primitives. |

Executables are thin composition roots over these concepts. Capture, local
queries, registration, authorship enrichment, and publication remain separate
operations so a read-only command cannot gain a remote or mutating behavior
through a mistaken option.

Reviewer registration runs through the shared `ss-skills-util` executable on
`PATH`, independently of these review-only PowerShell libraries. Its native
transport and identity operations also serve proactor selection and confirmation.
The remaining capture, analysis, and publication operations stay within this
skill. See the utility's [implementation guide](../../../packages/ss-skills-util/docs/implementation.md)
for its transaction and provider boundaries.

## Target resolution

Target resolution is a pure boundary operation. It parses browser URLs or
explicit coordinates, validates platform-specific requirements, applies
explicit overrides, and emits one canonical target plus its browser URL.

No credential or service request is created until resolution succeeds. This
makes malformed or contradictory targets cheap to reject and ensures every
consumer uses the same target identity.

## Platform dispatch

Dispatch is centralized and closed: each recognized platform maps to one adapter
and transport family, and an unknown platform is an error rather than a
fallback.

Dependencies are constructed lazily after selection. Code for an unused
platform is therefore neither initialized in production nor required by a
focused test. Composition roots remain free of service branching.

## Platform adapters

Adapters are anti-corruption layers between remote APIs and the review model.
They know service payloads, pagination, and resource identities. They do not
decide ordering, authorship policy, finding relevance, or publication approval.

An adapter preserves all actionable discussion and maps only concepts for which
the canonical model has a defined meaning. Native status and metadata remain
available beside normalized values when the platforms do not make equivalent
claims. Every paged collection is exhausted or the operation fails; truncation
must never look like an empty discussion.

### Azure DevOps

Azure DevOps iterations provide the reviewed source and merge-base commits as
well as the change identities required for anchored comments. Thread anchors
retain both pre-change and post-change positions when available.

The adapter filters service events only when they contain no actionable review
content. It preserves the service's native thread metadata rather than deriving
review meaning from undocumented values.

### GitHub

No single GitHub API exposes the complete review model. The adapter combines
REST resources with GraphQL thread, edit, and branch-rewrite data, joining them
by stable service identifiers.

Each piece of discussion has one canonical owner. Review-thread comments are
not repeated under their enclosing review, and review summaries are represented
separately from replyable conversations. The merge base is resolved from
commits rather than branch names so capture remains useful after branch
deletion.

## Canonical document

The canonical document is a versioned evidence model optimized for review
operations. It is deliberately not a full pull request model.

The document provides stable identifiers, deterministic chronology, normalized
conversation kinds and resolution states, verbatim bodies, the reviewed commit
pair, and the native details needed to resolve unusual cases. Ordering uses an
event timestamp with an identifier tie-breaker; identifiers remain opaque and
case-sensitive.

Comparison is defined over review-relevant change: discussion appearance and
disappearance, edits, resolution movement, and movement of either reviewed
commit. Lifecycle and approval state are excluded because the review does not
own merge administration.

The initial and all refreshed captures coexist and remain immutable.
Classification writes a separate capture-bound semantic sidecar; consumers
overlay validated decisions without rewriting captured bytes. Every refresh
uses a new prefix and compares against the last checked capture. The initial
capture remains the baseline for lifetime follow-up accounting, and the
audited capture remains the evidence to which the user's answer refers.

Freshness runs after synthesis before policy classification, then again after
audit immediately before publication. Semantic revalidation remains orchestration:
scripts prove identity and changed evidence, not whether a claim still stands.
Pre-audit retirement uses the ordinary removal plan; post-audit retirement uses
append-only dropped decisions. Approved schema version 2 adds a required
`reviewedContextHash` (null without PR context) for the checked source, merge
base, PR intent and discussion. Publication compares the approved fingerprint
with its input capture and a live canonical capture before the first remote
write, including draft disposal. Capture timestamps, live reviewer votes and
semantic-classification progress are excluded from this evidence fingerprint.
Exact bodies and attribution are compared even when provider edit timestamps
do not change. Bounded changes include a `reviewEvidence` entry for changed
evidence beyond the named comment, resolution and commit categories.

Schema mismatches fail explicitly. Silently comparing two incompatible models
could report either wholesale change or no change, both of which would corrupt
the publication decision.

## Bounded local query boundary

`Read-PullRequestContext.ps1` is the read-only composition root for agent access
to canonical captures. It validates named arguments, reads and validates local
capture data, selects a projection, and serializes a bounded page. It neither
fetches service data nor modifies a capture. Publication and validation keep
their full-document readers and path-based inputs; query projections are not
replacement canonical artifacts.

Workers create requests with typed arguments to `New-PullRequestQueryRequest.ps1`.
The existing serializer writes normalized selectors together with both capture
identities and their paths. Saved requests use the same reading-snapshot identity
as direct queries, including the authorship policy and applied semantic decisions.
Literal search text may come from a UTF-8 file;
typed file arrays bind inside a noninteractive PowerShell program rather than
JSON inside a shell string. The query recipe distinguishes PowerShell callers
from Bash callers. Bash-backed tools save the canonical PowerShell example to a
private script with a file-writing tool and invoke it through `pwsh -NonInteractive -File`.
This keeps query instructions in one program and prevents Bash from interpreting
PowerShell syntax. Scalar `-File` invocations can run directly.
Creation validates the selected query against its
capture before publishing the request.

The saved reader accepts only the request path and a replay, next, or restart
operation. Its retained lock covers each read-modify-write operation. Each read
persists its bounded page and starting cursor before returning it. Before replay
or continuation, the reader regenerates the page from the bound capture,
normalized selectors, and starting cursor and compares its entire serialized
projection. This validates item shapes and evidence against canonical data
rather than trusting a checksum stored alongside editable page contents.
Ordinary reads replay that validated page so an interrupted invocation cannot
skip evidence. Only explicit
continuation advances the stored opaque cursor. Both replays and continuations
check the request identity and capture hashes; changed selectors require a new
request, and changed capture content requires an explicit restart. The direct
query parameter set remains available to programmatic callers.

The projection layer exposes these operations:

| Query | Projection |
| --- | --- |
| `metadata` | One required allowlisted `-Field`. `reviewers` and `revisions` enumerate individual projected records; other fields expose only the selected value. |
| `threads` | Descriptors with IDs, including `openingCommentId`, URLs, canonical kind and resolution, outdated status, anchor, decision, opening-comment excerpt, and comment count; no full bodies. |
| `comments` | Chronological comments for a selected thread, an exact thread/comment pair, an authorship class, or an explicit file/global selection. Verbatim content and attribution support semantic interpretation. `-IndexOnly` omits bodies and excerpts for allocation and discovery. |
| `search` | Case-insensitive literal `-Text` matches against complete comment bodies and the description when scope permits; one bounded match excerpt per matching comment. |
| `changes` | Delta entries comparing required `-ComparePath` with `-Path`, with baseline/current/both references for targeted evidence retrieval. |

The metadata allowlist covers capture and PR identity, title and description,
author and creation time, refs and reviewed commits, self-authorship, draft status, reviewers,
and revisions. The exact field names and invocation examples are centralized
in `pwsh-calls/query-pr-context.md`. Native service metadata is explicitly
projected, not recursively exposed as an arbitrary provider object.
String-valued fields use `content` slices; non-string and null fields use
`value`, as does each enumerated reviewer or revision.

Thread, comment, and search selection share file, scope, canonical-kind, and
resolution filters. Saved requests use a nonempty typed `-Files` array of
repository-relative paths. The direct query interface also accepts mutually
exclusive `-FilesJson` and `-FilesPath`. File selection implies file
filtering; `-Scope global` cannot be combined with it. Scope defaults to
`all`; file and global scopes select anchored and unanchored discussion
respectively. Path matching normalizes the leading provider slash and slash
separator, then uses ordinal case-sensitive comparison. Known prior names
must be supplied explicitly; the query infers no rename history.

All resolution states and outdated threads are included by default. Comments
require an explicit thread ID, authorship, file, or global selection;
`-CommentId` also requires `-ThreadId`. Kind or resolution alone cannot enable
bulk comment reading. These are selection safeguards, not semantic judgments
about which discussion a finding must account for.

Each response uses `{schemaVersion, query, snapshotId, totalCount, items,
nextCursor, complete}` with query schema version 1. Every query accepts a page
size from 1 through 50, defaulting to 20. Independently of item count, serialized
UTF-8 stdout, including its newline, cannot exceed 16 KiB. The bound applies
after JSON escaping and encoding; projection size alone is not the output
budget. There is no override or full-document query.

Descriptions and comment bodies that exceed a page's budget use exact
contiguous `content` slices with `contentOffset`, `contentLength`, and
`contentComplete`. Offsets use UTF-16 code units, with boundaries adjusted so a
surrogate pair is never split. Discovery excerpts are deliberately distinct
from these verbatim slices and cannot substitute for complete evidence.

Cursors bind the effective capture snapshot, classification policy and original query arguments, including the
comparison snapshot for changes. Continuation repeats those arguments with the
opaque cursor. A semantic-decision update invalidates
its cursors explicitly; a reader reports the error and starts a fresh query,
not a guessed positional offset. Query errors block dependent analysis until
recovery succeeds, without authorizing a raw-file fallback. The shared
`error-recovery.md` policy directs workers and the primary orchestrator to
correct invocation mistakes and retry safe transient failures before escalating.
For request creation, the existing capture is the input and the saved request
is a separate output; confusing those paths requires a corrected invocation,
not suspension of the review. Independent workers remain active during repair.

Recovery uses documented retry/resume semantics and bounded transient retry
budgets. It never clears durable state or treats missing output as completion.
Before restarting a write-capable command, the orchestrator establishes that
no remote write was attempted and that its lifecycle permits re-entry.
Ambiguous or attempted publication writes retain their human-reconciliation
boundary; absence of a receipt alone does not authorize replay. Unrecoverable
external failures and required human decisions still suspend affected work.

Prompt policy governs the aggregate reading budget: workers process relevant
pages and retain identifiers, evidence references, and decisions instead of
concatenating a full capture. Required exhaustive classification and freshness
discovery use bounded batches, with complete relevant conversations read only
as needed. The CLI bounds its own output, not access to raw files; this is not
an access-control boundary.

## Capture and enrichment summaries

Capture stdout contains scalar metadata and counts, including
`indeterminateCommentCount` and `reusedClassificationCount`, rather than comment-ID lists. `delta` is null
without comparison; otherwise it contains `threadsAddedCount`,
`threadsRemovedCount`, `commentsAddedCount`, `commentsRemovedCount`,
`commentsEditedCount`, and `resolutionChangedCount`, alongside `hasChanges`,
`evidenceChanged`, source/merge-base movement flags, and the compared commits. Detailed changes
are available through the paged `changes` query, not summary arrays.

`Set-CommentAuthorship.ps1` emits only `contextPath`, `applied`, `unchanged`,
`indeterminate`, and `isComplete`. Its caller discovers unresolved records with
`comments -Authorship indeterminate`, not a printed ID list. These summaries
keep orchestration diagnostics independent of discussion volume.

## Verbatim fidelity

Strings received from a service are opaque evidence. JSON handling must not
reinterpret date-shaped comment bodies, localize timestamps, apply console
encoding, or truncate nested values.

Parsing therefore uses a representation that preserves string values, and
serialization uses explicit depth, UTF-8, and stable line endings. Timestamps
used for ordering are canonicalized separately from the original body text.

Transport choice follows the same requirement. GitHub CLI API output is
byte-faithful. Azure DevOps discussion uses direct REST because Azure CLI output
can lose characters through host encoding; Azure CLI is limited to acquiring
an ASCII-safe access token.

## Authorship classification

Authorship enrichment is monotonic within a capture and classification policy.

The capture first classifies identity kind and applies deterministic proofs of
agent authorship. The GitHub adapter also classifies vote-only reviews as human
when their body is empty or whitespace-only and the review's GraphQL comment
count is zero, unless agent attribution is already established. It records
`review:voteOnly` evidence; a missing or invalid count is an error, not evidence
of an empty review. The reading pass uses
`pwsh-calls/query-pr-context.md` to select indeterminate comments with their
verbatim text and attribution, including body continuations. The language model
classifies them as agent or human; projected metadata alone does not establish
authorship. It cannot weaken a classification already supported by mechanical
evidence.

Identity matching has separate modes for separate risks. Record joins use a
strict comparison so distinct accounts are not collapsed. Guards around remote
writes accept any authoritative agreeing identity field because an unnecessary
write is more dangerous than an unnecessary no-op.

`schemas/comment-authorship.schema.json` defines the capture's `.authorship.json`
sidecar: schema and policy versions, raw capture hash, and an explicit decisions
array. Each entry has opaque thread/comment IDs, an exact input fingerprint,
agent/human kind, and classified/reused origin. Empty decisions are valid;
completion is derived from the effective capture's remaining indeterminate
comments. Missing required fields, unknown fields, duplicate IDs and unsupported
schemas are errors. No sidecar means no semantic decisions.

The input fingerprint includes provider, repository/PR URLs, thread/comment
identity, exact body, full normalized author attribution, self attribution,
deterministic evidence, and policy/schema versions. Source movement and fetch
time are not authorship inputs. Increment the policy version when semantic
classification guidance or its evidence contract changes. A policy mismatch
leaves comments for classification rather than reusing old judgments.

Capture runs deterministic provider checks before matching prior decisions.
Only matching indeterminate comments receive semantic decisions; removed
comments retain their evidence only in earlier captures. The capture command
creates sidecars, classification appends decisions, and the shared capture
reader validates and consumes them. Reclassification under another policy
replaces the policy's semantic record, not the immutable evidence.

Batch enrichment requires the bounded query's `snapshotId`, validates every
update, and atomically replaces only the sidecar. An exclusive sibling lock
protects the command's read-modify-write operation. Invalid input or a stale
reading snapshot leaves stored decisions unchanged. Query snapshot hashes
include the effective overlay and policy, so a completed batch invalidates old
filtered cursors.

The orchestrator can allocate classification batches from a body-free
`comments -Authorship indeterminate -IndexOnly` query. Classifiers retrieve
the selected comments by their exact thread/comment ID pairs; index metadata
alone is never classification evidence.

Each reading batch observes an immutable snapshot. Workers collect decision
files, then a single writer applies them after all readers of that snapshot
finish. Large sets use bounded, nonoverlapping batches; after a write, a fresh
indeterminate query starts without a cursor because classified records leave
that result set. The pass finishes only when every indeterminate comment is
processed. This avoids both stale-cursor reads and offset-based skipping as
the selected set shrinks.

## Diff retrieval

The local Git repository is the authoritative source for changed lines because
service patch APIs may omit or truncate content. Publication computes the diff
from the captured commit pair and fails if either commit is unavailable.

Service-specific change identities remain adapter data where a platform needs
them to place comments. The installed `ss-skills-util git diff` operation owns
exact Git comparison and path/hunk parsing. The PowerShell adapter maps the
result into the reviewed-commit model and retains finding lookup and hunk
membership. Parser fixtures invoke the same native parser; isolated Rust
integration tests exercise the real Git process boundary.

## Finding rendering

Rendering is a pure transformation from structured findings to ordered Markdown
blocks. It has no service, file-system, clock, or mutable-state dependency.

Pull request comments, tracker items, audit views, and combined documents share
the same substantive block builder. Destination renderers add only their
required wrappers. This makes "approved prose equals published prose" an
architectural property rather than a convention between separate formatters.

Each prose section label is separated from its unchanged content by a blank line.
This lets lists, code fences, and other Markdown blocks begin at the start of a
field without becoming part of the label's paragraph. The same separation applies
to short prose, avoiding guesses about whether a field begins with block syntax.

The summary is derived from the approved finding set and diagnostic artifacts,
not from remote publication observations. The renderer also owns the required
speaker marker so no adapter can omit or duplicate it.

## Publication

Initial overview publication and final finding publication are independent
operations. The following finding-publication journal does not gate or replay an
overview attempt.

Remote publication is implemented as a guarded sequence:

1. **Plan** resolves targets, approved artifacts, destinations, and rendered
   bodies. Ordinary planning is read-only. Explicitly discarding a pre-existing
   GitHub draft is the sole authorized remote mutation at this point and occurs
   before the journal exists.
2. **Preflight** checks every locally knowable invariant and asks the adapter
   whether remote publication can begin.
3. **Journal** persists the intended operations before the first non-idempotent
   write.
4. **Execute** performs each operation without automatically retrying an
   ambiguous write.
5. **Verify findings** reads back the finding state attributable to the run
   before completion.
6. **Complete** publishes the summary and optional merge verdict only after
   finding verification succeeds.
7. **Verify completion** reads back the completed review, including its summary
   body. The optional merge verdict is sent during completion but is not
   separately read back.
8. **Reopen follow-ups** returns replied-to threads that were not active in the
   original capture to an unresolved state.

The journal is a recovery aid, not a distributed transaction. Each operation
distinguishes a definite service refusal from an ambiguous failure that may
have written state. Any prior operation that wrote or may have written blocks a
fresh run until a person reconciles local and remote state.

GitHub implements completion by submitting one pending review. Azure DevOps has
no draft review, so findings become visible individually and the summary is the
completion marker. These strategies differ at the adapter boundary while
sharing the same plan, preflight, journal, and verification model.

Before casting an Azure DevOps approval vote, the adapter reads the reviewer
collection and locates the running account using the identity matching shared
with registration. The single-reviewer `PUT` explicitly carries the entry's
`isRequired` value alongside the vote, because omission makes the reviewer
optional. An absent entry uses the optional default.

Thread reopening is follow-up state rather than proof that findings were
published. A run whose findings verify but whose thread-state cleanup fails is
reported as published with outstanding follow-up, preventing a retry from
duplicating the review.

Markdown delivery terminates after deterministic rendering and local
persistence. With no remote state to reconcile, it needs no publication journal,
service verification, or completion operation.

## Tracker target resolution

Tracker resolution is separate from pull request resolution because the source
of evidence and the destination of work are independent.

The resolver produces either a GitHub repository target or an Azure DevOps
project target and validates only metadata meaningful to that platform.
Keeping this concept independent enables cross-platform delivery without
teaching pull request adapters about issue management.

## Tracker publication

Tracker publication uses the same approved findings and deterministic rendering
but a different transaction model. A tracker item is visible immediately and
cannot participate in a review-level draft.

All items are rendered and validated before creation begins. Creation then
proceeds one item at a time, with read-back verification before the next item.
The journal records each durable item identity and blocks replay after any
successful or ambiguous creation.

PR-backed approved artifacts require a non-null evidence fingerprint and the
matching checked capture. The shared publication guard compares that capture
before rendering, opening a transport, or writing a continuation journal.
The binding concerns the reviewed PR, not the tracker destination, so
cross-platform delivery preserves the same evidence requirement. Reviews
without a PR use a null binding and no capture.

A paused body-adjudication state is the only resumable path. Continuation is
bound to the exact item, approved body, stored body, destination metadata, and
remaining publication plan. This allows the existing run to continue without
turning general failure recovery into a duplicate-producing retry mechanism.

The review summary is returned to the orchestrator rather than filed as work,
because it describes the delivery and does not represent an independently
actionable recommendation.

### Body verification

Read-back verification first checks exact title and body equality. Exact
equality is the strongest evidence that approved prose survived.

GitHub divergence is a failure because GitHub promises verbatim issue bodies.
Azure DevOps may pass Markdown through a storage transformation, so comparison
also accepts well-defined representational changes such as line-ending or entity
normalization. Clear substantive divergence fails.

Before an Azure DevOps description enters the JSON Patch document, the adapter
entity-encodes literal angle brackets. Azure DevOps Markdown accepts raw HTML,
so unencoded text such as an angle-delimited placeholder can be parsed as an
element and disappear from the rendered work item. Encoding only the brackets
preserves existing entities while making the bracketed text literal. Read-back
verification decodes that representation for comparison with the approved
prose. This matches the encoding applied to Markdown work-item fields by
[Microsoft's Azure DevOps MCP](https://github.com/microsoft/azure-devops-mcp/blob/main/src/utils.ts).

An uncertain transformation is a user decision, not a similarity heuristic
disguised as approval. The journal records enough exact and summarized evidence
for that decision, and content hashes ensure the accepted bodies have not
changed before continuation.

## Tracker adapters

Tracker adapters expose a narrow interface: validate a destination, create one
item, and read back what the service stored.

GitHub supplies repository identity and issue capability. Azure DevOps supplies
work-item type, area, visible body field, Markdown format, and optional parent
link. Required service metadata is validated during preflight rather than
invented by the adapter.

Azure DevOps work-item creation uses the `7.2-preview.3` REST resource with an
`application/json-patch+json` body. The description field operation is paired
with `/multilineFieldsFormat/<field> = Markdown`, as required by
[Azure DevOps work-item Markdown support](https://devblogs.microsoft.com/devops/markdown-support-arrives-for-work-items/).

No tracker adapter renders finding prose or decides whether a stored rewrite is
acceptable. Those responsibilities remain in the shared model.

## Initial overview publication

`Publish-ContentOverview.ps1` freezes the preparation overview in session state
and uses platform adapters separate from review submission. `overview-snapshot.json`
binds its immutable Markdown to the PR, inspected commits, run identifier, and
content hash. A separately frozen HTML artifact and its ZIP container serve ADO. The mutable local
overview and audit documents may subsequently describe newer content.

`overview-publication-result.json` records intent before each non-idempotent
write and retains returned IDs before verification. Upload is verified before the
linking comment is created. Known successful attempts are reused; an attempt
that wrote or may have written remote state is not automatically replayed.
The initial comment is not edited and later overview generation does not publish.
`Publish-ContentOverview.ps1` passes its optional `-CommentPrefix` through the shared
publication function to `New-OverviewComment`. The builder uses the verified artifact
URL on either platform and prepends a nonempty prefix verbatim, separated from the
linked sentence by a blank line. The default prefix is empty so callers explicitly
select attribution according to their active instructions rather than a hard-coded policy.
Creation and verification use the same complete comment body. Resuming a successful
publication reuses its receipt even if the caller supplies a different prefix.

All publication failures are explicit outcomes at this boundary, including
partial uploads, unresolved ADO threads, and diagnostic-write failures. The
orchestrator notifies the user and continues reviewing without a repair gate.
It still prompts for missing-tool installation, and may retry a tool-only
failure after consent and setup when no remote write was attempted.

GitHub uses a `public: false` Gist and a separate issue-comment endpoint for the
PR conversation. Native Mermaid source is preserved. Truncated Gist API content
is verified through the complete Git revision rather than treating the partial
JSON field as the report; this also covers the documented large-file raw retrieval
limit without another size-dependent code path. The comment links the rendered
revision, not Raw. No Gist web-rendering size ceiling is inferred from API limits.

ADO uploads and reads the exact ZIP bytes, then creates an unanchored thread with
`fixed` status. The archive contains only `report.html`; its stable entry timestamp
keeps packaging independent of wall-clock time. Its binary transport path bypasses JSON parsing and HTML sign-in sniffing
only for explicit attachment reads; HTTP status and frozen content-hash checks
establish identity. JSON callers retain their existing checks.

Captured discussion remains verbatim. The orchestrator and reviewers recognize
informational overview comments using receipt IDs and declared purpose, while
preserving substantive replies and later human activity. A report label or
`ss-review:initial-overview` marker is not an authorization grant or a reason to
exclude an entire thread.

## Report tools

Renderer dependencies use shared user-local storage rather than skill checkouts,
so relinking skills or starting from another worktree does not lose installed tools.
The root is `LOCALAPPDATA/ss-review/tools` when that variable is set, otherwise
`~/.local/share/ss-review/tools`. Portable Pandoc uses its pinned version directory;
Mermaid uses a directory keyed by the SHA-256 hash of `tools/package-lock.json`.
This keeps different dependency sets separate while sharing identical sets across
skill copies. Discovery and rendering resolve that directory directly, without
depending on npm's global prefix or exposing renderer commands on `PATH`.

The installer copies the package manifests into a unique sibling staging directory,
checks the copied lockfile against the selected directory key, and runs the locked
npm installation there. A checkout update during the manifest copy produces an
explicit failure rather than publishing dependencies under the wrong key.
Only npm uses the staging working directory; browser setup uses the caller's
working directory so its Puppeteer configuration matches discovery and rendering.

A retained user-wide installation lock excludes checks and rendering during setup;
rendering and discovery retain shared ownership. Reuse requires Mermaid's CLI,
Puppeteer's installer and manifest, and the runtime entry point named by that
manifest. Missing package files cause setup to rebuild through staging. Once npm
and browser setup succeed, the installer publishes the complete tree by directory
rename. Replacement retains the old tree until staging is ready and restores it
if publication fails. Failed staging trees are removed without deleting existing
installations. A complete dependency tree is reused while browser setup can still
repair a missing browser download. Checkout-local dependencies are neither used
nor deleted, and the skill directory requires no write access.

The PowerShell tool check reports missing Pandoc, Node.js, Mermaid CLI, and
Mermaid's browser explicitly, retaining browser-discovery diagnostics. Its JSON
record always contains `pandoc` and `mermaid` paths, nullable `node` and `browser`
paths, a `missing` array of tool identifiers, and a `diagnostics` array of strings.
Empty arrays mean no discovery failures. Identifiers are `pandoc`, `node`,
`mermaid-cli`, and `mermaid-browser`; discovery failures also mark the affected
tool missing. When nothing is missing, the check renders temporary Markdown with
a Mermaid diagram through the production HTML renderer and verifies the embedded
image. A failed render exits unsuccessfully rather than emitting a ready result.
The installer performs the same check before reporting success, retaining its
exclusive installation lock throughout the render. Temporary probe files are
removed on both success and failure.

The primary session obtains installation consent;
noninteractive scripts never ask, install implicitly, or launch an authentication
prompt. An explicit installer uses a pinned npm lockfile and the browser installer
from that dependency set. npm engine checking is strict so an unsupported Node.js
runtime fails installation instead of merely warning. On Windows x64, the installer
verifies the official Pandoc archive's SHA-256 digest before installing it in
user-local storage. Platform prerequisites and repair commands live in
`report-tool-setup.md`, loaded only when setup or diagnosis is necessary.
The installer marks its post-install discovery assertion as an installation attempt
so failures direct the primary session to diagnosis instead of repeating the consent
request. Browser-install failures also point to the guide's targeted incomplete-cache
procedure. Cache deletion remains a separately approved user action with exclusive
cache access; the installer never infers deletion targets from error text.

On Ubuntu, `-InstallBrowserDependencies` explicitly opts into system package
installation. Under the same tools lock, the installer reads `deb.deps` beside
the resolved Chrome executable and passes its dependency relations to
`apt-get satisfy`, following Puppeteer's dependency-installation mechanism.
This preserves the browser build's version constraints and distro package
alternatives instead of maintaining a second package list. Missing or empty
metadata is an error. It refreshes apt indexes and installs the dependency set
and Liberation fonts using noninteractive `sudo -n`; npm and browser downloads
remain under the review user's identity. It never elevates Node.js, changes
sandbox policy, or accepts a package-manager exit status as proof of rendering
readiness.

Node.js discovery prefers `PATH`, then the active nvm location (`NVM_BIN` on
POSIX systems or `NVM_SYMLINK` for nvm-windows). On POSIX systems without an
active runtime, it loads `nvm.sh` from `NVM_DIR`, or the standard
`XDG_CONFIG_HOME/nvm` or `~/.nvm` location, in a separate noninteractive shell.
It disables automatic activation and asks nvm to resolve its `default` alias.
It does not load shell profiles, select a repository's `.nvmrc`, install a
runtime, or change the caller's selected version. An unusable nvm default
produces an explicit diagnostic rather than selecting an arbitrary installed
version. Rendering invokes the resolved executable directly; installation
temporarily prepends its directory to `PATH` for npm and invokes the same
executable for Puppeteer's browser setup.

Pandoc and browser tools are HTML prerequisites, not GitHub Gist prerequisites.
An explicit installation decline permits Markdown audit presentation. The
user's decision is retained for the run to prevent repeated installation prompts.

## HTML report rendering

`ConvertTo-ReviewHtml.ps1` reads canonical Markdown and uses Pandoc's GFM syntax
tree, transformed in PowerShell. Source raw nodes become visible text/code, while
only the renderer's Mermaid transformation inserts controlled raw HTML.
Repository-relative finding references remain visible as labels and destinations
rather than linking relative to the session directory. External images and
unsafe-scheme hyperlinks are refused rather than fetched.

Mermaid runs with a fixed strict configuration, text labels, deterministic IDs,
system fonts, and a neutral theme. SVG is checked for active or external content,
then embedded as an image with escaped source in a native details element.
Pandoc performs static syntax highlighting and uses the skill-owned template.
The template's neutral palette, spacing, and scrollable content favor prolonged
reading and printing over platform-specific branding. A compatible CSP permits
only the embedded styles and data images.

Output is replaced atomically only after successful conversion. Results identify
the exact source hash and the HTML path/URI/hash, so an old file is never presented
as the current audit after failure. The existing Markdown renderer remains the
sole producer of finding prose and emits the audit document's path and file URI.
Before invoking the user-question tool, the orchestrator emits an ordinary
session message containing the audit request and applicable approval forms,
formats available document URIs as named Markdown hyperlinks with angle-bracketed
destinations, and retains their absolute paths separately as inline code so
Markdown cannot consume Windows path separators as escape syntax. Links are
emitted outside inline or block code, without backticks or quotation marks around
their destinations. For pull request reviews it selects `id`, `title`, and
`pullRequestUrl` from the audited capture through `pwsh-calls/query-pr-context.md`.
Every audit question repeats the PR number and full title in the question UI
and as a named PR hyperlink in the preceding session text, including when
findings will be filed in a tracker. This shared phase-7 reminder applies to
document and in-session audits, resumed or amended presentations, and the final
merge question. The tool
then asks for the user's decision or per-finding feedback by referring to the
links in session text rather than repeating their URIs or local paths in the
question UI. Document-mode amendments repeat this ordering with refreshed
documents; in-session amendments redisplay the complete current per-finding
Markdown before the next question.
HTML never becomes a finding database.

## Transport boundaries

Transports know requests, authentication, paging, throttling, and failure
classification; they know nothing about findings or pull requests.

Request executors, process invokers, delay behavior, and time sources are
injected. Models and adapters can therefore be tested offline against recorded
responses, and tests cannot reach a live service accidentally.

Read requests use bounded retries for server and connection faults.
Non-idempotent writes suppress these retries because a lost response cannot
prove that the service rejected the write. Explicit rate-limit refusals are
retried for both reads and writes until success, without consuming the transient
attempt budget. Local delays grow exponentially to a cap; service-supplied
`Retry-After` and primary-quota reset times are not shortened to that cap.
GitHub's fallback throttling delay follows its documented minimum pause.

GitHub non-paginated requests include response headers so recovery uses the
actual service guidance. Paginated reads retain the CLI's slurped JSON output
and use local backoff when no recovery time is available. GraphQL throttling
requires only rate-limit errors and no non-null execution data; partial mutation
results retain uncertain-write handling. ADO identifies throttling by HTTP 429.
Each wait emits a diagnostic. Interrupting a wait after a refusal remains a
definite failure; a lost response to the next attempt remains ambiguous.
Failure type is carried as structured metadata rather than inferred from error
prose by publication.

## Azure DevOps transport

Azure DevOps uses direct REST with an OAuth access token acquired through the
signed-in Azure CLI session. The token is refreshed only through a bounded
authentication path; personal access tokens are not part of the design.

The transport validates response shape, paging metadata, and declared counts.
A nominal success containing a sign-in page, an unexpected continuation, or a
missing list envelope is a failure rather than an empty result. This protects
the canonical document from plausible-looking truncation.

The proactor's paged reads explicitly request a page envelope carrying the
payload and continuation token; those callers own traversal and progress checks.
Ordinary capture reads retain the rejection of unexpected continuation.
Comment deletion explicitly permits an empty successful response, without
weakening the JSON requirement for reads.

## Verification against live pull requests

Most behavior is verified offline at the narrowest layer: pure model tests,
adapter tests over recorded payloads, composition-root tests with injected
transports, and publication tests over fake service state. Embedded GitHub
GraphQL documents are additionally checked against the live schema because a
mock cannot detect schema drift.

Live verification is reserved for claims only a real service can establish:
character fidelity, pagination completeness, anchor placement, draft and
notification behavior, reviewer preservation, Markdown storage, and read-back
shape. It is performed on disposable pull requests and tracker items whenever
the corresponding adapter contract changes.
