```text
ss-ai workflow consolidate-findings --reviewer-directory "{{SESSION_STATE}}/findings" --plan-path "{{SESSION_STATE}}/merge-plan.json" --output-path "{{SESSION_STATE}}/findings.json"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
