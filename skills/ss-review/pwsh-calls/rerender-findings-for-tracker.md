```text
ss-ai workflow prepare-markdown --path "{{SESSION_STATE}}/approved-findings.json" --output-directory "{{SESSION_STATE}}/rendered" --destination tracker --context-path "{{CONTEXT_PATH}}"
```

Omit `--context-path` when the review has no captured pull request.

Placeholders to replace:

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `CONTEXT_PATH` | Absolute path to the latest checked capture, when reviewing a pull request. |

The renderer consumes the full capture programmatically; agent inspection of its metadata or discussion must use `query-pr-context.md` instead. Do not replace `--context-path` with a query projection. Check the emitted JSON for the rendered outputs and use them for the destination-change handling in phase 9. On an error, apply [error recovery](../error-recovery.md) before publication.
