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
- `ss-ai-review-context` owns local captured evidence, bounded queries, saved
  requests, and input diagnostics.
- `ss-ai-review-workflow` owns local finding artifacts, consolidation, audit
  records, approved projections, shared Markdown rendering, worker assignments,
  bounded shared-document reads, instruction snapshots, focus-area preparation
  and reuse, and their workflow plans.

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
Local finding operations use the same runtime transactions as PowerShell
consumers, so hybrid runs preserve accepted updates and artifact history.
Audit and projection operations share the captured-evidence serializer and
exact Git comparison with other review commands. PowerShell adapters keep the
remaining publication commands compatible without duplicating finding prose.
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
  evidence, assessment, publication recovery, and supported provider boundaries.
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
