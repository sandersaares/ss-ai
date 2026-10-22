```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Test-FindingArtifact.ps1" `
  -Path "{{SESSION_STATE}}/findings" `
  -ManifestPath "{{MANIFEST_PATH}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `MANIFEST_PATH` | Absolute `manifestPath` returned by work initialization. The command reads all expected reviewers directly from this immutable coverage record. |

The JSON result reports `isValid`, `errors`, `missingReviewers`, `incompleteReviewers`, `failedReviewers`, and `unexpectedReviewers`. A nonzero exit or `isValid: false` blocks deduplication. Resume interrupted or failed workers using their existing group journals and findings, repeating only uncompleted groups. The gate checks every completed worker's journal against the immutable grouped inventory, validates required findings from batch receipts, and revalidates completed-empty exclusions against the bound focus metadata. Repair inconsistent state before repeating this gate; never replace the manifest or journal with a list reconstructed from conversation history.
