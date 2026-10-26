BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    . (Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts/lib/RustTestPreparation.ps1')

    # Retained byte fixtures isolate schema/ownership failures from the expensive real Cargo lifecycle.
    function New-InstallationEvidenceFixture {
        $owned = Join-Path $TestDrive ([guid]::NewGuid().ToString('N') + ' with spaces')
        $directory = Join-Path $owned 'installation'
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
        $artifacts = @(foreach ($name in @('old', 'new', 'original-source', 'changed-source', 'original-lock', 'final-lock')) {
            $path = Join-Path $directory $name
            [IO.File]::WriteAllText($path, $name)
            Get-TestArtifact -Path $path
        })
        $commands = @{}
        foreach ($name in @('conflict', 'baseline', 'baselineSetup', 'changed', 'stale', 'shadowed', 'missing', 'finalSetup')) {
            $commands[$name] = @{
                exitCode = $(if ($name -in @('baseline', 'baselineSetup', 'changed', 'finalSetup')) { 0 } else { 1 })
                stdout = ''
                stderr = ''
                cargoArtifacts = @()
            }
        }
        $evidence = @{
            schemaVersion = 1
            status = 'complete'
            artifacts = $artifacts
            commands = $commands
            nativeFlags = "-C$([char]0x1f)target-cpu=native"
            baseline = @{ version = 'ss-skills-util fixture'; help = 'baseline help' }
            changed = @{ version = 'ss-skills-util fixture'; help = 'Test-owned replacement from changed local source' }
        }
        $path = Join-Path $directory 'evidence.json'
        [IO.File]::WriteAllText($path, ($evidence | ConvertTo-Json -Depth 30))
        @{ ownedDirectory = $owned; installation = Get-TestArtifact -Path $path }
    }

    # Rehashing the outer record exercises semantic validation rather than only digest rejection.
    function Edit-InstallationEvidence {
        param($Preparation, [scriptblock] $Change)
        $evidence = [IO.File]::ReadAllText($Preparation.installation.path) | ConvertFrom-Json -AsHashtable
        & $Change $evidence
        [IO.File]::WriteAllText($Preparation.installation.path, ($evidence | ConvertTo-Json -Depth 30))
        $Preparation.installation = Get-TestArtifact -Path $Preparation.installation.path
    }
}

Describe 'Verified installation prerequisite boundary' {
    BeforeEach { $prepared = New-InstallationEvidenceFixture }

    It 'accepts intact complete installation evidence and independent immutable outputs' {
        Read-RustInstallationPreparation -Preparation $prepared -Required $true
        $other = New-InstallationEvidenceFixture
        Read-RustInstallationPreparation -Preparation $other -Required $true
        Read-RustInstallationPreparation -Preparation $prepared -Required $true
        $other.ownedDirectory | Should -Not -Be $prepared.ownedDirectory
    }

    It 'rejects absent or unexpected evidence for the selected target' {
        { Read-RustInstallationPreparation -Preparation $prepared -Required $false } | Should -Throw '*Unexpected*'
        $prepared.installation = $null
        { Read-RustInstallationPreparation -Preparation $prepared -Required $true } | Should -Throw
        Read-RustInstallationPreparation -Preparation $prepared -Required $false
    }

    It 'rejects changed evidence, missing binaries, and modified retained artifacts' {
        $original = [IO.File]::ReadAllBytes($prepared.installation.path)
        [IO.File]::AppendAllText($prepared.installation.path, ' ')
        { Read-RustInstallationPreparation -Preparation $prepared -Required $true } | Should -Throw '*modified*'
        [IO.File]::WriteAllBytes($prepared.installation.path, $original)
        $evidence = [IO.File]::ReadAllText($prepared.installation.path) | ConvertFrom-Json -AsHashtable
        [IO.File]::WriteAllText($evidence.artifacts[0].path, 'changed')
        { Read-RustInstallationPreparation -Preparation $prepared -Required $true } | Should -Throw '*artifact*'
        Remove-Item -LiteralPath $evidence.artifacts[0].path
        { Read-RustInstallationPreparation -Preparation $prepared -Required $true } | Should -Throw
    }

    It 'rejects malformed or unsuccessful evidence instead of coercing it' {
        foreach ($change in @(
            { param($e) $e.schemaVersion = '1' }
            { param($e) $e.status = 'preparing' }
            { param($e) $e.nativeFlags = '-C target-cpu=generic' }
            { param($e) $e.artifacts = @() }
            { param($e) $e.commands.Remove('stale') }
            { param($e) $e.commands.baseline.exitCode = 1 }
            { param($e) $e.commands.stale.exitCode = 0 }
            { param($e) $e.commands.finalSetup.exitCode = '0' }
            { param($e) $e.commands.changed.cargoArtifacts = 'not an array' }
            { param($e) $e.changed.version = 'ss-skills-util different' }
            { param($e) $e.changed.help = $e.baseline.help }
            { param($e) $e.baseline.version = 17 }
            { param($e) $e.extra = 'unknown' }
        )) {
            $invalid = New-InstallationEvidenceFixture
            Edit-InstallationEvidence -Preparation $invalid -Change $change
            { Read-RustInstallationPreparation -Preparation $invalid -Required $true } | Should -Throw
        }
    }

    It 'rejects artifacts owned by a different invocation even with valid hashes' {
        $other = New-InstallationEvidenceFixture
        $otherEvidence = [IO.File]::ReadAllText($other.installation.path) | ConvertFrom-Json -AsHashtable
        Edit-InstallationEvidence -Preparation $prepared -Change { param($e) $e.artifacts[0] = $otherEvidence.artifacts[0] }
        { Read-RustInstallationPreparation -Preparation $prepared -Required $true } | Should -Throw '*unowned*'
    }

    It 'requires an exact Cargo-discovered executable and restores the environment after failure' {
        { New-RustInstallationPreparation -OwnedDirectory $prepared.ownedDirectory -Executables @() } | Should -Throw '*exactly one*'
        $failed = Join-Path $TestDrive 'failed-preparation.ps1'
        [IO.File]::WriteAllText($failed, '$global:LASTEXITCODE = 19')
        $owner = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $owner | Out-Null
        $previous = $env:SS_AI_INSTALLATION_PREPARATION_DIRECTORY
        { New-RustInstallationPreparation -OwnedDirectory $owner -Executables @($failed) } | Should -Throw '*preparation failed*'
        $env:SS_AI_INSTALLATION_PREPARATION_DIRECTORY | Should -Be $previous
        Test-Path -LiteralPath (Join-Path $owner 'installation/evidence.json') | Should -BeFalse
    }
}
