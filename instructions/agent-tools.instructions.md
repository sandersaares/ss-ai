# Execute commands in the correct shell

Applies to: agent workflows and shell commands issued through tools.

Before constructing a command, identify the shell that the tool actually runs.
Do not infer it from the operating system or from an executable mentioned inside
the command. A PowerShell tool parses the whole command as PowerShell before
starting Bash, WSL, or another PowerShell process.

Use the available file/search tools for simple reads and searches. Use shell
commands when you need shell behavior, native programs, or structured processing.
Keep each command focused; write substantial control flow as a formatted script
in session state rather than a densely nested one-liner.

# Establish prerequisites in each execution environment

Applies to: agent workflows invoking commands or scripts through tools.

Treat a new tool invocation as a fresh process unless the tool explicitly promises
otherwise. A previous invocation's `Set-Location`, environment changes, imported
modules, and functions do not establish the next invocation's state. Windows and
WSL also have separate executable installations and search paths.

If a command reports "not recognized" or "not found", check availability in the
environment that will execute it. Do not repeat the same unresolved invocation.
For example, discover Cargo instead of assuming its installation directory:

```powershell
$cargo = Get-Command cargo -CommandType Application -ErrorAction Stop
& $cargo.Source --version
if ($LASTEXITCODE -ne 0) {
    throw 'Cargo version check failed.'
}
```

If discovery fails, use an available equivalent, an explicitly verified executable
path, or report the missing prerequisite. Obtain permission before installing
additional development tools. Do not change the user's persistent `PATH` merely
to make an agent command work.

Discover files before using them; do not guess filenames or script parameters.
Use `Get-ChildItem`, `Test-Path -LiteralPath`, script help, or `Get-Command` as
appropriate. If help shows `-MapPath`, do not substitute `-Path`. When using
Pester's configuration type, import the module in the same invocation first:

```powershell
Import-Module Pester -ErrorAction Stop
$configuration = [PesterConfiguration]::Default
```

# Keep code and data out of nested shell parsing

Applies to: agent workflows passing commands or arguments across shell or launcher boundaries.

You are crossing a parsing boundary when invoking `sh -c`, `bash -lc`,
`pwsh -Command`, `cmd /c`, or a Windows `.cmd`/`.bat` launcher. Identify which
interpreter owns each variable, quote, and metacharacter before proceeding.
Backslash does not escape a PowerShell quote; Bash's `'\''` idiom is not a
PowerShell quoting mechanism.

Prefer direct execution and separate arguments. For a simple Linux command,
use `wsl.exe --distribution Ubuntu --exec uname -s` rather than wrapping it in
`bash -lc`. Locate Linux executables inside the selected distribution rather
than assuming Windows `Get-Command` can discover them.

For a substantial script, prefer a script file and `-File` with a path visible
to the target process. If inline PowerShell is necessary across WSL, use a
single-quoted here-string and `-EncodedCommand` to preserve the script literally:

```powershell
$linuxPowerShell = '/usr/bin/pwsh' # Example only; verify this path in the target distribution.
$scriptText = @'
$ErrorActionPreference = 'Stop'
Get-Module -ListAvailable Pester | Select-Object Name, Version
'@
$encoded = [Convert]::ToBase64String(
    [Text.Encoding]::Unicode.GetBytes($scriptText)
)
wsl.exe --distribution Ubuntu --exec $linuxPowerShell -NoProfile -NonInteractive -EncodedCommand $encoded
if ($LASTEXITCODE -ne 0) {
    throw 'Linux PowerShell command failed.'
}
```

`-EncodedCommand` expects Base64 of UTF-16LE text. Encoding preserves syntax;
it does not encrypt content. Do not embed secrets in command arguments.

If `$env:PATH` arrives as `:PATH`, or the child reports "A command must follow
-Command", stop adding escape characters and remove a parsing layer instead.

On Windows, URLs containing `&` passed through `.cmd` launchers can become
multiple commands even when they look like one PowerShell argument. Errors
claiming query keys such as `refName` or `api-version` are executable names
identify this situation. Prefer a native HTTP request over another shell layer:

```powershell
# The caller supplies the endpoint and provider-appropriate OAuth headers.
$response = Invoke-RestMethod -Uri $uri -Headers $headers -ErrorAction Stop
```

Use single-quoted strings for literal query text containing `$top` or `$skip`;
double-quoted PowerShell strings interpolate those names as variables.

# Use PowerShell expressions at pipeline boundaries

Applies to: agent workflows constructing or executing PowerShell commands.

An "empty pipe element" after `foreach (...) { ... }` means a language statement
was used where a pipeline expression is required. Collect its output, then pipe:

```powershell
$records = foreach ($item in $items) {
    [pscustomobject]@{ Name = $item.Name }
}
$records | ConvertTo-Json
```

An error treating `-join` as `RemainingScripts`, or `-ne` as a command parameter,
usually means an operator was placed inside command argument parsing. Complete
the pipeline in parentheses before applying the operator:

```powershell
$names = ($items | ForEach-Object { $_.Name }) -join '; '
$uniqueCount = @($items | Select-Object -ExpandProperty Name -Unique).Count
if ($uniqueCount -ne $items.Count) {
    throw 'Names must be unique.'
}
```

PowerShell does not implement Bash filename brace expansion. Instead of
`Get-Item .\src\{first,second}.rs`, pass an explicit path array:

```powershell
Get-Item -LiteralPath @('.\src\first.rs', '.\src\second.rs')
```

For substantial generated PowerShell, parse it without executing it before
running it. This catches mismatched braces and parentheses, and typos such as
`$_..Name`, without starting the intended operations:

```powershell
$tokens = $null
$parseErrors = $null
[void][System.Management.Automation.Language.Parser]::ParseInput(
    $scriptText, [ref]$tokens, [ref]$parseErrors
)
if ($parseErrors.Count -gt 0) {
    throw ($parseErrors.Message -join [Environment]::NewLine)
}
```

# Preserve failures before running dependent commands

Applies to: agent workflows executing commands through tools or consuming their results.

A tool reporting success may only mean that it delivered the command result.
A final exit code of zero may belong to a later successful command, not the
operation you needed. A still-running response is not a successful completion;
retrieve the final result using the tool's returned execution identifier.

Use terminating error handling for PowerShell commands. For native programs,
check `$LASTEXITCODE` immediately, before another native command can replace it,
or enable `$PSNativeCommandUseErrorActionPreference` with terminating errors.
Do not rely on `$ErrorActionPreference = 'Stop'` alone to handle native failures.
Interpret exit codes according to the program's contract: a search reporting no
matches is not necessarily a broken invocation.

Do not parse failed command output as data or continue dependent work. For
example, check a GitHub API call before converting its response from JSON:

```powershell
$ErrorActionPreference = 'Stop'
$raw = gh api repos/octocat/Hello-World
$apiExitCode = $LASTEXITCODE
if ($apiExitCode -ne 0) {
    throw "GitHub API request failed with exit code $apiExitCode."
}
$repository = $raw | ConvertFrom-Json
```

Likewise, stop after a failed rebase before issuing commands that assume it
completed. Do not append an unrelated successful `git status` or `git diff`
and treat its exit code as the result of the rebase. If you deliberately collect
diagnostics after failure, preserve and report the original failure explicitly.
