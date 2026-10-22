```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Add-Finding.ps1" `
  -ArtifactPath "{{SESSION_STATE}}/findings.json" `
  -FindingPath "{{FINDING_PATH}}" `
  -Origin synthesis `
  -SourceFindingId "{{SOURCE_FINDING_IDS}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `FINDING_PATH` | Absolute path of the UTF-8 JSON file holding the finding to append. Write it under `{{SESSION_STATE}}/staging/synthesis/` using `review.md`'s finding structure and multiline Markdown escaping rules. |
| `SOURCE_FINDING_IDS` | Comma-separated identifiers of the findings this one generalizes. Drop the argument when it generalizes none. |

Pass the file path, not its contents or an encoded payload. The command emits one JSON object with `findingId` and `artifactPath`; retain the assigned identifier before removing any subsumed source findings. On a validation error, correct the finding file and retry before continuing.
