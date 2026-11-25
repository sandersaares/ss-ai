---
name: ss-test-audit
description: Audits test assertion sites against public contracts and emits a provenance-backed trace, complete test inventory, and module-level evidence rates for a repository or selected scope.
---

# ss-test-audit

Audit the tests in a repository or explicit scope and distinguish what each assertion proves from what the public contract promises. The workflow keeps deterministic inventory and evidence operations in `ss-ai`; the agent owns evidence selection and semantic classification.

# Scope

Resolve the repository path from the invocation. If the user names a crate, package, path, or module, translate it to one or more exact repository-relative scope paths. Without a scope, audit the complete current repository.

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

- parser-backed Rust records `#[test]`, recognized test attributes, rustdoc code blocks, assertions, panic expectations, compile-time static assertions, and direct failure branches;
- recognized panic expectation helpers such as `assert_panics` count once, while a panic deliberately triggered inside that expectation does not count again;
- C#, Java, JavaScript, TypeScript, Python, Go, Kotlin, and PowerShell use deliberately narrow text patterns;
- unsupported source language text remains searchable but receives no assertion or test inventory;
- syntax rejected by the Rust parser uses a Rust text fallback and carries a diagnostic.

Ordinary traversal skips generated and tool-state directories such as `.git`, `target`, `node_modules`, and build-output directories. Select such a directory explicitly if it is part of the requested source scope. The native tool rejects a symbolic link used as the scope and does not follow nested symbolic links; nested omissions receive a diagnostic.

Shared helpers, callbacks, and destructors count once at their definition. Production assertions and defensive branches do not become test assertions merely because tests call a same-named method. The Rust inventory links a shared assertion only from direct AST-visible free-function or qualified-path calls in a test; it does not infer callers from comments, function declarations, method calls, trait callbacks, or substring matches. Preserve every native caller link and add semantically confirmed `callingTestIds` in the classification record when static syntax cannot identify a call. Supply `unlinkedReason` rather than guessing when no calling test can be established.

Panic sites in test-owned `Drop` implementations do not count when they exist only to drive an expected unwind. Assertions in such callbacks remain countable and may require agent-owned caller confirmation.

# Stage 3: classify assertions in batches

Execute `next-batch` as specified in [commands.md](commands.md#export-the-next-assertion-batch). Keep the default count of 25. Read only the exported batch, its local context, and targeted evidence searches. For each assertion:

1. Record the actual outcome the source predicate checks.
2. Determine the public promise, if any, with exact `search` citations.
3. Explain private or incidental mechanics without treating observability as a promise.
4. Assign the exact classification and evidence status.
5. Validate that a shared site has complete caller evidence or an explicit unlinked reason.

Search API docs, design docs, public declarations, trait implementations, and inherited contracts using [commands.md](commands.md#search-evidence). Supply `--kind` to narrow a query only when the target is already clear. Terms are combined as an AND query.

Write one strict classification batch matching [classification-batch.schema.json](schemas/classification-batch.schema.json). Preserve the batch's `auditId` and `snapshotSha256`; never copy an earlier batch token. Each citation's `path`, `line`, and exact `excerpt` must come from a search result or a batch context entry.

Apply the batch through `record-batch` using [commands.md](commands.md#record-classification-results). If the command reports stale state, malformed citations, duplicate IDs, or an already different classification, stop that batch and report the diagnostic without overwriting state.

After every completed record, state cumulative progress in the conversation: `Classified C of T assertions; R remain. The batch completed N sites.` For the final partial batch, also state that the remainder has been completed.

Repeat until `next-batch` returns zero assertions. Never declare completion from an assertion count alone.

# Stage 4: validate and emit reports

Run `ss-ai test-audit report` using [commands.md](commands.md#emit-final-reports). The command requires exactly one accepted classification row for every assertion and one inventory row for every test.

Deliver these generated paths:

- `trace.json`: searchable assertion rows with source, actual outcome, classification, and citations;
- `test-inventory.json`: every test with direct and shared assertion IDs, including smoke cases with no counted predicate;
- `summary.md`: per-module totals and percentages, tests with incidental-only checks, tests with no counted assertion, and scan diagnostics.

Keep the rate labels exact:

- `No explicitly documented promise = implicit-only + incidental`;
- `No public-contract check = incidental only`.

Always show numerators and denominators. Mixed assertion sites enter neither numerator. Never call either rate a test-quality score.

# Stage 5: summarize the execution

Report completion only after the final native report command succeeds. The summary must state the audited repository and scopes, snapshot revision, test and assertion totals, classification totals, module rates, output paths, discovery cross-check, language coverage, and every material scan diagnostic or unresolved unsupported form.

Do not omit smoke tests, unlinked shared assertions, parse fallbacks, or unsupported forms to make coverage appear complete.
