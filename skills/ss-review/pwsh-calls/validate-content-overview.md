# Stage 1: validate overview structure

```powershell
pwsh -NoProfile -NonInteractive -File '{{SKILL_ROOT}}\scripts\Test-ReviewOverview.ps1' `
  -Path '{{OVERVIEW_PATH}}'
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `OVERVIEW_PATH` | Absolute path to the generated `content-overview.md`. |

For a PR review add `-ContextPath '{{CONTEXT_PATH}}'`, where `CONTEXT_PATH` is the absolute path to the intended canonical capture. The command consumes it programmatically without emitting discussion.

On error return the diagnostic to the preparation worker for correction and rerun this gate. On `isValid: true`, assess the content's design references, significant-change caveat where applicable, and completeness questions semantically. Structural success does not establish correctness of the overview's claims. Do not write a session-local heading parser.
