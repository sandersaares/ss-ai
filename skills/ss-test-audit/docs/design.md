# ss-test-audit design

## Purpose

`ss-test-audit` explains the relationship between test predicates and a repository's public contract. It produces evidence for maintainers to reason about tests that overconstrain implementation details, under-specify behavior, or verify a documented promise.

The workflow distinguishes:

- a **test case**, which is one executable test, doctest, or compile-time check;
- an **assertion site**, which is one source location containing a counted predicate;
- a **public promise**, which is authoritative public wording for tested behavior; and
- a **classification**, which records how one assertion relates to available promises.

## User-visible behavior

The skill audits one repository and any selected set of existing file or directory scopes. Scope paths limit test and assertion inventory. The search index covers repository-wide text under the normal traversal exclusions, so narrow test scopes retain access to out-of-scope public contracts. The index also includes the files discovered in explicit scopes so their assertions retain source provenance. The tool preserves a source snapshot, source revision, file hashes, line locations, source expressions, and search index on disk. Every assertion batch contains at most 25 sites and can contain fewer to fit the byte budget. Each accepted judgment remains resumable and bound to the exact snapshot.

The final trace contains exactly one classification row for each inventoried assertion. The test inventory contains every inventoried test and links direct and shared assertion IDs. Tests that execute smoke behavior without a counted predicate remain visible.

Final output has two matched presentation levels. The summary level is
`summary.json` plus `summary.md`, with stable identity, coverage, classification,
rate, gap, and diagnostic fields. The detail level is `details.json` plus
`details.md`, with matching assertion-trace, test-inventory, and diagnostic
sections. The report also publishes standalone `trace.json` and
`test-inventory.json` projections for consumers who need only one part of the
detail report.

The skill requests one initial assertion batch, then uses one combined command to record each completed batch and export the next one. Related evidence queries can share one command. Read-only operations remain bound to the immutable snapshot; commands that accept judgments or emit final reports also verify current source hashes.

The compact submission record contains only agent-owned semantic rows and
snapshot evidence handles. Native code binds those rows to the current batch,
resolves exact citations, and records the judgments before exporting the next batch. Reuse is
an explicit row shape requiring confirmed equivalence, replacement citations,
and caller attribution. Batch display and final report counts make ordinary
workflow completion independent of PowerShell JSON projection.

Using the [classification boundaries](#classification-boundaries), the
Markdown report shows per-module totals and percentages for:

- `No explicitly documented promise = implicit-only + incidental`;
- `No public-contract check = incidental only`.

The rates are descriptive audit measurements, not quality scores. Mixed sites enter neither numerator because they include a documented component.

## Classification boundaries

`Documented` requires exact public wording supporting the checked outcome. `Implicit` records a plausible public guarantee inferred from public shape, names, or conventions without wording for that exact behavior. `Mixed` combines a documented component and a separate undocumented constraint. `Incidental` identifies private mechanics, setup, or permitted behavior for which no public promise applies.

Public observability alone does not create a public promise. A private implementation comment does not create a public promise. Ambiguous evidence remains ambiguous rather than receiving a fabricated citation.

## Language coverage

Rust uses the repository's Rust syntax parser for test attributes, functions, assertion macros, panic expectations, static assertions, and unexpected-branch failures, with a separate rustdoc block scan. Direct calls to recognized panic-expectation helpers count as one site while deliberately triggered panics inside the helper argument do not. Production invariants and defensive failure branches remain out of the test inventory unless native syntax resolves a direct free-function or qualified-path test call within the same file. Rejected Rust file syntax receives an explicit line-pattern fallback diagnostic. Rustdoc fences use parsed macro invocations; unsupported snippets receive a diagnostic rather than text-based assertion counts. Compile-only fences retain the test entry without counting runtime predicates.

Rust static-assertion macros include size-equality checks. A call into an empty
generic helper with trait bounds counts at that call as a compile-time check.
Test-context `expect` calls on fallible API values are exported as
`fallibleValueCheck` candidates for semantic judgment, while common fixture and
synchronization access patterns remain excluded. Configuration attributes on
compile-time assertions remain visible as scan diagnostics.

C#, Java, JavaScript, TypeScript, Python, Go, Kotlin, and PowerShell use narrow language-specific text patterns and diagnostics. Other indexed source languages remain searchable but do not claim test or assertion coverage. Native diagnostics and an independent repository test-list command define the reachable coverage boundary.

Default traversal skips generated and tool-state directories and does not follow symbolic links. A caller can select a normally excluded directory as an explicit scope, except for Git administrative data. The tool rejects a symbolic link as the scope and reports nested link omissions so an audit cannot silently treat a skipped branch as covered.

Caller discovery records AST-visible direct calls rather than same-name text matches. It does not assign method calls, trait callbacks, comments, or declarations to a shared helper. A test-owned callback assertion may therefore have no native caller; the reviewing agent confirms or explains that relationship in the classification record.
