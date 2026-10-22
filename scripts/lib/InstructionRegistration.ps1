#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Opens the user registry boundary separately so tests never change real settings.
function Get-UserEnvironmentKey {
    [CmdletBinding(PositionalBinding = $false)]
    param()

    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if ($null -eq $key) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    }
    return $key
}

# Preserve unrelated entries and their registry expansion behavior during registration.
function Add-DirectoryToUserEnvironmentList {
    [CmdletBinding(PositionalBinding = $false)]
    param(
        [Parameter(Mandatory)] [string] $VariableName,
        [Parameter(Mandatory)] [string] $Directory,
        [string] $Separator = ';'
    )

    # Appends $Directory to a delimited, user-scoped environment
    # variable (PATH, COPILOT_CUSTOM_INSTRUCTIONS_DIRS, ...) without disturbing
    # the entries already there. Returns $true when the value was changed and
    # $false when $Directory was already present, so the caller knows whether a
    # WM_SETTINGCHANGE broadcast is warranted.
    #
    # Read and write the value directly through the registry rather than through
    # [Environment]::GetEnvironmentVariable/SetEnvironmentVariable. The .NET
    # helpers expand %VAR% references on read and always write a plain REG_SZ
    # value; that would bake literal paths into entries like %USERPROFILE%\...
    # and stop Windows from expanding any variable references the user keeps in
    # the value. Going through the registry with DoNotExpandEnvironmentNames and
    # preserving the REG_EXPAND_SZ value kind keeps both intact.
    $environmentKey = Get-UserEnvironmentKey

    try {
        $rawValue = $environmentKey.GetValue($VariableName, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($rawValue -isnot [string]) {
            throw "User environment variable '$VariableName' must contain a string; leaving malformed registration unchanged."
        }
        $currentValue = $rawValue
        Write-Verbose "Add-DirectoryToUserEnvironmentList: current user-scoped '$VariableName' is '$currentValue'."

        # Preserve the existing value kind so %VAR% references keep expanding;
        # default to REG_EXPAND_SZ when the variable does not exist yet.
        $valueKind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        foreach ($valueName in $environmentKey.GetValueNames()) {
            if ($valueName -ieq $VariableName) {
                $valueKind = $environmentKey.GetValueKind($VariableName)
                break
            }
        }
        Write-Verbose "Add-DirectoryToUserEnvironmentList: '$VariableName' value kind is '$valueKind'."

        $entries = [System.Collections.Generic.List[string]]::new()
        if (-not [string]::IsNullOrEmpty($currentValue)) {
            foreach ($entry in ($currentValue -split [regex]::Escape($Separator))) {
                if (-not [string]::IsNullOrEmpty($entry)) {
                    $entries.Add($entry)
                }
            }
        }

        # Compare without trailing separators so an existing entry is recognised
        # regardless of whether it was stored with a trailing backslash.
        $normalizedTarget = $Directory.TrimEnd('\')
        foreach ($entry in $entries) {
            if ($entry.TrimEnd('\') -ieq $normalizedTarget) {
                Write-Verbose "Add-DirectoryToUserEnvironmentList: '$Directory' already present in '$VariableName'; leaving it unchanged."
                return $false
            }
        }

        $entries.Add($Directory)
        $newValue = $entries -join $Separator
        Write-Verbose "Add-DirectoryToUserEnvironmentList: new '$VariableName' ($valueKind) is '$newValue'."
        $environmentKey.SetValue($VariableName, $newValue, $valueKind)
        return $true
    }
    finally {
        $environmentKey.Dispose()
    }
}

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

# Register the repo-shaped installed root, not the repository development guide.
function Add-AgentsDirectoryToCopilotInstructions {
    [CmdletBinding(PositionalBinding = $false)]
    param(
        [Parameter(Mandatory)][string] $AgentsDirectory,
        [string] $HomeDirectory = $HOME
    )

    $registration = Join-Path $HomeDirectory '.config\ss-ai\instruction-registration'
    [void][IO.Directory]::CreateDirectory((Split-Path -Parent $registration))
    $lock = Open-BashEnvironmentLock -Path $registration
    try {
        $changed = Add-DirectoryToUserEnvironmentList -VariableName 'COPILOT_CUSTOM_INSTRUCTIONS_DIRS' -Directory $AgentsDirectory -Separator ','
    } finally { $lock.Dispose() }
    if ($changed) { Send-EnvironmentChangeBroadcast }
}
