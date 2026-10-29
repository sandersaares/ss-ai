#Requires -Version 7.6
<#
.SYNOPSIS
    Installs ss-ai skills and their shared native dependency.
.DESCRIPTION
    Supports Windows and Ubuntu. Requires Rustup/Cargo and native build tools.
    Builds locked local source and verifies the executable on PATH before linking
    skills. Removes recognized legacy ss-ai instruction registration, but does not
    install instructions, configure Copilot interaction settings, or authenticate
    provider tools. Run separately in Windows and each Linux user environment.
.PARAMETER HomeDirectory
    User home containing .copilot and .config. Defaults to the current user's home.
#>
[CmdletBinding(PositionalBinding = $false)]
param([string] $HomeDirectory = $HOME)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\scripts\lib\BashEnvironment.ps1"
. "$PSScriptRoot\scripts\lib\InstructionRegistration.ps1"
. "$PSScriptRoot\scripts\lib\NativeUtility.ps1"
. "$PSScriptRoot\scripts\lib\AgentInstallation.ps1"

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-AiInstallation -HomeDirectory $HomeDirectory
}
