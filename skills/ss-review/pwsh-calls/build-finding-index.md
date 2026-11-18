```text
ss-ai workflow finding-index --reviewer-directory "{{SESSION_STATE}}/findings" --output-path "{{SESSION_STATE}}/finding-index.json"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
