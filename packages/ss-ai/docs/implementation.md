# Skills utility implementation

## Architecture

The nonpublished workspace packages implement the utility-wide [design](design.md):

- `ss-ai` composes the command-line application and its executable tests.
- `ss-ai-cargo` reads workspace manifests and builds dependency inventories.
- `ss-ai-git` compares immutable Git objects and interprets unified diffs.
- `ss-ai-assessment` validates source inventory and judgment coverage for
  automatic reapproval without provider effects.
- `ss-ai-runtime` supplies process, clock, diagnostics, and atomic file transactions.
- `ss-ai-review` owns provider-backed review and reapproval operations.

Dependencies flow from command composition toward focused operations and shared
runtime boundaries, never back to the executable. Review selection, provider
evidence, approval authority, and publication stay together because they share
decisions and transaction boundaries. Assessment owns the independent source
and judgment model, while review retains provider-backed authority decisions.
The Cargo inventory has no provider or
network dependency. The focused packages support targeted test builds and
incremental reuse; a cold build of the complete executable still builds its
dependency closure. None of these internal Rust APIs is a supported SDK.

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
