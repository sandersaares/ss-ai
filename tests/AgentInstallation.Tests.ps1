BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    . (Join-Path $script:RepoRoot 'Install.ps1')
}

Describe 'Complete agent installation' {
    BeforeEach {
        Mock Remove-LegacyCopilotInstructionRegistration {}
        Mock Remove-LegacyBashEnvironment {}
    }

    It 'builds before activating skills and leaves client settings and instructions untouched' {
        $homeDirectory = Join-Path $TestDrive 'home with spaces, and comma'
        New-Item -ItemType Directory -Path (Join-Path $homeDirectory '.copilot') -Force | Out-Null
        $settings = Join-Path $homeDirectory '.copilot\settings.json'
        [IO.File]::WriteAllText($settings, '{"mouse":true,"unrelated":"preserve"}')
        $original = $env:COPILOT_CUSTOM_INSTRUCTIONS_DIRS
        $script:Ready = $false
        Mock Install-SkillsUtility { $script:Ready = $true }
        Mock Set-AgentDirectoryLink {
            $script:Ready | Should -BeTrue
        }

        foreach ($run in @('initial', 'repeat')) {
            Invoke-AiInstallation -HomeDirectory $homeDirectory 6>$null
        }

        Should -Invoke Set-AgentDirectoryLink -Times (2 * @(Get-ChildItem (Join-Path $script:RepoRoot 'skills') -Directory).Count) -Exactly
        Test-Path -LiteralPath (Join-Path $homeDirectory '.config\ss-ai\agents') | Should -BeFalse
        $env:COPILOT_CUSTOM_INSTRUCTIONS_DIRS | Should -BeExactly $original
        [IO.File]::ReadAllText($settings) | Should -BeExactly '{"mouse":true,"unrelated":"preserve"}'
        if ($IsWindows) {
            Should -Invoke Remove-LegacyCopilotInstructionRegistration -Times 2 -Exactly
        } else {
            Should -Invoke Remove-LegacyBashEnvironment -Times 2 -Exactly
        }
    }

    It 'leaves activation and legacy cleanup untouched when compilation fails' {
        Mock Install-SkillsUtility { throw 'compiler failed' }
        Mock Install-CopilotSkills {}

        { Invoke-AiInstallation -HomeDirectory $TestDrive } | Should -Throw '*compiler failed*'
        Should -Invoke Install-CopilotSkills -Times 0 -Exactly
        Should -Invoke Remove-LegacyCopilotInstructionRegistration -Times 0 -Exactly
        Should -Invoke Remove-LegacyBashEnvironment -Times 0 -Exactly
    }

    It 'leaves legacy cleanup untouched when a skill collision prevents activation' {
        Mock Install-SkillsUtility {}
        Mock Install-CopilotSkills { throw 'skill collision' }

        { Invoke-AiInstallation -HomeDirectory $TestDrive } | Should -Throw '*skill collision*'
        Should -Invoke Remove-LegacyCopilotInstructionRegistration -Times 0 -Exactly
        Should -Invoke Remove-LegacyBashEnvironment -Times 0 -Exactly
    }

    It 'removes only the obsolete instruction link for this checkout' -ForEach @(
        @{ Ownership = 'managed' }
        @{ Ownership = 'unrelated link' }
        @{ Ownership = 'plain directory' }
    ) {
        $homeDirectory = Join-Path $TestDrive $Ownership
        $link = Join-Path $homeDirectory '.config\ss-ai\agents\.github\instructions'
        [void][IO.Directory]::CreateDirectory((Split-Path -Parent $link))
        $kind = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        $source = Join-Path $script:RepoRoot 'instructions'
        if ($Ownership -eq 'plain directory') {
            [void][IO.Directory]::CreateDirectory($link)
            [IO.File]::WriteAllText((Join-Path $link 'keep'), 'preserve')
        } else {
            # The removed instruction source can already be absent during migration.
            if ($Ownership -eq 'unrelated link') {
                $source = Join-Path $TestDrive 'other instructions'
                [void][IO.Directory]::CreateDirectory($source)
            }
            if ($IsWindows -and -not (Test-Path -LiteralPath $source)) {
                # Junction creation needs an existing target; retarget to the absent path afterward.
                $existingSource = Join-Path $TestDrive 'former instructions'
                [void][IO.Directory]::CreateDirectory($existingSource)
                New-Item -ItemType $kind -Path $link -Target $existingSource | Out-Null
                Mock Get-Item { [pscustomobject]@{ Attributes = [IO.FileAttributes]::ReparsePoint; Target = $source } } -ParameterFilter { $LiteralPath -eq $link }
            } else {
                New-Item -ItemType $kind -Path $link -Target $source | Out-Null
            }
        }
        Mock Install-SkillsUtility {}
        Mock Install-CopilotSkills {}

        Invoke-AiInstallation -HomeDirectory $homeDirectory 6>$null

        if ($Ownership -eq 'managed') {
            Test-Path -LiteralPath $link | Should -BeFalse
        } elseif ($Ownership -eq 'plain directory') {
            [IO.File]::ReadAllText((Join-Path $link 'keep')) | Should -BeExactly 'preserve'
        } else {
            (Get-Item -LiteralPath $link -Force).Target | Should -BeExactly $source
        }
    }

    It 'replaces a dangling skill link without depending on bundled instructions' {
        $removed = Join-Path $TestDrive 'removed'
        New-Item -ItemType Directory -Path $removed | Out-Null
        $link = Join-Path $TestDrive 'skill'
        $kind = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        New-Item -ItemType $kind -Path $link -Target $removed | Out-Null
        Remove-Item -LiteralPath $removed
        $source = Join-Path $script:RepoRoot 'skills'

        Set-AgentDirectoryLink -Source $source -Path $link 6>$null

        (Get-Item -LiteralPath $link -Force).Target | Should -Be $source
    }
}

Describe 'Legacy instruction environment cleanup' {
    BeforeEach {
        $script:Key = [pscustomobject]@{
            Value = 'existing-one,,%USERPROFILE%\personal,C:\home\.config\ss-ai\agents,other,'
            Kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
            Writes = 0
        }
        $script:Key | Add-Member ScriptMethod GetValue { param($name, $default, $options) $this.Value }
        $script:Key | Add-Member ScriptMethod GetValueKind { param($name) $this.Kind }
        $script:Key | Add-Member ScriptMethod SetValue {
            param($name, $value, $kind)
            $this.Value = $value
            $this.Kind = $kind
            $this.Writes++
        }
        $script:Key | Add-Member ScriptMethod Dispose {}
        Mock Get-UserEnvironmentKey { $script:Key }
        Mock Send-EnvironmentChangeBroadcast {}
    }

    It 'removes only the exact legacy entry while preserving expansion and empty entries' {
        Remove-LegacyDirectoryFromUserEnvironment -Directory 'C:\home\.config\ss-ai\agents' | Should -BeTrue
        Remove-LegacyDirectoryFromUserEnvironment -Directory 'C:\home\.config\ss-ai\agents' | Should -BeFalse
        $script:Key.Value | Should -BeExactly 'existing-one,,%USERPROFILE%\personal,other,'
        $script:Key.Kind | Should -Be ([Microsoft.Win32.RegistryValueKind]::ExpandString)
        $script:Key.Writes | Should -Be 1
    }

    It 'leaves near matches unchanged' {
        $script:Key.Value = 'C:\HOME\.config\ss-ai\agents,C:\home\.config\ss-ai\agents\,C:\home\.config\ss-ai\agents-extra'
        Remove-LegacyDirectoryFromUserEnvironment -Directory 'C:\home\.config\ss-ai\agents' | Should -BeFalse
        $script:Key.Writes | Should -Be 0
    }

    It 'rejects malformed registration without writing' {
        $script:Key.Value = 123
        { Remove-LegacyDirectoryFromUserEnvironment -Directory 'test' } | Should -Throw '*malformed registration unchanged*'
        $script:Key.Writes | Should -Be 0
    }

    It 'does not create an absent user environment key' {
        Mock Get-UserEnvironmentKey { $null }
        Remove-LegacyDirectoryFromUserEnvironment -Directory 'test' | Should -BeFalse
    }

    It 'broadcasts only when cleanup changes persisted registration' {
        Remove-LegacyCopilotInstructionRegistration -AgentsDirectory 'C:\home\.config\ss-ai\agents' -HomeDirectory $TestDrive
        Remove-LegacyCopilotInstructionRegistration -AgentsDirectory 'C:\home\.config\ss-ai\agents' -HomeDirectory $TestDrive
        Should -Invoke Send-EnvironmentChangeBroadcast -Times 1 -Exactly
        Test-Path -LiteralPath (Join-Path $TestDrive '.config\ss-ai\instruction-registration.lock') | Should -BeTrue
    }
}
