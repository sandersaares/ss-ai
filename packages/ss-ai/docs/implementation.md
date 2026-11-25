# Skills utility implementation

## Architecture

The nonpublished workspace packages implement the utility-wide [design](design.md):

- `ss-ai` composes the command-line application and its executable tests.
- `ss-ai-cargo` reads workspace manifests and builds dependency inventories.
- `ss-ai-git` compares immutable Git objects and interprets unified diffs.
- `ss-ai-assessment` validates source inventory and judgment coverage for
  automatic reapproval without provider effects.
- `ss-ai-runtime` supplies process, clock, diagnostics, BOM-aware text decoding,
  and atomic file transactions.
- `ss-ai-review` owns provider-backed review and reapproval operations.
- `ss-ai-review-context` owns local captured evidence, bounded queries, saved
  requests, and input diagnostics.
- `ss-ai-review-workflow` owns local finding artifacts, consolidation, audit
  records, approved projections, shared Markdown rendering, worker assignments,
  bounded shared-document reads, instruction snapshots, focus-area preparation
  and reuse, report tooling, HTML rendering, and their workflow plans.
- `ss-ai-test-audit` owns immutable local test inventory, searchable evidence
  indexing, bounded classification batches, snapshot freshness, and final
  trace/test/report aggregation for `ss-test-audit`.

Dependencies flow from command composition toward focused operations and shared
runtime boundaries, never back to the executable. Review selection, provider
evidence, approval authority, and publication stay together because they share
decisions and transaction boundaries. Assessment owns the independent source
and judgment model, while review retains provider-backed authority decisions.
Local context operations do not require provider transport and do not make
review or authorship judgments.
Capture creation composes the review provider adapters with the context
producer. The producer retains the destination lock across collection and
publishes the sidecar before the immutable capture, so readers never observe
a capture without its initial decisions. Semantic decisions use the same
sidecar lock and remain separate from provider evidence.
Classification JSON uses the context package's duplicate-preserving parser before
materializing a batch, so conflicting properties cannot disappear before validation.
Local finding operations use the shared runtime transactions to preserve
accepted updates and artifact history.
Individual audit decision input is normalized before the shared policy and
journal checks. Persisted plans and journals retain strict canonical values,
so normalization does not change the stored schema or approval authority.
Report commands take the active skill root explicitly, keeping the locked
dependencies and template tied to the skill that invokes them.
Initial overview publication keeps provider effects and the publication journal
in `ss-ai-review`. The command posts bounded Markdown comment bodies directly,
so provider transactions do not depend on report rendering or attachment tools.
Audit and projection operations share the captured-evidence serializer and
exact Git comparison with other review commands. Final review publication
combines these local operations with provider effects and a retained journal
lock in `ss-ai-review`. GitHub submission responses that may have reached the
service are reconciled with a completed-review readback before publication can
continue; reconciliation never resends the submission.
Tracker publication combines the native renderer with provider-specific
preflight and per-item journaled creation and verification.
Accepted Azure body rewrites become durable before the transaction records a
new creation intent. If saving that intent fails, the same accepted decision
can continue; after the intent is durable, the possible provider write requires
reconciliation instead of automatic replay. Native tracker journals require
the complete adjudication evidence produced by the native utility.
The Cargo inventory has no provider or
network dependency. The focused packages support targeted test builds and
incremental reuse; a cold build of the complete executable still builds its
dependency closure. None of these internal Rust APIs is a supported SDK.

The test-audit package is a one-way local consumer of the runtime process,
clock, file transaction, and error boundaries. `ss-ai` supplies local Git
identity without initializing provider transport. Rust syntax supplies
deterministic structural inventory; explicit text profiles cover the named
heuristic languages. Agents provide semantic judgments through strict snapshot-
bound records, so native validation can reject stale or incomplete work without
making public-contract decisions.

Rust scanning keeps test-owned sites distinct from production candidates. It
records direct call names while visiting each test and reconciles shared
assertions only after the complete scope is inventoried. The visitor recognizes
panic-expectation helper calls and suppresses deliberately triggered panics
inside them. This avoids method-name substring attribution while retaining
real free helpers and explicit static-assertion test cases.

`assert_eq_size` follows the static-assertion path. Empty generic helper
declarations with trait bounds are pre-indexed, and direct calls from test
context become compile-time assertion sites at the call. Selected `expect`
method calls on fallible API values become `fallibleValueCheck` sites; common
fixture and synchronization accessor names are excluded. Configuration
attributes on compile-time sites are preserved as scan diagnostics.

`advance` shares one loaded inventory and index between classification
validation and next-batch export. It still retains separate sibling file
transactions for the journal and output artifacts, so a durable record can be
replayed idempotently if publication fails. Multi-query evidence search shares
one state load and emits a bounded result per query. Read-only operations verify
stored state digests without traversing source files; mutation and report
operations rehash source content at their authority boundary.

Every mutation transition emits an explicit top-level continuation directive.
`continue` means another bounded batch is ready; `report` means classification
is complete but the final report remains mandatory; both forbid final-response
completion. The report alone emits `complete` with final-response permission.
This machine-readable gate complements the skill's prose continuation rules.

`submit` adds the agent-facing compact row boundary in front of `advance`.
Snapshot-bound evidence handles resolve to exact indexed source lines inside
the native utility. Explicit reuse requires equivalence confirmation and
replaces citations and callers rather than inheriting them. Batch exports carry
a machine record plus compact text display. Report results expose bounded
completion counts so ordinary workflow completion does not require a trace
read.

Final report publication builds one typed `AuditSummary` and publishes it as
JSON and Markdown. A matching `details` document carries the complete trace,
test inventory, and diagnostics in JSON and Markdown. Existing trace and
test-inventory JSON files remain unchanged compatibility projections.

The package boundaries follow the subject-owned, one-way dependency practices
used by [cargo-release-plan](https://github.com/folo-rs/folo/blob/db33786ec25196696b46f38e2b60a03bd33ec913/packages/cargo-release-plan/docs/implementation.md)
and [cargo-bench-history](https://github.com/folo-rs/folo/blob/db33786ec25196696b46f38e2b60a03bd33ec913/packages/cargo-bench-history/docs/implementation.md).

The command layer translates named inputs into explicit targets. Provider
adapters turn service evidence into identities, pull requests, and comments.
The selection state machine owns decisions and journal transitions; transport
and storage own external effects. Registration uses the same provider and
identity rules without requiring a selection journal.

## Implementation topics

Read the topics relevant to the responsibility being changed:

- [Automatic reapproval](reapproval.md) defines approval recognition, exact source
  evidence, assessment, publication, and supported provider boundaries.
- [Provider evidence and identities](implementation/providers.md) explains
  complete enumeration, coordinate resolution, and non-destructive registration.
- [Transport and authentication](implementation/transport.md) explains process
  and HTTP boundaries, Unicode handling, and request replay rules.
- [Journal transactions and selection](implementation/journals.md) explains
  stage transitions, ownership, locking, and atomic publication.
- [Runtime boundaries](implementation/runtime.md) explains cross-cutting time,
  dependency injection, errors, and command output.
- [Development and installation](implementation/development.md) explains
  validation coverage, platform setup, and executable discovery.

The topic structure separates shared mechanisms from functional areas. For
example, provider evidence supports both registration and selection, and the
time boundary supports both claim decisions and transport recovery waits.

## Assessment checkpoints

`ss-ai-assessment` owns the immutable unit manifest and exact conclusion coverage.
`units.rs` groups complete source hunks and provider discussion records,
including noncontiguous replies, then splits oversized text at exact scalar
ranges. The manifest covers the original serialized documents rather than a
second reconstruction of their bodies. Source/thread bindings accompany split
records through bounded digest-based binding references. Exact metadata remains
in the covered original ranges rather than being repeated on every fragment.
Workers use the stable parent to reacquire related evidence. Unit bounds reserve regular worker
context for policy, exact follow-up evidence, and reasoning.
Compact unit listings retain the references and sizes needed to schedule reads
without repeating range metadata. Units have independent receipts, so workers
can request their pages in parallel while the journal serializes updates. Complete
document coverage still requires each unit and its bindings.

The unit read plan groups independently addressable windows using serialized
evidence responses, including escaping and bindings. Fixed-size token fields
allow windows to finish in any order without requiring a manifest request before
every window. Planning simulates delivery on a private copy and never records
receipts. It reuses that copy across units rather than copying the full source
capture for each planned page.

`conclusions.rs` accepts grouped semantic conclusions and final synthesis.
Short references select inventory positions only within the exact snapshot and
state token. Native code expands groups into exact change judgments, binds policy,
patches and requested context, and derives conditional replacement digests.
This keeps evidence bookkeeping outside agent-authored prose without weakening
per-change validation. `read-next` stages required-document delivery using actual
serialized response sizes and current contiguous receipts; only the fitting batch
commits. Optional change selection filters patches and their requested context
before batching, so targeted assessment does not require worker-managed offsets.
A changed local token fails without receipts and requires no new snapshot.

`checkpoint.rs` owns immutable transport units and dependency digests.
Discussion notes and explicit checks are optional low-level records, not worker
workflow obligations. Final synthesis carries material discussion and remaining
concerns directly; native code binds all final judgments and discussion and
completes atomically. Unresolved concerns prevent renewal even with a favorable
label. Atomic batch requests
stage all reads or judgments against a state token before committing, reducing
round trips without allowing partial results. Earlier run schemas require a
fresh run and assessment; existing files are left untouched.
Source-context batches register known ranges in one transaction, without
recording unread text as delivered. Batch read responses put their common
snapshot and read token in the shared envelope rather than repeating them for
each evidence page. They omit repeated progress and unit IDs already carried by
page references; progress remains available from assessment metadata.
The semantic state token excludes read receipts so independent deliveries do
not invalidate a pending judgment batch. Judgment submission checks the current
receipts under the same transaction that records the judgments.

The shared dependency reader validates citations and bindings at recording and
reload boundaries. Replacements invalidate transitive dependent records. The
cumulative judgment binds final judgments, discussion, and any recorded obligations;
read receipts establish delivery only. The review run persists time, work, and
worker reservations before work so recovery cannot renew those budgets.
The review budget persists native completion time and excluded completed-idle
time. All semantic completion commands share the final work-time check and
freeze the clock only after that check succeeds. Authorized stale refresh
resumes the remaining active budget. Finalization takes a separate deadline
from its invocation time and checks it at the existing pre-effect boundary,
so delayed parent observation cannot corrupt assessor timing or bypass
freshness checks.
