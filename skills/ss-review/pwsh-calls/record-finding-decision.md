Create a new `decisions` plan with `write-workflow-data.md` containing only the
human decision and no inline feedback, then run:

```text
ss-ai workflow audit-add-decisions --path "{{SESSION_STATE}}/audit-decisions.json" --plan-path "{{PLAN_PATH}}" --recorded-utc "{{NOW_UTC}}" --audit-feedback-path "{{FEEDBACK_PATH}}"
```

For an existing individual decision object instead of a plan, use:

```text
ss-ai workflow audit-add-decisions --path "{{SESSION_STATE}}/audit-decisions.json" --item-path "{{ITEM_PATH}}" --recorded-utc "{{NOW_UTC}}" --audit-feedback-path "{{FEEDBACK_PATH}}"
```

The object contains `findingId` and `decision`, with an optional `reason`.
The command accepts mixed case and surrounding whitespace in an individual
decision and stores its canonical value. Do not combine `--item-path` and
`--plan-path`.

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `FINDING_ID` | Identifier of the finding the user decided on. |
| `DECISION` | One of `approved`, `rejected` or `dropped`. Policy classification uses `record-auto-approval-decision.md` instead. |
| `FEEDBACK_PATH` | Absolute `audit-feedback.json` path. Omit the argument only if no feedback log exists. |
| `PLAN_PATH` | Absolute path to the schema-validated decisions plan. |
| `ITEM_PATH` | Absolute path to the individual decision JSON object, without inline feedback. |
| `NOW_UTC` | Current UTC time as an RFC 3339 timestamp. |

Success reports `recorded` identifiers. The command validates and joins feedback;
apply [error recovery](../error-recovery.md) on errors rather than writing a decision without its recorded human feedback.
