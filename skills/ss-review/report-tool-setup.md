# Report renderer prerequisites

Load this guide only when the report-tool check finds incomplete setup or rendering fails.
HTML generation needs Pandoc for Markdown and Mermaid CLI for diagrams. Mermaid CLI launches
Puppeteer's Chrome for Testing, a downloaded browser that also needs operating-system libraries,
fonts, writable storage, and a working sandbox. npm installs JavaScript packages; downloading
Chrome does not install its Ubuntu system packages.

## Required components

| Component | Requirement |
| --- | --- |
| PowerShell | The stable release line required by the skill scripts, currently 7.6. Use `pwsh`, not Windows PowerShell. |
| Node.js and npm | A supported Node.js LTS release with npm; the locked Puppeteer dependency requires Node.js 22.12.0 or later. Mermaid CLI's own lower Node requirement is not sufficient. |
| Pandoc | 3.8 or later with `--syntax-highlighting` support. The portable installer pins 3.11. Ubuntu's repository version may be too old; the render probe checks the actual executable. |
| JavaScript packages | The active skill's `tools/package-lock.json`, installed user-wide with `npm ci` and strict engine checking. Skill copies with the same lockfile share an installation. Do not substitute an unrelated Mermaid CLI, install a different Puppeteer version, or regenerate the lockfile. |
| Browser | The Chrome for Testing revision selected by the locked Puppeteer installation. Download it explicitly because npm lifecycle scripts are disabled. An unrelated installed Chrome/Edge or Playwright browser is not a substitute. |
| Host | Windows x64 or Ubuntu x64 for the pinned browser's documented support. Do not assume that regular Chrome's platform support also applies to this pinned Chrome for Testing build, especially on Linux ARM64. |
| Storage and environment | Writable user-local tool storage, user browser cache, temporary/profile storage, and report output directory. The skill directory needs only read access. Keep the same user, host, working directory, and Puppeteer configuration for install, check, and rendering. |
| Installation access | Access to the lockfile's npm registry, the official Pandoc release when needed, and Puppeteer's browser download host. Ubuntu also needs access to its configured apt repositories. Rendering itself needs no network resources. |

Report tools live under `%LOCALAPPDATA%/ss-review/tools` when `LOCALAPPDATA` is set,
or `~/.local/share/ss-review/tools` otherwise. Mermaid dependency directories are keyed by
the lockfile's SHA-256 hash so different skill versions can coexist. Installation applies
across sessions and checkouts for the same user and environment; it does not require a
checkout-local `node_modules` or changes to `PATH`. Existing checkout-local packages are
not used or deleted.

Puppeteer normally caches browsers under the user's `~/.cache/puppeteer`, including on Windows;
`PUPPETEER_CACHE_DIR` can override it. Check `PUPPETEER_EXECUTABLE_PATH`, download-skip variables,
and Puppeteer configuration if the selected browser is unexpected. Do not clear deliberate
configuration or security policy without permission. Windows installation does not provision WSL,
and installation in one container, distribution, or user account does not provision another.

# Step 1: obtain permission for the applicable setup

Explain the missing or unusable components and the changes needed. On Ubuntu, include system
libraries and fonts in the installation request, not just npm packages and the browser download.
The primary session must receive the user's answer before running installation commands.

Run the applicable platform procedure below. Commands without placeholders are literal.
For all skill commands, replace `SKILL_ROOT` with the absolute directory containing the active
`SKILL.md`. Run PowerShell commands with terminating error handling and check native exit codes;
stop the setup procedure at any failure rather than executing later steps as if it succeeded.

## Windows setup

Use a Windows version supported by Chrome for Testing and the selected Node.js LTS release.
Install approved missing PowerShell or Node.js prerequisites using the platform's supported
installer. For example, when using Windows Package Manager after permission:

```powershell
winget install --id Microsoft.PowerShell --exact --source winget --disable-interactivity
winget install --id OpenJS.NodeJS.LTS --exact --source winget --disable-interactivity
```

There are no placeholders. Run only the command for a missing prerequisite and inspect its exit
status. Refresh the session environment if the new installation is not visible, then rerun the
report-tool check. Node.js discovery also recognizes nvm-windows' `NVM_SYMLINK`.

Chrome extraction needs Windows `tar.exe` or PowerShell's archive support. Puppeteer runs Chrome's
`setup.exe` to configure sandbox permissions on the downloaded browser. Ubuntu libraries such as
`libnss3` and `libnspr4` have no Windows installation equivalent in this workflow.

Install the report tools:

```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Install-ReviewReportTools.ps1"
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Absolute path of the active skill. |

The installer downloads digest-verified portable Pandoc into user-local storage when Pandoc is
absent, installs the pinned npm packages in shared user-local storage and the browser in its
user cache, and runs the HTML/Mermaid probe before
emitting the tool record. An existing incompatible Pandoc on `PATH` requires an approved upgrade;
the portable copy does not override it. If Chrome reports access-denied, sandbox, or enterprise
policy errors, preserve the diagnostic and request an approved host repair. Do not disable the
sandbox or change enterprise policies to make the check pass.

## Incomplete browser cache recovery

This procedure applies on Windows and Ubuntu when Puppeteer reports
`IncompleteInstallationError`: a browser revision directory exists but its expected executable
is missing. An executable that exists but cannot launch needs the platform diagnosis instead.
The installer does not delete browser caches automatically. Installation permission alone does
not authorize deleting cached files.

Preserve the full browser installer output, including each affected browser name, revision
directory, and missing executable path. Use the paths from the current failure, not a revision
copied from this guide or an earlier review. Both `chrome` and `chrome-headless-shell` can have
incomplete downloads; address each directory explicitly reported as incomplete. The report check
selects Chrome and is not an inventory of all downloads attempted by Puppeteer.

Confirm the paths belong to the current review user's effective Puppeteer cache. Account for
`PUPPETEER_CACHE_DIR`, `PUPPETEER_EXECUTABLE_PATH`, download-skip variables, and Puppeteer
configuration. Preserve intentional overrides. If the diagnostic identifies an unexpected
location, resolve that configuration with the user before proposing deletion.

Ask the user for separate cleanup approval, naming the exact revision directories and missing
executables. Ask the user to arrange exclusive access to that cache and remove only those
incomplete revision directories: stop other browser installations and report rendering first,
recheck that each directory exists and its executable is still absent, and leave any directory
whose executable now exists untouched. Do not remove the cache root, browser-family directories,
other revisions, configuration files, or lock files. Do not follow symbolic links or directory
junctions to remove another location; have the user resolve those paths first. If exclusive
access or the exact directory identity is uncertain, stop rather than deleting anything.

Once the user confirms targeted cleanup, rerun the applicable approved Windows or Ubuntu
installation command in this guide, in the same environment and working directory. Keep
`-InstallBrowserDependencies` for the Ubuntu procedure. No new general installation prompt is
needed if that approval still covers the retry; request permission only for additional changes.
Inspect the full output and exit status. If the download fails again, preserve its specific
diagnostic and stop rather than clearing broader directories or repeatedly retrying.

After installation succeeds, run Step 2's real HTML/Mermaid check. Until that probe succeeds,
retain Markdown presentation and report that HTML rendering remains unavailable.

## Ubuntu setup

Use Ubuntu's native PowerShell and Node.js, not Windows executables reached through WSL interop.
Install approved missing prerequisites using the [PowerShell Ubuntu instructions](https://learn.microsoft.com/powershell/scripting/install/install-ubuntu)
and [Node.js LTS download instructions](https://nodejs.org/en/download).
nvm discovery uses the active runtime or configured default; it does
not install or switch versions. Ensure npm is available with that runtime.

Bootstrap browser extraction and HTTPS trust before the browser download:

```powershell
sudo -n apt-get update
sudo -n apt-get install --yes --no-install-recommends ca-certificates unzip
```

There are no placeholders. Inspect each exit status and stop on failure. `sudo -n` must have
authorized noninteractive access; if it cannot proceed, ask the user to arrange installation.
Do not start an interactive password prompt inside an agent command.

Check the available Pandoc before choosing its installation source:

```powershell
apt-cache policy pandoc
```

There are no placeholders. Read the installed and candidate versions. If the repository provides
a suitable version, install it after permission:

```powershell
sudo -n apt-get install --yes pandoc
```

There are no placeholders. Check its exit status. Otherwise obtain an approved official Pandoc
release package from the [Pandoc installation guide](https://pandoc.org/installing.html), verify
its release digest, and install the downloaded Ubuntu-compatible package:

```powershell
sudo -n apt-get install --yes "{{PANDOC_DEB}}"
```

| Placeholder | Description |
| --- | --- |
| `PANDOC_DEB` | Absolute path of the verified official `.deb` for the host architecture; prefer the installer's pinned Pandoc release. |

Check the exit status. Merely installing Ubuntu's `pandoc` package does not prove that it supports
the renderer's options.

Install the user-wide JavaScript packages, browser, and its system dependencies together:

```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Install-ReviewReportTools.ps1" `
  -InstallBrowserDependencies
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Absolute path of the active skill inside the Ubuntu environment. |

Run this as the review user, not through `sudo pwsh` or `sudo npm`. Only apt operations elevate.
The switch reads `deb.deps` beside the downloaded Chrome executable, refreshes apt indexes,
uses `apt-get satisfy` for that exact browser build's dependency relations, and installs
Liberation fonts for diagram text. This follows Puppeteer's system-dependency mechanism without
moving the browser cache into root's home. Missing or empty dependency metadata is an error,
not permission to skip system dependencies.

The dependency file is the authoritative package list, including minimum versions and
alternatives. It covers NSS/NSPR (`libnss3`, `libnspr4`), C/C++ runtimes, accessibility, audio,
printing, D-Bus, graphics, font/text layout, and X11 libraries. Ubuntu releases rename some ABI
packages, for example `libasound2t64` on newer releases. Let apt resolve the browser's relations;
do not copy an old package list, install only the first missing `.so`, or install development
headers in place of runtime dependencies. Add fonts for other writing systems only if needed
for the report's text. Headless rendering does not require a desktop session or Xvfb.

The installer then runs the real HTML/Mermaid probe. If it still reports missing libraries,
inspect the selected binary's direct dependencies:

```powershell
ldd "{{BROWSER_PATH}}"
```

| Placeholder | Description |
| --- | --- |
| `BROWSER_PATH` | Absolute Chrome path reported by the check or its failure; use only the trusted installed browser. |

Read all `not found` entries and the full launch error, then repair the approved package setup.
A clean `ldd` result alone does not prove browser readiness. Root execution, containers, and
Ubuntu's AppArmor/user-namespace restrictions can prevent sandbox creation even when every
library exists. `No usable sandbox!` needs a host-policy diagnosis, not another browser download.
Request an administrator-approved sandbox configuration; never add `--no-sandbox`, disable
AppArmor, or change system-wide namespace restrictions as an automatic repair.

# Step 2: verify in the rendering environment

Execute `pwsh-calls/check-report-tools.md` again from the same environment and working directory
that will generate the report. Both installation and this check must succeed, with empty
`missing` and `diagnostics` arrays. The probe exercises Pandoc's actual options, Mermaid CLI,
Chrome startup, SVG generation, and self-contained HTML embedding. Do not substitute
`--version`, an executable-existence check, an npm message, or a diagram-free HTML document.

If setup fails or permission is declined, record the specific reason in the session's prose
checkpoint, report the Markdown fallback, and continue under the skill's existing fallback rules.
Do not repeatedly ask the same installation question or claim HTML readiness.

# Sources

- [Pinned Puppeteer system requirements](https://github.com/puppeteer/puppeteer/blob/puppeteer-v25.11.0/docs/guides/system-requirements.md): Node.js, platforms, and archive tools.
- [Pinned Puppeteer troubleshooting](https://github.com/puppeteer/puppeteer/blob/puppeteer-v25.11.0/docs/troubleshooting.md): Linux libraries, cache identity, Windows permissions, and Linux sandbox restrictions.
- [Pinned browser installer](https://github.com/puppeteer/puppeteer/blob/puppeteer-v25.11.0/packages/browsers/src/install.ts): `deb.deps`, apt dependency resolution, and Windows sandbox setup.
- [Pandoc option introduction](https://github.com/jgm/pandoc/blob/3.8/changelog.md): the renderer's `--syntax-highlighting` requirement.

Recheck these requirements against `tools/package-lock.json` when updating the renderer dependencies.
