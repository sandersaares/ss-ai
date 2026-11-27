Create a new `decisions` plan with `write-workflow-data.md` containing only the
finding ID, decision, and reason for this operation. Then run:

```text
ss-ai workflow audit-add-decisions --path "{{SESSION_STATE}}/audit-decisions.json" --plan-path "{{PLAN_PATH}}" --recorded-utc "{{NOW_UTC}}" --policy-classification
```

Records one policy classification. `auto-approved` carries the finding into the approved set;
`needs-audit` advances the classification loop but leaves the finding undecided for publication.
`--policy-classification` rejects user verdicts and feedback. The command emits a single JSON object with `decisionsPath` and
`recorded`; verify that `recorded` contains the finding identifier, then poll for the next finding.
On an error, apply [error recovery](../error-recovery.md). The review orchestrator also uses `needs-audit`
when a substantive revision requires renewed user approval; it then returns directly to the audit.

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `FINDING_ID` | Identifier returned by the polling command, or identified by the review orchestrator as needing renewed user approval. |
| `DECISION` | `auto-approved` or `needs-audit`, as determined by `auto-approval-rules.md`. |
| `REASON` | The motivating focus-area path and guideline, or why no clear direct trace supports auto-approval. |
| `PLAN_PATH` | Absolute path to the schema-validated decisions plan. |
| `NOW_UTC` | Current UTC time as an RFC 3339 timestamp. |
