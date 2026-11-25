---
name: ss-test-audit
description: Audits test assertion sites against public contracts and emits a provenance-backed trace, complete test inventory, and module-level evidence rates for a repository or selected scope.
---

# ss-test-audit

Audit the tests in a repository or explicit scope and distinguish what each assertion proves from what the public contract promises. The workflow keeps deterministic inventory and evidence operations in `ss-ai`; the agent owns evidence selection and semantic classification.

# Scope

Resolve the repository path from the invocation. If the user names a crate, package, path, or module, translate it to one or more exact repository-relative scope paths. Those paths control test and assertion inventory. The evidence index still covers the repository-wide text snapshot, subject to the same traversal exclusions, so public declarations and documentation outside a narrow assertion scope remain citable. `scopePaths` reports assertion coverage; indexed file and language totals report evidence coverage. Without a scope, audit assertions and evidence across the complete current repository.

Use one empty state directory for each immutable audit. Reuse that directory only to resume the same audit. State created by another utility schema version is incompatible and must be rejected rather than migrated or interpreted.

# Evidence boundary

A public promise must be wording in public API documentation or an authoritative public design contract that promises the checked behavior. Public declarations, type shape, naming, conventions, and private implementation comments can guide `Implicit` or locate evidence; none alone promises an undocumented exact behavior.

Treat tool output as inventory evidence, not semantic authority. Do not classify an assertion with a keyword rule. Read the assertion's source, containing test, called API, and only the targeted contract evidence needed to explain its outcome.

# Classification contract

Use exactly one classification for every assertion site:

- `Documented`: exact public wording supports the tested outcome.
- `Implicit`: the public API shape, name, or convention makes the outcome a plausible public guarantee, but no wording promises that exact behavior.
- `Mixed`: explicit evidence supports one component while the same site also constrains an undocumented detail. Supply the undocumented detail separately.
- `Incidental`: the assertion checks private mechanics, setup, or behavior the public contract permits but does not require. Explain why no public promise applies.

Use `Confirmed` only when the cited evidence establishes the claimed relationship. Use `Ambiguous` for `Implicit` or other genuinely uncertain evidence. Do not invent citations to remove ambiguity.

# Stage 1: initialize an immutable audit

Resolve or create an absolute state directory outside the audited scope. If resuming, retain its exact path and skip initialization after verifying the retained `auditId`, repository, and snapshot summary from the last session.

Run `ss-ai test-audit initialize` using the first recipe in [commands.md](commands.md#initialize). Pass `--scope-path` once for each resolved scope. If initialization reports an unsupported-language or parse-fallback diagnostic, retain that diagnostic as a coverage limit; do not describe the later inventory as exhaustive.

Record the returned `auditId`, `snapshotSha256`, counts, state paths, and diagnostics count in the session checkpoint.

# Stage 2: inspect inventory and cross-check discovery

Do not raw-dump `inventory.json` or `search-index.jsonl` into the conversation. Use `next-batch` for assertions and `search` for targeted evidence.

Compare the native inventory with an independent repository test-discovery method when the repository provides one, such as `cargo test -- --list`, a language test runner's list mode, or the repository's documented discovery command. Do not execute arbitrary test suites merely to invent a list.

Reconcile discrepancies before classification:

- parser-backed Rust records `#[test]`, recognized test attributes, rustdoc code blocks, assertions, panic expectations, `static_assertions` checks (including `assert_eq_size!`), and direct failure branches;
- compile-only checks count at a call site that instantiates an empty generic helper with trait bounds, rather than at the helper declaration;
- fallible value access in test code may appear as `fallibleValueCheck` sites. Judge these individually: a standalone `.expect(...)` on an extracted API value can verify presence, while lock, pointer, join, ownership, and other fixture extraction remains excluded. Do not blanket-count every `unwrap` or `expect`;
- recognized panic expectation helpers such as `assert_panics` count once, while a panic deliberately triggered inside that expectation does not count again;
- C#, Java, JavaScript, TypeScript, Python, Go, Kotlin, and PowerShell use deliberately narrow text patterns;
- unsupported source language text remains searchable but receives no assertion or test inventory;
- syntax rejected by the Rust parser uses a Rust text fallback and carries a diagnostic.

Ordinary traversal skips generated and tool-state directories such as `.git`, `target`, `node_modules`, and build-output directories. Select such a directory explicitly if it is part of the requested source scope. The native tool rejects a symbolic link used as the scope and does not follow nested symbolic links; nested omissions receive a diagnostic.

Shared helpers, callbacks, and destructors count once at their definition. Production assertions and defensive branches do not become test assertions merely because tests call a same-named method. The Rust inventory links a shared assertion only from direct AST-visible free-function or qualified-path calls in a test; it does not infer callers from comments, function declarations, method calls, trait callbacks, or substring matches. Preserve every native caller link and add semantically confirmed `callingTestIds` in the classification record when static syntax cannot identify a call. Supply `unlinkedReason` rather than guessing when no calling test can be established.

Panic sites in test-owned `Drop` implementations do not count when they exist only to drive an expected unwind. Assertions in such callbacks remain countable and may require agent-owned caller confirmation.

# Stage 3: classify assertions in batches

Execute `next-batch` as specified in [commands.md](commands.md#export-the-next-assertion-batch). Keep the default count of 25. Read the returned `displayPath` for compact batch judgment. The companion `batchPath` is a machine-readable record whose `assertions` array is flat: each item is an assertion directly, not `{ assertion: ... }`. Do not use PowerShell JSON projection scripts to reinterpret it.

Obey the initial batch's `nextAction` too: if it is `report`, continue directly to stage 4; if it is `continue`, classify the returned batch. An initial empty batch is still a transition to the report, never a final response.

Read only the exported batch display, its local context, and targeted evidence searches. For each assertion:

1. Record the actual outcome the source predicate checks.
2. Determine the public promise, if any, with exact `search` citations.
3. Explain private or incidental mechanics without treating observability as a promise.
4. Assign the exact classification and evidence status.
5. Validate that a shared site has complete caller evidence or an explicit unlinked reason.

Search API docs, design docs, public declarations, trait implementations, and inherited contracts using [commands.md](commands.md#search-evidence). Group independent evidence lookups into one `search` command by repeating `--query`; the command returns one result page per query. Supply `--kind` to narrow queries only when the target is already clear. Terms within one query are combined as an AND query.

Write one compact submission matching [compact-submission.schema.json](schemas/compact-submission.schema.json). Copy the current batch result's `batchFingerprint` exactly; do not recompute it or copy a fingerprint from an earlier generation. `submit` uses that fingerprint to bind the rows to the exact current `batchPath`. Each `citationHandles` entry must be an exact `evidenceHandle` returned by search. Multiple citations are independent array entries.

For a fresh row, supply assertion ID, classification, evidence status, actual outcome, rationale, citation handles, caller IDs, and applicable reasons directly.

To reuse an accepted row, supply `reuseFrom`, explicit `equivalenceConfirmed: true`, replacement `citationHandles`, and explicit `callingTestIds`. Do not reuse implicitly, infer equivalence, or omit caller attribution. An empty citation replacement is rejected rather than falling back to the prior assertion's evidence. If the reused assertion requires an unlinked reason, provide it explicitly.

Submit the rows and export the next assertion batch through one `submit` command using [commands.md](commands.md#submit-a-compact-row-batch). `submit` is the normal transition after every classified batch. Use the lower-level `advance`, `record-batch`, and `next-batch` commands only while recovering or inspecting an intermediate state.

If the command reports stale state, malformed citations, duplicate IDs, or an already different classification, stop that batch and report the diagnostic without overwriting state. Errors name the compact row assertion ID, field, and evidence reference. A failed next-batch publication after a successful record can be resumed by replaying the identical compact submission; record insertion remains idempotent.

After every successful `submit`, read its explicit `nextAction` before doing anything else. Use its `record.completed` and `record.remaining` fields to emit the following informational progress message in the conversation: `Classified C of T assertions; R remain. The batch completed N sites.`

This progress message is not a completion, checkpoint, pause, handoff, request for input, or permission to end the turn. Do not wait for a reply after it. In the same skill invocation, immediately continue with the next required action even when `R` is zero:

1. If `nextAction` is `continue`, read `nextBatch.displayPath` and classify its assertions without another CLI round trip. Retain `nextBatch.batchPath` unchanged for the subsequent compact submission.
2. If `nextAction` is `report`, continue to stage 4 and run the final `report`; the report command verifies exact classification coverage and fails if the continuation directive is stale or incorrect.

Never emit any final assistant response while the latest transition has `nextAction: "continue"` or `remaining > 0`. This is a hard final-response gate. Saving state, writing SQL/checkpoint records, reading the next batch display, beginning the next batch, or reaching a round batch number does not remove this gate. Continue the loop immediately. Do not reinterpret a long duration or large remaining count as a context-limit event.

The only permitted ways to stop this invocation are:

- a successful `report` result with `nextAction: "complete"` and `finalResponseAllowed: true`;
- a concrete blocking diagnostic from a native command that names the assertion, field, or evidence problem and cannot be corrected without user input; or
- an actual runtime interruption explicitly reported by the execution platform.

A normal user turn boundary, checkpoint write, progress message, or self-authored statement that work is lengthy is not a runtime interruption. If the platform does interrupt, preserve durable state without claiming completion; resumption must begin by reading the last transition directive and the corresponding batch. Preserve the exact machine batch and submission when replaying; do not write either next output onto one of those inputs.

Repeat the classification loop while `nextAction` is `continue` or `R` is greater than zero. Never declare completion from an assertion count, a progress message, mutation summary, checkpoint, or partial next-batch read alone.

# Stage 4: validate and emit reports

Run `ss-ai test-audit report` using [commands.md](commands.md#emit-final-reports). The command requires exactly one accepted classification row for every assertion and one inventory row for every test.

Deliver these generated paths:

- `trace.json`: searchable assertion rows with source, actual outcome, classification, and citations;
- `test-inventory.json`: every test with direct and shared assertion IDs, including smoke cases with no counted predicate;
- `summary.md`: per-module totals and percentages, tests with incidental-only checks, tests with no counted assertion, and scan diagnostics.
- `summary.json`: machine-readable versioned summary matching the fields in the native report result.
- `details.md`: human-readable assertion trace, test inventory, and diagnostics.
- `details.json`: machine-readable assertion trace, test inventory, and diagnostics.

Keep the rate labels exact:

- `No explicitly documented promise = implicit-only + incidental`;
- `No public-contract check = incidental only`.

Always show numerators and denominators. Mixed assertion sites enter neither numerator. Never call either rate a test-quality score.

# Stage 5: summarize the execution

Report completion only from a `report` result with `nextAction: "complete"` and `finalResponseAllowed: true`. That report may follow an initial empty batch, a resumed all-classified state, a low-level `record-batch`, or the latest `submit`; a `submit` is not required when no submission remains. Use the returned path fields; do not infer report paths under the state root. Keep the existing compatibility artifacts (`trace.json`, `test-inventory.json`) and add the standardized pairs (`summary.json` / `summary.md`, `details.json` / `details.md`).

Emit the final conversational summary in this fixed order: audit identity, repository/scope/revision, coverage totals, classification totals, rates, inventory gaps, diagnostics, and artifact paths. The Markdown summary and JSON summary use the same field set; `details.md` and `details.json` use `assertionTrace`, `testInventory`, and `diagnostics`.

The report result also returns `noCountedAssertionCount`, `incidentalOnlyCount`, `unlinkedAssertionCount`, `fallibleValueCheckCount`, `conditionalAssertionCount`, and `diagnosticCount`. The first two fields count tests; the unlinked, fallible, and conditional fields count assertion sites or site diagnostics. Use those bounded counts for ordinary completion without opening the complete trace unless detailed validation requires it. A nonzero fallible count means the agent had to judge heuristic expect-style candidates; it is not an unsupported success-shaped zero.

The summary must state the audited repository and scopes, snapshot revision, test and assertion totals, classification totals, module rates, output paths, discovery cross-check, language coverage, and every material scan diagnostic or unresolved unsupported form.

Execute native commands through their direct process boundary and inspect `$LASTEXITCODE`; a wrapper completing successfully is not evidence that the native command succeeded. In the rare case PowerShell is used for recovery, normalize projected arrays with `@(...)` and use literal string checks such as `.Contains()` rather than `-like` when Rust backticks are present.

Do not omit smoke tests, unlinked shared assertions, parse fallbacks, or unsupported forms to make coverage appear complete.
