#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# One link operation serves both installed instruction discovery and skill discovery.
# Preserve non-link content and make incomplete installation an explicit error.
function Set-AgentDirectoryLink {
    [CmdletBinding(PositionalBinding = $false)]
    param(
        [Parameter(Mandatory)][string] $Source,
        [Parameter(Mandatory)][string] $Path
    )

    if (-not (Test-Path -LiteralPath $Source -PathType Container)) {
        throw "Agent source directory not found: $Source"
    }
    $existing = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($null -ne $existing) {
        if (-not $existing.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
            throw "Agent installation collision: '$Path' is not a symlink or junction; preserve or relocate it before installation."
        }
        Remove-Item -LiteralPath $Path -Force
    }
    [void][IO.Directory]::CreateDirectory((Split-Path -Parent $Path))
    $linkType = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
    New-Item -ItemType $linkType -Path $Path -Target $Source | Out-Null
    Write-Host "Linked $(Split-Path -Leaf $Path) -> $Source" -ForegroundColor Yellow
}

# User-scoped skill links avoid exposing maintainer scripts on PATH.
function Install-CopilotSkills {
    [CmdletBinding(PositionalBinding = $false)]
    param(
        [string] $SkillsSource = (Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'skills'),
        [string] $TargetRoot = (Join-Path $HOME '.copilot\skills')
    )

    foreach ($skill in Get-ChildItem -LiteralPath $SkillsSource -Directory) {
        Set-AgentDirectoryLink -Source $skill.FullName -Path (Join-Path $TargetRoot $skill.Name)
    }
}

# Build failures must precede all instruction/skill activation, and settings stay client-owned.
function Invoke-AiInstallation {
    [CmdletBinding(PositionalBinding = $false)]
    param([string] $HomeDirectory = $HOME)

    if (-not ($IsWindows -or $IsLinux)) {
        throw 'Install.ps1 supports Windows and Linux (Ubuntu); PowerShell-only helpers also support macOS.'
    }
    $repository = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $agentsDirectory = Join-Path $HomeDirectory '.config\ss-ai\agents'
    if ($agentsDirectory.Contains(',')) {
        throw 'The instruction registration path must not contain a comma: it delimits the environment list.'
    }
    Install-SkillsUtility
    Set-AgentDirectoryLink -Source (Join-Path $repository 'instructions') -Path (Join-Path $agentsDirectory '.github\instructions')
    Install-CopilotSkills -TargetRoot (Join-Path $HomeDirectory '.copilot\skills')
    if ($IsWindows) {
        Add-AgentsDirectoryToCopilotInstructions -AgentsDirectory $agentsDirectory -HomeDirectory $HomeDirectory
    } else {
        Set-BashUserEnvironment -AgentsDirectory $agentsDirectory -HomeDirectory $HomeDirectory
    }
    Write-Host 'AI installation complete. Start a new shell and agent session.' -ForegroundColor Green
}
