# ss-test-audit design

## Purpose

`ss-test-audit` explains the relationship between test predicates and a repository's public contract. It produces evidence for maintainers to reason about tests that overconstrain implementation details, under-specify behavior, or verify a documented promise.

The workflow distinguishes:

- a **test case**, which is one executable test, doctest, or compile-time check;
- an **assertion site**, which is one source location containing a counted predicate;
- a **public promise**, which is authoritative public wording for tested behavior; and
- a **classification**, which records how one assertion relates to available promises.

## User-visible behavior

The skill audits one repository and any selected set of existing file or directory scopes. It preserves a source snapshot, source revision, file hashes, line locations, source expressions, and search index on disk. Every assertion batch contains at most 25 sites. Each accepted judgment remains resumable and bound to the exact snapshot.

The final trace contains exactly one classification row for each inventoried assertion. The test inventory contains every inventoried test and links direct and shared assertion IDs. Tests that execute smoke behavior without a counted predicate remain visible.

The Markdown report shows per-module totals and percentages for:

- `No explicitly documented promise = implicit-only + incidental`;
- `No public-contract check = incidental only`.

The rates are descriptive audit measurements, not quality scores. Mixed sites enter neither numerator because they include a documented component.

## Classification boundaries

`Documented` requires exact public wording supporting the checked outcome. `Implicit` records a plausible public guarantee inferred from public shape, names, or conventions without wording for that exact behavior. `Mixed` combines a documented component and a separate undocumented constraint. `Incidental` identifies private mechanics, setup, or permitted behavior for which no public promise applies.

Public observability alone does not create a public promise. A private implementation comment does not create a public promise. Ambiguous evidence remains ambiguous rather than receiving a fabricated citation.

## Language coverage

Rust uses the repository's Rust syntax parser for test attributes, functions, assertion macros, panic expectations, static assertions, and unexpected-branch failures, with a separate rustdoc block scan. Rejected Rust syntax receives an explicit line-pattern fallback diagnostic.

C#, Java, JavaScript, TypeScript, Python, Go, Kotlin, and PowerShell use narrow language-specific text patterns and diagnostics. Other indexed source languages remain searchable but do not claim test or assertion coverage. Native diagnostics and an independent repository test-list command define the reachable coverage boundary.

Default traversal skips generated and tool-state directories and does not follow symbolic links. A caller can select a normally excluded directory as an explicit scope. The tool rejects a symbolic link as the scope and reports nested link omissions so an audit cannot silently treat a skipped branch as covered.
