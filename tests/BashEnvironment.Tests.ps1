BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:ConfigurePc = Join-Path $script:RepoRoot 'Install.ps1'
    # A PATH may carry several git.exe (Git for Windows ships bin, cmd and mingw64); take the first.
    $script:GitExecutable = (Get-Command git -CommandType Application | Select-Object -First 1).Source
    . $script:ConfigurePc

    function Invoke-TestGit {
        param([Parameter(Mandatory)] [string[]] $Arguments)

        $output = & $script:GitExecutable @Arguments 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw "git $($Arguments -join ' ') failed: $($output -join [Environment]::NewLine)"
        }
        return $output
    }

    # Runs actual Linux shells and setup in a disposable home, without inheriting
    # personal Git, Copilot, or shell configuration from the test runner.
    function Invoke-PcTestProcess {
        param(
            [string] $Executable,
            [string[]] $Arguments,
            [string] $HomeDirectory,
            [string] $Instructions = 'existing-one,existing-two'
        )

        $start = [Diagnostics.ProcessStartInfo]::new($Executable)
        $start.UseShellExecute = $false
        $start.WorkingDirectory = $HomeDirectory
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment.Clear()
        $start.Environment['HOME'] = $HomeDirectory
        $start.Environment['PATH'] = '/usr/bin:/bin'
        $start.Environment['GIT_CONFIG_GLOBAL'] = Join-Path $HomeDirectory '.gitconfig'
        $start.Environment['GIT_CONFIG_NOSYSTEM'] = '1'
        $start.Environment['COPILOT_CUSTOM_INSTRUCTIONS_DIRS'] = $Instructions
        foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $errors = $process.StandardError.ReadToEndAsync()
            $output = $process.StandardOutput.ReadToEnd()
            $process.WaitForExit()
            $errorText = $errors.GetAwaiter().GetResult()
            if ($process.ExitCode -ne 0) { throw "$Executable failed: $errorText $output" }
            return $output.TrimEnd("`r", "`n")
        } finally { $process.Dispose() }
    }
}
Describe 'Bash environment persistence' {
    BeforeEach {
        $script:HomeDirectory = Join-Path $TestDrive ([guid]::NewGuid())
        New-Item -ItemType Directory -Path $script:HomeDirectory | Out-Null
        $script:EnvironmentPath = Join-Path $script:HomeDirectory '.config\ss-ai\environment.sh'
        $script:ProfilePath = Join-Path $script:HomeDirectory '.profile'
        $script:BashrcPath = Join-Path $script:HomeDirectory '.bashrc'
        $script:SetupParameters = @{
            HomeDirectory = $script:HomeDirectory
            AgentsDirectory = '/checkout/agents'
        }
    }

    It 'preserves startup content and installs the environment only once' {
        $profile = "export EXISTING_PROFILE=yes`n"
        $bashrc = "return # noninteractive shell`n"
        [IO.File]::WriteAllText($script:ProfilePath, $profile)
        [IO.File]::WriteAllText($script:BashrcPath, $bashrc)

        Set-BashUserEnvironment @script:SetupParameters 6>$null
        $firstProfile = Read-BashConfigurationText -Path $script:ProfilePath
        $firstBashrc = Read-BashConfigurationText -Path $script:BashrcPath
        $firstEnvironment = Read-BashConfigurationText -Path $script:EnvironmentPath
        Set-BashUserEnvironment @script:SetupParameters 6>$null

        (Read-BashConfigurationText -Path $script:ProfilePath) | Should -BeExactly $firstProfile
        (Read-BashConfigurationText -Path $script:BashrcPath) | Should -BeExactly $firstBashrc
        (Read-BashConfigurationText -Path $script:EnvironmentPath) | Should -BeExactly $firstEnvironment
        $firstProfile.StartsWith($profile) | Should -BeTrue
        $firstBashrc.EndsWith($bashrc) | Should -BeTrue
        $firstEnvironment | Should -Not -Match "`r"
        Test-Path -LiteralPath "$script:EnvironmentPath.lock" | Should -BeTrue
    }

    It 'wires the active Bash login profile without creating a higher-precedence one' -ForEach @(
        @{ LoginProfile = '.bash_profile' }
        @{ LoginProfile = '.bash_login' }
    ) {
        $loginPath = Join-Path $script:HomeDirectory $LoginProfile
        [IO.File]::WriteAllText($loginPath, "export EXISTING_LOGIN=yes`n")

        Set-BashUserEnvironment @script:SetupParameters 6>$null

        (Read-BashConfigurationText -Path $loginPath) | Should -Match 'EXISTING_LOGIN=yes'
        (Read-BashConfigurationText -Path $loginPath) | Should -Match 'ss-ai/environment.sh'
        if ($LoginProfile -eq '.bash_login') {
            Test-Path -LiteralPath (Join-Path $script:HomeDirectory '.bash_profile') | Should -BeFalse
        }
    }

    It 'rejects paths that cannot be represented in the environment lists' -ForEach @(
        @{ Scripts = '/checkout/scripts'; Agents = '/comma,path/agents' }
    ) {
        { Get-BashEnvironmentContent -AgentsDirectory $Agents } | Should -Throw '*delimit*'
    }

    It 'publishes complete files while a reader still has the original file open' {
        [IO.File]::WriteAllText($script:ProfilePath, "preserved`n")
        $stream = [IO.File]::Open($script:ProfilePath, [IO.FileMode]::Open, [IO.FileAccess]::Read,
            [IO.FileShare]::Read -bor [IO.FileShare]::Delete)
        try {
            Set-BashUserEnvironment @script:SetupParameters 6>$null
            $reader = [IO.StreamReader]::new($stream)
            try { $reader.ReadToEnd() | Should -BeExactly "preserved`n" } finally { $reader.Dispose() }
            (Read-BashConfigurationText -Path $script:ProfilePath) | Should -Match 'ss-ai/environment.sh'
        } finally { $stream.Dispose() }
    }

    It 'does not retry unrelated I/O failures' {
        Mock Start-Sleep { throw 'Unexpected retry.' }

        { Open-BashEnvironmentLock -Path (Join-Path $script:HomeDirectory 'missing\environment.sh') } | Should -Throw

        Should -Invoke Start-Sleep -Times 0 -Exactly
    }

    It 'preserves symlinked profiles and their permissions' -Skip:(-not $IsLinux) {
        $target = Join-Path $script:HomeDirectory 'real-profile'
        [IO.File]::WriteAllText($target, "export KEEP=yes`n")
        # A private dotfile must remain private after atomic replacement.
        $mode = [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite
        [IO.File]::SetUnixFileMode($target, $mode)
        New-Item -ItemType SymbolicLink -Path $script:ProfilePath -Target $target | Out-Null

        Set-BashUserEnvironment @script:SetupParameters 6>$null

        (Get-Item -LiteralPath $script:ProfilePath -Force).LinkTarget | Should -Be $target
        [IO.File]::GetUnixFileMode($target) | Should -Be $mode
        (Read-BashConfigurationText -Path $target) | Should -Match 'export KEEP=yes'
        (Read-BashConfigurationText -Path $target) | Should -Match 'ss-ai/environment.sh'
    }

    It 'loads literal paths and preserves existing lists across repeated Bash sourcing' -Skip:(-not $IsLinux) {
        # Quotes, spaces and pattern/shell syntax must remain path characters.
        $script:SetupParameters.AgentsDirectory = '/checkout with ''quotes'' $dollar [glob]/agents'
        [IO.File]::WriteAllText($script:BashrcPath, "return`n")
        Set-BashUserEnvironment @script:SetupParameters 6>$null
        $command = @'
set -eu
. "$HOME/.bashrc"
. "$HOME/.profile"
. "$HOME/.bashrc"
printf '%s\n' "${PATH-}" "$COPILOT_CUSTOM_INSTRUCTIONS_DIRS"
'@
        $output = Invoke-PcTestProcess -Executable '/bin/bash' -HomeDirectory $script:HomeDirectory -Arguments @('--noprofile', '--norc', '-c', $command.Replace("`r`n", "`n"))

        $output -split "`n" | Should -Be @(
            '/usr/bin:/bin'
            "existing-one,existing-two,$($script:SetupParameters.AgentsDirectory)"
        )
    }

    It 'does not create empty entries when the variables are unset' -Skip:(-not $IsLinux) {
        Set-BashUserEnvironment @script:SetupParameters 6>$null
        $command = @'
set -eu
unset PATH COPILOT_CUSTOM_INSTRUCTIONS_DIRS
. "$HOME/.config/ss-ai/environment.sh"
. "$HOME/.config/ss-ai/environment.sh"
printf '%s\n' "${PATH-}" "$COPILOT_CUSTOM_INSTRUCTIONS_DIRS"
'@
        $output = Invoke-PcTestProcess -Executable '/bin/bash' -HomeDirectory $script:HomeDirectory -Arguments @('--noprofile', '--norc', '-c', $command.Replace("`r`n", "`n"))

        $output -split "`n" | Should -Be @('', '/checkout/agents')
    }

    It 'exports shell-local instructions without changing the PATH export state' -Skip:(-not $IsLinux) {
        Set-BashUserEnvironment @script:SetupParameters 6>$null
        $command = @'
set -eu
PATH='/usr/bin:/checkout/scripts:/bin'
COPILOT_CUSTOM_INSTRUCTIONS_DIRS='existing-one,/checkout/agents,existing-two'
export -n PATH COPILOT_CUSTOM_INSTRUCTIONS_DIRS
. "$HOME/.config/ss-ai/environment.sh"
printf '%s\n' "$PATH"
/usr/bin/env
'@
        $output = Invoke-PcTestProcess -Executable '/bin/bash' -HomeDirectory $script:HomeDirectory -Arguments @('--noprofile', '--norc', '-c', $command.Replace("`r`n", "`n"))

        $lines = $output -split "`n"
        $lines[0] | Should -BeExactly '/usr/bin:/checkout/scripts:/bin'
        $lines | Should -Contain 'COPILOT_CUSTOM_INSTRUCTIONS_DIRS=existing-one,/checkout/agents,existing-two'
        # PATH belongs to machine setup, so AI setup must leave even its export state alone.
        @($lines | Where-Object { $_.StartsWith('PATH=') }) | Should -BeNullOrEmpty
    }

    It 'loads the environment automatically in a fresh <Mode> Bash session' -Skip:(-not $IsLinux) -ForEach @(
        @{ Mode = 'login'; ShellArguments = @('--login', '-c'); LoginProfile = '.profile' }
        @{ Mode = 'custom login'; ShellArguments = @('--login', '-c'); LoginProfile = '.bash_profile' }
        @{ Mode = 'alternate login'; ShellArguments = @('--login', '-c'); LoginProfile = '.bash_login' }
        @{ Mode = 'interactive'; ShellArguments = @('--noprofile', '-i', '-c'); LoginProfile = '.profile' }
    ) {
        [IO.File]::WriteAllText((Join-Path $script:HomeDirectory $LoginProfile), "export KEEP=yes`n")
        Set-BashUserEnvironment @script:SetupParameters 6>$null
        $output = Invoke-PcTestProcess -Executable '/bin/bash' -HomeDirectory $script:HomeDirectory -Arguments (
            $ShellArguments + @('printf "%s\n" "$COPILOT_CUSTOM_INSTRUCTIONS_DIRS"')
        )

        $output | Should -BeExactly 'existing-one,existing-two,/checkout/agents'
    }

    It 'serializes overlapping setup without losing startup content or duplicating loaders (shared participant: <IndependentParticipant>)' -ForEach @(
        @{ IndependentParticipant = $false }
        @{ IndependentParticipant = $true }
    ) {
        [IO.File]::WriteAllText($script:ProfilePath, "preserved profile`n")
        [IO.File]::WriteAllText($script:BashrcPath, "preserved bashrc`n")
        [void][IO.Directory]::CreateDirectory((Split-Path -Parent $script:EnvironmentPath))
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
                $signal = $item.process.StandardOutput.ReadLine()
                $signal | Should -BeExactly 'lock-contended'
            }
            $lock.Dispose()
            # Each contender has already tried the real lock. Admit retries individually
            # so the expected final checkout is deterministic, without clocks or sleeps.
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

        $profile = Read-BashConfigurationText -Path $script:ProfilePath
        $bashrc = Read-BashConfigurationText -Path $script:BashrcPath
        $loader = "# Load the repository's per-user AI environment.`n" +
            '[ ! -f "$HOME/.config/ss-ai/environment.sh" ] || . "$HOME/.config/ss-ai/environment.sh"' + "`n"
        $independentEntry = if ($IndependentParticipant) { "# independent startup entry`n" } else { '' }
        $profile | Should -BeExactly ("preserved profile`n`n" + $loader + $independentEntry)
        $bashrc | Should -BeExactly ($loader + "preserved bashrc`n" + $independentEntry)
        (Read-BashConfigurationText -Path $script:EnvironmentPath) | Should -BeExactly (
            Get-BashEnvironmentContent -AgentsDirectory $(if ($IndependentParticipant) { '/first/agents' } else { '/second/agents' })
        )
    }
}
