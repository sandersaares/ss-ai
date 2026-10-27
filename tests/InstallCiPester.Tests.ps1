BeforeAll {
    $script:Installer = Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts\Install-CiPester.ps1'
}

Describe 'CI Pester installation' {
    BeforeEach {
        $global:CiPesterTestAttempts = 0
        Mock Start-Sleep {}
    }

    AfterEach {
        Remove-Variable -Name CiPesterTestAttempts -Scope Global
    }

    It 'does not retry a successful installation' {
        Mock Install-Module { $global:CiPesterTestAttempts++ }
        & $script:Installer
        $global:CiPesterTestAttempts | Should -Be 1
        Should -Invoke Start-Sleep -Times 0 -Exactly
        Should -Invoke Install-Module -Times 1 -Exactly -ParameterFilter {
            $Name -eq 'Pester' -and $RequiredVersion -eq '5.7.1' -and
            $Scope -eq 'CurrentUser' -and $Force -and $SkipPublisherCheck -and $ErrorAction -eq 'Stop'
        }
    }

    It 'retries download failures and stops after success' {
        Mock Install-Module {
            $global:CiPesterTestAttempts++
            if ($global:CiPesterTestAttempts -lt 3) { Write-Error "Package 'Pester' failed to download." }
        }
        & $script:Installer
        $global:CiPesterTestAttempts | Should -Be 3
        Should -Invoke Start-Sleep -Times 1 -Exactly -ParameterFilter { $Seconds -eq 5 }
        Should -Invoke Start-Sleep -Times 1 -Exactly -ParameterFilter { $Seconds -eq 10 }
    }

    It 'fails after the bounded download attempts' {
        Mock Install-Module { $global:CiPesterTestAttempts++; throw "Package 'Pester' failed to download." }
        { & $script:Installer } | Should -Throw "*failed to download*"
        $global:CiPesterTestAttempts | Should -Be 3
        Should -Invoke Start-Sleep -Times 2 -Exactly
    }

    It 'does not retry unrelated installation errors' {
        Mock Install-Module { $global:CiPesterTestAttempts++; throw 'Access denied' }
        { & $script:Installer } | Should -Throw '*Access denied*'
        $global:CiPesterTestAttempts | Should -Be 1
        Should -Invoke Start-Sleep -Times 0 -Exactly
    }

    It 'stops when a retry encounters an unrelated error' {
        Mock Install-Module {
            $global:CiPesterTestAttempts++
            if ($global:CiPesterTestAttempts -eq 1) { throw "Package 'Pester' failed to download." }
            throw 'Access denied'
        }
        { & $script:Installer } | Should -Throw '*Access denied*'
        $global:CiPesterTestAttempts | Should -Be 2
        Should -Invoke Start-Sleep -Times 1 -Exactly
    }
}
