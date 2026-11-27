# Test audit follow-up work

These improvements remain outside the current implementation scope. Resolve each item by adding deterministic native behavior or measured workflow evidence; do not leave equivalent gaps in the active skill guidance.

## Rust caller attribution

Resolve unambiguous imported, aliased, and cross-module test-to-helper calls against the immutable snapshot. Retain separate native direct, agent-confirmed indirect, and unresolved-candidate provenance with source spans for every edge. Reject `unlinkedReason` when effective callers exist and never promote same-name text, comments, or dynamic dispatch guesses.

Tests must cover aliases, qualified cross-module calls, same-name decoys, multiple callers, and indirect callbacks that remain unresolved.

## Evidence ranges and source roles

Return bounded, snapshot-bound contiguous documentation ranges rather than only a matching line. Preserve owning item or section, full qualifiers and negation, blank context, and an explicit evidence role for public API prose, public examples, authoritative public design, inherited contracts, or private context. Do not classify a filename or `pub` marker as proof of authority.

Tests must cover multiline sentences, blank rustdoc lines, empty-marker-only citations, same-site examples, private same-name declarations, and inherited external contracts that are unavailable in the indexed snapshot.

## Assertion drill-down

Provide a compact per-site card with the tested operation and type, containing test or helper, and bounded access to complete enclosing code and relevant definitions. Surface value and flag origins, writes, callback or panic observation points, nested assertions, compile-time trait subjects, cfgs, features, and public export paths when the AST can resolve them.

Keep `proven`, `candidate`, and `unresolved` states distinct. Start with local AST-backed navigation; do not require whole-program resolution.

## Consistency diagnostics

Before publication, flag identical rationale reused for different predicate shapes, unusually broad citation reuse across unrelated operations, conflicting claims about one public symbol, and analogous sites with unexplained classification differences. Present these as review diagnostics, never automatic relabels.

Tests must compare caller, test-gap, and rate membership across synthetic cases without pinning exact prose.

## Evaluation

Maintain a small human-adjudicated paired-case set with accepted equivalent citations and rationale wording. Measure per-site transitions, effective caller links, denominator membership, and evidence sufficiency across repeated blinded runs. Keep debatable cases separate from deterministic expected outcomes.

Record the tool and rubric version with the audited content digest so comparisons distinguish source drift from judgment drift.

Acceptance cases should include:

- one helper called across modules through an import, alias, and qualified path alongside a same-name decoy, plus an unresolved indirect callback;
- a multiline public sentence with a qualifying or negating continuation, blank rustdoc context, and a private same-name declaration;
- a public operation that explicitly calls a closure once for each previously registered item, contrasted with an internal counter without a promised cardinality;
- separate assertions for arbitrary fixture panic text and public post-unwind cleanup guarantees;
- a public endpoint with promised `Send` behavior, followed by a private backing type carrying the same bound;
- one source predicate constraining an explicitly promised trait and a separate undocumented trait, producing `Mixed` while preserving one site;
- a public example payload compared with a general payload contract, requiring independent public prose before generalizing;
- a fixture holder `expect` and an independently produced optional API value, with only the latter entering the denominator.
