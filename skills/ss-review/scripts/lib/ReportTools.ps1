#Requires -Version 7.6

# Locates and invokes report tools without implicit installation or child-process prompts.
# The primary skill owns installation consent (docs/implementation.md, "Report tools").
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Pin the portable Pandoc installation so rendering does not depend on a moving download.
$script:ReportPandocVersion = '3.11'

# User-wide storage survives skill relinking and disposable review checkouts.
function Get-ReportToolsDirectory {
    $root = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { Join-Path $HOME '.local\share' }
    Join-Path $root 'ss-review\tools'
}

function Get-ReportPandocDirectory {
    Join-Path (Get-ReportToolsDirectory) "pandoc-$script:ReportPandocVersion"
}

# Lockfile-addressed installations let different skill versions coexist without replacing
# another session's renderer. Ref: docs/implementation.md, "Report tools".
function Get-ReportMermaidDirectory {
    param([Parameter(Mandatory)][string] $SkillRoot)

    $lockfile = Join-Path $SkillRoot 'tools\package-lock.json'
    $hash = (Get-FileHash -LiteralPath $lockfile -Algorithm SHA256).Hash.ToLowerInvariant()
    Join-Path (Get-ReportToolsDirectory) "mermaid-$hash"
}

# Reuse requires the renderer and browser setup entry points, not just a surviving directory.
# Puppeteer's manifest selects its runtime entry point so this check follows the pinned package.
function Test-ReportMermaidInstallation {
    param([Parameter(Mandatory)][string] $Directory)

    foreach ($relativePath in @(
        'node_modules\@mermaid-js\mermaid-cli\src\cli.js',
        'node_modules\puppeteer\install.mjs',
        'node_modules\puppeteer\package.json'
    )) {
        if (-not [IO.File]::Exists((Join-Path $Directory $relativePath))) { return $false }
    }
    $puppeteer = Join-Path $Directory 'node_modules\puppeteer'
    $manifestText = Get-Content -LiteralPath (Join-Path $puppeteer 'package.json') -Raw
    if ([string]::IsNullOrWhiteSpace($manifestText)) { return $false }

    try {
        $manifest = ConvertFrom-Json -InputObject $manifestText -AsHashtable -NoEnumerate -ErrorAction Stop
    } catch [ArgumentException] {
        # Invalid JSON means the cached package needs rebuilding. File-read failures stay
        # outside this catch so setup does not attempt to repair an access or storage error.
        return $false
    }

    if ($manifest -isnot [Collections.IDictionary] -or -not $manifest.Contains('main') -or
        $manifest['main'] -isnot [string] -or [string]::IsNullOrWhiteSpace($manifest['main'])) { return $false }
    [IO.File]::Exists((Join-Path $puppeteer $manifest['main']))
}

# Pandoc and Puppeteer's browser cache are shared by skill copies as well as sessions.
function Get-ReportToolsLockPath {
    Join-Path (Get-ReportToolsDirectory) 'installation'
}

# Ask nvm to resolve its aliases rather than duplicating its version-selection rules.
# Load only nvm itself, not shell profiles or the reviewed repository's .nvmrc.
function Get-ReportNvmNodePath {
    $directory = if ($env:NVM_DIR) { $env:NVM_DIR }
        elseif ($env:XDG_CONFIG_HOME) { Join-Path $env:XDG_CONFIG_HOME 'nvm' }
        else { Join-Path $HOME '.nvm' }
    if (-not [IO.File]::Exists((Join-Path $directory 'nvm.sh'))) { return $null }

    $shell = Get-Command sh -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $shell) { throw 'A POSIX shell (sh) is required to locate the default Node.js installation managed by nvm.' }

    # Positional arguments keep paths out of shell source. Set -- explicitly because not every
    # POSIX shell forwards arguments to dot-sourced files. --no-use prevents automatic activation.
    $nodePath = (Invoke-ReportProcess -InvokeProcess (Get-ReportProcessInvoker) -FilePath $shell.Source -ArgumentList @(
        '-c', 'export NVM_DIR="$1"; set -- --no-use; . "$NVM_DIR/nvm.sh" >&2 && nvm which default',
        'ss-review-node', $directory
    )).TrimEnd("`r", "`n")
    if (-not [IO.Path]::IsPathFullyQualified($nodePath) -or -not [IO.File]::Exists($nodePath)) {
        throw "nvm did not return an installed Node.js executable for its default alias: '$nodePath'."
    }
    $nodePath
}

# Preserve the caller's runtime choice before consulting nvm's active location or default.
# Ref: docs/implementation.md, "Report tools".
function Get-ReportNodePath {
    $node = Get-Command node -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($node) { return $node.Source }

    $directory = if ($IsWindows) { $env:NVM_SYMLINK } else { $env:NVM_BIN }
    if ($directory) {
        $name = if ($IsWindows) { 'node.exe' } else { 'node' }
        $nodePath = Join-Path $directory $name
        if ([IO.File]::Exists($nodePath)) { return $nodePath }
    }
    if (-not $IsWindows) { Get-ReportNvmNodePath }
}

function Get-ReviewReportTools {
    param(
        [Parameter(Mandatory)][string] $SkillRoot,
        # The installer already holds exclusive ownership while checking its own result.
        [switch] $InstallationLocked
    )

    $toolsLock = if (-not $InstallationLocked) { Open-PullRequestFileLock -Path (Get-ReportToolsLockPath) -Shared }
    try {
        $pandoc = Get-Command pandoc -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        $pandocPath = if ($pandoc) { $pandoc.Source } else {
            $name = if ($IsWindows) { 'pandoc.exe' } else { 'pandoc' }
            Join-Path (Get-ReportPandocDirectory) $name
        }
        $nodePath = Get-ReportNodePath
        $mermaidDirectory = Get-ReportMermaidDirectory -SkillRoot $SkillRoot
        $mermaid = Join-Path $mermaidDirectory 'node_modules\@mermaid-js\mermaid-cli\src\cli.js'
        $packagesReady = Test-ReportMermaidInstallation -Directory $mermaidDirectory
        $missing = [Collections.Generic.List[string]]::new()
        if (-not [IO.File]::Exists($pandocPath)) { $missing.Add('pandoc') }
        if (-not $nodePath) { $missing.Add('node') }
        if (-not $packagesReady) { $missing.Add('mermaid-cli') }
        $diagnostics = [Collections.Generic.List[string]]::new()
        $browserPath = $null
        if ($nodePath -and $packagesReady) {
            # Puppeteer owns the browser version and cache layout; ask it rather than guessing a path.
            $browserResult = & (Get-ReportProcessInvoker) -FilePath $nodePath -ArgumentList @(
                '-e', 'require(process.argv[1]).executablePath().then(p => process.stdout.write(p))',
                (Join-Path $mermaidDirectory 'node_modules\puppeteer')
            )
            if ($null -eq $browserResult) { throw 'Browser discovery returned no process result.' }
            if ($browserResult['exitCode'] -eq 0) {
                $browserPath = [Text.Encoding]::UTF8.GetString($browserResult['standardOutputBytes']).TrimEnd("`r", "`n")
            } else {
                $diagnostics.Add("Browser discovery failed (exit $($browserResult['exitCode'])): $($browserResult['standardErrorText'])")
            }
            if (-not $browserPath -or -not [IO.File]::Exists($browserPath)) {
                $missing.Add('mermaid-browser')
                if ($browserResult['exitCode'] -eq 0) {
                    $diagnostics.Add("Puppeteer's browser executable is absent: '$browserPath'.")
                }
            }
        }

        [ordered]@{
            pandoc = $pandocPath
            node = $nodePath
            mermaid = $mermaid
            browser = $browserPath
            missing = $missing.ToArray()
            diagnostics = $diagnostics.ToArray()
        }
    } finally { if ($null -ne $toolsLock) { $toolsLock.Dispose() } }
}

# The consent-gated installer uses the discovered runtime for both npm and Puppeteer.
# Temporarily expose its directory so npm launchers and their children can also find Node.js.
function Install-ReviewMermaidTools {
    param(
        [Parameter(Mandatory)][string] $NodePath,
        [Parameter(Mandatory)][string] $SkillRoot
    )

    $toolsDirectory = Get-ReportMermaidDirectory -SkillRoot $SkillRoot
    $staging = $null
    $installationDirectory = $toolsDirectory
    if (-not (Test-ReportMermaidInstallation -Directory $toolsDirectory)) {
        # Publish a complete dependency tree only after setup succeeds. The caller holds
        # the user-wide installation lock, so no reader can observe an unfinished tree.
        $staging = Join-Path (Get-ReportToolsDirectory) ".mermaid-$([guid]::NewGuid().ToString('N'))"
        [IO.Directory]::CreateDirectory($staging) | Out-Null
        $installationDirectory = $staging
    }
    $originalPath = $env:PATH
    try {
        if ($staging) {
            foreach ($name in @('package.json', 'package-lock.json')) {
                Copy-Item -LiteralPath (Join-Path $SkillRoot "tools\$name") -Destination (Join-Path $staging $name)
            }
            # The tools lock does not protect a checkout update or a relinked skill source.
            $copiedHash = (Get-FileHash -LiteralPath (Join-Path $staging 'package-lock.json') -Algorithm SHA256).Hash.ToLowerInvariant()
            if ((Split-Path -Leaf $toolsDirectory) -cne "mermaid-$copiedHash") {
                throw 'The skill dependency lockfile changed during setup. Rerun setup against a stable skill checkout.'
            }
        }
        $env:PATH = (Split-Path -Parent $NodePath) + [IO.Path]::PathSeparator + $originalPath
        if ($staging) {
            Push-Location -LiteralPath $installationDirectory
            try {
                & npm ci --ignore-scripts --engine-strict --no-audit --no-fund | ForEach-Object { [Console]::Error.WriteLine([string] $_) }
                if ($LASTEXITCODE -ne 0) { throw "Installing Mermaid CLI failed with exit code $LASTEXITCODE." }
            } finally {
                Pop-Location
            }
            if (-not (Test-ReportMermaidInstallation -Directory $installationDirectory)) {
                throw 'The npm installation did not provide the required Mermaid and Puppeteer package files.'
            }
        }
        # Browser configuration must resolve from the caller's rendering directory, not
        # shared storage. Only npm needs the staging working directory.
        & $NodePath (Join-Path $installationDirectory 'node_modules\puppeteer\install.mjs') |
            ForEach-Object { [Console]::Error.WriteLine([string] $_) }
        if ($LASTEXITCODE -ne 0) {
            throw "Installing Mermaid's browser failed with exit code $LASTEXITCODE. Preserve Puppeteer's diagnostic. For IncompleteInstallationError, follow report-tool-setup.md, 'Incomplete browser cache recovery'; cache deletion requires separate approval."
        }
        if ($staging) {
            # Keep the old tree until its replacement is ready, and restore it if publication
            # fails. The retained installation lock excludes readers throughout these renames.
            $previous = $null
            if ([IO.Directory]::Exists($toolsDirectory)) {
                $previous = Join-Path (Get-ReportToolsDirectory) ".mermaid-previous-$([guid]::NewGuid().ToString('N'))"
                [IO.Directory]::Move($toolsDirectory, $previous)
            }
            try {
                [IO.Directory]::Move($staging, $toolsDirectory)
            } catch {
                if ($previous) { [IO.Directory]::Move($previous, $toolsDirectory) }
                throw
            }
            if ($previous) { Remove-Item -LiteralPath $previous -Recurse -Force }
        }
    } finally {
        $env:PATH = $originalPath
        if ($staging -and [IO.Directory]::Exists($staging)) {
            Remove-Item -LiteralPath $staging -Recurse -Force
        }
    }
}

# Install the exact downloaded browser's Debian dependency relations without elevating
# npm or changing the review user's browser cache. The caller holds the installation lock.
# Ref: docs/implementation.md, "Report tools".
function Install-ReviewBrowserDependencies {
    param([Parameter(Mandatory)][string] $BrowserPath)

    if (-not $IsLinux) { throw 'Browser system dependency installation requires Ubuntu or Debian Linux.' }
    $apt = Get-Command apt-get -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    $sudo = Get-Command sudo -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $apt -or -not $sudo) {
        throw 'Browser system dependency installation requires apt-get and noninteractive sudo access. Ask the user to arrange system package installation.'
    }

    $dependencyPath = Join-Path (Split-Path -Parent $BrowserPath) 'deb.deps'
    if (-not [IO.File]::Exists($dependencyPath)) {
        throw "Chrome dependency metadata is missing: '$dependencyPath'. Use the skill's pinned Chrome for Testing installation."
    }
    $relations = @(
        (Read-PullRequestFileText -Path $dependencyPath -Encoding ([Text.Encoding]::UTF8)) -split '\r?\n' |
            ForEach-Object { $_.Trim() } | Where-Object { $_ }
    )
    if ($relations.Count -eq 0) { throw "Chrome dependency metadata is empty: '$dependencyPath'." }

    $invoke = Get-ReportProcessInvoker
    Invoke-ReportProcess -InvokeProcess $invoke -FilePath $sudo.Source -ArgumentList @('-n', $apt.Source, 'update') |
        ForEach-Object { [Console]::Error.WriteLine($_) }
    Invoke-ReportProcess -InvokeProcess $invoke -FilePath $sudo.Source `
        -ArgumentList @('-n', $apt.Source, 'satisfy', '--yes', '--no-install-recommends', ($relations -join ', ')) |
        ForEach-Object { [Console]::Error.WriteLine($_) }
    # Chrome's library relations do not ensure readable diagram text. Liberation provides
    # the Arial-compatible sans-serif metrics selected by the report renderer.
    Invoke-ReportProcess -InvokeProcess $invoke -FilePath $sudo.Source `
        -ArgumentList @('-n', $apt.Source, 'install', '--yes', '--no-install-recommends', 'fonts-liberation') |
        ForEach-Object { [Console]::Error.WriteLine($_) }
}

# Keep readiness failures explicit while distinguishing discovery consent from approved setup diagnosis.
function Assert-ReviewReportTools {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $Tools,
        # An approved installation has already run; diagnose its result instead of asking again.
        [switch] $InstallationAttempted
    )

    if ($Tools['missing'].Count -gt 0) {
        $detail = if ($Tools.Contains('diagnostics')) { $Tools['diagnostics'] -join "`n" } else { '' }
        $action = if ($InstallationAttempted) {
            "The approved installation did not provide all required tools. Preserve the installer output and diagnose the failure using report-tool-setup.md. For IncompleteInstallationError, follow 'Incomplete browser cache recovery'; cache deletion requires separate approval."
        } else {
            'Ask the user for installation permission before retrying HTML rendering.'
        }
        $error = [InvalidOperationException]::new("Missing report tools: $($Tools['missing'] -join ', '). $action`n$detail")
        $error.Data['missingTools'] = $Tools['missing']
        throw $error
    }
}

# Reuse the existing byte-faithful process host, including concurrent stream draining.
function Get-ReportProcessInvoker {
    {
        param([string] $FilePath, [string[]] $ArgumentList, [AllowNull()][string] $StandardInput)
        & (Get-GitHubCliInvoker -FileName $FilePath) -ArgumentList $ArgumentList -StandardInput $StandardInput
    }
}

function Invoke-ReportProcess {
    param(
        [Parameter(Mandatory)][scriptblock] $InvokeProcess,
        [Parameter(Mandatory)][string] $FilePath,
        [string[]] $ArgumentList = @(),
        [AllowNull()][string] $StandardInput
    )

    $result = & $InvokeProcess -FilePath $FilePath -ArgumentList $ArgumentList -StandardInput $StandardInput
    if ($null -eq $result -or $result['exitCode'] -ne 0) {
        $detail = if ($null -eq $result) { 'No process result.' } else { [string] $result['standardErrorText'] }
        throw "Report tool '$FilePath' failed: $detail"
    }
    [Text.Encoding]::UTF8.GetString($result['standardOutputBytes'])
}
