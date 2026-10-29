#Requires -Version 7.6

# Snapshots the skill's built-in focus areas and the caller's configured instructions
# before cache lookup (docs/implementation.md, "Worker input boundary").
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $SessionStateDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\InstructionSources.ps1"

$session = [IO.Path]::GetFullPath($SessionStateDirectory)
[void][IO.Directory]::CreateDirectory($session)
$lock = Open-PullRequestFileLock -Path (Join-Path $session 'focus-area-cache-operation')
try {
    Copy-ReviewInstructionSources -SessionStateDirectory $session `
        -SkillRoot (Split-Path -Parent $PSScriptRoot) `
        -DirectoryList ([Environment]::GetEnvironmentVariable('COPILOT_CUSTOM_INSTRUCTIONS_DIRS')) `
        -BaseDirectory (Get-Location).ProviderPath |
        ConvertTo-Json -Depth 5 -Compress
} finally { $lock.Dispose() }
