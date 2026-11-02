```text
ss-ai workflow remove-finding --artifact-path "{{SESSION_STATE}}/findings.json" --plan-path "{{PLAN_PATH}}" --now-utc "{{NOW_UTC}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `PLAN_PATH` | Versioned `removals` plan built with `write-workflow-data.md`; each item names `findingId` and `reason`. |
| `NOW_UTC` | Current UTC time as an RFC 3339 timestamp. |

Check the result and apply [error recovery](../error-recovery.md) on failure.
