---
name: ss-test-audit
description: Audits test assertion sites against public contracts and emits a provenance-backed trace, complete test inventory, and module-level evidence rates for a repository or selected scope.
---

# ss-test-audit

Audit the tests in a repository or explicit scope and distinguish what each assertion proves from what the public contract promises. The workflow keeps deterministic inventory and evidence operations in `ss-ai`; the agent owns evidence selection and semantic classification.

# Continuation gate

A successful transition is not a completed response unless it is the final
`report` receipt with `nextAction: "complete"` and
`finalResponseAllowed: true`. When any command returns `nextAction: "continue"`,
the next action in this invocation must be a tool call that reads the returned
`nextBatch.displayPath` and continues classification. Retry the same compact
submission only for a diagnosed submission failure; after a retry succeeds,
resume the loop immediately. Audit size, progress counts, and a user turn
boundary do not authorize stopping.

# Scope

Resolve the repository path from the invocation. If the user names a crate, package, path, or module, translate it to one or more exact repository-relative scope paths. Those paths control test and assertion inventory. The evidence index covers the repository-wide text snapshot under the normal traversal exclusions, plus explicitly selected source outside that walk, so public declarations and documentation outside a narrow assertion scope remain citable. `scopePaths` reports assertion coverage; indexed file and language totals report evidence coverage. Without a scope, audit assertions and evidence across the complete current repository.

Use a new, empty state directory outside the repository, even for an audit limited by `--scope-path`. Reuse that directory only to resume the same audit. State created by another utility schema version is incompatible and must be rejected rather than migrated or interpreted.

# Evidence boundary

A public promise must be wording in public API documentation or an authoritative public design contract that promises the checked behavior. Public declarations, type shape, naming, conventions, and private implementation comments can guide `Implicit` or locate evidence; none alone promises an undocumented exact behavior.

Before initialization, derive one contract profile for the audited
product/interface. Name its intended audience, supported entry points, explicitly
internal surfaces, admissible behavioral sources, non-authoritative sources,
and unresolved authority questions. A profile decision applies to every batch in
that audit; do not reverse-engineer a policy separately for each assertion.
Every citation receives the exact matching profile rule through native handle
resolution. `Documented` and `Mixed` require at least one admissible rule, while
a non-authoritative citation may remain as contextual evidence. A citation from
an unresolved source remains a review limitation, not a silent promise.

Treat tool output as inventory evidence, not semantic authority. Do not classify an assertion with a keyword rule. Read the assertion's source, containing test, called API, and only the targeted contract evidence needed to explain its outcome.

Search handles bind a complete bounded documentation paragraph, list item, or
source line and identify its end line, owner, and structural source role. Use
those fields to preserve qualifiers, negation, cfg conditions, and context.
The source role is provenance, not authority; an excerpt in a public example or
authoritative design can still require independent wording before generalizing
it. A same-site doctest illustrates that scenario and is not independent proof
of a broader promise. Private implementation, fixture, and test comments explain
mechanics but do not become public promises.

Search previews are bounded. When `previewTruncated` is true, run the
`evidence` recipe for the exact handle and inspect the complete source range
before using it. Never treat a truncated preview as complete contract evidence.

# Classification contract

Use exactly one classification for every assertion site:

- `Documented`: exact public wording supports the tested outcome.
- `Implicit`: the public API shape, name, or convention makes the outcome a plausible public guarantee, but no wording promises that exact behavior.
- `Mixed`: explicit evidence supports one component while the same site also constrains an undocumented detail. Supply the undocumented detail separately.
- `Incidental`: the assertion checks private mechanics, setup, or behavior the public contract permits but does not require. Explain why no public promise applies.

Use `Confirmed` only when the cited evidence establishes the claimed relationship. Use `Ambiguous` for `Implicit` or other genuinely uncertain evidence. Do not invent citations to remove ambiguity.

# Stage 1: initialize an immutable audit

Use the absolute state directory path. If resuming, retain its exact path and skip initialization after verifying the retained `auditId`, repository, and snapshot summary from the last session.

Create the strict contract profile before initialization and run `ss-ai test-audit
initialize` using the first recipe in [commands.md](commands.md#initialize).
Pass `--contract-profile-path` and pass `--scope-path` once for each resolved
scope. If initialization reports an unsupported-language or parse-fallback
diagnostic, retain that diagnostic as a coverage limit; do not describe the
later inventory as exhaustive.

Record the returned `auditId`, `snapshotSha256`, `contentSha256`,
`contractProfileSha256`, counts, correction count, state paths, and diagnostics
count in the session checkpoint.

Persisted audit state and inventory use schema version 7 with judgment rubric
version 2. Use the same utility
version throughout one audit. A reader rejects state from another schema version
without changing it. Do not edit, migrate, or reinterpret an old inventory.

Contract profile `sourceRoles` use the native evidence-index vocabulary:
`markdownDocumentation`, `markdownExample`, `rustExample`, `rustItemDoc`,
`rustModuleDoc`, `rustPublicExample`, `rustTraitDoc`, and `sourceLine`. An empty
array matches every role under that rule's path. Initialization rejects an
unknown role and any authority rule that matches no indexed source; use the
returned `sourceRoles` array as the authoritative vocabulary.

Before initialization, reconcile the profile's `entryPoints` and
`internalSurfaces` against resolved public re-exports, their feature conditions,
and the purpose of each cited document. A feature-gated re-export can be public
for that supported feature even when the backing module is internal. A
design/implementation companion can explain mechanics without becoming public
authority. Record unresolved contradictions in the profile instead of silently
resolving them per assertion.

Use the optional initialization correction record from
[inventory-corrections.schema.json](schemas/inventory-corrections.schema.json)
only when source syntax cannot establish whether an exported candidate extracts a
test fixture. Bind every entry to the exact audited repository revision, source
path, line range, and scanner expression, and explain why the site is not an
audited predicate. The scanner applies these reviewed exclusions before it
publishes the immutable inventory and records them as provenance diagnostics.
Each correction states `candidateRole` (`fixtureSetup`, `faultInjection`, or
`cleanup`) and `candidateDisposition: "excluded"`. Pass the file with
`--correction-path`; there is no post-initialization edit.
Never classify a known fixture extraction as `Incidental` merely to finish a
batch, because that keeps it in assertion denominators.

If an existing snapshot contains a detected miscount or has an incompatible
schema, preserve that state directory as evidence. Reinitialize a new state
directory at the same immutable repository revision with the corrected utility
and any necessary initialization corrections. Reconcile prior accepted semantic
judgments against the new snapshot by rerunning evidence searches and building
fresh submissions; do not copy old audit IDs, snapshot digests, evidence handles,
or source citations blindly. Export and classify from the new state. Discard
old batch files as transition inputs; do not submit them to the corrected audit.

An initialization correction is exclusion-only. It can remove an existing
ambiguous assertion candidate but cannot insert a missing test record. Do not
use a correction to acknowledge a runner-only test or to continue with an
incomplete inventory.

Do not stop a successful `continue` loop merely because one inventory
discrepancy is ambiguous. Record unsupported or ambiguous forms as a
diagnostic/review limitation and continue classifying. For a confirmed missing
test row, preserve the state, stop the audit before semantic classification or
reporting, and wait for a corrected scanner. Reinitialize a fresh immutable
state at the same source revision; do not copy the old state or batch. Record a
source-keyed revalidation list using `path:start-end:expression`, the old test
or assertion ID, and the disposition (`missing test`, `phantom test`,
`phantom assertion`, `fixture correction`, or `mock predicate`). Re-run evidence
selection and semantic judgment for every listed source key even if its
classification survived on another audit.
For a proven fixture
false positive, preserve the current state, create a revision-bound correction
record, reinitialize a new audit, and replay only source-identical semantic work
with fresh evidence handles. A changed denominator requires a corrected report,
not an `Incidental` label or invented classification.

# Stage 2: inspect inventory and cross-check discovery

Do not raw-dump `inventory.json` or `search-index.jsonl` into the conversation. Use `next-batch` for assertions, `context` for the complete bounded enclosing test or helper, and `search` for targeted evidence.

Compare the native inventory with an independent repository test-discovery method when the repository provides one, such as a language test runner's list mode. Verify the runner is available and run it in the audited checkout in the same invocation, or explicitly select its manifest (for example, `cargo test --manifest-path "{{AUDITED_MANIFEST}}" -- --list`, where `AUDITED_MANIFEST` is the absolute manifest path for that checkout in the runner's environment). For a platform-specific scope, match the audited platform; for a Linux scope from Windows, use WSL when available. If the matching environment or runner is unavailable, record a discovery limitation rather than relying on a host-only result or installing tools. Do not execute arbitrary test suites merely to invent a list.

For Rustdoc, also run ignored-list mode when the source contains ignored
fences, for example
`cargo test --locked -p {{PACKAGE}} --doc -- --ignored --list`. Match native and
runner records by repository-relative path and fence opening line. Ignored
Rustdoc entries use `ignoredDoctest`, remain in the complete test inventory,
carry no assertion IDs, and do not represent executed examples. Intentional
exclusions need an explicit scanner diagnostic or a documented runner/platform
reason. A supported runner-only row with no matching native test is a confirmed
scanner omission, not an intentional exclusion and not a correction case.

Reconcile discrepancies before classification:

- parser-backed Rust records `#[test]`, recognized test attributes, rustdoc code blocks, assertions, panic expectations, test-owned `static_assertions` checks (including `assert_eq_size!`), and direct failure branches. Ungated production compile-time guards remain implementation constraints and do not create test records;
- compile-only checks count at a call site that instantiates an empty generic helper with trait bounds, rather than at the helper declaration;
- fallible value access in test code may appear as `fallibleValueCheck` sites. Judge these individually: a standalone `.expect(...)` on an extracted API value can verify presence, while lock, pointer, join, ownership, and other fixture extraction remains excluded. Do not blanket-count every `unwrap` or `expect`;
- a candidate export can distinguish an observation from fixture setup, fault injection, or cleanup. Resolve `fallibleValueCheck` and `unexpectedBranch` candidates before semantic classification. An included candidate must explicitly use `candidateRole: "observation"` and `candidateDisposition: "included"`; any other role requires a correction in a new initialization. Do not count a consumed injected fault as a second assertion;
- an ordinary `.unwrap()` remains setup. In rustdoc examples, a post-send `unwrap()` used only to print or display a result is not an assertion site unless the resulting value participates in another recognized predicate;
- recognized panic expectation helpers such as `assert_panics` count once, while a panic deliberately triggered inside that expectation does not count again;
- test-owned mock `.withf(...)` argument matchers contribute `mockArgumentCheck`
  sites, and explicit `.times(...)` constraints contribute separate
  `mockCallCount` sites. Unconditional return setup contributes no predicate and
  argument constraints do not imply call cardinality. Before classification,
  compare the `mockPredicateCrossCheck` source-marker and inventoried-predicate
  counts for each file; a mismatch blocks publication as an inventory omission,
  even when the runner's test list matches;
- C#, Java, JavaScript, TypeScript, Python, Go, Kotlin, and PowerShell use deliberately narrow text patterns;
- multiline synchronization access is identified from the receiver's proven
  `Mutex`, `RwLock`, or `JoinHandle` type, including fields typed in the current
  impl. Recognized lock, pointer, join, ownership, and fixture extractions leave
  the assertion denominator automatically and retain correction diagnostics;
- unsupported source language text remains searchable but receives no assertion or test inventory;
- syntax rejected by the Rust parser uses a Rust text fallback and carries a diagnostic.

`conditionalAssertion` diagnostics are coverage metadata. Continue semantic
classification; record the effective `cfg`/`cfg_attr(..., cfg(...))` context in
the judgment only when it affects the contract interpretation. Instrumentation
attributes such as `cfg_attr(coverage_nightly, coverage(off))` do not gate the
owner and do not produce this diagnostic. A diagnostic alone is not a command
failure and does not authorize stopping the batch loop or reporting an audit as
incomplete.

Ordinary traversal skips generated and tool-state directories such as `target`, `node_modules`, and build-output directories. Select these directories explicitly when they belong to the requested source scope. Git administrative data (`.git`) is not source and cannot be selected. The native tool rejects a symbolic link used as the scope and does not follow nested symbolic links; nested omissions receive a diagnostic.

Shared helpers, callbacks, and destructors count once at their definition. Production assertions and defensive branches do not become test assertions merely because tests call a same-named method. The Rust inventory links a shared assertion only from direct AST-visible free-function or qualified-path calls in a test; it does not infer callers from comments, function declarations, method calls, trait callbacks, or substring matches. Preserve every native caller link and add semantically confirmed `callingTestIds` in the classification record when static syntax cannot identify a call. Supply `unlinkedReason` rather than guessing when no calling test can be established.

Panic sites in test-owned `Drop` implementations do not count when they exist only to drive an expected unwind. Assertions in such callbacks remain countable and may require agent-owned caller confirmation.

# Stage 3: classify assertions in batches

Execute `next-batch` as specified in [commands.md](commands.md#export-the-next-assertion-batch). Keep the default count of 25. Read `displayPath` for judgment, and retain the companion machine record at `batchPath` unchanged for `submit --batch-path`. Do not write PowerShell projection scripts to reinterpret it.

Read only the exported batch display, its local context, and targeted evidence searches. For each assertion:

Before the numbered checks, read the contract profile exported in the batch and retain its declared audience, entry points, authority rules, and unresolved questions for this judgment.
1. Confirm that the source site belongs in the denominator. Distinguish extracting an API result from acquiring a fixture, lock, guard, pointer, join result, or other test-owned storage. Distinguish a consumed fault stimulus, setup, cleanup, and an unexpected-branch observation. Use the initialization correction path for a non-observation candidate; do not hide it by choosing `Incidental`.
2. State the weakest outcome the predicate actually demonstrates. Name the tested subject, preconditions, observation point, and polarity. Test names and panic messages are context, not evidence of a promise.
3. Split independently constrained facts in the reasoning, including exact counts, values, order, traits, and message matching. Keep one classification row for the source site. Do not count the same nested expectation as an additional outer predicate.
4. Identify the actual public operation and candidate authority. Prefer operation-specific public API wording and applicable traits, then explicitly authoritative public design contracts or independent examples. A public API used only for setup and private fixture comments can explain context but cannot create a promise by themselves.
5. Identify the decisive clause, not merely a nearby paragraph on the right API. State how it requires the observed predicate for this subject and precondition set. Distinguish a producer's state guarantee from an observer's query, permission (`may`) from obligation (`must`), and the first observation from later registration or cleanup states.
   Submit the clause verbatim in `decisiveClause`; the native validator requires it to occur in an admissible cited range.
6. Ask whether a contract-conforming implementation may make this predicate fail for the stated subject and conditions. Decide whether the outcome is required, only permitted, or unaddressed. A missing search result does not prove absence; refine the search and preserve ambiguity.
   For a private-layer observation, name the public operation or result it
   represents and give a concrete contract-conforming alternative under which
   that public result remains correct while this private predicate fails. Do
   not treat “the code feeds the API” or “the code is inside a platform layer”
   as a substitute for this equivalence argument.
   For a `substring`, `presence`, or `other` observation used by `Documented` or `Mixed`, record a concrete alternative result that still passes the predicate in `passingCounterexample`. This separates necessary conformance from evidence that would prove the claimed relationship.
7. Use `Documented` when every checked fact is explicitly required; `Mixed` when a documented fact is combined with a distinct undocumented constraint; `Implicit` only for a named plausible public guarantee with a stated missing wording; otherwise use `Incidental` and name the fixture, private choice, setup, or permitted alternative. Do not use uncertainty alone as the reason for `Implicit`. Set `observationStrength` for every profile-backed row.
8. Verify exact citations, caller attribution, and any explicit unlinked reason before submission. Reject an unlinked explanation when native or agent-confirmed effective callers already exist.

Use `unresolvedCallerReason` for a possible additional indirect caller that still
needs inspection. Do not overload `evidenceStatus`, which remains tied to the
classification's evidence, or mark an assertion unlinked when effective callers
already exist.

Use `reviewReason` when authority or applicability remains unresolved after the
bounded evidence process. It records why follow-up review may be useful without
inventing a citation or changing the four-label classification contract.

Search API docs, design docs, public declarations, trait implementations, and inherited contracts using [commands.md](commands.md#search-evidence). Group independent evidence lookups into one `search` command by repeating `--query`; the command returns one result page per query. Supply `--kind` to narrow queries only when the target is already clear. Terms within one query are combined as an AND query.

Use `context` first when the compact card omits setup or observation timing.
Its bounded owner source, native caller evidence, unresolved candidate evidence,
and nested assertion list distinguish an API result from fixture acquisition and
show whether a predicate observes behavior before, during, or after a callback or
caught panic. For inherited external contracts that are absent from the index,
record the missing evidence source; do not silently convert a convention into a
citation.

Write one compact submission matching [compact-submission.schema.json](schemas/compact-submission.schema.json). Copy the current batch result's `batchFingerprint` exactly; do not recompute it or copy a fingerprint from an earlier generation. `submit` uses that fingerprint to bind the rows to the exact current `batchPath`. Each `citationHandles` entry must be an exact `evidenceHandle` returned by search. Multiple citations are independent array entries.

For a fresh row, supply assertion ID, classification, evidence status, actual
outcome, rationale, citation handles, observation strength, and
applicable reasons directly. `Documented` and `Mixed` also carry a decisive
clause. Weak observations carry a passing counterexample. Uncertain source
candidates carry candidate role and disposition.

Do not transcribe a caller ID for a directly owned assertion: omit
`callingTestIds`, and native ownership supplies its enclosing test. If supplied,
an unrelated valid test ID is rejected before journal insertion. Keep explicit
caller attribution only for a genuinely shared assertion with source-backed or
confirmed indirect caller evidence.

To reuse an accepted row, supply `reuseFrom`, explicit
`equivalenceConfirmed: true`, and replacement `citationHandles`. Include
`callingTestIds` only for a genuinely shared assertion; omit it for a directly
owned assertion. Do not reuse implicitly or infer equivalence. An empty citation
replacement is rejected rather than falling back to the prior assertion's
evidence. If the reused assertion requires an unlinked reason, provide it
explicitly.

Submit the rows and export the next assertion batch through one `submit` command using [commands.md](commands.md#submit-a-compact-row-batch). `submit` is the normal transition after every classified batch. Use the lower-level `advance`, `record-batch`, and `next-batch` commands only while recovering or inspecting an intermediate state.

After every successful `submit` with `continue`, read the returned
`nextBatch.displayPath` immediately and start the next batch. A prior failed
submission attempt changes neither this loop nor the completion gate.

## Follow the current directive

Use the latest successful transition's `nextAction`, including on the initial batch or after resumption. After `submit`, use the `record` counts to report progress (`Classified C of T assertions; R remain. The batch completed N sites.`). Counts, progress updates, and checkpoints are informational only; do not pause or wait for a reply.

- `continue`: immediately read the returned display and classify its assertions in this invocation. For `submit` or `advance`, these paths are `nextBatch.displayPath` and `nextBatch.batchPath`; do not request the batch again. The recovery-only `record-batch` does not export a batch, so use `next-batch` after its `continue`.
- `report`: run stage 4 immediately, regardless of which supported path reached reporting.
- Only a successful `report` with `nextAction: "complete"` and `finalResponseAllowed: true` authorizes a completed response.

A concrete command or environment diagnostic that requires user action leaves
the audit incomplete. Preserve the failed operation, its diagnostic, and the last
successful state. Do not claim completion or continue dependent work. This path is
for unrecoverable blockers, not transient or agent-correctable failures.

## Recover from failure

Correct diagnosed row defects before retrying. Stop on stale state or a conflicting accepted classification; report the diagnostic without overwriting state. If export failed after the journal write succeeded, replay the identical compact submission with the original machine batch; record insertion is idempotent. Do not overwrite either input.

Stop without claiming completion for a concrete assertion, field, evidence,
command, or environment diagnostic that cannot be corrected without user action,
or for an interruption explicitly reported by the execution platform. Preserve
the failed operation and exact diagnostic, report the audit as incomplete, and
preserve durable state. Do not use this stop path for a transient or
agent-correctable failure. A user turn boundary, progress message, or long
duration is not a runtime interruption. Begin resumption by reading the last
successful directive and corresponding batch.

# Stage 4: validate and emit reports

Run `ss-ai test-audit report` using [commands.md](commands.md#emit-final-reports). The command requires exactly one accepted classification row for every assertion and one inventory row for every test.

Review `consistencyDiagnostics` before delivery. Investigate broad citation reuse,
one rationale used for different predicates, and analogous predicates with
different classifications. Handle `high` priority items before report delivery;
use `medium` and `low` items as targeted calibration prompts, not automatic
relabels. A repeated short rationale must still contain a discriminating
operation, condition, missing premise, or permitted alternative. Record any
corrected judgment by initializing a new state directory at the same source
snapshot, rerunning evidence selection, and replaying the revised judgments
with fresh handles. Never submit a changed row to an existing completed
classification journal; do not silently edit completed reports.

When comparing completed runs, review the prioritized queue in order: contract
profile mismatch, denominator and correction membership, authority-policy
transitions, decisive-clause and observation-strength differences, candidate
roles, caller-set drift, and test-gap drift. Agreement is not a winner signal.
Inspect sampled agreeing sites as well as disagreements.
Group private-layer differences by the public operation they claim to observe,
not by a repeated boilerplate rationale. Compare the effective authority rule
and the cited export/document passage; a different profile hash alone is not a
policy diagnosis. Resolve the receiver type and fixture preconditions before
deciding whether a promise applies.

Deliver these generated paths:

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

The summary must state the audited repository and scopes, snapshot revision,
checkout-independent content and index digests, tool and rubric versions, test
and assertion totals, classification totals, module rates, inventory corrections,
consistency diagnostics, output paths, discovery cross-check, language coverage,
and every material scan diagnostic or unresolved unsupported form.

Execute native commands through their direct process boundary and inspect `$LASTEXITCODE`; a wrapper completing successfully is not evidence that the native command succeeded. In the rare case PowerShell is used for recovery, normalize projected arrays with `@(...)` and use literal string checks such as `.Contains()` rather than `-like` when Rust backticks are present.

When comparing repeated audits, run `compare` on complete states and report the
content, index, tool, and rubric identities with the transition matrix. Do not
select a winner from net percentages; inspect changed caller edges, unmatched
sites, evidence sufficiency, and test-gap membership. Follow
[the evaluation procedure](docs/evaluation.md) when changing scanner behavior,
schemas, or the rubric.

Do not omit smoke tests, unlinked shared assertions, parse fallbacks, or unsupported forms to make coverage appear complete.
