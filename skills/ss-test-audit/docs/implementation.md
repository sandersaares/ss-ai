# ss-test-audit implementation

## Ownership boundary

The `ss-ai-test-audit` Rust package owns deterministic source discovery, parser-backed inventory, line indexing, state validation, bounded batch export, classification record validation, source freshness, and final aggregation. The `ss-ai` executable exposes those operations under `test-audit`.

The skill agent owns semantic evidence selection, the actual outcome description, classification, rationale, shared-caller confirmation, and presentation. Native code never infers `Documented`, `Implicit`, `Mixed`, or `Incidental` from keywords.

## State flow

```text
repository + scopes
        |
        v
inventory + search index
        |
        v
initial 25-site batch -> agent evidence judgment
        |
        v
advance: strict record + next 25-site batch
        |
        v
trace + test inventory + summary
```

The inventory, source file digests, and search index bind each batch to one audit ID and snapshot digest. Every later read verifies the persisted state and index digests. Mutating commands additionally verify current source hashes. A changed source returns a stale-state failure before accepting a judgment against moved lines or emitting a final report.

Read-only `search` and `next-batch` verify the persisted snapshot files without
rehashing every audited source. Mutating `record-batch` and `advance` operations
and final `report` generation rehash source files; mutation commands check
again immediately before writing. This avoids repeated repository traversal
while retaining a fresh source boundary for durable judgments and publication.

## Classification ingestion

The agent writes one strict batch with the active audit ID and snapshot digest. Native validation rejects unknown or repeated assertion IDs and requires category-specific fields. Citations must select an exact indexed path and line, and their excerpt must match that line. Documented and mixed categories require confirmed citations. Implicit requires ambiguous evidence. Incidental and mixed classifications require an explicit undocumented-mechanics explanation.

The primary `advance` operation records the batch and exports the next batch
using one validated state load. Separate `record-batch` and `next-batch`
commands remain available for recovery and intermediate inspection. Replaying
an identical classification is idempotent, including after the record succeeds
but next-batch output publication fails.

The primary agent-facing `submit` operation resolves compact semantic rows and
evidence handles into the same canonical classification batch, then delegates
to the advance transaction. Evidence handles contain a snapshot prefix and
exact index position, so native code resolves citations without accepting
caller-supplied path/line pairs. Reuse requires an explicit source row and
equivalence confirmation. Native code replaces, rather than inherits, reuse
citations and caller IDs.

Search accepts repeated independent queries and evaluates them from one index
load. Query count, per-query result count, and batch sizes remain bounded.

Batch export writes a machine record and a bounded text display under retained
file transactions. Report results include no-assertion, incidental-only,
unlinked-assertion, and diagnostic counts so completion does not require loading
the complete trace.

Static call matching links parser-visible helper names across indexed test bodies. The agent can add calling tests discovered through callbacks, traits, registration, or dynamic paths. A shared assertion without callers requires a reason; the tool does not silently assign it to every test.

The Rust visitor records direct `ExprCall` names for each test and reconciles those names after all files are scanned. Production assertions remain candidates and survive only when that exact call map selects their helper. Text is never a caller oracle, so declarations, comments, method names, and trait callbacks cannot create false links. Test-owned `Drop` panic sites are excluded because they commonly exist to induce an expected unwind; other callback assertions remain visible for agent attribution. Recognized `assert_panics` calls become one panic expectation, and the visitor suppresses panic macros evaluated inside that expected-unwind argument.

## Count semantics

The inventory counts source sites, not runtime loop iterations or repeated trait names in one macro. Rust `#[should_panic]` counts at the expectation attribute, not at the deliberately triggered panic. Ordinary `unwrap` and `expect` calls are not inventory predicates. A rustdoc fence is a test case; a compile-only fence remains in the inventory with no counted predicate.

The report computes assertion-site rates over classified rows and preserves test inventory entries independently. This prevents smoke tests and no-predicate doctests from disappearing while keeping their lack of checks explicit.

## Failure and coverage semantics

Schema version mismatches, stale files, malformed citations, duplicate judgments, and incomplete reports fail without changing the accepted journal. A versioned state does not cross-read or migrate older formats. Unsupported or unverifiable forms appear as scan diagnostics and workflow caveats rather than successful empty coverage.
