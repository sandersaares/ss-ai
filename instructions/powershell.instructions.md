---
applyTo: "**/*.ps1,**/*.psm1,**/*.psd1,**/*.just,**/Justfile,**/justfile,**/.justfile"
---

# Target the latest stable PowerShell

Use the latest stable PowerShell release as the compatibility target. Prefer current language features, APIs, and native argument passing instead of version-dependent branches or legacy workarounds. Raise an affected script's `#Requires -Version` to the current stable release line when needed. Support older releases only when explicitly requested.

# Perfect edge case handling is not required in PowerShell scripts

PowerShell is a "softly typed" language and has more edge cases that anyone could reasonably cover in scripts. Do not worry about purely theoretical edge cases - a PowerShell script just needs to be good enough to cover the most likely scenarios.

This applies to PowerShell code wherever it lives, including justfile recipes. An edge case remains theoretical when its trigger is merely possible but the repository provides no evidence that it occurs in likely supported usage, even if the resulting failure can be traced precisely. Documentation, a published template contract, or a declared supported platform is evidence even without an in-repository call site. Security-relevant input that crosses a documented trust boundary or that an adversary can influence remains in scope without a benign occurrence.

# Use strict language mode with error handling enabled

PowerShell scripts should enable strict mode and abort on any errors. A typical script header looks like this:

```powershell
#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
```