```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Restore-FocusAreaCache.ps1" `
  -SessionStateDirectory "{{SESSION_STATE}}" `
  -InputDirectory "{{SESSION_STATE}}/instruction-set-source"
```
