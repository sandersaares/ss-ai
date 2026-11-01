```text
ss-ai workflow add-finding --artifact-path "{{SESSION_STATE}}/findings.json" --finding-path "{{FINDING_PATH}}" --origin synthesis --source-id "{{SOURCE_FINDING_ID}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `FINDING_PATH` | Absolute path of the UTF-8 JSON file holding the finding to append. Write it under `{{SESSION_STATE}}/staging/synthesis/` using `review.md`'s finding structure and multiline Markdown escaping rules. |
| `SOURCE_FINDING_ID` | Identifier of a finding this one generalizes. Repeat `--source-id` for each source, or omit it when there are none. |

Pass the file path, not its contents or an encoded payload. The command emits one JSON object with `findingId` and `artifactPath`; retain the assigned identifier before removing any subsumed source findings. On a validation error, correct the finding file and retry before continuing.
