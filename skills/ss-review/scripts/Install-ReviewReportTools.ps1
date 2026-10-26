#Requires -Version 7.6

# Run only after the primary orchestrator obtains explicit installation permission.
[CmdletBinding(PositionalBinding = $false)]
param(
    [switch] $InstallBrowserDependencies
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\PullRequestTransportHost.ps1"
. "$PSScriptRoot\lib\ReportTools.ps1"
. "$PSScriptRoot\lib\ReviewHtml.ps1"

if ($InstallBrowserDependencies -and -not $IsLinux) {
    throw '-InstallBrowserDependencies requires Ubuntu or Debian Linux.'
}

$toolsLock = Open-PullRequestFileLock -Path (Get-ReportToolsLockPath)
try {
    $skillRoot = Split-Path -Parent $PSScriptRoot
    $tools = Get-ReviewReportTools -SkillRoot $skillRoot -InstallationLocked
    if ($tools.missing -contains 'node') {
        throw 'Install a supported Node.js LTS release (at least 22.12.0) with npm, then rerun this installer.'
    }

    if ($tools.missing -contains 'pandoc') {
        if (-not $IsWindows -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
            throw "Install Pandoc $script:ReportPandocVersion (or a compatible release supporting --syntax-highlighting) from the official release or your platform package manager, then rerun this installer."
        }
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/jgm/pandoc/releases/tags/$script:ReportPandocVersion"
        $asset = @($release.assets | Where-Object name -CEQ "pandoc-$script:ReportPandocVersion-windows-x86_64.zip")
        if ($asset.Count -ne 1 -or $asset[0].digest -notmatch '^sha256:([0-9a-f]{64})$') {
            throw 'Pandoc release metadata did not identify a unique archive with a SHA-256 digest.'
        }
        $expectedHash = $Matches[1]
        $directory = Get-ReportPandocDirectory
        $staging = Join-Path ([IO.Path]::GetTempPath()) "ss-review-pandoc-$([guid]::NewGuid().ToString('N'))"
        [IO.Directory]::CreateDirectory($staging) | Out-Null
        try {
            $archive = Join-Path $staging 'pandoc.zip'
            Invoke-WebRequest -Uri $asset[0].browser_download_url -OutFile $archive
            if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ine $expectedHash) {
                throw 'The downloaded Pandoc archive does not match the release digest.'
            }
            Expand-Archive -LiteralPath $archive -DestinationPath (Join-Path $staging 'expanded')
            $executable = @(Get-ChildItem -LiteralPath (Join-Path $staging 'expanded') -Filter pandoc.exe -File -Recurse)
            if ($executable.Count -ne 1) { throw 'The Pandoc archive did not contain one executable.' }
            [IO.Directory]::CreateDirectory($directory) | Out-Null
            Copy-Item -LiteralPath $executable[0].FullName -Destination (Join-Path $directory 'pandoc.exe')
        } finally {
            Remove-Item -LiteralPath $staging -Recurse -Force
        }
    }

    Install-ReviewMermaidTools -NodePath $tools.node -SkillRoot $skillRoot
    $installed = Get-ReviewReportTools -SkillRoot $skillRoot -InstallationLocked
    Assert-ReviewReportTools -Tools $installed -InstallationAttempted
    if ($InstallBrowserDependencies) {
        Install-ReviewBrowserDependencies -BrowserPath $installed.browser
    }
    Assert-ReviewReportRendering -SkillRoot $skillRoot -Tools $installed -InstallationLocked
    $installed | ConvertTo-Json -Depth 8 -Compress
} finally { $toolsLock.Dispose() }
