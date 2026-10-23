BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    . (Join-Path $script:RepoRoot 'Install.ps1')
}
    Describe 'Skills utility installation' {
        BeforeAll {
            # These command boundaries must remain mockable even when Cargo is not installed.
            function cargo {}
        }
        BeforeEach {
            $script:InstalledUtility = Join-Path $TestDrive 'installed.ps1'
            $script:BuiltUtility = Join-Path $TestDrive 'artifact.ps1'
            $script:VersionMarker = Join-Path $TestDrive 'version-invoked'
            Remove-Item -LiteralPath $script:VersionMarker -ErrorAction SilentlyContinue
            $content = "[IO.File]::WriteAllText('$($script:VersionMarker.Replace("'", "''"))', 'invoked'); 'ss-skills-util 0.1.0'; exit 0"
            [IO.File]::WriteAllText($script:InstalledUtility, $content)
            [IO.File]::WriteAllText($script:BuiltUtility, $content)
            Mock Get-Command { [pscustomobject]@{ Name = $Name } } -ParameterFilter {
                ($Name -join ',') -in @('rustup', 'cargo') -and $CommandType -eq 'Application'
            }
            Mock Get-Command { [pscustomobject]@{ Source = $script:InstalledUtility } } -ParameterFilter {
                ($Name -join ',') -eq 'ss-skills-util' -and $CommandType -eq 'Application'
            }
            Mock Invoke-SkillsUtilityCargo {
                $script:UtilityPath = $UtilityPath
                $script:InstallLocation = (Get-Location).Path
                $global:LASTEXITCODE = 0
                @{ reason = 'compiler-artifact'; target = @{ name = 'ss-skills-util'; kind = @('bin') }; executable = $script:BuiltUtility } |
                    ConvertTo-Json -Compress
            }
        }

        It 'installs locked local source from the toolchain root and verifies PATH invocation' {
            Push-Location $TestDrive
            try {
                Install-SkillsUtility 6>$null
                (Get-Location).Path | Should -Be $TestDrive
            } finally { Pop-Location }
            $script:InstallLocation | Should -Be $script:RepoRoot
            $script:UtilityPath | Should -Be (Join-Path $script:RepoRoot 'packages\ss-skills-util')
            Test-Path -LiteralPath $script:VersionMarker | Should -BeTrue
        }

        It 'announces the expected wait before starting compilation' {
            Mock Write-Host {}
            Mock Invoke-SkillsUtilityCargo {
                Should -Invoke Write-Host -Times 1 -Exactly -ParameterFilter {
                    $Object -eq 'Building and installing ss-skills-util. Please wait; this may take a few minutes.'
                }
                @{ reason = 'compiler-artifact'; target = @{ name = 'ss-skills-util'; kind = @('bin') }; executable = $script:BuiltUtility } |
                    ConvertTo-Json -Compress
            }

            Install-SkillsUtility
        }

        It 'rebuilds when setup repeats without forcing unrelated binary conflicts' {
            Install-SkillsUtility 6>$null
            Install-SkillsUtility 6>$null
            Should -Invoke Invoke-SkillsUtilityCargo -Times 2 -Exactly
        }

        It 'optimizes only the install child and preserves Cargo flag precedence' {
            $originalEncoded = $env:CARGO_ENCODED_RUSTFLAGS
            $originalPlain = $env:RUSTFLAGS
            Mock Get-Command { [pscustomobject]@{ Source = 'first-cargo' }; [pscustomobject]@{ Source = 'second-cargo' } } -ParameterFilter { ($Name -join ',') -eq 'cargo' }
            try {
                $env:RUSTFLAGS = '-C opt-level=2'
                $env:CARGO_ENCODED_RUSTFLAGS = $null
                $plain = New-SkillsUtilityCargoStartInfo -UtilityPath 'local package'
                $plain.FileName | Should -Be 'first-cargo'
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

        It 'reports missing development prerequisites' {
            Mock Get-Command { $null } -ParameterFilter { ($Name -join ',') -eq 'rustup' }
            { Install-SkillsUtility } | Should -Throw '*rustup is required on PATH*'
            Should -Invoke Invoke-SkillsUtilityCargo -Times 0 -Exactly
            Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
        }

        It 'preserves compiler failure and restores the caller location' {
            Mock Invoke-SkillsUtilityCargo { throw 'ss-skills-util compilation or installation failed (exit 23).' }
            Push-Location $TestDrive
            try {
                { Install-SkillsUtility } | Should -Throw '*exit 23*'
                (Get-Location).Path | Should -Be $TestDrive
            } finally { Pop-Location }
            Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
        }

        It 'refuses to link skills when Cargo fails' {
            Mock Add-AgentsDirectoryToCopilotInstructions {}
            Mock Set-BashUserEnvironment {}
            Mock Install-CopilotSkills {}
            Mock Invoke-SkillsUtilityCargo { throw 'ss-skills-util compilation or installation failed (exit 23).' }
            { Invoke-AiInstallation -HomeDirectory $TestDrive 6>$null } | Should -Throw '*exit 23*'
            Should -Invoke Install-CopilotSkills -Times 0 -Exactly
        }

        It 'reports missing PATH discovery rather than assuming the default Cargo root' {
            Mock Get-Command { $null } -ParameterFilter { ($Name -join ',') -eq 'ss-skills-util' }
            { Install-SkillsUtility } | Should -Throw "*configured installation binary directory*"
            Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
        }

        It 'rejects an unsuccessful installed executable' {
            $content = "'ss-skills-util broken'; exit 31"
            [IO.File]::WriteAllText($script:InstalledUtility, $content)
            [IO.File]::WriteAllText($script:BuiltUtility, $content)
            { Install-SkillsUtility } | Should -Throw '*PATH ss-skills-util invocation failed*'
        }

        It 'rejects an older PATH artifact even when its version is unchanged' {
            [IO.File]::AppendAllText($script:InstalledUtility, "`n# Different build with the same version")
            { Install-SkillsUtility } | Should -Throw "*'$script:InstalledUtility'*'$script:BuiltUtility'*before older copies*"
            Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
        }

        It 'rejects incomplete Cargo artifact evidence' {
            Mock Invoke-SkillsUtilityCargo { '{"reason":"build-finished","success":true}' }
            { Install-SkillsUtility } | Should -Throw '*exactly one*'
            Test-Path -LiteralPath $script:VersionMarker | Should -BeFalse
        }
    }
