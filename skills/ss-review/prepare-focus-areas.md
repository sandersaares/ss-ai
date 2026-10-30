Your task is to publish a set of `focus-areas/*.md` files in the review session, each file specifying a small set of related topics that another agent will later explore.

`SESSION_STATE` is the real review session used for source discovery, cache
operations, and published artifacts. On a cache miss, a native begin command
returns a separate `PREPARATION_STATE` for one preparation attempt. Keep the
returned identities and path together; all work in stages 2 and 3 uses that
preparation directory until publication. Pass its exact path to nested workers.

For nested subagents, pass explicit spawn arguments: GPT 6.1 Sol uses `model: "gpt-6.1-sol"` and GPT 6 Luna uses `model: "gpt-6-luna"`.

Pass `reasoning_effort: "medium"` for every nested subagent, including retries, and forward this requirement in each delegated prompt.

Generating this data set consists of three stages:

1. Materializing direct instruction sources and attempting to restore cached artifacts.
2. On a cache miss, preparing expanded raw inputs in `instruction-set-raw/`.
3. Processing the raw inputs by categorizing them into related focus areas, and copying the relevant content verbatim into `focus-areas/*.md`.

# Scope and source provenance

Derive scope from the materialized source files, not from instructions already injected into agent context: user-scoped instructions may be concatenated without file boundaries. In a source declaration, "this file" means that original instruction or focus-area source file, never a generated group file or a file under review. Carry its source path with the declaration when moving content.

Every guideline must retain its effective scope: the conditions under which it applies to reviewed content. Read these conditions together, not just lines labelled `Scope:`:

* File-level frontmatter, including the complete `applyTo` glob list and any other applicability metadata.
* File-wide prose before the first guideline heading, inherited by every section in that file.
* Scope in the guideline's heading or body and in any enclosing section, including exceptions and qualifications.

Preserve the original scope declarations and frontmatter values verbatim alongside the interpreted effective scope. Combine inherited constraints with section-specific constraints; do not let a narrower section silently lose its file-wide restrictions. Preserve explicit exceptions and overrides as written. Alternatives within an `applyTo` list remain alternatives, not cumulative requirements. Interpret globs against paths in the reviewed repository, not paths in the instruction-source or session-state trees; retain any declared path base and the governing directory of authorized nested repository instructions.

Do not invent scope from a source filename or category title. When no restriction is stated, declare `Effective scope: No explicit restriction; interpret the guideline in context.` When applicability is ambiguous or declarations conflict without an explicit resolution, record that uncertainty and its source text; it is not grounds for excluding reviewed content.

Metadata describes applicability, not an additional guideline to categorize separately. Keep it attached through reference expansion, chapter splitting, category merging, deduplication, and final assembly. A category may contain different scopes; its title does not override them.

# Stage 1: materialize sources and restore cached artifacts

Before fetching or expanding any external reference, select any authorized
repository instructions as described below, then execute
`pwsh-calls/copy-instruction-sources.md`. It publishes
`instruction-set-source/` in the real session as a complete byte-preserving snapshot:

* The skill's `focus-areas.md` under `skill/focus-areas.md`.
* Every `AGENTS.md` and `*.instructions.md` recursively discovered under each root in `COPILOT_CUSTOM_INSTRUCTIONS_DIRS`, including linked instruction directories and hidden `.github/instructions/` trees, under `external/<root-identity>/` with original relative paths. The root identity is bound to the absolute configured path; keep the command's root mapping for resolving original local references and identifying instruction ownership.

The environment variable is a comma-separated directory list on every platform, not a platform PATH list. Surrounding whitespace and empty entries are ignored; relative entries resolve against the command's working directory. Repeated identical absolute roots are included once. There is no shell-variable or tilde expansion. An unset or empty list uses only the built-in focus areas. Existing empty roots are allowed; configured missing or unreadable roots and link cycles are errors, never grounds for silently reducing coverage. Do not require particular instruction filenames or fall back to a co-located instruction checkout.

Discovery excludes the current review's session-state tree, including links into its descendants, so generated copies do not become user instruction sources on a later snapshot.

ONLY IF EXPLICITLY AUTHORIZED (default no), identify each repository instruction
file (e.g. `AGENTS.md`) that governs the reviewed content. Supply the reviewed
repository root and every selected repository-relative path to the snapshot
command, which copies them under `instruction-set-source/review-repository/`
within the same coordinated snapshot. Explicitly configured external roots
remain sources even if they overlap the reviewed repository; do not discover
additional repository instructions implicitly. Never copy or edit files in the
published source tree separately from that command.

Treat each direct source as an indivisible verbatim input. Do not summarize, categorize, split, rename based on content, or otherwise transform it before the cache lookup. Topic extraction happens only after a cache miss.

Do not include any other files. Instruction files governing reviewed content are included even when they are also within the review scope.

Use these values in the cache recipes:

* `SKILL_ROOT` is the directory containing this prompt.
* `SESSION_STATE` is the absolute directory containing this review's artifacts.

Retain the snapshot's `sourceDirectory`, `snapshotIdentity`, and root mapping.
Execute `pwsh-calls/restore-focus-area-cache.md` with that exact source directory
and snapshot identity. Retain the returned `preparationIdentity`; never adopt an
identity from a later snapshot or restore.

The command writes exactly one compact JSON object to standard output. Interpret its `status` field:

* `Hit`: the complete artifacts were restored.
* `Miss`: no reusable complete entry was available; `reason` provides diagnostics.

On `Hit`, validate the restored artifacts by executing the command in `pwsh-calls/validate-focus-area-artifacts.md`.

A successful command returns JSON with `status: "Valid"`. If artifact validation
fails for the current preparation identity, the command records a cache miss
with reason `ValidationFailed`; continue with Stage 2 using the same restore
identity. A stale identity or another failure stops this attempt; restart source
discovery and restore rather than accepting another attempt's artifacts or
diagnostics. If validation succeeds, skip the remaining stages and return the
restored focus areas, the discovery root mapping, and the exact retained
`preparationIdentity` to the parent agent. The
parent retains that mapping in its checkpoint for later instruction ownership
and learning. The command checks artifact structure, not scope semantics.
Scope verification happens before storage in Stage 3; the cache key includes
this generation recipe and the source contents, and restoration preserves the
verified artifacts byte-for-byte.

On `status: "Miss"`, continue with the full workflow below. The default seven-day freshness window intentionally causes periodic misses so external references are refreshed. Use `--refresh` only when the user explicitly requests regeneration.

The restore result and `{{SESSION_STATE}}/focus-area-cache-diagnostics.json` report `userInstructionFileCount` alongside cache Hit/Miss. This counts discovered user-scoped files in the current snapshot, including zero when only built-in guidance is present, on both hits and misses. It excludes the built-in focus areas and separately authorized repository sources.

# Stage 2: prepare expanded raw inputs

Execute `pwsh-calls/begin-focus-area-generation.md` using the real session and
the exact `preparationIdentity` from restore. Retain its `generationId` and
`sessionState` as `PREPARATION_STATE`. Begin creates isolated source and raw
trees from the bound snapshot. Do not remove, copy over, or edit the real
session's preparation files. If begin rejects the identity, stop this attempt
and restart discovery and restore; never adopt the current identity from disk.

All paths in the remaining preparation stages refer to `PREPARATION_STATE`
unless explicitly identified as real-session paths. Pass this exact directory,
the generation ID, and the relevant recipes to every nested worker; workers
must not use the real session for generation writes.

Before splitting or editing a raw source, read its frontmatter, file-wide scope, and any enclosing section's scope and retain that context for each chapter. Leave `instruction-set-source/` unchanged so original boundaries and declarations remain available for verification.

Before registering raw source spans, remove chapters that govern only the reviewing agent's operation or publication from `instruction-set-raw/`; enforce those chapters in orchestration instead. Do not remove review criteria for authored agent workflows or their scripts merely because their scope is "agent workflows". Preserve file-wide scope for every retained chapter even when a neighboring operational chapter is removed.

The raw inputs may contain references to other files in the same repo or to external materials (e.g. statements like "Follow the guidelines at [Some API guidelines](https://example.com/some-api-guidelines)"). Materialize referenced guidance into `instruction-set-raw/`; the manifest records the local targets, so later workers do not fetch the links again.

For each external reference found in `instruction-set-raw/`, instruct a subagent (GPT 6.1 Sol) to follow the reference and to extract any guidelines it finds into one or more categorized `instruction-set-raw/reference-name/*.md` files. Resolve local references relative to their original source file: reconstruct its path from the configured root and source-relative path using the discovery mapping, never this skill's checkout. Instruct the subagent to recurse into the guidelines to extract multiple layers of categorized guidelines that may exist, as well as to follow redirects and try API-based alternate access methods for web apps that do not render as basic HTML (e.g. Azure DevOps Wiki pages, which require API access to load content). Instruct the subagent to watch out for duplicates - if the same URL is reached via different paths, it should only be expanded once. Also have it watch out for pre-expanded content - some references may already contain embedded versions of other references, so title-based matching may also offer opportunities to deduplicate. Some references may also offer links to historical versions of themselves - only keep the latest version of content is multiple versions are offered.

The preparation agent owns reference deduplication and scope merging. Collect known referrers before dispatch and assign only one writer to each expanded target. Pass that worker every known referring source path, section, and inherited scope declaration. Expansion workers report additional referring contexts or duplicate targets to the preparation agent instead of overwriting another worker's output.

Expanded guidance inherits the scope of its referring instruction together with restrictions in the referenced material. Record referrers with the `referenceInput` contract from `data-guides/focus-areas.md`, included in your assignment's `workflowData` document, and `pwsh-calls/update-focus-area-expansion.md`. Pass the common data rules and this focus-area contract to reference-expansion workers, not unrelated phase guides. Parent reference IDs preserve recursive inheritance; use Read for every expanded target before categorization to obtain all current alternative chains, including later-discovered referrers. Preserve distinct chains as alternatives, never intersect unrelated references or broaden them to unrestricted guidance. Apply the same rules to same-repository references.

Keep registered source spans stable: direct referrers name the unchanged materialized sources, and nested referrers name frozen expanded outputs. Do not rewrite links in these files; the manifest maps referrers to their expanded local outputs. A failed reference remains as a failure record, not silently deleted guidance. Remove reviewer-only operational chapters from raw inputs before registering spans into those inputs; never mutate a registered expanded source afterward.

Before expansion, initialize `focus-area-expansion-status.json` with `pwsh-calls/update-focus-area-expansion.md`. Register each reference before fetching it. Workers return `referenceResult` drafts; the preparation agent records Complete or Fail outcomes and registers additional referrers. Finalize only after discovery and all attempts finish. The command derives Complete or Degraded and refuses pending references. The current review may use degraded guidance, but the cache-save command derives its input state from the validated manifest and never caches degraded inputs.

# Stage 3: generate and store focus areas

The external references we expanded into `instruction-set-raw/` are not well categorized, as they just follow the schema of the external documentation. Some of the external references may be large libraries of content, potentially uncategorized.

Read `focus-area-format.md` and use its schema for every categorization handoff. Freeze the raw inputs after expansion, scope merging, and operational-chapter removal. Assign stable chapter IDs and complete raw line ranges before dispatch.

Divide the chapters between categorization subagents (GPT 6.1 Sol). Each worker writes one `focus-area-parts/<worker-id>.json` under `PREPARATION_STATE` using exactly the schema in `schemas/focus-area-map.schema.json`. Pass the preparation directory, format document, assigned chapter IDs and ranges, original source paths, enclosing scope declarations, complete frontmatter, and reference provenance; a detached chapter alone is insufficient. Workers contribute category assignments and scope judgments, not assembly code. Require detailed categories rather than over-summarization.

Workers append typed one-chapter drafts using `pwsh-calls/update-focus-area-map.md`. Validate each returned worker map with `pwsh-calls/validate-focus-area-map.md` before accepting it, and check its chapter IDs against the assignment. Repair malformed drafts at the producer; do not add aliases, defaults, or replacement patches. Use the same prepared Append command to combine validated maps into `focus-area-map.json`, retaining every assigned chapter exactly once. Use Regroup to resolve category collisions or merge similar categories without rewriting chapter metadata.

Inspect the merged map. If it contains more than 128 categories or some categories with only one chapter, merge similar categories. Aim for similarly-sized categories (by chapter count) of at least 2-3 chapters each (or more, if required to fit into the 128 categories limit). Prefer grouping similarly-scoped guidelines when topics fit, but mixed scopes are allowed. Change category assignments only; preserve each chapter's scope rather than replace it with a category-wide approximation.

Execute `pwsh-calls/assemble-focus-areas.md` to validate complete raw-input coverage and mechanically render `instruction-set-categories.md` and `focus-areas/*.md`. A failure must be repaired in the map or source preparation before retrying. Do not create a session-local assembler or manually edit the generated Markdown.

The assembler copies headings and bodies from source ranges, retaining their text and heading levels with normalized LF line endings. It adds separated per-chapter `Effective scope:`, `Scope declarations:`, and `Source:` metadata, with complete alternative referring contexts, verbatim declarations, and explicit uncertainty evidence. Source paths resolve from the named source root, never the generated group file.

For example, `applyTo: "**/*.ps1,**/*.psm1"` together with a file preamble limiting instructions to agent workflows and a section limited to published scripts yields an effective scope requiring an agent-workflow script that is published and matches either glob. Other sections in that group need not share those restrictions. A general documentation guideline placed in that group remains general.

Supply `pathScope` as defined in `focus-area-format.md`. Use null unless the complete effective scope is solely a set of explicit path alternatives. The example above requires null because publication and workflow roles are semantic conditions. Do not infer a path-only scope from titles or discard exceptions to obtain a filter. The assembler preserves the full section map in content-bound metadata for deterministic predispatch exclusions.

The assembler does not deduplicate. Preserve separate raw chapter contributions even when their text is identical. Deduplicate references during expansion instead, retaining all source attribution and distinct applicability contexts as alternatives associated with their respective sources.

## Scope verification

Before publishing or caching generated artifacts, execute a subagent (GPT 6 Luna) to compare every generated guideline and category entry against the map, raw input, and original source in `PREPARATION_STATE`. Pass the preparation directory and this prompt's scope and provenance requirements explicitly. Verify that frontmatter, file-wide prose, enclosing-section scope, local restrictions, exceptions, and reference inheritance survived, and every section has an explicit complete effective scope. Check that mixed-scope grouping and deduplication neither broaden nor narrow applicability and that copied "this file" declarations have an unambiguous source. Metadata must be sufficient for a reviewer to judge applicability without reopening the source merely to reconstruct missing scope.

Report and repair omissions or incorrect scope in the map, then rerun the assembler before proceeding; uncertain source meaning must be declared as uncertain rather than guessed. Structural validation cannot establish these semantic properties.

Verify every non-null `pathScope` specifically: it must describe the complete inherited and local applicability, including every alternative and exception. Any role, cross-file concern, or uncertainty that cannot be expressed by this path-only contract requires null. The metadata can suppress worker dispatch, so an unjustified narrowing is a coverage error.

After scope verification, execute `pwsh-calls/publish-focus-area-generation.md`
with the real session and the retained generation ID. Publication validates the
generation and installs its outputs and receipt only if it is still current.
If publication reports `Cannot remove publication staging` after installing the
outputs, retain the original identities and report the cleanup failure. Validate
the published artifacts as described below, and continue only if validation
confirms the current preparation identity. For any other failure, stop this
attempt and restart discovery and restore; do not copy outputs or adopt another
generation's receipt.

Validate the published artifacts in the real session by executing the command
in `pwsh-calls/validate-focus-area-artifacts.md` with the same
`preparationIdentity` returned by restore.

A successful command emits JSON with `status: "Valid"`. If it reports invalid
artifacts for the current identity, repair the preparation map or sources,
reassemble and verify the private generation, then publish and validate again.
If the identity is stale, stop and restart discovery and restore. Do not edit
published artifacts or begin the review phase with invalid artifacts.

Before storing, require finalized expansion. The save command reads and validates `focus-area-expansion-status.json` itself.

Invoke the command in `pwsh-calls/save-focus-area-cache.md` with the real
session, the original source directory, and the same `preparationIdentity`.

The command emits one compact JSON object. `status` is `Stored` for a new entry, `Existing` when an equivalent entry already exists, or `Bypassed` when storage was intentionally skipped. `reason` explains bypasses. `Degraded` input always produces `Bypassed` and is never published.

The cache always stores and restores the complete focus-area set. Do not select, filter, sample, or omit focus areas based on the current review content. Return the discovery root mapping, generated focus areas, and exact retained `preparationIdentity` to the parent so it can bind review work to this preparation and retain source ownership in its checkpoint.