#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Opens the cleanup boundary separately so tests never change real settings.
function Get-UserEnvironmentKey {
    [CmdletBinding(PositionalBinding = $false)]
    param()

    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    return $key
}

# Remove only the exact root written by ss-ai, preserving all other text and value kinds.
function Remove-LegacyDirectoryFromUserEnvironment {
    [CmdletBinding(PositionalBinding = $false)]
    param(
        [Parameter(Mandatory)] [string] $Directory
    )

    $VariableName = 'COPILOT_CUSTOM_INSTRUCTIONS_DIRS'
    $environmentKey = Get-UserEnvironmentKey
    if ($null -eq $environmentKey) { return $false }
    try {
        $rawValue = $environmentKey.GetValue($VariableName, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($rawValue -isnot [string]) {
            throw "User environment variable '$VariableName' must contain a string; leaving malformed registration unchanged."
        }
        $entries = @($rawValue -split ',')
        if ($entries -cnotcontains $Directory) { return $false }
        # Do not expand references or normalize unrelated entries during migration.
        $valueKind = $environmentKey.GetValueKind($VariableName)
        $newValue = @($entries | Where-Object { $_ -cne $Directory }) -join ','
        $environmentKey.SetValue($VariableName, $newValue, $valueKind)
        return $true
    }
    finally {
        $environmentKey.Dispose()
    }
}

# Notify applications after a persisted user-environment change.
function Send-EnvironmentChangeBroadcast {
    [CmdletBinding(PositionalBinding = $false)]
    param()

    Write-Verbose "Send-EnvironmentChangeBroadcast: broadcasting WM_SETTINGCHANGE so running processes refresh their environment..."
    if (-not ('SsAi.NativeMethods' -as [type])) {
        Add-Type -Namespace 'SsAi' -Name 'NativeMethods' -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Auto)]
public static extern System.IntPtr SendMessageTimeout(
    System.IntPtr hWnd,
    uint Msg,
    System.IntPtr wParam,
    string lParam,
    uint fuFlags,
    uint uTimeout,
    out System.IntPtr lpdwResult);
'@
    }

    $HWND_BROADCAST = [System.IntPtr]0xffff
    $WM_SETTINGCHANGE = [uint32]0x1A
    $SMTO_ABORTIFHUNG = [uint32]0x2
    $result = [System.IntPtr]::Zero
    [void][SsAi.NativeMethods]::SendMessageTimeout($HWND_BROADCAST, $WM_SETTINGCHANGE, [System.IntPtr]::Zero, 'Environment', $SMTO_ABORTIFHUNG, [uint32]5000, [ref]$result)
}

# Serialize cleanup with previous ss-ai installers using their retained lock.
function Remove-LegacyCopilotInstructionRegistration {
    [CmdletBinding(PositionalBinding = $false)]
    param(
        [Parameter(Mandatory)][string] $AgentsDirectory,
        [string] $HomeDirectory = $HOME
    )

    $registration = Join-Path $HomeDirectory '.config\ss-ai\instruction-registration'
    [void][IO.Directory]::CreateDirectory((Split-Path -Parent $registration))
    $lock = Open-BashEnvironmentLock -Path $registration
    try {
        $changed = Remove-LegacyDirectoryFromUserEnvironment -Directory $AgentsDirectory
    } finally { $lock.Dispose() }
    if ($changed) { Send-EnvironmentChangeBroadcast }
}
