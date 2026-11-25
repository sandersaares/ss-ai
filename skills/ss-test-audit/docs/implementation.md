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
25-site batch -> agent evidence judgment -> strict record
        |
        v
validated classification journal
        |
        v
trace + test inventory + summary
```

The inventory, source file digests, and search index bind each batch to one audit ID and snapshot digest. Every later read verifies both stored digests and current source hashes. A changed source returns a stale-state failure instead of accepting a judgment against moved lines.

## Classification ingestion

The agent writes one strict batch with the active audit ID and snapshot digest. Native validation rejects unknown or repeated assertion IDs and requires category-specific fields. Citations must select an exact indexed path and line, and their excerpt must match that line. Documented and mixed categories require confirmed citations. Implicit requires ambiguous evidence. Incidental and mixed classifications require an explicit undocumented-mechanics explanation.

Static call matching links parser-visible helper names across indexed test bodies. The agent can add calling tests discovered through callbacks, traits, registration, or dynamic paths. A shared assertion without callers requires a reason; the tool does not silently assign it to every test.

## Count semantics

The inventory counts source sites, not runtime loop iterations or repeated trait names in one macro. Rust `#[should_panic]` counts at the expectation attribute, not at the deliberately triggered panic. Ordinary `unwrap` and `expect` calls are not inventory predicates. A rustdoc fence is a test case; a compile-only fence remains in the inventory with no counted predicate.

The report computes assertion-site rates over classified rows and preserves test inventory entries independently. This prevents smoke tests and no-predicate doctests from disappearing while keeping their lack of checks explicit.

## Failure and coverage semantics

Schema version mismatches, stale files, malformed citations, duplicate judgments, and incomplete reports fail without changing the accepted journal. A versioned state does not cross-read or migrate older formats. Unsupported or unverifiable forms appear as scan diagnostics and workflow caveats rather than successful empty coverage.
