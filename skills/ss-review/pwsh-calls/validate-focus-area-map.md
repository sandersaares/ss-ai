```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Test-FocusAreaMap.ps1" `
  -SessionStateDirectory "{{SESSION_STATE}}" -MapPath "{{MAP_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute path to this skill. |
| `SESSION_STATE` | Absolute path to the review's session-state directory. |
| `MAP_PATH` | Absolute path to the worker's categorization JSON. |

Success emits one JSON object with `status: "Valid"`, `categoryCount`, and
`chapterCount`. On failure, have the worker repair its JSON and repeat validation;
do not accept the partial map.
