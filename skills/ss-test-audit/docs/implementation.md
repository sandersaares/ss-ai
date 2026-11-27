# ss-test-audit implementation

## Ownership boundary

The `ss-ai-test-audit` Rust package owns deterministic source discovery, parser-backed inventory, line indexing, state validation, bounded batch export, classification record validation, source freshness, and final aggregation. The `ss-ai` executable exposes those operations under `test-audit`.

The skill agent owns semantic evidence selection, the actual outcome description, classification, rationale, shared-caller confirmation, and presentation. Native code never infers `Documented`, `Implicit`, `Mixed`, or `Incidental` from keywords.

## State flow

```text
repository + scopes -> inventory + search index -> initial batch

initial batch
  |-- report (empty) -----------------------------> publish final reports
  `-- continue --> judge evidence --> submit --> advance
                        ^                          |   |
                        `-- next batch (continue) -'   `-- report (exhausted)
                                                             |
                                                             v
                                                     publish final reports
```

The inventory, source file digests, and search index bind each batch to one audit ID and snapshot digest. Initialization publishes the state record last so readers cannot treat a partially initialized directory as a ready audit. Every later read verifies the persisted state and index digests. Mutating commands additionally verify current source hashes. A changed source returns a stale-state failure before accepting a judgment against moved lines or emitting a final report.

Audit state and inventory use schema version 3. A proven `Some(...)` fixture
holder receives an assertion identifier while scanning, then leaves the
effective inventory after all source IDs are assigned. This keeps identifiers
for later sites stable without including fixture extraction in denominators.
Rc/Arc clones and borrow guards retain the holder marker; an independently
initialized API `Option` remains a candidate.

For syntax that cannot reliably classify fixture ownership, initialization can
consume one strict correction request. Native code requires the exact repository
revision, path, line range, and scanner expression to match exactly one source
site before exclusion. The corrected inventory persists the original assertion
ID, column, expression, file digest, and reason and adds an
`inventoryCorrection` diagnostic. Corrections apply before the inventory digest
and audit identity are computed. Existing snapshots cannot be edited in place.

Read-only `search` and `next-batch` verify the persisted snapshot files without
rehashing every audited source. Mutating `record-batch` and `advance` operations
and final `report` generation rehash source files; mutation commands check
again immediately before writing. This avoids repeated repository traversal
while retaining a fresh source boundary for durable judgments and publication.

## Classification ingestion

The agent writes one strict batch with the active audit ID and snapshot digest. Native validation rejects unknown or repeated assertion IDs and requires category-specific fields. Citations must select an exact indexed path and line, and their excerpt must match that line. Documented and mixed categories require confirmed citations. Implicit requires ambiguous evidence. Incidental and mixed classifications require an explicit undocumented-mechanics explanation.

The `advance` operation records the batch and exports the next batch
using one validated state load. Separate `record-batch` and `next-batch`
commands remain available for recovery and intermediate inspection. Replaying
an identical classification is idempotent, including after the record succeeds
but next-batch output publication fails.

The primary agent-facing `submit` operation resolves compact semantic rows and
evidence handles into the same canonical classification batch, then delegates
to the advance sequence. The journal write is the durable commit point.
Evidence handles contain a snapshot prefix and
exact index position, so native code resolves citations without accepting
caller-supplied path/line pairs. Reuse requires an explicit source row and
equivalence confirmation. Native code replaces, rather than inherits, reuse
citations and caller IDs.

Search accepts repeated independent queries and evaluates them from one index
load. Search records retain complete bounded documentation paragraphs or list
items, source ranges, owners, and structural source roles. Marker-only ranges
do not enter the index. Matching and citation resolution use complete range
text. Agent-facing search output uses a bounded preview with an explicit
`previewTruncated` flag; the `evidence` command returns the complete immutable
range for one exact handle. The assertion `context` command returns the complete
bounded owner source, nested assertion identifiers, native and unresolved caller
spans, local binding candidates, and lexical evidence candidates. These
candidate fields are evidence-navigation hints, not semantic authority. Query
count, per-query result count, and batch sizes remain bounded.

Batch export writes a machine record and a bounded text display under retained
file transactions, outside the report publication area. Report results include no-assertion, incidental-only,
unlinked-assertion, and diagnostic counts so completion does not require loading
the complete trace. The report publishes `details.json` as the canonical
machine-readable assertion trace and test inventory, paired with aggregate
`summary.json`.

The Rust visitor reconciles declarations and direct calls resolvable within
one parsed file after visiting it, so declaration order does not matter. A
conservative cross-file pass then resolves explicit crate imports, aliases,
crate reexports, and module-qualified calls to unique helper definitions and
records each source span. Ambiguous paths remain unresolved candidates rather
than effective callers. The scanner does not treat matching names in source
text as evidence that a test calls a helper. The agent can confirm indirect
callers discovered through callbacks, traits, registration, or dynamic paths.
A shared assertion without effective callers requires a reason. The visitor
excludes test-owned `Drop` method
panic sites that can drive an expected unwind, not ordinary functions named
`drop`. Other callback assertions remain visible for agent attribution.
Recognized `assert_panics` calls become one panic expectation; the visitor
suppresses panic macros evaluated inside that expected-unwind argument.

`assert_eq_size` macros use the same compile-time path as trait assertions. A
prepass identifies empty functions whose generic type parameters have trait
bounds; direct calls from test-context code count at the enforcing invocation.
The parser emits test-context `expect` method calls as `fallibleValueCheck`
sites after excluding common lock, join, pointer, ownership, and fixture
accessors. Test-created `Some(...)` option holders are tracked through `Rc`/`Arc`
clones and `RefCell` borrow guards without suppressing separately constructed
API values inside the same callback. Contiguous cfg attributes on compile-time sites become
`conditionalAssertion` diagnostics, preserving branch provenance. Inclusion
gates inherit from enclosing modules, tests, and owner attributes. `cfg_attr`
is included only when it applies a nested `cfg`; instrumentation-only
attributes do not become inclusion diagnostics.

## Count semantics

The inventory counts source sites, not runtime loop iterations or repeated trait names in one macro. Rust `#[should_panic]` counts at the expectation attribute, not at the deliberately triggered panic. Ordinary `unwrap` calls remain setup rather than predicates. Selected test-context `expect` calls that extract API values count as heuristic `fallibleValueCheck` sites; common fixture and synchronization accessors remain excluded. A rustdoc fence is a test case; a compile-only fence remains in the inventory with no counted predicate unless it contains one of the recognized static or fallible checks.

Standalone `unwrap()` calls are setup unless another recognized predicate or
contract-bearing assertion consumes their result. The rustdoc scanner therefore
does not invent a predicate for a post-send `receiver.into_value().unwrap()`
that is only formatted or printed. A counted let-else panic, explicit assertion,
or test of the resulting value remains independently inventoried.

The report computes assertion-site rates over classified rows and preserves test inventory entries independently. This prevents smoke tests and no-predicate doctests from disappearing while keeping their lack of checks explicit.

Final reports add nonmutating consistency diagnostics for rationale reuse across
different predicates, identical predicates with different labels, broad or
conflicting citation reuse, and audits with no `Mixed` sites. Summary and detail
records carry tool and rubric versions, source and index digests, correction
counts, and these diagnostics. The native `compare` command uses source
provenance rather than audit-local IDs to report classification transitions,
effective caller-set drift, and test-gap membership between complete runs.

## Failure and coverage semantics

Schema version mismatches, stale files, malformed citations, duplicate judgments, and incomplete reports fail without changing the accepted journal. A versioned state does not cross-read or migrate older formats. Recovery from an old or incorrectly scoped snapshot preserves the old directory and initializes a new state at the exact same revision with the corrected scan inputs. Agents replay semantic work only after rerunning evidence selection for the new snapshot. Unsupported or unverifiable forms appear as scan diagnostics and workflow caveats rather than successful empty coverage.
