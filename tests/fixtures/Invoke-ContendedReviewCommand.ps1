#Requires -Version 7.6

# Lets the parent release each contending reader without relying on scheduler timing.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $CommandName,
    [Parameter(Mandatory)][string] $ParametersJson
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$global:ReviewFileFixtureWaitCount = 0

# Replace only the wait: lock acquisition and the reader's entire data path stay real.
function Start-Sleep {
    param([int] $Milliseconds)
    $global:ReviewFileFixtureWaitCount++
    # The parent releases contenders individually after releasing its lock, so a second
    # wait signals a broken retry rather than a reason to block the test indefinitely.
    if ($global:ReviewFileFixtureWaitCount -gt 1) { throw 'Unexpected repeated fixture wait.' }
    [Console]::WriteLine('lock-contended')
    if ([Console]::ReadLine() -cne 'retry') { throw 'Fixture retry was not authorized.' }
}

$parameters = ConvertFrom-Json -InputObject $ParametersJson -AsHashtable
& (Join-Path $PSScriptRoot "..\..\skills\ss-review\scripts\$CommandName") @parameters
