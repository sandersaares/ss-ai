```text
ss-ai workflow approved build --path "{{SESSION_STATE}}/findings.json" --decisions-path "{{SESSION_STATE}}/audit-decisions.json" --output-path "{{SESSION_STATE}}/approved-findings.json" --context-path "{{CONTEXT_PATH}}" --repository-path "{{REPOSITORY_PATH}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `CONTEXT_PATH` | Absolute path to the latest capture whose evidence has completed revalidation. |
| `REPOSITORY_PATH` | Local clone holding the reviewed commits. |

Keep `--context-path` pointed at the full refreshed capture so commit, anchor, and follow-up validation use complete evidence. Any agent investigation of a reported target or metadata value uses `query-pr-context.md`, never a direct capture read or a query result substituted for this input. Handle the validation summary and any excluded anchors as phase 8 describes; report errors before continuing to publication.
