#Requires -Version 7

<#
.SYNOPSIS
    Symlinks every skill in this repo into a Copilot skill directory.

.DESCRIPTION
    Creates one directory link per skill under skills/ pointing into a target
    skill directory recognised by Copilot. Existing links with the same name
    are replaced. Existing plain directories with the same name are left in
    place and reported as warnings — never deleted.

    On Windows the script uses NTFS directory junctions, which do not
    require Developer Mode or an elevated shell. On Linux and macOS the
    script uses symbolic links.

.PARAMETER Scope
    User (default): link into $HOME/.copilot/skills, making the skills
    available to all repos via Copilot CLI and any other client that reads
    that location.

    Workspace: link into .github/copilot/skills under -WorkspacePath
    (current working directory by default), scoping the skills to that repo
    (e.g. for GitHub Copilot in VS Code).

.PARAMETER WorkspacePath
    When -Scope is Workspace, the repo root to install the skills into.
    Defaults to the current working directory.

.EXAMPLE
    pwsh ./Install-CopilotSkills.ps1
    Links all skills into $HOME/.copilot/skills.

.EXAMPLE
    pwsh ./Install-CopilotSkills.ps1 -Scope Workspace
    Links all skills into ./.github/copilot/skills under the current dir.
#>

[CmdletBinding()]
param(
    [ValidateSet('User', 'Workspace')]
    [string]$Scope = 'User',

    [string]$WorkspacePath = (Get-Location).Path
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$skillsSource = Join-Path $PSScriptRoot 'skills'
if (-not (Test-Path -LiteralPath $skillsSource -PathType Container)) {
    throw "Skills source directory not found: $skillsSource"
}

switch ($Scope) {
    'User'      { $targetRoot = Join-Path $HOME '.copilot/skills' }
    'Workspace' { $targetRoot = Join-Path $WorkspacePath '.github/copilot/skills' }
}

if (-not (Test-Path -LiteralPath $targetRoot)) {
    New-Item -ItemType Directory -Path $targetRoot -Force | Out-Null
}

$linkType = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }

$skills = @(Get-ChildItem -LiteralPath $skillsSource -Directory)
$installed = 0
$skipped = 0

foreach ($skill in $skills) {
    $linkPath = Join-Path $targetRoot $skill.Name

    if (Test-Path -LiteralPath $linkPath) {
        $existing = Get-Item -LiteralPath $linkPath -Force
        $isReparse = $existing.Attributes.HasFlag([System.IO.FileAttributes]::ReparsePoint)
        if (-not $isReparse) {
            Write-Warning "Skipping '$($skill.Name)': '$linkPath' exists and is not a symlink/junction."
            $skipped++
            continue
        }
        Remove-Item -LiteralPath $linkPath -Force
    }

    New-Item -ItemType $linkType -Path $linkPath -Target $skill.FullName | Out-Null
    Write-Host "Linked $($skill.Name) -> $($skill.FullName)"
    $installed++
}

Write-Host ""
Write-Host "Installed $installed skill(s) into $targetRoot."
if ($skipped -gt 0) {
    Write-Host "Skipped $skipped existing non-link director(y/ies)."
}
