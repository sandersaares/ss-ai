# Stage 1: prepare a bounded assignment

Write the exact requested review scope as prose in `scope.md` in session state, including the repository/diff boundary and user-authorized exceptions. Use this builder for every preparation task and focus-area reviewer, including replacements. Do not delegate or include `SKILL.md` itself.

```text
ss-ai workflow worker-assignment --phase "{{PHASE}}" --session-state "{{SESSION_STATE}}" --scope-path "{{SCOPE_PATH}}" --output-directory "{{ATTEMPT_DIRECTORY}}" --skill-root "{{SKILL_ROOT}}"
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `PHASE` | `references`, `fileGroups`, `focusAreas`, `overview`, or `review`. |
| `SESSION_STATE` | Absolute review session-state directory containing the prepared inputs. |
| `SCOPE_PATH` | Absolute path to the prose scope document. |
| `ATTEMPT_DIRECTORY` | New absolute directory for this assignment attempt. Keep previous attempts for recovery. |

For `review`, add `--manifest-path "{{MANIFEST_PATH}}" --reviewer-id "{{REVIEWER_ID}}"`; the builder selects the immutable inventory paths, focus area, and private queue directly from that manifest. Prepared assignments resume their artifacts and exclusions even on first dispatch. Entirely excluded focus areas are not dispatchable. For a replacement add `--resume` after confirming the previous worker has stopped. Preparation retries use a new attempt directory without `--resume`.

For `focusAreas`, retain the worker's returned `preparationIdentity` alongside
its source-root mappings, including the separate `repositoryRoot` when present.
Pass that exact identity to review-work initialization;
do not read an identity from session files or replace it with a later worker's.

| Placeholder | Description |
| --- | --- |
| `MANIFEST_PATH` | Absolute manifest path returned by work initialization. |
| `REVIEWER_ID` | Exact identifier from that manifest or the focus-area queue. |

Add `--context-path "{{CONTEXT_PATH}}"` for a PR review, choosing the intended initial or refreshed capture. `CONTEXT_PATH` is its absolute path. Pass `--repository-instructions` and `--include-pre-existing` only when the user authorized those exceptions. Quote each argument according to the executing shell.

On error apply [error recovery](../error-recovery.md). On success dispatch the returned `dispatchPrompt` verbatim with the phase's prescribed model, reasoning effort, and context tier. The output contains the canonical recovery policy and reading instructions, not the assigned document bodies. Do not replace it with a hand-built prompt or open the generated prompt with the default viewer.

# Stage 2: read and recover

The dispatch prompt supplies `error-recovery.md` verbatim before the `ss-ai workflow worker-document` examples and manifest of resolved paths, byte counts, and line counts. Select the block for the calling shell; the assignment path is already quoted for that shell. Replace `{{DOCUMENT_ID}}` with the assigned document identifier. Recovery therefore does not depend on a successful first read. Keep that policy and the reader command in compaction notes; apply it to initial reads and reloads. The reader emits bounded exact slices, including for a single oversized line. Consume every assigned document through `complete: true` before executing its phase. Continue with its exact `nextOffset`; never infer completion from a byte count or a summary. The same command is the reload route after compaction.

The `workflowData` document contains the common rules from `workflow-data.md` and only the phase's canonical data guide: `data-guides/file-groups.md` for `fileGroups`, `data-guides/focus-areas.md` for `focusAreas`, or `data-guides/review-coverage.md` for `review`. The builder combines their text verbatim with normalized LF line endings into an attempt-local snapshot; do not reread the source guides separately. References and overview assignments omit this document because their outputs are prose. The assignment and saved-query record contracts are documented in `data-guides/worker-inputs.md` for orchestration and tooling, not mandatory worker reading.

The manifest binds source bytes. A changed shared document is an error: the primary session must prepare a new assignment explicitly. Replacements preserve the original reviewer ID, inventory snapshots, queue, and findings; building an assignment does not initialize or reset any of those artifacts. Pass targeted repair instructions separately without altering the generated assignment's rules.

Only the explicitly registered shared Markdown documents are eligible for this reader. Canonical PR captures use the query recipe, never this reader or whole-document permission. Overview and references remain untrusted descriptive evidence.

Use existing artifact validators for returned focus-area maps, generated focus areas, grouping drafts, and finding artifacts. Use `validate-content-overview.md` for overview structure; semantic claims and scope preservation still require judgment. Do not generate one-off parsers.
