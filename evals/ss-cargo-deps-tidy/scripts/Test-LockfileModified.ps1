#requires -Version 7.0
<#
.SYNOPSIS
    Program-grader script. Verifies the agent ran cargo (or otherwise
    refreshed the lockfile) by checking that workspace Cargo.lock either
    differs from the upstream cache or was created when the cache had
    none.

.DESCRIPTION
    Invoked by waza's `program` grader. Reads `WAZA_WORKSPACE_DIR` from
    the environment and the cache root from `-CacheDir`. Pass conditions:

      - Cache has Cargo.lock AND workspace Cargo.lock exists AND content
        differs (any difference is enough).
      - Cache has no Cargo.lock AND workspace Cargo.lock now exists.

    Failure modes: workspace Cargo.lock missing, or content matches the
    cache exactly (suggesting the agent never ran cargo).
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$CacheDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$workspace = $env:WAZA_WORKSPACE_DIR
if (-not $workspace) { Write-Error 'WAZA_WORKSPACE_DIR not set'; exit 2 }

$workspaceLock = Join-Path $workspace 'Cargo.lock'
$cacheLock     = Join-Path $CacheDir  'Cargo.lock'

if (-not (Test-Path $workspaceLock)) {
    [Console]::Error.WriteLine("Workspace Cargo.lock missing - agent likely did not run cargo")
    exit 1
}

if (Test-Path $cacheLock) {
    $upstreamHash  = (Get-FileHash -Algorithm SHA256 -Path $cacheLock).Hash
    $workspaceHash = (Get-FileHash -Algorithm SHA256 -Path $workspaceLock).Hash
    if ($upstreamHash -eq $workspaceHash) {
        [Console]::Error.WriteLine("Workspace Cargo.lock is byte-identical to upstream - lockfile not refreshed")
        exit 1
    }
    Write-Output "Cargo.lock differs from upstream (refreshed by cargo)."
} else {
    Write-Output "Cargo.lock created (upstream had none)."
}
exit 0
