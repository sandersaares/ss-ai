Use the common rules in `workflow-data.md` and the feedback contract in
`data-guides/audit.md`.

```text
ss-ai workflow audit-add-feedback --path "{{SESSION_STATE}}/audit-feedback.json" --findings-path "{{SESSION_STATE}}/findings.json" --response-id "{{RESPONSE_ID}}" --finding-id "{{FINDING_ID}}" --text-path "{{TEXT_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `SESSION_STATE` | Absolute session-state directory. |
| `RESPONSE_ID` | Stable identifier for this human response, using letters, digits, underscores or dashes. |
| `FINDING_ID` | Existing finding addressed by the response. |
| `TEXT_PATH` | Absolute text-file path containing the verbatim feedback fragment. |

Success emits `status: "Recorded"`, `findingId`, and `responseId`. On error apply [error recovery](../error-recovery.md);
do not lose feedback or turn clarification into approval.
