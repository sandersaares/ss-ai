BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    . (Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts/lib/RustTestPreparation.ps1')

    # Command-boundary fixtures make compiler counts and failures deterministic without compiling.
    function rustc {
        $global:LASTEXITCODE = 0
        if ($args[0] -eq '-vV') {
            return 'rustc fixture host'
        }

        $script:ProviderBuilds++
        if ($script:FailProvider) {
            # Distinct native exit statuses keep provider and Cargo failures distinguishable.
            $global:LASTEXITCODE = 13
            return
        }
        [IO.File]::WriteAllText($args[[array]::IndexOf($args, '-o') + 1], 'provider')
    }

    # Emit only the metadata and artifact evidence consumed by preparation and execution.
    function cargo {
        $global:LASTEXITCODE = 0
        if ($args[0] -eq '--version') {
            return 'cargo fixture'
        }

        $script:CargoCalls.Add(@($args))
        if ($script:FailCargo) {
            $global:LASTEXITCODE = 17
            return
        }
        if ($args[0] -eq 'metadata') {
            return '{"workspace_members":["fixture"],"packages":[{"id":"fixture","targets":[{"name":"native","kind":["test"]},{"name":"git","kind":["test"]}]}]}'
        }

        $target = $args[[array]::IndexOf($args, '--target-dir') + 1]
        [IO.Directory]::CreateDirectory($target) | Out-Null
        $executable = Join-Path $target 'test with spaces'
        [IO.File]::WriteAllText($executable, 'test')
        if ($script:TouchDuringBuild) {
            # Move by a full day to remain distinct on coarse timestamp filesystems.
            $stamp = [IO.File]::GetLastWriteTimeUtc($script:TouchDuringBuild)
            [IO.File]::SetLastWriteTimeUtc($script:TouchDuringBuild, $stamp.AddDays(1))
        }
        @{
            reason = 'compiler-artifact'
            target = @{ name = 'fixture' }
            fresh = $false
            executable = $executable
        } | ConvertTo-Json -Compress
    }

    # Each invocation gets a source tree and record parent independent of the real checkout.
    function New-PreparationFixture {
        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N') + ' with spaces')
        $support = Join-Path $root 'packages/ss-ai/tests/support'
        New-Item -ItemType Directory -Path $support -Force | Out-Null
        [IO.File]::WriteAllText((Join-Path $support 'provider.rs'), 'source')
        [IO.File]::WriteAllText((Join-Path $root 'Cargo.lock'), 'lock')

        @{
            root = $root
            record = Join-Path $root 'prerequisites.json'
        }
    }
}

Describe 'Rust preparation boundary' {
    BeforeEach {
        Mock Invoke-RustTestCargo {
            cargo @ArgumentList
        }
        $script:ProviderBuilds = 0
        $script:CargoCalls = [Collections.Generic.List[object]]::new()
        $script:FailProvider = $false
        $script:FailCargo = $false
        $script:TouchDuringBuild = $null
        $fixture = New-PreparationFixture
    }

    It 'rejects external <Location> <ConfigName> configuration after <Change>' -ForEach @(
        foreach ($location in @('ancestor', 'home')) {
            foreach ($configName in @('config', 'config.toml')) {
                foreach ($change in @('content', 'timestamp', 'added', 'removed', 'home selection')) {
                    @{ Location = $location; ConfigName = $configName; Change = $change }
                }
            }
        }
    ) {
        $originalHome = $env:CARGO_HOME
        $isolated = Join-Path $fixture.root 'external'
        $cargoHome = Join-Path $isolated 'cargo-home'
        $ancestor = Join-Path $isolated 'ancestor'
        $workspace = Join-Path $ancestor 'workspace'
        New-Item -ItemType Directory -Path $cargoHome, $workspace -Force | Out-Null
        $configDirectory = if ($Location -eq 'home') { $cargoHome } else { Join-Path $ancestor '.cargo' }
        New-Item -ItemType Directory -Path $configDirectory -Force | Out-Null
        $config = Join-Path $configDirectory $ConfigName
        [IO.File]::WriteAllText((Join-Path $workspace 'Cargo.lock'), 'lock')
        try {
            $env:CARGO_HOME = $cargoHome
            if ($Change -ne 'added') { [IO.File]::WriteAllText($config, '[build] rustflags = ["--cfg=first"]') }
            $null = New-RustTestPreparation -RepositoryRoot $workspace -PrerequisitePath $fixture.record -TestTarget git
            $null = Read-RustTestPreparation -RepositoryRoot $workspace -PrerequisitePath $fixture.record -TestTarget git
            switch ($Change) {
                'content' {
                    $stamp = [IO.File]::GetLastWriteTimeUtc($config)
                    [IO.File]::WriteAllText($config, '[build] rustflags = ["--cfg=second"]')
                    [IO.File]::SetLastWriteTimeUtc($config, $stamp)
                }
                'timestamp' {
                    # A full day is distinct even on coarse timestamp filesystems.
                    [IO.File]::SetLastWriteTimeUtc($config, [IO.File]::GetLastWriteTimeUtc($config).AddDays(1))
                }
                'added' { [IO.File]::WriteAllText($config, '[build] rustflags = []') }
                'removed' { Remove-Item -LiteralPath $config }
                'home selection' {
                    $env:CARGO_HOME = Join-Path $isolated 'another-home'
                    New-Item -ItemType Directory -Path $env:CARGO_HOME -Force | Out-Null
                    Copy-Item -LiteralPath $config -Destination $env:CARGO_HOME
                }
            }
            { Read-RustTestPreparation -RepositoryRoot $workspace -PrerequisitePath $fixture.record -TestTarget git } |
                Should -Throw '*cargoConfigurationSha256*'
            $script:CargoCalls.Count | Should -Be 1
            Get-Content -LiteralPath $fixture.record -Raw | Should -Not -Match '\[build\]|--cfg=first'
        } finally {
            $env:CARGO_HOME = $originalHome
        }
    }

    It 'resolves relative Cargo home against the workspace rather than the caller directory' {
        $originalHome = $env:CARGO_HOME
        try {
            $env:CARGO_HOME = 'relative-home'
            $before = Get-RustCargoConfigurationHash -RepositoryRoot $fixture.root
            Push-Location -LiteralPath $fixture.root
            try {
                Get-RustCargoConfigurationHash -RepositoryRoot $fixture.root | Should -Be $before
            } finally {
                Pop-Location
            }
        } finally {
            $env:CARGO_HOME = $originalHome
        }
    }

    It 'binds the default Cargo home when CARGO_HOME is unset' {
        $originalCargoHome = $env:CARGO_HOME
        $homeVariable = if ($IsWindows) { 'USERPROFILE' } else { 'HOME' }
        $originalUserHome = [Environment]::GetEnvironmentVariable($homeVariable)
        $userHome = Join-Path $fixture.root 'user-home'
        $configDirectory = Join-Path $userHome '.cargo'
        New-Item -ItemType Directory -Path $configDirectory -Force | Out-Null
        try {
            $env:CARGO_HOME = $null
            [Environment]::SetEnvironmentVariable($homeVariable, $userHome)
            $before = Get-RustCargoConfigurationHash -RepositoryRoot $fixture.root
            [IO.File]::WriteAllText((Join-Path $configDirectory 'config.toml'), 'configuration')
            Get-RustCargoConfigurationHash -RepositoryRoot $fixture.root | Should -Not -Be $before
        } finally {
            $env:CARGO_HOME = $originalCargoHome
            [Environment]::SetEnvironmentVariable($homeVariable, $originalUserHome)
        }
    }

    It 'rejects configuration freshness changes during preparation without publishing' {
        $originalHome = $env:CARGO_HOME
        $cargoHome = Join-Path $fixture.root 'cargo-home'
        New-Item -ItemType Directory -Path $cargoHome -Force | Out-Null
        $config = Join-Path $cargoHome 'config'
        [IO.File]::WriteAllText($config, 'configuration')
        try {
            $env:CARGO_HOME = $cargoHome
            $script:TouchDuringBuild = $config
            { New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget git } |
                Should -Throw '*sources changed during preparation*'
            Test-Path -LiteralPath $fixture.record | Should -BeFalse
        } finally {
            $env:CARGO_HOME = $originalHome
        }
    }

    It 'follows Cargo legacy precedence and ignores workspace member configuration' {
        $originalHome = $env:CARGO_HOME
        $cargoHome = Join-Path $fixture.root 'cargo-home'
        New-Item -ItemType Directory -Path $cargoHome -Force | Out-Null
        $legacy = Join-Path $cargoHome 'config'
        $modern = Join-Path $cargoHome 'config.toml'
        [IO.File]::WriteAllText($legacy, 'legacy')
        [IO.File]::WriteAllText($modern, 'modern')
        try {
            $env:CARGO_HOME = $cargoHome
            $before = Get-RustCargoConfigurationHash -RepositoryRoot $fixture.root
            [IO.File]::WriteAllText($modern, 'ignored')
            $member = Join-Path $fixture.root 'packages/member/.cargo'
            New-Item -ItemType Directory -Path $member -Force | Out-Null
            [IO.File]::WriteAllText((Join-Path $member 'config'), 'ignored')
            Get-RustCargoConfigurationHash -RepositoryRoot $fixture.root | Should -Be $before
            Remove-Item -LiteralPath $legacy
            Get-RustCargoConfigurationHash -RepositoryRoot $fixture.root | Should -Not -Be $before
        } finally {
            $env:CARGO_HOME = $originalHome
        }
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

    It 'never publishes a record after source freshness changes during compilation' {
        $script:TouchDuringBuild = Join-Path $fixture.root 'packages/ss-ai/tests/support/provider.rs'
        { New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } |
            Should -Throw '*sources changed during preparation*'

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

    It 'preserves selected targets and filters as separate Cargo arguments' {
        $prepared = @{ status = 'complete'; targetDirectory = 'target with spaces' }
        Mock Invoke-RustTestCargo {
            $script:CargoCalls.Add(@($ArgumentList))
            $PrerequisitePath | Should -Be $fixture.record
            $global:LASTEXITCODE = 0
        }

        Invoke-PreparedRustTests -Preparation $prepared -PrerequisitePath $fixture.record -TestTarget native,git -TestFilter 'filter with spaces'
        $script:CargoCalls[0] | Should -Be @(
            'test', '--workspace', '--all-features', '--locked', '--no-fail-fast',
            '--target-dir', 'target with spaces', '--test', 'native', '--test', 'git', 'filter with spaces'
        )
    }

    It 'still executes doctests after a provider-independent test failure' {
        Mock Invoke-RustTestCargo {
            $script:CargoCalls.Add(@($ArgumentList))
            # Only the ordinary tests fail; the subsequent doctest success must not erase that failure.
            $global:LASTEXITCODE = if ($ArgumentList -contains '--doc') { 0 } else { 17 }
        }
        $prepared = @{ status = 'providerFailed'; providerFailure = 'fixture'; targetDirectory = 'unused' }

        { Invoke-PreparedRustTests -Preparation $prepared -PrerequisitePath $fixture.record -TestFilter 'filter' } |
            Should -Throw '*Rust test execution failed*'
        $script:CargoCalls.Count | Should -Be 3
        $script:CargoCalls[1] | Should -Contain '--no-fail-fast'
        $script:CargoCalls[2] | Should -Contain '--doc'
        $script:CargoCalls[2][-1] | Should -Be 'filter'
    }

    It 'rejects missing records and mismatched selection instead of building on demand' {
        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*Prepare first*'

        $null = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget native
        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*selection*'
        $script:ProviderBuilds | Should -Be 1
    }

    It 'invalidates a same-version source edit' {
        $null = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        Add-Content -LiteralPath (Join-Path $fixture.root 'packages/ss-ai/tests/support/provider.rs') -Value 'changed'

        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*sourceSha256*'
    }

    It 'rejects malformed status <Label> before execution' -ForEach @(
        @{ Label = 'complete array'; Status = @('complete') }
        @{ Label = 'empty array'; Status = @() }
        @{ Label = 'null'; Status = $null }
        @{ Label = 'number'; Status = 1 }
        @{ Label = 'boolean'; Status = $true }
        @{ Label = 'object'; Status = @{ value = 'complete' } }
    ) {
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $prepared.status = $Status
        [IO.File]::WriteAllText($fixture.record, ($prepared | ConvertTo-Json -Depth 20))

        {
            $validated = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -AllowProviderFailure
            Invoke-PreparedRustTests -Preparation $validated -PrerequisitePath $fixture.record
        } | Should -Throw '*Incomplete or unsupported*'
        $script:CargoCalls.Count | Should -Be 1
        $script:ProviderBuilds | Should -Be 1
    }

    It 'rejects source freshness changes for <Asset> with rewrite = <Rewrite> before execution' -ForEach @(
        # All identity inputs need mtime coverage, not only Rust modules.
        foreach ($asset in @(
            'packages/ss-ai/tests/support/provider.rs',
            'packages/ss-ai/src/commits.graphql',
            'packages/ss-ai/tests/fixtures/github-approval.json',
            'packages/ss-ai-cargo-fuzz/seeds/dependencies.toml',
            'packages/ss-ai-cargo-fuzz/seeds/malformed.toml',
            'skills/ss-autoreapprove/significance.md',
            'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml'
        )) {
            foreach ($rewrite in @($false, $true)) {
                @{ Asset = $asset; Rewrite = $rewrite }
            }
        }
    ) {
        $assetPath = Join-Path $fixture.root $Asset
        New-Item -ItemType Directory -Path (Split-Path -Parent $assetPath) -Force | Out-Null
        [IO.File]::WriteAllText($assetPath, 'unchanged input')
        # Fixed, widely separated timestamps avoid sleeps and filesystem resolution assumptions.
        $before = [datetime]::new(2020, 1, 1, 0, 0, 0, [DateTimeKind]::Utc)
        $after = $before.AddDays(1)
        [IO.File]::SetLastWriteTimeUtc($assetPath, $before)
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        if ($Rewrite) {
            [IO.File]::WriteAllBytes($assetPath, [IO.File]::ReadAllBytes($assetPath))
        }
        [IO.File]::SetLastWriteTimeUtc($assetPath, $after)
        (Get-RustTestIdentity -RepositoryRoot $fixture.root).sourceSha256 | Should -Be $prepared.identity.sourceSha256

        {
            $validated = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
            Invoke-PreparedRustTests -Preparation $validated -PrerequisitePath $fixture.record
        } | Should -Throw '*sourceFreshnessSha256*'
        $script:CargoCalls.Count | Should -Be 1
        $script:ProviderBuilds | Should -Be 1
    }

    It 'rejects changed embedded input <Asset> before Cargo execution' -ForEach @(
        # Cover each embedded input kind, including the policy outside the package tree.
        @{ Asset = 'packages/ss-ai/src/commits.graphql' }
        @{ Asset = 'packages/ss-ai/tests/fixtures/github-approval.json' }
        @{ Asset = 'packages/ss-ai-cargo-fuzz/seeds/dependencies.toml' }
        @{ Asset = 'packages/ss-ai-cargo-fuzz/seeds/malformed.toml' }
        @{ Asset = 'skills/ss-autoreapprove/significance.md' }
    ) {
        $assetPath = Join-Path $fixture.root $Asset
        New-Item -ItemType Directory -Path (Split-Path -Parent $assetPath) -Force | Out-Null
        [IO.File]::WriteAllText($assetPath, 'original embedded input')
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $null = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record

        $stamp = [IO.File]::GetLastWriteTimeUtc($assetPath)
        [IO.File]::WriteAllText($assetPath, 'modified embedded input')
        [IO.File]::SetLastWriteTimeUtc($assetPath, $stamp)
        (Get-RustTestIdentity -RepositoryRoot $fixture.root).sourceFreshnessSha256 |
            Should -Be $prepared.identity.sourceFreshnessSha256
        {
            $validated = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
            Invoke-PreparedRustTests -Preparation $validated -PrerequisitePath $fixture.record
        } | Should -Throw '*sourceSha256*'

        $script:CargoCalls.Count | Should -Be 1
        foreach ($artifact in $prepared.executables) {
            (Get-TestArtifact -Path $artifact.path).sha256 | Should -Be $artifact.sha256
        }
    }

    It 'ignores unrelated TOML files and generated fuzz corpus inputs' {
        $paths = @(
            'packages/ss-ai-cargo-fuzz/corpus/manifest/dependencies.toml',
            'packages/ss-ai-cargo-fuzz/seeds/unrelated.toml',
            'packages/another/fuzz/seeds/malformed.toml'
        )
        foreach ($path in $paths) {
            $assetPath = Join-Path $fixture.root $path
            New-Item -ItemType Directory -Path (Split-Path -Parent $assetPath) -Force | Out-Null
            [IO.File]::WriteAllText($assetPath, 'original')
        }
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        foreach ($path in $paths) {
            $assetPath = Join-Path $fixture.root $path
            [IO.File]::WriteAllText($assetPath, 'modified')
            [IO.File]::SetLastWriteTimeUtc($assetPath, [datetime]::new(2020, 1, 1, 0, 0, 0, [DateTimeKind]::Utc))
        }
        $identity = Get-RustTestIdentity -RepositoryRoot $fixture.root
        $identity.sourceSha256 | Should -Be $prepared.identity.sourceSha256
        $identity.sourceFreshnessSha256 | Should -Be $prepared.identity.sourceFreshnessSha256
        $null = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
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
        } finally {
            $env:RUSTFLAGS = $previous
        }

        [IO.File]::WriteAllText($prepared.provider.executable.path, 'modified')
        { Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record } | Should -Throw '*Modified prepared artifact*'
    }

    It 'rejects malformed schema fields rather than coercing them' {
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $original = $prepared | ConvertTo-Json -Depth 20
        foreach ($change in @(
            { param($record) $record.schemaVersion = '1' }
            { param($record) $record.schemaVersion = 1 }
            { param($record) $record.identity.Remove('sourceFreshnessSha256') }
            { param($record) $record.identity.sourceFreshnessSha256 = @($record.identity.sourceFreshnessSha256) }
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

            # ReplaceFile supports Windows delete-sharing readers; an overwrite move does not.
            [IO.File]::Replace($replacement, $fixture.record, [NullString]::Value)
            $null = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        } finally {
            $stream.Dispose()
        }
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

    It 'fails publication under controlled sibling-lock contention without deleting the retained lock or publishing success' {
        $lockPath = "$($fixture.record).lock"
        $held = [IO.File]::Open($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        try {
            { New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget native } | Should -Throw
            Test-Path -LiteralPath $fixture.record | Should -BeFalse
            Test-Path -LiteralPath $lockPath | Should -BeTrue
            @(Get-ChildItem -LiteralPath $fixture.root -Directory -Filter 'rust-tests-*') | Should -BeNullOrEmpty
        } finally {
            $held.Dispose()
        }

        $null = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget native
        Test-Path -LiteralPath $lockPath | Should -BeTrue
        $null = Read-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record -TestTarget native
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

            # Each worker owns its commands and outputs; only the publication barrier is shared.
            function rustc {
                $global:LASTEXITCODE = 0
                if ($args[0] -eq '-vV') {
                    return 'rustc fixture host'
                }

                [IO.File]::WriteAllText($args[[array]::IndexOf($args, '-o') + 1], 'provider')
            }

            # Synchronize after artifact creation so neither worker publishes before the other builds.
            function cargo {
                $global:LASTEXITCODE = 0
                if ($args[0] -eq '--version') {
                    return 'cargo fixture'
                }

                $target = $args[[array]::IndexOf($args, '--target-dir') + 1]
                [IO.Directory]::CreateDirectory($target) | Out-Null
                $executable = Join-Path $target 'test'
                [IO.File]::WriteAllText($executable, 'test')
                $Barrier.SignalAndWait() | Out-Null
                @{
                    reason = 'compiler-artifact'
                    target = @{ name = 'fixture' }
                    fresh = $false
                    executable = $executable
                } | ConvertTo-Json -Compress
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
            try {
                $results += $first.EndInvoke($firstPending)
            } catch {
                $failures += $_
            }
            try {
                $results += $second.EndInvoke($secondPending)
            } catch {
                $failures += $_
            }

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

    It 'keeps unfiltered Cargo execution for doctests and preserves the prerequisite environment on failure' {
        $prepared = New-RustTestPreparation -RepositoryRoot $fixture.root -PrerequisitePath $fixture.record
        $previous = $env:SS_AI_RUST_TEST_PREREQUISITES
        $script:FailCargo = $true
        try {
            { Invoke-PreparedRustTests -Preparation $prepared -PrerequisitePath $fixture.record } | Should -Throw '*execution failed*'
            $script:CargoCalls[1] | Should -Not -Contain '--test'
            $script:CargoCalls[1] | Should -Not -Contain '--tests'
            $script:CargoCalls[1] | Should -Not -Contain '--no-run'
            $env:SS_AI_RUST_TEST_PREREQUISITES | Should -Be $previous
        } finally {
            $env:SS_AI_RUST_TEST_PREREQUISITES = $previous
        }
    }
}

Describe 'Cargo child environment' {
    It 'forwards child output, diagnostics and exit code <ExitCode>' -ForEach @(
        @{ ExitCode = 0 }, @{ ExitCode = 17 }
    ) {
        Mock New-RustTestCargoStartInfo {
            $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
            $start.UseShellExecute = $false
            $start.RedirectStandardOutput = $true
            $start.RedirectStandardError = $true
            $code = @"
#Requires -Version 7.6
Set-StrictMode -Version Latest
`$ErrorActionPreference = 'Stop'
`$PSNativeCommandUseErrorActionPreference = `$true
[Console]::Out.WriteLine('first')
[Console]::Out.WriteLine()
[Console]::Out.Write('last')
[Console]::Error.Write('diagnostic')
exit $ExitCode
"@
            foreach ($argument in @('-NoProfile', '-NonInteractive', '-EncodedCommand',
                [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code)))) {
                $start.ArgumentList.Add($argument)
            }
            return $start
        }

        # Capture the console sink used for native diagnostics without mixing it with pipeline output.
        $originalError = [Console]::Error
        $diagnostics = [IO.StringWriter]::new()
        try {
            [Console]::SetError($diagnostics)
            $output = @(Invoke-RustTestCargo -ArgumentList @('test') -PrerequisitePath 'unused')
            $LASTEXITCODE | Should -Be $ExitCode
            $output | Should -Be @('first', '', 'last')
            $diagnostics.ToString() | Should -Be 'diagnostic'
        } finally {
            [Console]::SetError($originalError)
            $diagnostics.Dispose()
        }
    }

    It 'preserves exact arguments and the caller working directory' {
        Mock Get-Command { [pscustomobject]@{ Source = 'fixture-cargo' } } -ParameterFilter { $Name -eq 'cargo' }
        $arguments = @('test', '--target-dir', 'directory with spaces', '--test', 'git', 'filter "quoted"')
        $start = New-RustTestCargoStartInfo -ArgumentList $arguments -PrerequisitePath 'record with spaces.json'
        @($start.ArgumentList) | Should -Be $arguments
        $start.WorkingDirectory | Should -Be (Get-Location).Path
        $start.Environment['SS_AI_RUST_TEST_PREREQUISITES'] | Should -Be 'record with spaces.json'
    }

    It 'isolates overlapping executions and preserves parent value <ParentValue>' -ForEach @(
        @{ ParentValue = $null }, @{ ParentValue = 'parent-only.json' }
    ) {
        $library = Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts/lib/RustTestPreparation.ps1'
        $previous = $env:SS_AI_RUST_TEST_PREREQUISITES
        # The coordinator joins both workers before launch and after completion. Neither
        # worker can finish its invocation while the other is still constructing its child.
        $barrier = [Threading.Barrier]::new(3)
        $worker = {
            param($Library, $Record, $Barrier, $Executable)
            Set-StrictMode -Version Latest
            $ErrorActionPreference = 'Stop'
            $PSNativeCommandUseErrorActionPreference = $true
            . $Library
            $builder = ${function:New-RustTestCargoStartInfo}

            # Cargo discovery is independent of the installed toolchain for this child fixture.
            function Get-Command { [pscustomobject]@{ Source = $Executable } }

            # Retain production environment construction but substitute a real PowerShell child
            # for Cargo, so this test observes inherited environment without invoking a compiler.
            function New-RustTestCargoStartInfo {
                param($ArgumentList, $PrerequisitePath)
                $start = & $builder -ArgumentList $ArgumentList -PrerequisitePath $PrerequisitePath
                $start.ArgumentList.Clear()
                $code = {
                    #Requires -Version 7.6
                    Set-StrictMode -Version Latest
                    $ErrorActionPreference = 'Stop'
                    $PSNativeCommandUseErrorActionPreference = $true
                    $env:SS_AI_RUST_TEST_PREREQUISITES
                }.ToString()
                foreach ($argument in @('-NoProfile', '-NonInteractive', '-EncodedCommand',
                    [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code)))) {
                    $start.ArgumentList.Add($argument)
                }
                $Barrier.SignalAndWait() | Out-Null
                $Barrier.SignalAndWait() | Out-Null
                return $start
            }

            try {
                Invoke-PreparedRustTests -Preparation @{ status = 'complete'; targetDirectory = 'unused' } -PrerequisitePath $Record
            } finally {
                $Barrier.SignalAndWait() | Out-Null
            }
        }

        $first = [PowerShell]::Create()
        $second = [PowerShell]::Create()
        try {
            # Set the baseline before starting any worker, covering both absent and existing values.
            $env:SS_AI_RUST_TEST_PREREQUISITES = $ParentValue
            $executable = (Get-Process -Id $PID).Path
            $records = @((Join-Path $TestDrive 'first record.json'), (Join-Path $TestDrive 'second record.json'))
            $null = $first.AddScript($worker).AddArgument($library).AddArgument($records[0]).AddArgument($barrier).AddArgument($executable)
            $null = $second.AddScript($worker).AddArgument($library).AddArgument($records[1]).AddArgument($barrier).AddArgument($executable)
            $firstPending = $first.BeginInvoke()
            $secondPending = $second.BeginInvoke()
            $barrier.SignalAndWait() | Out-Null
            $during = $env:SS_AI_RUST_TEST_PREREQUISITES
            $barrier.SignalAndWait() | Out-Null
            $barrier.SignalAndWait() | Out-Null
            $firstResult = @($first.EndInvoke($firstPending))
            $secondResult = @($second.EndInvoke($secondPending))

            $first.Streams.Error | Should -BeNullOrEmpty
            $second.Streams.Error | Should -BeNullOrEmpty
            $firstResult | Should -Be @($records[0])
            $secondResult | Should -Be @($records[1])
            $during | Should -Be $ParentValue
            $env:SS_AI_RUST_TEST_PREREQUISITES | Should -Be $ParentValue
        } finally {
            $first.Dispose()
            $second.Dispose()
            $barrier.Dispose()
            $env:SS_AI_RUST_TEST_PREREQUISITES = $previous
        }
    }
}
