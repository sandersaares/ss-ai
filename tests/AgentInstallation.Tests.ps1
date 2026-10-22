BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    . (Join-Path $script:RepoRoot 'Install.ps1')
}

Describe 'Complete agent installation' {
    It 'builds before activating instructions and skills and leaves client settings untouched' {
        $homeDirectory = Join-Path $TestDrive 'home with spaces'
        New-Item -ItemType Directory -Path (Join-Path $homeDirectory '.copilot') -Force | Out-Null
        $settings = Join-Path $homeDirectory '.copilot\settings.json'
        [IO.File]::WriteAllText($settings, '{"mouse":true,"unrelated":"preserve"}')
        $script:Ready = $false
        Mock Install-SkillsUtility { $script:Ready = $true }
        Mock Add-AgentsDirectoryToCopilotInstructions { $script:Ready | Should -BeTrue }
        Mock Set-BashUserEnvironment { $script:Ready | Should -BeTrue }

        foreach ($run in @('initial', 'repeat')) {
            Invoke-AiInstallation -HomeDirectory $homeDirectory 6>$null
        }

        $root = Join-Path $homeDirectory '.config\ss-ai\agents'
        $installed = Join-Path $root '.github\instructions'
        (Get-Item -LiteralPath $installed -Force).Target | Should -Be (Join-Path $script:RepoRoot 'instructions')
        $instructions = @(Get-ChildItem -LiteralPath (Join-Path $script:RepoRoot 'instructions') -File)
        @(Get-ChildItem -LiteralPath $installed -File).Count | Should -Be $instructions.Count
        foreach ($instruction in $instructions) {
            [IO.File]::ReadAllBytes((Join-Path $installed $instruction.Name)) |
                Should -Be ([IO.File]::ReadAllBytes($instruction.FullName))
        }
        foreach ($skill in Get-ChildItem -LiteralPath (Join-Path $script:RepoRoot 'skills') -Directory) {
            (Get-Item -LiteralPath (Join-Path $homeDirectory ".copilot\skills\$($skill.Name)") -Force).Target |
                Should -Be $skill.FullName
        }
        Test-Path -LiteralPath (Join-Path $root 'AGENTS.md') | Should -BeFalse
        [IO.File]::ReadAllText($settings) | Should -BeExactly '{"mouse":true,"unrelated":"preserve"}'
        if ($IsWindows) {
            Should -Invoke Add-AgentsDirectoryToCopilotInstructions -Times 2 -Exactly -ParameterFilter { $AgentsDirectory -eq $root }
        } else {
            Should -Invoke Set-BashUserEnvironment -Times 2 -Exactly -ParameterFilter { $AgentsDirectory -eq $root }
        }
    }

    It 'leaves activation untouched when compilation fails' {
        Mock Install-SkillsUtility { throw 'compiler failed' }
        Mock Set-AgentDirectoryLink {}
        Mock Install-CopilotSkills {}
        Mock Add-AgentsDirectoryToCopilotInstructions {}
        Mock Set-BashUserEnvironment {}

        { Invoke-AiInstallation -HomeDirectory $TestDrive } | Should -Throw '*compiler failed*'
        Should -Invoke Set-AgentDirectoryLink -Times 0 -Exactly
        Should -Invoke Install-CopilotSkills -Times 0 -Exactly
        Should -Invoke Add-AgentsDirectoryToCopilotInstructions -Times 0 -Exactly
        Should -Invoke Set-BashUserEnvironment -Times 0 -Exactly
    }

    It 'replaces a dangling link and preserves the canonical instruction source' {
        $removed = Join-Path $TestDrive 'removed'
        New-Item -ItemType Directory -Path $removed | Out-Null
        $link = Join-Path $TestDrive 'instructions'
        $kind = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        New-Item -ItemType $kind -Path $link -Target $removed | Out-Null
        Remove-Item -LiteralPath $removed
        Set-AgentDirectoryLink -Source (Join-Path $script:RepoRoot 'instructions') -Path $link 6>$null
        (Get-Item -LiteralPath $link -Force).Target | Should -Be (Join-Path $script:RepoRoot 'instructions')
    }

    It 'rejects malformed registration without writing to the user environment' {
        $key = [pscustomobject]@{}
        $key | Add-Member ScriptMethod GetValue { param($name, $default, $options) 123 }
        $key | Add-Member ScriptMethod Dispose {}
        Mock Get-UserEnvironmentKey { $key }
        { Add-DirectoryToUserEnvironmentList -VariableName 'COPILOT_CUSTOM_INSTRUCTIONS_DIRS' -Directory 'test' -Separator ',' } |
            Should -Throw '*malformed registration unchanged*'
    }

    It 'registers the installed root with the Copilot comma separator' {
        Mock Add-DirectoryToUserEnvironmentList { $false }
        Mock Send-EnvironmentChangeBroadcast { throw 'Unchanged environment must not broadcast.' }
        Add-AgentsDirectoryToCopilotInstructions -AgentsDirectory '/installed/agents' -HomeDirectory $TestDrive
        Should -Invoke Add-DirectoryToUserEnvironmentList -Times 1 -Exactly -ParameterFilter {
            $VariableName -ceq 'COPILOT_CUSTOM_INSTRUCTIONS_DIRS' -and $Directory -ceq '/installed/agents' -and $Separator -ceq ','
        }
    }

    It 'preserves unrelated registrations and expansion without duplicate entries' {
        $key = [pscustomobject]@{
            Value = 'existing-one,%USERPROFILE%\personal'
            Kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
            Writes = 0
        }
        $key | Add-Member ScriptMethod GetValue { param($name, $default, $options) $this.Value }
        $key | Add-Member ScriptMethod GetValueNames { 'COPILOT_CUSTOM_INSTRUCTIONS_DIRS' }
        $key | Add-Member ScriptMethod GetValueKind { param($name) $this.Kind }
        $key | Add-Member ScriptMethod SetValue {
            param($name, $value, $kind)
            $this.Value = $value
            $this.Kind = $kind
            $this.Writes++
        }
        $key | Add-Member ScriptMethod Dispose {}
        Mock Get-UserEnvironmentKey { $key }

        Add-DirectoryToUserEnvironmentList -VariableName 'COPILOT_CUSTOM_INSTRUCTIONS_DIRS' -Directory 'C:\AI checkout\agents' -Separator ',' |
            Should -BeTrue
        Add-DirectoryToUserEnvironmentList -VariableName 'COPILOT_CUSTOM_INSTRUCTIONS_DIRS' -Directory 'C:\AI checkout\agents' -Separator ',' |
            Should -BeFalse
        $key.Value | Should -BeExactly 'existing-one,%USERPROFILE%\personal,C:\AI checkout\agents'
        $key.Kind | Should -Be ([Microsoft.Win32.RegistryValueKind]::ExpandString)
        $key.Writes | Should -Be 1
    }
}
