# Mission complete?

Does the pull request actually achieve what it sets out to do? Fully?

If there are gaps left, are those gaps acknowledged in the description and/or documentation?

If there are gaps left in the code, do they contain references to follow-up work items?

# No cargo cults

Look for cargo-culted patterns, logic or configuration that does not seem to be relevant to a specific scenario and may just be a copy-paste artifact because some other logic elsewhere needed it for a different reason.

# Apply guidance within its scope

Before deriving a finding from a policy or guideline, verify that it applies to the reviewed role, artifact, ecosystem and workflow. Do not turn preferences scoped to one activity into requirements for another.

# Maintainability

It is important that future maintainers understand why specific decisions were made by past maintainers.

## Logic requires justification if not obvious

It must be clear to future maintainers why the logic in each function is the way it is. This is done by documenting any non-obvious decisions in the logic flow with inline comments.

Consider each branching point, function call, mathematical operation and comparison in the function logic. Is it obvious from context why each item exists? If not, justification must be added. There are two ways to justify code:

1. By adding contextual information (e.g. top-level inner or outer comment in a function, explaining what it is meant to achieve).
2. By adding inline comments with specific justification (e.g. explaining why a loop exit condition exists - why is it important for the algorithm to break the loop here).

When possible, prefer documenting the big picture justification on function/type/file level - higher level comments tend to add more value and are less repetitive.

If the code is an implementation of some well-known algorithm, the standard description of the algorithm should be provided (e.g. a Wikipedia link). This alone is not sufficient - each item in the logic flow must be justified. An algorithm reference simply allows for easier cross-referencing of the inline justification to the algorithm template.

# Examples teach recommended practices

Examples are teaching surfaces that consumers may copy into production code. Review them as recommended usage, not as disposable demonstration code.

Examples should demonstrate reuse of existing project and ecosystem abstractions, especially for correctness-sensitive logic. If multiple examples duplicate substantial plumbing, prefer a shared support helper when it preserves standalone clarity and keeps essential steps visible. Keep scenario-specific setup local; do not extract the behavior the example exists to teach.

# Challenge the framing

When reviewing a possible solution to a problem, consider alternative ways to solve the problem. Raise a finding when concrete evidence shows an alternative offers a material benefit, such as avoiding a current defect or realistic failure or substantially reducing cost or complexity. If several policies or remedies are valid, identify the tradeoff rather than prescribing one without evidence.

Furthermore, consider the nature of the problem. Are we operating under a fixed mindset, zoomed in too far? Could we take a step back and solve a bigger meta-problem that eliminates our concrete problem? Raise an alternative interpretation when evidence shows it materially improves the solution.

Can we simplify and generalize to eliminate entire classes of problems instead of addressing issues one by one? Consider possible ways to solve our problems by removing rather than adding logic. The best code is code that does not exist. Raise simplification findings when the benefit is concrete and material.

# Technical debt

If a change adds technical debt to a system, this debt must be:

1. Justified - why is it OK to leave something in a suboptimal state? It had better not just be laziness or rushing to deadlines!
2. Documented - the suboptimal code/module/design needs to have a reference to a work item that tracks the follow-up work to eliminate the technical debt.

# Surprise!

When reviewing a pull request, evaluate the changes against the pull request title and description. If some of the changes are surprising in this context, call them out as findings.

We do not require a pull request to only be on a "single topic"; the main goal of this check is to avoid accidents (e.g. author changing something temporarily and then forgetting it).

Conversely, is it surprising that something is missing? Do the pull request title and description suggest a wider scope of achievement than is actually achieved by the change? Raise such findings, as well.

# Terminology

Ensure that clear and consistent terminology is used. Avoid introducing new terminology if existing terms can be used.

# The author decides the version number

The author of a published package is free to choose whatever version they wish. When operating in a strict semantic versioning environment (e.g. Cargo/Rust packages), they need to follow semantic versioning rules (1.x.x -> 2.0.0 on breaking changes in stable versions, 0.1.x -> 0.2.0 on breaking changes in unstable versions, etc).
The author is not required to follow any other rules or conventions for versioning, and may choose to increment the version number in any way they see fit, including skipping versions.

Only raise a finding about a version increment when the reviewed change publishes the package or explicitly asserts the release version, that version looks insufficient to represent the change in semantic versioning terms, and the package type requires strict semantic versioning (e.g. Cargo packages).

# A proxy is not the same as the real thing

A typical mistake in designs and tests is to make assumptions based on the presence of a proxy condition instead of verifying the real thing.
Check for hidden assumptions where we a half-truth could pass for a full truth by accident or poor design.

Examples:

- Checking for existence of a file without verifying the contents, if the file exists in potentially multiple stages
  (e.g. "before approval" and "after approval" - do not assume content state from mere presence of a file).
- Checking for a fact that may have alternate unchecked explanations than the one assumed - verify all possibilities to reach a state
  (e.g. verifying that our canary is not in a "rejected.txt" could also be explained by a hidden failure to create a "rejected.txt")

# Half-kept promises are defects to avoid

If a design or piece of code makes a promise, trace the logic to verify that the promise is upheld at all times, not only sometimes or
by coincidence. Complex code should have debug-build assertions to detect deviations from its promises.

# Public API contract changes must be justified by the pull request description

Applies to: code files containing public API definitions and any standalone public API documentation files.

When reviewing a pull request that changes a public API contract, ensure that the pull request description justifies the changes. The
justification does not need to be specific or exhaustive - it is sufficient to provide a general rationale/mission that explains every
change to the public API contract. If the justification is missing or insufficient, request clarification from the author of the pull
request and point out change that may be unintentional (e.g. accidentally expanding the API contract when not required to by the mission).

This covers not only technical API contracts (type/function/argument names and types) but also semantic contracts expressed via
metadata or documentation (expected behavior, invariants, and usage constraints).

Be careful about reviewing changes to "public" types that are not actually part of a published public API. Some entire libraries may be
technically marked as having a public API but only consumed internally as a dependency. Likewise, some programming languages often mark
items "public" but then limit that visibility on a higher (module) level, making them effectively internal rather than part of the
published public API. We only care about real user-facing public API changes that get delivered to end users in libraries that are
intended for public consumption.

# Be paranoid about synchronization logic

For any hand-rolled synchronization logic (e.g. any use of atomics), be paranoid. Look for obvious bugs and verify that a thorough
test suite exists to catch less obvious bugs and confirm correctness. Any hand-rolled synchronization logic requires a thorough
multithreaded test suite.

# Is this needed?

Look at the pieces in front of you and consider: "is this needed?".

Often we may find ourselves accumulating code, layers, fields, documentation, edge cases and other cruft over iterative development
without realizing that we could arrive at a simpler design by removing large parts of it. Consider whether the pieces in front of you
are actually relevant to achieving the goals of the content you are reviewing. If something serves no purpose, it should be removed.

This applies to both high-level concepts (design tenets, APIs) as well as individual fields in structs, layers of types in those fields
(e.g. does something need to be wrapped in a mutex or smart pointer or can we simplify and do without). There are many opportunities,
try finding them.

# Look for unjustified duplication

Examine the content for unexpected duplication - does the body of a function seem to match some arbitrary inline logic elsewhere?
Are two parts of the app independently computing the same value? Do some other parts feel like the same thing expressed in two ways?

Such duplication creates risk of accidental divergence when only one part gets updated as part of a future change - avoid such mutation risk.

Sometimes duplication is unavoidable (e.g. an implementation often resembles the API contract it implements). Analyze each candidate duplication case by case to determine whether it is justified.