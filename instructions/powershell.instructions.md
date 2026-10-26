---
applyTo: "**/*.ps1,**/*.psm1,**/*.psd1,**/*.just,**/Justfile,**/justfile,**/.justfile"
---

# Target the latest stable PowerShell

Use the latest stable PowerShell release as the compatibility target. Prefer current language features, APIs, and native argument passing instead of version-dependent branches or legacy workarounds. Raise an affected script's `#Requires -Version` to the current stable release line when needed. Support older releases only when explicitly requested.

# Perfect edge case handling is not required in PowerShell scripts

PowerShell is a "softly typed" language and has more edge cases that anyone could reasonably cover in scripts. Do not worry about purely theoretical edge cases - a PowerShell script just needs to be good enough to cover the most likely scenarios.

This applies to PowerShell code wherever it lives, including justfile recipes. An edge case remains theoretical when its trigger is merely possible but the repository provides no evidence that it occurs in likely supported usage, even if the resulting failure can be traced precisely. Documentation, a published template contract, or a declared supported platform is evidence even without an in-repository call site. Security-relevant input that crosses a documented trust boundary or that an adversary can influence remains in scope without a benign occurrence.

# Use strict language mode with error handling enabled

Every PowerShell script and executable documentation snippet must require
PowerShell 7.6 or later, enable strict mode, and stop on PowerShell and native
command failures. Start each independent snippet with this header, even when
nearby examples repeat it:

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
```

Apply this discipline to ad hoc tool commands and generated scripts as well as
checked-in files. Initialize each fresh PowerShell process, including child
processes launched through WSL; do not assume a parent process's settings carry
over. When a command tool does not honor `#Requires`, check its PowerShell
version explicitly before executing the snippet.

If a native program uses nonzero exit codes for expected results, narrowly
disable `$PSNativeCommandUseErrorActionPreference` for that invocation, interpret
`$LASTEXITCODE` immediately, and restore the preference in `finally`. Do not
disable strict mode or failure handling for the rest of the script to accommodate
one command.