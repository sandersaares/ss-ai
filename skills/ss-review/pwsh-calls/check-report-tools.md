```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

& (Join-Path "{{SKILL_ROOT}}" 'scripts/Test-ReviewReportTools.ps1')
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Absolute directory containing the active `SKILL.md`. |

The adapter selects the shared user-local tools directory and passes it and the active skill
directory to `ss-ai workflow test-review-report-tools`.

Run in the review's actual host/container/WSL distribution, as the same user and with the same
environment and working directory that will render reports. The command emits JSON with
`pandoc`, `node`, `mermaid`, and `browser` paths, a `missing` array, and a `diagnostics` array.
When nothing is missing, it also renders a temporary HTML report containing a Mermaid diagram
through the production renderer, then removes the probe files.

Readiness requires a successful exit and empty `missing` and `diagnostics` arrays. A nonzero
exit, absent result, or reported missing tool means setup is incomplete. Preserve error output:
it can identify incompatible runtimes, missing shared libraries, or a sandbox failure even
when every executable exists. A browser download, npm success, or a report without diagrams
does not establish readiness.

If setup is incomplete, read `report-tool-setup.md` at the skill root and follow its applicable
platform procedure. Ask the user for permission before installing anything, including Ubuntu
system packages. Do not silently ignore missing tools, install without permission, or infer
permission from an unanswered question. Apply the skill's user-decision rules.

Rerun this check after installation or repair. Record the user's choice and readiness outcome
in the session's prose checkpoint so a declined installation does not cause repeated questions.
If the user declines, or setup fails, report the specific limitation and continue with Markdown
audit documents. GitHub overview publication does not need the HTML tools; ADO HTML publication
reports its limitation. Never describe a failed probe or a partially completed install as success.
