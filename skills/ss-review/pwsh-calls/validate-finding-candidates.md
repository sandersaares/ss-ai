```text
ss-ai workflow validate-candidates --path "{{SESSION_STATE}}/merge-candidates.json" --index-path "{{SESSION_STATE}}/finding-index.json"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `SESSION_STATE` | Absolute session-state directory. |

Success emits `status: "Valid"` and `clusterCount`. Repair candidate drafts and
rebuild the plan if validation fails; do not dispatch adjudicators on invalid IDs.
