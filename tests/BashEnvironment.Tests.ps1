BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    . (Join-Path $script:RepoRoot 'Install.ps1')
    $script:Loader = "# Load the repository's per-user AI environment.`n" +
        '[ ! -f "$HOME/.config/ss-ai/environment.sh" ] || . "$HOME/.config/ss-ai/environment.sh"' + "`n"
}

Describe 'Legacy Bash instruction cleanup' {
    BeforeEach {
        $script:HomeDirectory = Join-Path $TestDrive ([guid]::NewGuid())
        [void][IO.Directory]::CreateDirectory($script:HomeDirectory)
        $script:EnvironmentPath = Join-Path $script:HomeDirectory '.config\ss-ai\environment.sh'
        $script:ProfilePath = Join-Path $script:HomeDirectory '.profile'
        $script:BashrcPath = Join-Path $script:HomeDirectory '.bashrc'
        $script:AgentsDirectory = Join-Path $script:HomeDirectory '.config\ss-ai\agents'
        $script:SetupParameters = @{
            HomeDirectory = $script:HomeDirectory
            AgentsDirectory = $script:AgentsDirectory
        }
        [void][IO.Directory]::CreateDirectory((Split-Path -Parent $script:EnvironmentPath))
    }

    It 'removes recognized legacy exports and loaders from all existing startup files idempotently' {
        [IO.File]::WriteAllText($script:EnvironmentPath, (Get-LegacyBashEnvironmentContent -AgentsDirectory $script:AgentsDirectory))
        foreach ($name in @('.profile', '.bashrc', '.bash_profile', '.bash_login')) {
            [IO.File]::WriteAllText((Join-Path $script:HomeDirectory $name), ($script:Loader + "export KEEP=yes`n"))
        }

        Remove-LegacyBashEnvironment @script:SetupParameters
        Remove-LegacyBashEnvironment @script:SetupParameters

        foreach ($name in @('.profile', '.bashrc', '.bash_profile', '.bash_login')) {
            Read-BashConfigurationText -Path (Join-Path $script:HomeDirectory $name) | Should -BeExactly "export KEEP=yes`n"
        }
        Test-Path -LiteralPath $script:EnvironmentPath | Should -BeFalse
        Test-Path -LiteralPath "$script:EnvironmentPath.lock" | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $script:HomeDirectory '.config\shell-startup.lock') | Should -BeTrue
    }

    It 'does not create startup files or instruction exports for a fresh installation' {
        Remove-LegacyBashEnvironment @script:SetupParameters
        foreach ($name in @('.profile', '.bashrc', '.bash_profile', '.bash_login', '.config\ss-ai\environment.sh')) {
            Test-Path -LiteralPath (Join-Path $script:HomeDirectory $name) | Should -BeFalse
        }
    }

    It 'clears loader-only startup files without rejecting empty content' {
        [IO.File]::WriteAllText($script:EnvironmentPath, (Get-LegacyBashEnvironmentContent -AgentsDirectory $script:AgentsDirectory))
        foreach ($name in @('.profile', '.bashrc', '.bash_profile', '.bash_login')) {
            [IO.File]::WriteAllText((Join-Path $script:HomeDirectory $name), $script:Loader)
        }

        Remove-LegacyBashEnvironment @script:SetupParameters
        Remove-LegacyBashEnvironment @script:SetupParameters

        foreach ($name in @('.profile', '.bashrc', '.bash_profile', '.bash_login')) {
            Read-BashConfigurationText -Path (Join-Path $script:HomeDirectory $name) | Should -BeExactly ''
        }
        Test-Path -LiteralPath $script:EnvironmentPath | Should -BeFalse
    }

    It 'removes a recognized loader with a missing environment and preserves line endings' {
        $original = "export KEEP=yes`r`n" + $script:Loader.Replace("`n", "`r`n") + "export ALSO_KEEP=yes`r`n"
        [IO.File]::WriteAllText($script:ProfilePath, $original)

        Remove-LegacyBashEnvironment @script:SetupParameters

        Read-BashConfigurationText -Path $script:ProfilePath | Should -BeExactly "export KEEP=yes`r`nexport ALSO_KEEP=yes`r`n"
    }

    It 'preserves user exports and their loader while removing only the managed block' {
        $userContent = "export COPILOT_CUSTOM_INSTRUCTIONS_DIRS='personal,other'`nexport KEEP=yes`n"
        [IO.File]::WriteAllText($script:EnvironmentPath,
            ((Get-LegacyBashEnvironmentContent -AgentsDirectory $script:AgentsDirectory) + $userContent))
        [IO.File]::WriteAllText($script:ProfilePath, $script:Loader)

        Remove-LegacyBashEnvironment @script:SetupParameters
        Remove-LegacyBashEnvironment @script:SetupParameters

        Read-BashConfigurationText -Path $script:EnvironmentPath | Should -BeExactly $userContent
        Read-BashConfigurationText -Path $script:ProfilePath | Should -BeExactly $script:Loader
    }

    It 'preserves unrecognized environment content and startup references' -ForEach @(
        @{ Content = "export COPILOT_CUSTOM_INSTRUCTIONS_DIRS='personal'`n" }
        @{ Content = "# user managed`n" }
        @{ Content = '' }
    ) {
        [IO.File]::WriteAllText($script:EnvironmentPath, $Content)
        [IO.File]::WriteAllText($script:ProfilePath, $script:Loader)

        Remove-LegacyBashEnvironment @script:SetupParameters

        Read-BashConfigurationText -Path $script:EnvironmentPath | Should -BeExactly $Content
        Read-BashConfigurationText -Path $script:ProfilePath | Should -BeExactly $script:Loader
    }

    It 'does not mistake another root or a similar loader for managed content' {
        $content = Get-LegacyBashEnvironmentContent -AgentsDirectory '/another/agents'
        $loader = $script:Loader.Replace('ss-ai/environment.sh', 'other/environment.sh')
        [IO.File]::WriteAllText($script:EnvironmentPath, $content)
        [IO.File]::WriteAllText($script:ProfilePath, $loader)

        Remove-LegacyBashEnvironment @script:SetupParameters

        Read-BashConfigurationText -Path $script:EnvironmentPath | Should -BeExactly $content
        Read-BashConfigurationText -Path $script:ProfilePath | Should -BeExactly $loader
    }

    It 'publishes complete startup files while readers retain the old version' {
        [IO.File]::WriteAllText($script:ProfilePath, ($script:Loader + "preserved`n"))
        $stream = [IO.File]::Open($script:ProfilePath, [IO.FileMode]::Open, [IO.FileAccess]::Read,
            [IO.FileShare]::Read -bor [IO.FileShare]::Delete)
        try {
            Remove-LegacyBashEnvironment @script:SetupParameters
            $reader = [IO.StreamReader]::new($stream)
            try { $reader.ReadToEnd() | Should -BeExactly ($script:Loader + "preserved`n") } finally { $reader.Dispose() }
            Read-BashConfigurationText -Path $script:ProfilePath | Should -BeExactly "preserved`n"
        } finally { $stream.Dispose() }
    }

    It 'does not retry unrelated I/O failures' {
        Mock Start-Sleep { throw 'Unexpected retry.' }

        { Open-BashEnvironmentLock -Path (Join-Path $script:HomeDirectory 'missing\environment.sh') } | Should -Throw

        Should -Invoke Start-Sleep -Times 0 -Exactly
    }

    It 'preserves symlinked profiles and their permissions' -Skip:(-not $IsLinux) {
        $target = Join-Path $script:HomeDirectory 'real-profile'
        [IO.File]::WriteAllText($target, ($script:Loader + "export KEEP=yes`n"))
        # A private dotfile must remain private after atomic replacement.
        $mode = [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite
        [IO.File]::SetUnixFileMode($target, $mode)
        New-Item -ItemType SymbolicLink -Path $script:ProfilePath -Target $target | Out-Null

        Remove-LegacyBashEnvironment @script:SetupParameters

        (Get-Item -LiteralPath $script:ProfilePath -Force).LinkTarget | Should -Be $target
        [IO.File]::GetUnixFileMode($target) | Should -Be $mode
        Read-BashConfigurationText -Path $target | Should -BeExactly "export KEEP=yes`n"
    }

    It 'preserves symlinked environment files instead of removing their targets' -Skip:(-not $IsLinux) {
        $target = Join-Path $script:HomeDirectory 'user environment'
        $content = Get-LegacyBashEnvironmentContent -AgentsDirectory $script:AgentsDirectory
        [IO.File]::WriteAllText($target, $content)
        New-Item -ItemType SymbolicLink -Path $script:EnvironmentPath -Target $target | Out-Null
        [IO.File]::WriteAllText($script:ProfilePath, $script:Loader)

        Remove-LegacyBashEnvironment @script:SetupParameters

        (Get-Item -LiteralPath $script:EnvironmentPath -Force).LinkTarget | Should -Be $target
        Read-BashConfigurationText -Path $target | Should -BeExactly $content
        Read-BashConfigurationText -Path $script:ProfilePath | Should -BeExactly $script:Loader
    }

    It 'leaves external roots unchanged in a fresh Bash session after migration' -Skip:(-not $IsLinux) {
        [IO.File]::WriteAllText($script:EnvironmentPath, (Get-LegacyBashEnvironmentContent -AgentsDirectory $script:AgentsDirectory))
        [IO.File]::WriteAllText($script:BashrcPath, ($script:Loader + "return`n"))
        Remove-LegacyBashEnvironment @script:SetupParameters
        $start = [Diagnostics.ProcessStartInfo]::new('/bin/bash')
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment['HOME'] = $script:HomeDirectory
        $start.Environment['COPILOT_CUSTOM_INSTRUCTIONS_DIRS'] = 'personal,external'
        foreach ($argument in @('--noprofile', '--norc', '-c', '. "$HOME/.bashrc"; printf "%s" "$COPILOT_CUSTOM_INSTRUCTIONS_DIRS"')) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $errors = $process.StandardError.ReadToEndAsync()
            $output = $process.StandardOutput.ReadToEnd()
            $process.WaitForExit()
            $process.ExitCode | Should -Be 0 -Because $errors.GetAwaiter().GetResult()
            $output | Should -BeExactly 'personal,external'
        } finally { $process.Dispose() }
    }

    It 'serializes overlapping cleanup without losing independent startup edits (shared participant: <IndependentParticipant>)' -ForEach @(
        @{ IndependentParticipant = $false }
        @{ IndependentParticipant = $true }
    ) {
        [IO.File]::WriteAllText($script:EnvironmentPath, (Get-LegacyBashEnvironmentContent -AgentsDirectory $script:AgentsDirectory))
        [IO.File]::WriteAllText($script:ProfilePath, ($script:Loader + "preserved profile`n"))
        [IO.File]::WriteAllText($script:BashrcPath, ($script:Loader + "preserved bashrc`n"))
        $lockPath = if ($IndependentParticipant) { Join-Path $script:HomeDirectory '.config\shell-startup' } else { $script:EnvironmentPath }
        $lock = Open-BashEnvironmentLock -Path $lockPath
        $processes = [Collections.Generic.List[object]]::new()
        try {
            foreach ($checkout in @('first', 'second')) {
                $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
                $start.UseShellExecute = $false
                $start.RedirectStandardInput = $true
                $start.RedirectStandardOutput = $true
                $start.RedirectStandardError = $true
                foreach ($argument in @('-NoProfile', '-NonInteractive', '-File',
                    (Join-Path $PSScriptRoot 'fixtures\Invoke-ContendedBashSetup.ps1'),
                    '-HomeDirectory', $script:HomeDirectory, '-CheckoutName', $checkout)) {
                    $start.ArgumentList.Add($argument)
                }
                if ($IndependentParticipant -and $checkout -eq 'second') { $start.ArgumentList.Add('-IndependentParticipant') }
                $process = [Diagnostics.Process]::Start($start)
                $processes.Add(@{ process = $process; errors = $process.StandardError.ReadToEndAsync() })
            }
            foreach ($item in $processes) {
                $item.process.StandardOutput.ReadLine() | Should -BeExactly 'lock-contended'
            }
            $lock.Dispose()
            # Admit retries individually to make overlap deterministic without clocks.
            foreach ($item in $processes) {
                $item.process.StandardInput.WriteLine('retry')
                $item.process.StandardInput.Close()
                $output = $item.process.StandardOutput.ReadToEnd()
                $item.process.WaitForExit()
                $item.process.ExitCode | Should -Be 0 -Because "$output $($item.errors.GetAwaiter().GetResult())"
            }
        } finally {
            $lock.Dispose()
            foreach ($item in $processes) { $item.process.StandardInput.Close() }
            foreach ($item in $processes) {
                $item.process.WaitForExit()
                $item.process.Dispose()
            }
        }

        $independentEntry = if ($IndependentParticipant) { "# independent startup entry`n" } else { '' }
        Read-BashConfigurationText -Path $script:ProfilePath | Should -BeExactly ("preserved profile`n" + $independentEntry)
        Read-BashConfigurationText -Path $script:BashrcPath | Should -BeExactly ("preserved bashrc`n" + $independentEntry)
        Test-Path -LiteralPath $script:EnvironmentPath | Should -BeFalse
    }
}
