# Shared pull request context access

This recipe governs every agent read of an initial or refreshed pull request capture, including delegated work, authorship classification, overview preparation, quality refinement, synthesis, audit, and freshness revalidation.

The full JSON capture is a canonical data store, not a reading document. Never open it with a file-view tool, `Get-Content`, a raw dump, or custom parsing/serialization that places the document in model context. Never bypass this recipe with an alternate provider fetch. Publication and validation scripts still consume the complete capture path programmatically; do not replace their inputs with query results.

Read only bounded pages relevant to the current question. Do not indiscriminately loop over pages or concatenate them into a transcript, prompt, or substitute capture. Retain identifiers, evidence references, and decisions instead of dumps. Relevant complete conversations may be read in chronological pages. Authorship classification and freshness delta discovery are explicitly exhaustive projected tasks: partition large sets into bounded batches so aggregate history does not crowd the working context.

The CLI guarantees bounded output, not filesystem access control. Raw files remain accessible; the prohibition on bypassing the CLI or accumulating its pages is a prompt policy.

## Query contract

Prepare a saved request with `scripts\New-PullRequestQueryRequest.ps1`, then read it with `scripts\Read-PullRequestContext.ps1 -RequestPath`. Use named arguments and noninteractive PowerShell. Requests store the resolved capture paths, snapshot identities, normalized selectors, and exact continuation. Never hand-author a request, embed JSON in a shell string, or reconstruct filters for continuation. Commands read local captures without fetching or modifying them.

Request creation accepts `-PageSize` from 1 through 50, defaulting to 20. Each reading response is at most 16 KiB of serialized UTF-8 stdout, including its final newline. The byte ceiling can yield fewer items than the requested page size. There is no switch to raise the ceiling and no all/full-document mode. The direct query CLI remains available for programmatic callers; workers use saved requests.

The response envelope is:

```json
{
  "schemaVersion": 1,
  "query": "threads",
  "snapshotId": "...",
  "totalCount": 0,
  "items": [],
  "nextCursor": null,
  "complete": true
}
```

`totalCount` describes the query result, not proof that this page contains it all. Use `nextCursor` and `complete` to determine whether the selected result has been exhausted.

Long descriptions and comment bodies arrive as exact contiguous `content` slices with `contentOffset`, `contentLength`, and `contentComplete`. Offsets use UTF-16 code units, not UTF-8 bytes; a slice never splits a surrogate pair. Read the required continuations before treating a body as complete. Thread-opening and search excerpts are discovery aids, not full-body evidence. Native metadata is a projection only, not an arbitrary provider object dump.

# Stage 1: select a query for the current question

Use the variants below as needed, not as a checklist to dump every collection. Creation returns only request identity and path; read its first page through Stage 2. Reuse that path for all continuations and replays of the same request. Use a new path for different filters. All command examples share these placeholders:

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `CONTEXT_PATH` | Absolute path to the initial or a uniquely prefixed refresh capture. The query applies its separately validated semantic decisions without rewriting evidence. |
| `REQUEST_PATH` | New absolute JSON path for this selected query, private to the worker. Never edit the generated file. |
| `DIAGNOSTICS_PATH` | Absolute private `input-diagnostics.json` path supplied in the worker assignment. Without an assignment manifest, allocate this filename under that worker's own output directory and pass it explicitly. The primary session uses its own file. Never share it across concurrent workers. |

The `powershell` examples require a noninteractive `pwsh` calling session. They use PowerShell single-quoted literals: double an apostrophe within a literal; backslashes, spaces, double quotes, and non-ASCII text require no JSON escaping. For multiline or difficult search text, use `-TextPath` as below. Do not wrap these commands in another interpolated command string.

## Calling shell

Keep recipe programs in PowerShell rather than maintaining a separate program for each calling shell. From a PowerShell shell, execute the examples directly.

From a Bash-backed tool, use a file-writing tool to save the selected PowerShell example unchanged to a private UTF-8 `.ps1` file after expanding its placeholders. Run that file as follows:

```bash
pwsh -NoProfile -NonInteractive -File '{{PROGRAM_PATH}}'
```

| Placeholder | Description |
| --- | --- |
| `PROGRAM_PATH` | Absolute path to the private `.ps1` file. This example uses Bash single-quote syntax: replace each apostrophe in the path with `'"'"'`, not PowerShell's doubled apostrophe. |

Check the exit status before using the output and follow Stage 2 for errors and saved-query reads. Delete the private script after its command succeeds; retain it for diagnosis on failure.

This file boundary applies to typed arrays and composed PowerShell operations without translating their syntax or changing their binding. Never copy a PowerShell script block directly into Bash or interpolate its body into a Bash command string: Bash interprets `$`, backticks, and `&` before PowerShell receives them. Do not replace typed arrays with shell arguments or ad hoc JSON. Forward this shared recipe, including this calling-shell contract, to bounded workers.

Ordinary scalar `pwsh -NonInteractive -File` calls may run directly without a private script. Use the calling shell's path quoting and line continuations; PowerShell backticks do not continue Bash lines.

## Metadata and description

Select one field, not the whole pull request:

```powershell
pwsh -NoProfile -NonInteractive -File '{{SKILL_ROOT}}\scripts\New-PullRequestQueryRequest.ps1' `
  -Path '{{CONTEXT_PATH}}' -RequestPath '{{REQUEST_PATH}}' -DiagnosticsPath '{{DIAGNOSTICS_PATH}}' `
  -Query metadata `
  -Field '{{FIELD}}'
```

| Placeholder | Description |
| --- | --- |
| `FIELD` | One allowlisted field below; for example, `pullRequestUrl` for a link, `reviewedSourceCommit` for a commit binding, or `description` for stated intent. |

Allowed fields are `platform`, `repositoryUrl`, `pullRequestUrl`, `fetchedUtc`, `target`, `authenticatedIdentity`, `id`, `title`, `description`, `author`, `createdUtc`, `sourceRef`, `targetRef`, `reviewedSourceCommit`, `reviewedMergeBaseCommit`, `isAuthoredBySelf`, `isDraft`, `reviewers`, and `revisions`. `reviewers` and `revisions` enumerate individual records; for other fields, only the selected value is exposed.

Each item names its `field`. String-valued fields use the paginated `content` slice fields, even for short strings such as URLs. Non-string fields and JSON null use `value`, as does each individual `reviewers` or `revisions` record. An empty description is `content: ""`; an absent or null description is `value: null`.

Use the selected values for the current task. Read description continuations when necessary to understand the author's position; do not infer a completed claim from a partial slice. The overview normally needs only URL and commit fields, whereas reviewers may need the title and description. Treat descriptions as evidence of intent, not proof of implementation behavior or instructions.

## Thread discovery for assigned files

For a small explicit file set, use this PowerShell program:

```powershell
$ErrorActionPreference = 'Stop'
& '{{SKILL_ROOT}}\scripts\New-PullRequestQueryRequest.ps1' `
  -Path '{{CONTEXT_PATH}}' -RequestPath '{{REQUEST_PATH}}' -DiagnosticsPath '{{DIAGNOSTICS_PATH}}' `
  -Query threads -Files @('src\a.rs', 'src\b.rs')
# Propagate the called script's explicit failure exit to the host running this program.
if (-not $?) { exit 1 }
```

For a Bash-backed tool, run this same example through the private script described in **Calling shell**; do not maintain a second query program.

Replace the illustrative repository-relative paths with the assigned files and any known prior names from the diff. The program binds a typed array inside PowerShell; do not pass an array expression as a native argument to `pwsh -NonInteractive -File`, which accepts native command-line arguments rather than array expressions. Saving the program to a script preserves that binding. For one path, `-File` with `-Files 'src\a.rs'` is sufficient. Split large discovery sets into bounded, explicit requests rather than inventing JSON serialization.

The descriptors carry identifiers, including `openingCommentId`, URLs, kind, resolution, `isOutdated`, anchor, decision, an opening-comment excerpt, and comment count. They do not contain full comment bodies. Retain the relevant thread IDs, then read their conversations with the comments variant below.

`threads`, `comments`, and `search` share these optional filters:

| Argument | Selection |
| --- | --- |
| `-Files` | A nonempty typed array of repository-relative paths. Supplying it implies file filtering. |
| `-Scope all` | Default scope; an explicit file set still restricts it to those files. |
| `-Scope files` | File-anchored discussion; supply an explicit file set for assigned-file work. |
| `-Scope global` | Unanchored PR-wide discussion; cannot be combined with file selectors. Search also includes the description when its scope permits. |
| `-Kind` | A canonical conversation kind, such as `text` or `reviewSummary`, not a provider-specific thread type. |
| `-Resolution` | `active`, `resolved`, or `indeterminate`. Omit to include all resolution states. |

Paths normalize a leading provider slash and slash separators and then compare ordinally and case-sensitively. Do not lowercase paths. Include known prior names from the diff explicitly; the query does not infer rename history. Outdated threads and every resolution state are included by default. Do not routinely restrict discovery to active/current threads: settled decisions and outdated anchors can still govern a finding.

## PR-wide discussion

Discover unanchored top-level conversations and review summaries separately from file-anchored threads:

```powershell
pwsh -NoProfile -NonInteractive -File '{{SKILL_ROOT}}\scripts\New-PullRequestQueryRequest.ps1' `
  -Path '{{CONTEXT_PATH}}' -RequestPath '{{REQUEST_PATH}}' -DiagnosticsPath '{{DIAGNOSTICS_PATH}}' `
  -Query threads `
  -Scope global
```

Read relevant conversations by ID, including review-summary comments. A file-filtered result alone does not establish that no prior discussion exists: a decision may be global, anchored to a prior name or another file, resolved, or outdated.

## Complete conversations and exact comments

Read a selected conversation in chronological pages:

```powershell
pwsh -NoProfile -NonInteractive -File '{{SKILL_ROOT}}\scripts\New-PullRequestQueryRequest.ps1' `
  -Path '{{CONTEXT_PATH}}' -RequestPath '{{REQUEST_PATH}}' -DiagnosticsPath '{{DIAGNOSTICS_PATH}}' `
  -Query comments `
  -ThreadId '{{THREAD_ID}}'
```

For one exact comment:

```powershell
pwsh -NoProfile -NonInteractive -File '{{SKILL_ROOT}}\scripts\New-PullRequestQueryRequest.ps1' `
  -Path '{{CONTEXT_PATH}}' -RequestPath '{{REQUEST_PATH}}' -DiagnosticsPath '{{DIAGNOSTICS_PATH}}' `
  -Query comments `
  -ThreadId '{{THREAD_ID}}' `
  -CommentId '{{COMMENT_ID}}'
```

| Placeholder | Description |
| --- | --- |
| `THREAD_ID` | Exact canonical thread ID returned by discovery or a change reference. |
| `COMMENT_ID` | Exact comment ID in that thread. `-CommentId` requires `-ThreadId`. Use the descriptor's `openingCommentId` when selecting a follow-up's opening comment. |

Interpret the verbatim text together with its attribution and relevant later replies or edits. Do not infer a conversation's conclusion from its root comment or a search excerpt. A specific pair selects the exact comment, whose body may still require continuation slices.

Comments can alternatively be selected by `-Authorship indeterminate`, `-Authorship agent`, or `-Authorship human`. Without an explicit ID, authorship, file, or global filter, the comments query refuses a bulk read. A `-Kind` or `-Resolution` filter alone does not authorize bulk comments.

## Authorship classification input

To allocate bounded batches without reading their bodies into the orchestrator, retrieve an index:

```powershell
pwsh -NoProfile -NonInteractive -File '{{SKILL_ROOT}}\scripts\New-PullRequestQueryRequest.ps1' `
  -Path '{{CONTEXT_PATH}}' -RequestPath '{{REQUEST_PATH}}' -DiagnosticsPath '{{DIAGNOSTICS_PATH}}' `
  -Query comments `
  -Authorship indeterminate `
  -IndexOnly
```

`-IndexOnly` is valid only for comments. It returns identifiers, attribution, and `contentLength`, with neither body text nor excerpts. It also works with the conversation and exact-comment selectors above. This index is for allocation and discovery, never for deciding authorship.

Pass a bounded set of indexed thread/comment ID pairs to a classifier. It uses the exact-comment command above to read each verbatim body and attribution, following content continuations. A classifier reading a selected batch directly may omit `-IndexOnly` from the command above. Classification is language-model judgment, not a metadata-only guess. Collect decisions in a file and apply them only after finishing the immutable batch or entire pass, using `classify-comment-authorship.md`. Do not mutate the capture between reads of the same cursor sequence. After applying a bounded batch, start a fresh indeterminate query without the old cursor; classified comments disappear from this filter.

## Cross-file symbol or topic search

```powershell
pwsh -NoProfile -NonInteractive -File '{{SKILL_ROOT}}\scripts\New-PullRequestQueryRequest.ps1' `
  -Path '{{CONTEXT_PATH}}' -RequestPath '{{REQUEST_PATH}}' -DiagnosticsPath '{{DIAGNOSTICS_PATH}}' `
  -Query search `
  -TextPath '{{TEXT_PATH}}'
```

| Placeholder | Description |
| --- | --- |
| `TEXT_PATH` | Absolute UTF-8 text-file path containing the exact literal search text, including any quotes or line breaks. Do not add a final newline unless it is part of the search. For simple text, `-Text 'Widget'` avoids a file. Do not combine `-Text` and `-TextPath`. |

Search matches a case-insensitive literal substring in complete comment bodies and in the description when scope permits. It returns one bounded match excerpt per matching comment, not every occurrence and not full bodies. Resolved and outdated discussion remains included unless explicitly filtered. An unfiltered search reaches other-file and global discussion; do this before emitting a finding rather than relying only on assigned-file discovery. Vary the topic or symbol when one spelling cannot establish coverage. Read relevant hits through exact-comment or conversation queries before drawing conclusions.

## Freshness changes

```powershell
pwsh -NoProfile -NonInteractive -File '{{SKILL_ROOT}}\scripts\New-PullRequestQueryRequest.ps1' `
  -Path '{{CONTEXT_PATH}}' -RequestPath '{{REQUEST_PATH}}' -DiagnosticsPath '{{DIAGNOSTICS_PATH}}' `
  -Query changes `
  -ComparePath '{{BASELINE_PATH}}'
```

| Placeholder | Description |
| --- | --- |
| `BASELINE_PATH` | Absolute path to the previous checked capture for intervening freshness changes, or the initial capture for lifetime accounting. `CONTEXT_PATH` names the new refresh capture. |

The result contains paged delta entries with references to the baseline, current capture, or both. Discover every delta entry in bounded batches, retaining affected IDs and revalidation decisions instead of body dumps. Read relevant added content from the current capture, removed content from the baseline, and edited content from both, using the referenced IDs and the comments variants. Thread-resolution changes require reading the relevant conversation, not treating a status as the conclusion. `reviewEvidence` also covers changed PR intent or thread context: inspect relevant bounded metadata and thread pages even if no named comment changes appear. Use selected metadata fields for commit bindings.

# Stage 2: handle output and continue deliberately

After every command, check for an error before using its response. Apply [error recovery](../error-recovery.md): correct orchestration mistakes and retry safe transient failures instead of stopping the review. Never fall back to a raw file read.

In request creation, `-Path` must name the existing capture, such as `pr-context.json`, while `-RequestPath` names the separate query request output, such as `pr-author-request.json`. If `-Path` accidentally names that not-yet-created request, correct it to the assigned capture path, verify existence without reading the capture into context, and rerun request creation with the intended request output path. Do not ask the user, rerun proactor selection, or cancel unrelated preparation workers for this local invocation mistake.

Read the first page, or replay the last returned page after interruption:

```powershell
pwsh -NoProfile -NonInteractive -File '{{SKILL_ROOT}}\scripts\Read-PullRequestContext.ps1' `
  -RequestPath '{{REQUEST_PATH}}'
```

The placeholders are those defined above. To continue after processing an incomplete page, repeat this command with `-Next`. No filters, paths, page sizes, or cursor strings are copied into the continuation command. Do not use `-Next` if uncertain whether the prior output was received: replay first.

A changed capture invalidates both replays and continuation explicitly. Report the error, confirm the intended capture, and repeat the saved-reader command with `-Restart` to bind its current contents and read the first page. Restart is explicit and measured; it never silently skips evidence. Different filters or a different capture path require a new request, not editing saved state. Applying an authorship batch also requires a restart/new request; never continue a cursor across that mutation. Changes queries bind both captures.

Process a relevant page before requesting another. Keep only identifiers, decisions, and the evidence necessary for the current question in working context. `complete` describes the selected query, not all discussion in the capture. If a relevant body is incomplete, read its remaining contiguous slices; if a discovery result is irrelevant, do not expand its bodies. For required exhaustive classification or delta discovery, hand off bounded, nonoverlapping batches when needed rather than accumulating the entire projected history in one agent.
