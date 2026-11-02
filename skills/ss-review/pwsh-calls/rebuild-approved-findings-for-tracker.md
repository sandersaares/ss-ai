```text
ss-ai workflow approved build --path "{{SESSION_STATE}}/findings.json" --decisions-path "{{SESSION_STATE}}/audit-decisions.json" --output-path "{{SESSION_STATE}}/approved-findings.json"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `CONTEXT_PATH` | Absolute path to the latest checked capture, when reviewing a pull request. |

For a pull request, append `--context-path "{{CONTEXT_PATH}}"` to retain the checked evidence binding and follow-up validation. Omit `--repository-path`: tracker findings do not need anchors in the PR diff. With no pull request, omit both parameters. Inspect the emitted approval diagnostics and apply [error recovery](../error-recovery.md) to validation errors before continuing; never supply a missing human decision.
