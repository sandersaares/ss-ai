```text
ss-ai workflow prepare-markdown --path "{{SESSION_STATE}}/findings.json" --output-directory "{{SESSION_STATE}}/rendered" --finding-id "{{FINDING_ID}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `FINDING_ID` | Identifier of a finding to re-render. Repeat `--finding-id` for each selected finding. |

Inspect the emitted JSON and apply [error recovery](../error-recovery.md) before using the rendered files.
