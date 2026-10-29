BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:PowerShellExecutable = (Get-Process -Id $PID).Path
    . (Join-Path $script:RepoRoot 'Install.ps1')

    # Run simulated commands through the installed PowerShell executable, never a compiler or Cargo.
    function New-InstallerFixtureStartInfo {
        param([string] $Code)

        $start = [Diagnostics.ProcessStartInfo]::new($script:PowerShellExecutable)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-EncodedCommand',
            [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($Code)))) {
            $start.ArgumentList.Add($argument)
        }
        return $start
    }

    # Isolate PATH tests in children so overlapping callers never observe fixture environment changes.
    function Invoke-InstallerFixture {
        param(
            [string] $Code,
            [string] $PathValue
        )

        $start = New-InstallerFixtureStartInfo -Code $Code
        $start.Environment['PATH'] = $PathValue
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()

            @{
                exitCode = $process.ExitCode
                stdout = $stdout.GetAwaiter().GetResult()
                stderr = $stderr.GetAwaiter().GetResult()
            }
        } finally {
            $process.Dispose()
        }
    }
}

Describe 'Skills utility installation' {
    BeforeAll {
        # This command boundary must remain mockable even when Cargo is not installed.
        function cargo {}
    }

    BeforeEach {
        $script:InstalledUtility = Join-Path $TestDrive 'installed.ps1'
        $script:BuiltUtility = Join-Path $TestDrive 'artifact.ps1'
        $script:VersionMarker = Join-Path $TestDrive 'version-invoked'
        Remove-Item -LiteralPath $script:VersionMarker -ErrorAction SilentlyContinue

        $content = "[IO.File]::WriteAllText('$($script:VersionMarker.Replace("'", "''"))', 'invoked'); 'ss-ai 0.1.0'; exit 0"
        [IO.File]::WriteAllText($script:InstalledUtility, $content)
        [IO.File]::WriteAllText($script:BuiltUtility, $content)

        Mock Get-Command { [pscustomobject]@{ Name = $Name } } -ParameterFilter {
            ($Name -join ',') -in @('rustup', 'cargo') -and $CommandType -eq 'Application'
        }
        Mock Get-Command { [pscustomobject]@{ Source = $script:InstalledUtility } } -ParameterFilter {
            ($Name -join ',') -eq 'ss-ai' -and $CommandType -eq 'Application'
        }
        Mock Invoke-SkillsUtilityCargo {
            $script:UtilityPath = $UtilityPath
            $script:InstallLocation = (Get-Location).Path
            $global:LASTEXITCODE = 0
            @{
                reason = 'compiler-artifact'
                target = @{ name = 'ss-ai'; kind = @('bin') }
                executable = $script:BuiltUtility
            } | ConvertTo-Json -Compress
        }
    }

    It 'installs locked local source from the toolchain root and verifies PATH invocation' {
        Push-Location $TestDrive
        try {
            Install-SkillsUtility 6>$null
            (Get-Location).Path | Should -Be $TestDrive
        } finally {
            Pop-Location
        }

        $script:InstallLocation | Should -Be $script:RepoRoot
        $script:UtilityPath | Should -Be (Join-Path $script:RepoRoot 'packages\ss-ai')
        Test-Path -LiteralPath $script:VersionMarker | Should -BeTrue
    }

    It 'announces the expected wait before starting compilation' {
        Mock Write-Host {}
        Mock Invoke-SkillsUtilityCargo {
            Should -Invoke Write-Host -Times 1 -Exactly -ParameterFilter {
                $Object -eq 'Building and installing ss-ai. Please wait; this may take a few minutes.'
            }
            @{
                reason = 'compiler-artifact'
                target = @{ name = 'ss-ai'; kind = @('bin') }
                executable = $script:BuiltUtility
            } | ConvertTo-Json -Compress
        }

        Install-SkillsUtility
    }

    It 'delegates every setup invocation to Cargo' {
        Install-SkillsUtility 6>$null
        Install-SkillsUtility 6>$null

        Should -Invoke Invoke-SkillsUtilityCargo -Times 2 -Exactly
    }

    It 'optimizes only the install child and preserves Cargo flag precedence' {
        $originalEncoded = $env:CARGO_ENCODED_RUSTFLAGS
        $originalPlain = $env:RUSTFLAGS
        Mock Get-Command {
            [pscustomobject]@{ Source = 'first-cargo' }
            [pscustomobject]@{ Source = 'second-cargo' }
        } -ParameterFilter { ($Name -join ',') -eq 'cargo' }

        try {
            $env:RUSTFLAGS = '-C opt-level=2'
            $env:CARGO_ENCODED_RUSTFLAGS = $null
            $plain = New-SkillsUtilityCargoStartInfo -UtilityPath 'local package'

            $plain.FileName | Should -Be 'first-cargo'
            $plain.WorkingDirectory | Should -Be (Get-Location).Path
            $plain.UseShellExecute | Should -BeFalse
            $plain.RedirectStandardOutput | Should -BeTrue
            $plain.RedirectStandardError | Should -BeTrue
            @($plain.ArgumentList) | Should -Be @('install', '--path', 'local package', '--locked', '--message-format=json-render-diagnostics')
            ($plain.Environment['CARGO_ENCODED_RUSTFLAGS'] -split [char]0x1f) | Should -Be @('-C', 'opt-level=2', '-C', 'target-cpu=native')
            $env:RUSTFLAGS | Should -Be '-C opt-level=2'
            $env:CARGO_ENCODED_RUSTFLAGS | Should -BeNullOrEmpty

            $env:CARGO_ENCODED_RUSTFLAGS = "-C$([char]0x1f)debuginfo=1"
            $encoded = New-SkillsUtilityCargoStartInfo -UtilityPath 'local package'

            ($encoded.Environment['CARGO_ENCODED_RUSTFLAGS'] -split [char]0x1f) | Should -Be @('-C', 'debuginfo=1', '-C', 'target-cpu=native')
            $env:CARGO_ENCODED_RUSTFLAGS | Should -Be "-C$([char]0x1f)debuginfo=1"
        } finally {
            $env:CARGO_ENCODED_RUSTFLAGS = $originalEncoded
            $env:RUSTFLAGS = $originalPlain
        }
    }

    It 'uses only the native CPU flags when no flags exist or encoded flags are explicitly empty' {
        $originalEncoded = $env:CARGO_ENCODED_RUSTFLAGS
        $originalPlain = $env:RUSTFLAGS
        Mock Get-Command { [pscustomobject]@{ Source = $script:PowerShellExecutable } } -ParameterFilter { ($Name -join ',') -eq 'cargo' }

        try {
            $env:RUSTFLAGS = $null
            $env:CARGO_ENCODED_RUSTFLAGS = $null
            $default = New-SkillsUtilityCargoStartInfo -UtilityPath 'package with spaces'
            ($default.Environment['CARGO_ENCODED_RUSTFLAGS'] -split [char]0x1f) | Should -Be @('-C', 'target-cpu=native')

            $env:RUSTFLAGS = '-C opt-level=2'
            $env:CARGO_ENCODED_RUSTFLAGS = ''
            $empty = New-SkillsUtilityCargoStartInfo -UtilityPath 'package with spaces'
            ($empty.Environment['CARGO_ENCODED_RUSTFLAGS'] -split [char]0x1f) | Should -Be @('-C', 'target-cpu=native')
            $env:RUSTFLAGS | Should -Be '-C opt-level=2'
            $env:CARGO_ENCODED_RUSTFLAGS | Should -BeExactly ''
        } finally {
            $env:CARGO_ENCODED_RUSTFLAGS = $originalEncoded
            $env:RUSTFLAGS = $originalPlain
        }
    }

    It 'reports missing development prerequisites' {
        Mock Get-Command { $null } -ParameterFilter { ($Name -join ',') -eq 'rustup' }

        { Install-SkillsUtility } | Should -Throw '*rustup is required on PATH*'
        Should -Invoke Invoke-SkillsUtilityCargo -Times 0 -Exactly
        Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
    }

    It 'reports missing Cargo without invoking the installation command' {
        Mock Get-Command { $null } -ParameterFilter { ($Name -join ',') -eq 'cargo' }

        { Install-SkillsUtility } | Should -Throw '*cargo is required on PATH*'
        Should -Invoke Invoke-SkillsUtilityCargo -Times 0 -Exactly
        Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
    }

    It 'requires the repository lockfile before invoking Cargo' {
        Mock Test-Path { $false } -ParameterFilter { $LiteralPath -eq (Join-Path $script:RepoRoot 'Cargo.lock') }

        { Install-SkillsUtility } | Should -Throw '*Cargo.lock is required*'
        Should -Invoke Invoke-SkillsUtilityCargo -Times 0 -Exactly
    }

    It 'preserves compiler failure and restores the caller location' {
        Mock Invoke-SkillsUtilityCargo { throw 'ss-ai compilation or installation failed (exit 23).' }

        Push-Location $TestDrive
        try {
            { Install-SkillsUtility } | Should -Throw '*exit 23*'
            (Get-Location).Path | Should -Be $TestDrive
        } finally {
            Pop-Location
        }

        Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
    }

    It 'refuses to link skills when Cargo fails' {
        Mock Remove-LegacyCopilotInstructionRegistration {}
        Mock Remove-LegacyBashEnvironment {}
        Mock Install-CopilotSkills {}
        Mock Invoke-SkillsUtilityCargo { throw 'ss-ai compilation or installation failed (exit 23).' }

        { Invoke-AiInstallation -HomeDirectory $TestDrive 6>$null } | Should -Throw '*exit 23*'
        Should -Invoke Install-CopilotSkills -Times 0 -Exactly
    }

    It 'reports missing PATH discovery rather than assuming the default Cargo root' {
        Mock Get-Command { $null } -ParameterFilter { ($Name -join ',') -eq 'ss-ai' }

        { Install-SkillsUtility } | Should -Throw "*configured installation binary directory*"
        Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
    }

    It 'rejects an unsuccessful installed executable' {
        $content = "'ss-ai broken'; exit 31"
        [IO.File]::WriteAllText($script:InstalledUtility, $content)
        [IO.File]::WriteAllText($script:BuiltUtility, $content)

        { Install-SkillsUtility } | Should -Throw '*PATH ss-ai invocation failed*'
    }

    It 'rejects a successful executable with an invalid version banner (<Banner>)' -ForEach @(
        @{ Banner = '' }, @{ Banner = 'unrelated executable 0.1.0' }
    ) {
        $content = "'$Banner'; exit 0"
        [IO.File]::WriteAllText($script:InstalledUtility, $content)
        [IO.File]::WriteAllText($script:BuiltUtility, $content)

        { Install-SkillsUtility } | Should -Throw '*PATH ss-ai invocation failed*'
    }

    It 'rejects an older PATH artifact even when its version is unchanged' {
        [IO.File]::AppendAllText($script:InstalledUtility, "`n# Different build with the same version")

        { Install-SkillsUtility } | Should -Throw "*'$script:InstalledUtility'*'$script:BuiltUtility'*before older copies*"
        Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
    }

    It 'selects the exact utility binary among dependency, library and diagnostic messages' {
        Mock Invoke-SkillsUtilityCargo {
            '{"reason":"build-finished","success":true}'
            '{"reason":"compiler-message","message":{"rendered":"fixture diagnostic"}}'
            '{"reason":"compiler-artifact","target":{"name":"dependency","kind":["bin"]},"executable":"must not be selected"}'
            '{"reason":"compiler-artifact","target":{"name":"ss-ai","kind":["lib"]},"executable":"must not be selected"}'
            @{
                reason = 'compiler-artifact'
                target = @{ name = 'ss-ai'; kind = @('bin') }
                executable = $script:BuiltUtility
            } | ConvertTo-Json -Compress
        }

        Install-SkillsUtility 6>$null
        Test-Path -LiteralPath $script:VersionMarker | Should -BeTrue
    }

    It 'rejects <Case> Cargo artifact output before invoking the installed executable' -ForEach @(
        @{ Case = 'missing'; Reply = '{"reason":"build-finished","success":true}' }
        @{ Case = 'wrong-case name'; Reply = '{"reason":"compiler-artifact","target":{"name":"SS-SKILLS-UTIL","kind":["bin"]},"executable":"not selected"}' }
        @{ Case = 'missing executable'; Reply = '{"reason":"compiler-artifact","target":{"name":"ss-ai","kind":["bin"]}}' }
        @{ Case = 'empty executable'; Reply = '{"reason":"compiler-artifact","target":{"name":"ss-ai","kind":["bin"]},"executable":""}' }
        @{ Case = 'malformed'; Reply = '{not JSON}' }
    ) {
        $script:CargoReply = $Reply
        Mock Invoke-SkillsUtilityCargo { $script:CargoReply }

        { Install-SkillsUtility } | Should -Throw
        Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
    }

    It 'rejects multiple matching utility executables instead of choosing arbitrarily' {
        Mock Invoke-SkillsUtilityCargo {
            foreach ($path in @($script:BuiltUtility, $script:InstalledUtility)) {
                @{
                    reason = 'compiler-artifact'
                    target = @{ name = 'ss-ai'; kind = @('bin') }
                    executable = $path
                } | ConvertTo-Json -Compress
            }
        }

        { Install-SkillsUtility } | Should -Throw '*exactly one*'
        Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
    }

    It 'reports a missing reported artifact rather than accepting a version-only match' {
        Remove-Item -LiteralPath $script:BuiltUtility

        { Install-SkillsUtility } | Should -Throw
        Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
    }
}

Describe 'Cargo process result handling' {
    BeforeEach {
        $script:CargoProcessCode = @'
[Console]::Out.WriteLine('{"reason":"compiler-artifact","target":{"name":"fixture"}}')
[Console]::Out.WriteLine()
[Console]::Out.WriteLine('{"reason":"build-finished","success":true}')
[Console]::Error.WriteLine('fixture Cargo diagnostic')
exit 0
'@
        Mock New-SkillsUtilityCargoStartInfo { New-InstallerFixtureStartInfo -Code $script:CargoProcessCode }
        $script:OriginalError = [Console]::Error
        $script:CapturedError = [IO.StringWriter]::new()
        [Console]::SetError($script:CapturedError)
    }

    AfterEach {
        [Console]::SetError($script:OriginalError)
        $script:CapturedError.Dispose()
    }

    It 'returns nonempty Cargo output lines and preserves stderr diagnostics' {
        $messages = @(Invoke-SkillsUtilityCargo -UtilityPath 'package with spaces')
        $messages.Count | Should -Be 2
        ($messages[0] | ConvertFrom-Json).reason | Should -Be 'compiler-artifact'
        ($messages[1] | ConvertFrom-Json).reason | Should -Be 'build-finished'
        $script:CapturedError.ToString() | Should -Match 'fixture Cargo diagnostic'
        Should -Invoke New-SkillsUtilityCargoStartInfo -Times 1 -Exactly -ParameterFilter { $UtilityPath -eq 'package with spaces' }
    }

    It 'fails on nonzero Cargo exit status even when stdout resembles successful artifact output' {
        $script:CargoProcessCode = $script:CargoProcessCode.Replace('exit 0', 'exit 23')
        { Invoke-SkillsUtilityCargo -UtilityPath 'package with spaces' } | Should -Throw '*exit 23*'
        $script:CapturedError.ToString() | Should -Match 'fixture Cargo diagnostic'
    }

    It 'surfaces process launch errors without reporting an installed utility' {
        Mock New-SkillsUtilityCargoStartInfo {
            $start = New-InstallerFixtureStartInfo -Code ''
            $start.FileName = Join-Path $TestDrive 'missing executable'
            return $start
        }
        { Invoke-SkillsUtilityCargo -UtilityPath 'package with spaces' } | Should -Throw
    }
}

Describe 'Real installer PATH and executable identity' {
    It 'handles <Case> PATH discovery using isolated fixture launchers' -ForEach @(
        @{ Case = 'correct'; Prefix = @('new'); ExitCode = 0; ErrorText = '' }
        @{ Case = 'correct before stale'; Prefix = @('new', 'old'); ExitCode = 0; ErrorText = '' }
        @{ Case = 'stale'; Prefix = @('old'); ExitCode = 1; ErrorText = "does not match Cargo's artifact" }
        @{ Case = 'shadowed'; Prefix = @('old', 'new'); ExitCode = 1; ErrorText = "does not match Cargo's artifact" }
        @{ Case = 'missing'; Prefix = @(); ExitCode = 1; ErrorText = 'is not on PATH' }
    ) {
        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N') + ' with spaces')
        $version = Join-Path $root 'version.ps1'
        $marker = Join-Path $root 'version-invoked'
        New-Item -ItemType Directory -Path (Join-Path $root 'new'), (Join-Path $root 'old') -Force | Out-Null

        $body = @"
`$ErrorActionPreference = 'Stop'
if (`$args.Count -ne 1 -or `$args[0] -cne '--version') {
    throw 'Unexpected version invocation'
}

[IO.File]::WriteAllText('$($marker.Replace("'", "''"))', 'invoked')
'ss-ai fixture'
exit 0
"@
        [IO.File]::WriteAllText($version, $body)

        if ($IsWindows) {
            $name = 'ss-ai.cmd'
            $launcher = "@echo off`r`n@`"$script:PowerShellExecutable`" -NoProfile -NonInteractive -File `"$version`" %*`r`n"
            $oldLauncher = "@rem stale same-version fixture`r`n$launcher"
        } else {
            $name = 'ss-ai'
            # A shell launcher supports interpreter paths containing spaces; all fixture logic stays in PowerShell.
            $runtime = $script:PowerShellExecutable.Replace("'", "'\''")
            $fixture = $version.Replace("'", "'\''")
            $launcher = "#!/bin/sh`nexec '$runtime' -NoProfile -NonInteractive -File '$fixture' `"`$@`"`n"
            $oldLauncher = "$launcher# stale same-version fixture`n"
        }

        $built = Join-Path $root 'artifact'
        $current = Join-Path $root "new/$name"
        $old = Join-Path $root "old/$name"
        [IO.File]::WriteAllText($built, $launcher)
        [IO.File]::WriteAllText($current, $launcher)
        [IO.File]::WriteAllText($old, $oldLauncher)

        if (-not $IsWindows) {
            # Only the fixture owner needs to execute these private launchers.
            $mode = [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite -bor [IO.UnixFileMode]::UserExecute
            [IO.File]::SetUnixFileMode($current, $mode)
            [IO.File]::SetUnixFileMode($old, $mode)
        }

        $reply = @{
            reason = 'compiler-artifact'
            target = @{ name = 'ss-ai'; kind = @('bin') }
            executable = $built
        } | ConvertTo-Json -Compress
        $code = @"
. '$((Join-Path $script:RepoRoot 'Install.ps1').Replace("'", "''"))'

# Bypass toolchain discovery while leaving utility discovery on the child's real PATH.
function Get-Command {
    param(`$Name, `$CommandType, `$ErrorAction)

    if (`$Name -in @('cargo', 'rustup')) {
        return [pscustomobject]@{ Source = 'unused prerequisite fixture' }
    }
    Microsoft.PowerShell.Core\Get-Command @PSBoundParameters
}

# Cargo output names the exact fixture artifact; no compilation or installation occurs.
function Invoke-SkillsUtilityCargo {
    param([string] `$UtilityPath)

    '$($reply.Replace("'", "''"))'
}

try {
    Install-SkillsUtility
    exit 0
} catch {
    [Console]::Error.WriteLine(`$_.Exception.Message)
    exit 1
}
"@

        $paths = @($Prefix | ForEach-Object { Join-Path $root $_ })
        $result = Invoke-InstallerFixture -Code $code -PathValue ($paths -join [IO.Path]::PathSeparator)

        $result.exitCode | Should -Be $ExitCode -Because ($result.stdout + $result.stderr)
        if ($ExitCode -eq 0) {
            Test-Path -LiteralPath $marker | Should -BeTrue
            $result.stdout | Should -Match 'Installed ss-ai fixture'
        } else {
            $result.stderr | Should -Match ([regex]::Escape($ErrorText))
            Test-Path -LiteralPath $marker | Should -BeFalse
        }
    }
}
