---
name: ss-test-audit
description: Audits test assertion sites against public contracts and emits a provenance-backed trace, complete test inventory, and module-level evidence rates for a repository or selected scope.
---

# ss-test-audit

Audit the tests in a repository or explicit scope and distinguish what each assertion proves from what the public contract promises. The workflow keeps deterministic inventory and evidence operations in `ss-ai`; the agent owns evidence selection and semantic classification.

# Scope

Resolve the repository path from the invocation. If the user names a crate, package, path, or module, translate it to one or more exact repository-relative scope paths. Those paths control test and assertion inventory. The evidence index covers the repository-wide text snapshot under the normal traversal exclusions, plus explicitly selected source outside that walk, so public declarations and documentation outside a narrow assertion scope remain citable. `scopePaths` reports assertion coverage; indexed file and language totals report evidence coverage. Without a scope, audit assertions and evidence across the complete current repository.

Use a new, empty state directory outside the repository, even for an audit limited by `--scope-path`. Reuse that directory only to resume the same audit. State created by another utility schema version is incompatible and must be rejected rather than migrated or interpreted.

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

Use the absolute state directory path. If resuming, retain its exact path and skip initialization after verifying the retained `auditId`, repository, and snapshot summary from the last session.

Run `ss-ai test-audit initialize` using the first recipe in [commands.md](commands.md#initialize). Pass `--scope-path` once for each resolved scope. If initialization reports an unsupported-language or parse-fallback diagnostic, retain that diagnostic as a coverage limit; do not describe the later inventory as exhaustive.

Record the returned `auditId`, `snapshotSha256`, counts, state paths, and diagnostics count in the session checkpoint.

# Stage 2: inspect inventory and cross-check discovery

Do not raw-dump `inventory.json` or `search-index.jsonl` into the conversation. Use `next-batch` for assertions and `search` for targeted evidence.

Compare the native inventory with an independent repository test-discovery method when the repository provides one, such as a language test runner's list mode. Verify the runner is available and run it in the audited checkout in the same invocation, or explicitly select its manifest (for example, `cargo test --manifest-path "{{AUDITED_MANIFEST}}" -- --list`, where `AUDITED_MANIFEST` is the absolute manifest path for that checkout in the runner's environment). For a platform-specific scope, match the audited platform; for a Linux scope from Windows, use WSL when available. If the matching environment or runner is unavailable, record a discovery limitation rather than relying on a host-only result or installing tools. Do not execute arbitrary test suites merely to invent a list.

Reconcile discrepancies before classification:

- parser-backed Rust records `#[test]`, recognized test attributes, rustdoc code blocks, assertions, panic expectations, `static_assertions` checks (including `assert_eq_size!`), and direct failure branches;
- compile-only checks count at a call site that instantiates an empty generic helper with trait bounds, rather than at the helper declaration;
- fallible value access in test code may appear as `fallibleValueCheck` sites. Judge these individually: a standalone `.expect(...)` on an extracted API value can verify presence, while lock, pointer, join, ownership, and other fixture extraction remains excluded. Do not blanket-count every `unwrap` or `expect`;
- recognized panic expectation helpers such as `assert_panics` count once, while a panic deliberately triggered inside that expectation does not count again;
- C#, Java, JavaScript, TypeScript, Python, Go, Kotlin, and PowerShell use deliberately narrow text patterns;
- unsupported source language text remains searchable but receives no assertion or test inventory;
- syntax rejected by the Rust parser uses a Rust text fallback and carries a diagnostic.

Ordinary traversal skips generated and tool-state directories such as `target`, `node_modules`, and build-output directories. Select these directories explicitly when they belong to the requested source scope. Git administrative data (`.git`) is not source and cannot be selected. The native tool rejects a symbolic link used as the scope and does not follow nested symbolic links; nested omissions receive a diagnostic.

Shared helpers, callbacks, and destructors count once at their definition. Production assertions and defensive branches do not become test assertions merely because tests call a same-named method. The Rust inventory links a shared assertion only from direct AST-visible free-function or qualified-path calls in a test; it does not infer callers from comments, function declarations, method calls, trait callbacks, or substring matches. Preserve every native caller link and add semantically confirmed `callingTestIds` in the classification record when static syntax cannot identify a call. Supply `unlinkedReason` rather than guessing when no calling test can be established.

Panic sites in test-owned `Drop` implementations do not count when they exist only to drive an expected unwind. Assertions in such callbacks remain countable and may require agent-owned caller confirmation.

# Stage 3: classify assertions in batches

Execute `next-batch` as specified in [commands.md](commands.md#export-the-next-assertion-batch). Keep the default count of 25. Read `displayPath` for judgment, and retain the companion machine record at `batchPath` unchanged for `submit --batch-path`. Do not write PowerShell projection scripts to reinterpret it.

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

## Follow the current directive

Use the latest successful transition's `nextAction`, including on the initial batch or after resumption. After `submit`, use the `record` counts to report progress (`Classified C of T assertions; R remain. The batch completed N sites.`). Counts, progress updates, and checkpoints are informational only; do not pause or wait for a reply.

- `continue`: immediately read the returned display and classify its assertions in this invocation. For `submit` or `advance`, these paths are `nextBatch.displayPath` and `nextBatch.batchPath`; do not request the batch again. The recovery-only `record-batch` does not export a batch, so use `next-batch` after its `continue`.
- `report`: run stage 4 immediately, regardless of which supported path reached reporting.
- Only a successful `report` with `nextAction: "complete"` and `finalResponseAllowed: true` authorizes a completed response.

## Recover from failure

Correct diagnosed row defects before retrying. Stop on stale state or a conflicting accepted classification; report the diagnostic without overwriting state. If export failed after the journal write succeeded, replay the identical compact submission with the original machine batch; record insertion is idempotent. Do not overwrite either input.

Stop without claiming completion only for a concrete native diagnostic that names an assertion, field, or evidence problem and cannot be corrected without user input, or for an interruption explicitly reported by the execution platform. A user turn boundary, progress message, or long duration is not a runtime interruption. Preserve durable state and begin resumption by reading the last directive and corresponding batch.

# Stage 4: validate and emit reports

Run `ss-ai test-audit report` using [commands.md](commands.md#emit-final-reports). The command requires exactly one accepted classification row for every assertion and one inventory row for every test.

Deliver these generated paths:

- `trace.json`: searchable assertion rows with source, actual outcome, classification, and citations;
- `test-inventory.json`: every test with direct and shared assertion IDs, including smoke cases with no counted predicate;
- `summary.md`: per-module totals and percentages, tests with incidental-only checks, tests with no counted assertion, and scan diagnostics.
- `summary.json`: machine-readable audit summary defined by [summary.schema.json](schemas/summary.schema.json).
- `details.md`: human-readable assertion trace, test inventory, and diagnostics.
- `details.json`: machine-readable assertion trace, test inventory, and diagnostics.

Keep the rate labels exact:

- `No explicitly documented promise = implicit-only + incidental`;
- `No public-contract check = incidental only`.

Always show numerators and denominators. Mixed assertion sites enter neither numerator. Never call either rate a test-quality score.

# Stage 5: summarize the execution

Use the successful report receipt for completion and artifact paths; do not infer report paths under the state root.

Emit the final conversational summary in this fixed order: audit identity, repository/scope/revision, coverage totals, classification totals, rates, inventory gaps, diagnostics, and artifact paths. The Markdown summary and JSON summary use the same field set; `details.md` and `details.json` use `assertionTrace`, `testInventory`, and `diagnostics`.

The report result also returns `noCountedAssertionCount` and `incidentalOnlyCount` for tests, `unlinkedAssertionCount` and `fallibleValueCheckCount` for assertion sites, and `conditionalAssertionCount` and `diagnosticCount` for diagnostics. Use those bounded counts without opening the complete trace unless detailed validation requires it. A nonzero fallible count means inventoried expect-style candidates required case-by-case judgment.

The summary must state the audited repository and scopes, snapshot revision, test and assertion totals, classification totals, module rates, output paths, discovery cross-check, language coverage, and every material scan diagnostic or unresolved unsupported form.

Execute native commands through their direct process boundary and inspect `$LASTEXITCODE`; a wrapper completing successfully is not evidence that the native command succeeded. In the rare case PowerShell is used for recovery, normalize projected arrays with `@(...)` and use literal string checks such as `.Contains()` rather than `-like` when Rust backticks are present.

Do not omit smoke tests, unlinked shared assertions, parse fallbacks, or unsupported forms to make coverage appear complete.
