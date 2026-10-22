Use the common rules in `workflow-data.md` and the feedback contract in
`data-guides/audit.md`.

```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Add-ReviewAuditFeedback.ps1" `
  -OutputDirectory "{{SESSION_STATE}}" -FindingsPath "{{SESSION_STATE}}\findings.json" `
  -ResponseId "{{RESPONSE_ID}}" -FindingId "{{FINDING_ID}}" -TextPath "{{TEXT_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `SESSION_STATE` | Absolute session-state directory. |
| `RESPONSE_ID` | Stable identifier for this human response, using letters, digits, underscores or dashes. |
| `FINDING_ID` | Existing finding addressed by the response. |
| `TEXT_PATH` | Absolute text-file path containing the verbatim feedback fragment. |

Success emits `status: "Recorded"`, `findingId`, and `responseId`. Stop on error;
do not lose feedback or turn clarification into approval.
