# Test-audit evaluation

## Purpose

Evaluation distinguishes input drift, scanner drift, and judgment drift. It uses immutable audit records and does not designate one run as the answer.

## Native evidence

Completed audits retain the repository revision, scope, tool version, rubric
version, checkout-independent content digest, index digest, and
contract-profile identity. The `compare` command requires complete
classification on both sides and reports:

- matched and unmatched source-site identities;
- classification transition counts and exact-label agreement;
- changed rationale and evidence-status counts;
- authority-rule use and policy transitions;
- active and corrected denominator membership;
- decisive-clause, observation-strength, and passing-counterexample drift;
- candidate stimulus, setup, cleanup, and observation role drift;
- effective native and agent-confirmed caller-set differences;
- tests with no counted assertion; and
- incidental-only test membership.

Resolve contract-profile mismatches and denominator drift before interpreting
classification transitions. Compare rates only for the common active set.
Investigate unmatched sites before interpreting a transition matrix. A higher
net agreement is not sufficient evidence that a particular judgment is correct.

## Deterministic acceptance cases

The native regression suite covers:

- alias, qualified cross-module, same-name decoy, and unresolved callback caller resolution;
- complete contiguous documentation ranges, qualifiers and negation on continuation lines, public examples, and marker-only rejection;
- `Some(...)`-backed fixture holders through `Rc`/`Arc` clones and `RefCell` borrow guards, with independent API values inside callbacks retained;
- exact initialization corrections and refusal to reinterpret incompatible state;
- explicit profile authority for public design versus private implementation,
  weak-observation counterexamples, `Mixed` component boundaries, and candidate
  eligibility;
- explicit incomplete and continuation receipts, including post-retry progress;
- consistency warnings for reused rationale, conflicting identical predicates, and broad or conflicting citation reuse; and
- checkout-independent content identity and prioritized policy, denominator,
  caller, and test-gap comparison.

## Human-adjudicated cases

Use a small paired set when changing the scanner, schemas, or rubric:

1. A helper invoked across modules through an import, alias, and qualified path alongside a same-name decoy, plus an indirect callback that remains unresolved.
2. A multiline public sentence whose qualifier or negation is on the continuation line, a blank rustdoc line, and a private same-name item.
3. A public operation that explicitly calls a closure once for every previously registered item, contrasted with an internal counter with no promised cardinality.
4. Separate fixture panic text and public post-unwind cleanup guarantees.
5. A public endpoint with promised `Send` behavior, followed by a private backing type with the same bound.
6. One source predicate combining an explicitly promised trait with an undocumented trait, contrasted with two source sites for the same traits.
7. A public example payload compared with independent prose that makes a general claim.
8. A fixture-holder `expect` and an independently produced optional API value.

Permit equivalent citation ranges and rationale wording. Keep debatable cases separate from expected deterministic outcomes. Review every changed classification against its applicability condition before using agreement as evaluation evidence.

## Rollout

Validate deterministic caller, range, correction, and arithmetic behavior first. Re-run a bounded human-adjudicated set before changing the judgment rubric. Add heavier AST or semantic analysis only when measured transition drift identifies a concrete gap that the bounded mechanisms cannot resolve.
