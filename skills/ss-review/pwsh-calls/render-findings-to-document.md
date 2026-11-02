```text
ss-ai workflow prepare-markdown --path "{{SESSION_STATE}}/approved-findings.json" --output-directory "{{SESSION_STATE}}/rendered" --combined-path "{{SESSION_STATE}}/findings.md"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |

Use the emitted `combinedPath` as the local deliverable; report errors rather than presenting a stale file.
