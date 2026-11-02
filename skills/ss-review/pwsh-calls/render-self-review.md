```text
ss-ai workflow prepare-markdown --path "{{SESSION_STATE}}/findings.json" --output-directory "{{SESSION_STATE}}/self-review-rendered" --destination tracker --combined-path "{{SESSION_STATE}}/self-review-findings.md"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Absolute directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Absolute session state directory for this review. |

The command emits JSON containing `combinedPath` and `findings`, whose entries contain `findingId` and absolute `path`. Retain these exact paths for in-session delivery. Omitting `--finding-id` deliberately renders every current finding, including those requiring audit. The input is `findings.json`, never `approved-findings.json`. The `tracker` format preserves each finding's primary source location in standalone Markdown; it selects formatting only and does not file tracker items. Omit `--context-path` to render locally without a PR capture, remote calls, approval records, or report-tool dependencies.

An empty findings artifact still produces the combined report and an empty `findings` list. Treat command errors as delivery failures, not an empty review; apply the self-review error rules before continuing.
