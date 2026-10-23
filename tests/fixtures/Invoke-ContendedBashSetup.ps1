#Requires -Version 7.6

# The parent controls retries through pipes; neither success nor failure waits on a clock.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $HomeDirectory,
    [Parameter(Mandatory)][string] $CheckoutName,
    [switch] $IndependentParticipant
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\..\..\scripts\lib\BashEnvironment.ps1"
$script:WaitCount = 0

# Replace only the wait, leaving lock acquisition and file publication unchanged.
function Start-Sleep {
    param([int] $Milliseconds)

    $script:WaitCount++
    if ($script:WaitCount -gt 1) { throw 'Unexpected repeated fixture wait.' }
    [Console]::WriteLine('lock-contended')
    if ([Console]::ReadLine() -cne 'retry') { throw 'Fixture retry was not authorized.' }
}

if ($IndependentParticipant) {
    # Emulate the independent infrastructure installer's shared-file contract.
    # The fixture exercises the contract without requiring another installed tool.
    $privatePath = Join-Path $HomeDirectory '.config\infrastructure\environment.sh'
    [void][IO.Directory]::CreateDirectory((Split-Path -Parent $privatePath))
    $privateLock = Open-BashEnvironmentLock -Path $privatePath
    $sharedLock = $null
    try {
        $sharedLock = Open-BashEnvironmentLock -Path (Join-Path $HomeDirectory '.config\shell-startup')
        foreach ($name in @('.profile', '.bashrc')) {
            $path = Join-Path $HomeDirectory $name
            $existing = Read-BashConfigurationText -Path $path
            Write-BashConfigurationText -Path $path -Content ($existing + "# independent startup entry`n")
        }
    } finally {
        if ($null -ne $sharedLock) { $sharedLock.Dispose() }
        $privateLock.Dispose()
    }
} else {
    Set-BashUserEnvironment -HomeDirectory $HomeDirectory -AgentsDirectory "/$CheckoutName/agents" 6>$null
}
