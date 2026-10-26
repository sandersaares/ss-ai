BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    . (Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts/lib/RustTestPreparation.ps1')

    # Command-boundary fixtures make compiler counts and failures deterministic without compiling.
    function rustc {
        $global:LASTEXITCODE = 0
        if ($args[0] -eq '-vV') { return 'rustc fixture host' }
        $script:ProviderBuilds++
        if ($script:FailProvider) { $global:LASTEXITCODE = 13; return }
        [IO.File]::WriteAllText($args[[array]::IndexOf($args, '-o') + 1], 'provider')
    }

    function cargo {
        $global:LASTEXITCODE = 0
        if ($args[0] -eq '--version') { return 'cargo fixture' }
        $script:CargoCalls.Add(@($args))
        if ($script:FailCargo) { $global:LASTEXITCODE = 17; return }
        if ($args[0] -eq 'metadata') {
            return '{"workspace_members":["fixture"],"packages":[{"id":"fixture","targets":[{"name":"native","kind":["test"]},{"name":"installation","kind":["test"]},{"name":"git","kind":["test"]}]}]}'
        }
        $target = $args[[array]::IndexOf($args, '--target-dir') + 1]
        [IO.Directory]::CreateDirectory($target) | Out-Null
        $executable = Join-Path $target 'test with spaces'
        [IO.File]::WriteAllText($executable, 'test')
        @{ reason = 'compiler-artifact'; target = @{ name = 'fixture' }; fresh = $false; executable = $executable } |
            ConvertTo-Json -Compress
    }

    # Each invocation gets a source tree and record parent independent of the real checkout.
    function New-PreparationFixture {
        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N') + ' with spaces')
        $support = Join-Path $root 'packages/ss-skills-util/tests/support'
        New-Item -ItemType Directory -Path $support -Force | Out-Null
        [IO.File]::WriteAllText((Join-Path $support 'provider.rs'), 'source')
        [IO.File]::WriteAllText((Join-Path $root 'Cargo.lock'), 'lock')
        @{ root = $root; record = Join-Path $root 'prerequisites.json' }
    }
}

Describe 'Rust preparation boundary' {
    BeforeEach {
        # This suite isolates the outer provider/test graph; installation has its own boundary suite.
        Mock New-RustInstallationPreparation { $null }
        Mock Read-RustInstallationPreparation {}
        $script:ProviderBuilds = 0
        $script:CargoCalls = [Collections.Generic.List[object]]::new()
        $script:FailProvider = $false
        $script:FailCargo = $false
        $fixture = New-PreparationFixture
    }

    It 'builds the provider once and discovers exact selected executable paths through Cargo JSON' {
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget native
        $script:ProviderBuilds | Should -Be 1
        $script:CargoCalls.Count | Should -Be 1
        $script:CargoCalls[0] | Should -Contain '--no-run'
        $script:CargoCalls[0] | Should -Contain 'native'
        $prepared.executables[0].path | Should -Be (Join-Path $prepared.targetDirectory 'test with spaces')
        $null = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget native
        $script:ProviderBuilds | Should -Be 1
    }

    It 'never publishes a record after Cargo compilation fails' {
        $script:FailCargo = $true
        { New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw
        Test-Path -LiteralPath $fixture.record | Should -BeFalse
        @(Get-ChildItem -LiteralPath $fixture.root -Directory -Filter 'rust-tests-*') | Should -BeNullOrEmpty
    }

    It 'reports provider failure explicitly, blocks native only, and still runs other tests and doctests' {
        $script:FailProvider = $true
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $prepared.status | Should -Be 'providerFailed'
        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*Incomplete*'
        $prepared = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -AllowProviderFailure
        { Invoke-PreparedRustTests -Preparation $prepared -PrerequisitePath $fixture.record } | Should -Throw '*execution failed*'
        $script:CargoCalls.Count | Should -Be 4
        $script:CargoCalls[2] | Should -Contain 'git'
        $script:CargoCalls[2] | Should -Contain '--lib'
        $script:CargoCalls[2] | Should -Not -Contain 'native'
        $script:CargoCalls[3] | Should -Contain '--doc'
    }

    It 'does not prepare a provider for a provider-independent target' {
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget git
        $prepared.status | Should -Be 'complete'
        $prepared.provider | Should -BeNullOrEmpty
        $script:ProviderBuilds | Should -Be 0
    }

    It 'blocks installation only, or installation and native together, while independent tests and doctests run (provider failure = <ProviderFailure>)' -ForEach @(
        @{ ProviderFailure = $false }, @{ ProviderFailure = $true }
    ) {
        $script:FailProvider = $ProviderFailure
        Mock New-RustInstallationPreparation { throw 'release fixture compilation failed' }
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $prepared.status | Should -Be $(if ($ProviderFailure) { 'providerAndInstallationFailed' } else { 'installationFailed' })
        $prepared.installation | Should -BeNullOrEmpty
        $prepared.installationFailure | Should -Match 'release fixture compilation failed'
        if ($ProviderFailure) { $prepared.provider | Should -BeNullOrEmpty }
        else { $prepared.provider.executable | Should -Not -BeNullOrEmpty }
        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*Incomplete*'
        $prepared = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -AllowPrerequisiteFailure
        { Invoke-PreparedRustTests -Preparation $prepared -PrerequisitePath $fixture.record } | Should -Throw '*execution failed*'
        $script:CargoCalls.Count | Should -Be 4
        $script:CargoCalls[2] | Should -Contain 'git'
        $script:CargoCalls[2] | Should -Contain '--lib'
        $script:CargoCalls[2] | Should -Not -Contain 'installation'
        if ($ProviderFailure) { $script:CargoCalls[2] | Should -Not -Contain 'native' }
        else { $script:CargoCalls[2] | Should -Contain 'native' }
        $script:CargoCalls[3] | Should -Contain '--doc'
    }

    It 'keeps explicitly selected independent tests after installation failure, without widening selection' {
        Mock New-RustInstallationPreparation { throw 'release fixture compilation failed' }
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget @('installation', 'git')
        { Invoke-PreparedRustTests -Preparation $prepared -PrerequisitePath $fixture.record -TestTarget @('installation', 'git') } |
            Should -Throw '*execution failed*'
        $script:CargoCalls.Count | Should -Be 2
        $script:CargoCalls[1] | Should -Contain 'git'
        $script:CargoCalls[1] | Should -Not -Contain 'installation'
        $script:CargoCalls[1] | Should -Not -Contain '--lib'
        { Invoke-PreparedRustTests -Preparation $prepared -PrerequisitePath $fixture.record -TestTarget installation } |
            Should -Throw '*All selected Rust tests are blocked*'
    }

    It 'rejects inconsistent failed-installation records rather than treating them as completed' {
        Mock New-RustInstallationPreparation { throw 'release fixture compilation failed' }
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $original = $prepared | ConvertTo-Json -Depth 30
        foreach ($change in @(
            { param($r) $r.installationFailure = '' }
            { param($r) $r.installationFailure = 17 }
            { param($r) $r.installation = @{ path = 'success-shaped'; sha256 = 'invalid' } }
            { param($r) $r.status = 'complete' }
            { param($r) $r.status = 'providerAndInstallationFailed' }
        )) {
            $invalid = $original | ConvertFrom-Json -AsHashtable
            & $change $invalid
            [IO.File]::WriteAllText($fixture.record, ($invalid | ConvertTo-Json -Depth 30))
            { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -AllowPrerequisiteFailure } |
                Should -Throw
        }
    }

    It 'rejects missing records and mismatched selection instead of building on demand' {
        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*Prepare first*'
        $null = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget native
        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*selection*'
        $script:ProviderBuilds | Should -Be 1
    }

    It 'invalidates a same-version source edit' {
        $null = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        Add-Content -LiteralPath (Join-Path $fixture.root 'packages/ss-skills-util/tests/support/provider.rs') -Value 'changed'
        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*sourceSha256*'
    }

    It 'invalidates installer and installer-library edits as well as Rust source' {
        $library = Join-Path $fixture.root 'scripts/lib'
        New-Item -ItemType Directory -Path $library -Force | Out-Null
        foreach ($path in @((Join-Path $fixture.root 'Install.ps1'), (Join-Path $library 'NativeUtility.ps1'))) {
            [IO.File]::WriteAllText($path, 'baseline installer')
        }
        $null = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        foreach ($path in @((Join-Path $fixture.root 'Install.ps1'), (Join-Path $library 'NativeUtility.ps1'))) {
            [IO.File]::WriteAllText($path, 'changed installer')
            { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*sourceSha256*'
            [IO.File]::WriteAllText($path, 'baseline installer')
        }
    }

    It 'rejects incomplete records, changed artifacts, and incompatible flags' {
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $bytes = [IO.File]::ReadAllBytes($fixture.record)
        $prepared.status = 'preparing'
        [IO.File]::WriteAllText($fixture.record, ($prepared | ConvertTo-Json -Depth 20))
        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*Incomplete*'
        [IO.File]::WriteAllBytes($fixture.record, $bytes)
        $previous = $env:RUSTFLAGS
        try {
            $env:RUSTFLAGS = 'different fixture flags'
            { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*environment*'
        } finally { $env:RUSTFLAGS = $previous }
        [IO.File]::WriteAllText($prepared.provider.executable.path, 'modified')
        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*Modified prepared artifact*'
    }

    It 'rejects malformed schema fields rather than coercing them' {
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $original = $prepared | ConvertTo-Json -Depth 20
        foreach ($change in @(
            { param($record) $record.schemaVersion = '1' }
            { param($record) $record.testTargets = 'native' }
            { param($record) $record.executables = @() }
            { param($record) $record.unrecognized = 'field' }
            { param($record) $record.provider.executable.sha256 = 123 }
            { param($record) $record.Remove('status') }
        )) {
            $invalid = $original | ConvertFrom-Json -AsHashtable
            $null = & $change $invalid
            [IO.File]::WriteAllText($fixture.record, ($invalid | ConvertTo-Json -Depth 20))
            { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw
        }
    }

    It 'allows concurrent readers and atomic replacement while a record is open' {
        $null = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $stream = Open-RustTestInput -Path $fixture.record
        try {
            $replacement = Join-Path $fixture.root 'replacement.json'
            [IO.File]::WriteAllBytes($replacement, [IO.File]::ReadAllBytes($fixture.record))
            [IO.File]::Move($replacement, $fixture.record, $true)
            $null = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        } finally { $stream.Dispose() }
    }

    It 'retains existing owned outputs across additional preparations and refuses record replacement' {
        $first = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $other = Join-Path $fixture.root 'second.json'
        $second = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $other
        $first.ownedDirectory | Should -Not -Be $second.ownedDirectory
        $null = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        { New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*already exists*'
        $script:ProviderBuilds | Should -Be 2
    }

    It 'prepares concurrently with shared record = <SharedRecord> without replacing active outputs' -ForEach @(
        @{ SharedRecord = $false }, @{ SharedRecord = $true }
    ) {
        $library = Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts/lib/RustTestPreparation.ps1'
        # Both workers reach Cargo preparation before either publishes, forcing an actual overlap.
        $barrier = [Threading.Barrier]::new(2)
        $worker = {
            param($Library, $Root, $Record, $Barrier)
            Set-StrictMode -Version Latest
            $ErrorActionPreference = 'Stop'
            . $Library
            function rustc {
                $global:LASTEXITCODE = 0
                if ($args[0] -eq '-vV') { return 'rustc fixture host' }
                [IO.File]::WriteAllText($args[[array]::IndexOf($args, '-o') + 1], 'provider')
            }
            function cargo {
                $global:LASTEXITCODE = 0
                if ($args[0] -eq '--version') { return 'cargo fixture' }
                $target = $args[[array]::IndexOf($args, '--target-dir') + 1]
                [IO.Directory]::CreateDirectory($target) | Out-Null
                $executable = Join-Path $target 'test'
                [IO.File]::WriteAllText($executable, 'test')
                $Barrier.SignalAndWait() | Out-Null
                @{ reason = 'compiler-artifact'; target = @{ name = 'fixture' }; fresh = $false; executable = $executable } |
                    ConvertTo-Json -Compress
            }
            New-RustTestPreparation -RepositoryRoot $Root -PrerequisitePath $Record -TestTarget native
        }
        $first = [PowerShell]::Create()
        $second = [PowerShell]::Create()
        try {
            $other = if ($SharedRecord) { $fixture.record } else { Join-Path $fixture.root 'other.json' }
            $null = $first.AddScript($worker).AddArgument($library).AddArgument($fixture.root).AddArgument($fixture.record).AddArgument($barrier)
            $null = $second.AddScript($worker).AddArgument($library).AddArgument($fixture.root).AddArgument($other).AddArgument($barrier)
            $firstPending = $first.BeginInvoke()
            $secondPending = $second.BeginInvoke()
            $results = @()
            $failures = @()
            # The shared-record race must have one explicit loser, not an overwritten winner.
            try { $results += $first.EndInvoke($firstPending) } catch { $failures += $_ }
            try { $results += $second.EndInvoke($secondPending) } catch { $failures += $_ }
            if ($SharedRecord) {
                $results.Count | Should -Be 1
                $failures.Count | Should -Be 1
                @(Get-ChildItem -LiteralPath $fixture.root -Directory -Filter 'rust-tests-*').Count | Should -Be 1
            } else {
                $failures | Should -BeNullOrEmpty
                $first.Streams.Error | Should -BeNullOrEmpty
                $second.Streams.Error | Should -BeNullOrEmpty
                $results.Count | Should -Be 2
                $results[0].ownedDirectory | Should -Not -Be $results[1].ownedDirectory
            }
            $null = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget native
            $null = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $other -TestTarget native
        } finally {
            $first.Dispose()
            $second.Dispose()
            $barrier.Dispose()
        }
    }

    It 'keeps unfiltered Cargo execution for doctests and restores the prerequisite environment on failure' {
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $previous = $env:SS_AI_RUST_TEST_PREREQUISITES
        $script:FailCargo = $true
        try {
            { Invoke-PreparedRustTests -Preparation $prepared -PrerequisitePath $fixture.record } | Should -Throw '*execution failed*'
            $script:CargoCalls[1] | Should -Not -Contain '--test'
            $script:CargoCalls[1] | Should -Not -Contain '--tests'
            $script:CargoCalls[1] | Should -Not -Contain '--no-run'
            $env:SS_AI_RUST_TEST_PREREQUISITES | Should -Be $previous
        } finally { $env:SS_AI_RUST_TEST_PREREQUISITES = $previous }
    }
}
