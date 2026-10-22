```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\New-FocusAreaArtifacts.ps1" `
  -SessionStateDirectory "{{SESSION_STATE}}" -MapPath "{{MAP_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute path to this skill. |
| `SESSION_STATE` | Absolute path to the review's session-state directory. |
| `MAP_PATH` | Absolute path to the merged `focus-area-map.json` in session state. |

Success emits one JSON object with `status: "Assembled"`, `categoryCount`, and
`chapterCount`. Continue to semantic scope verification. On failure, repair the
map or source preparation and retry; do not review or cache partial artifacts.
