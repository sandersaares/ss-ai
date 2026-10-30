#Requires -Version 7.6

# Keep phase reporting outside command output so Cargo artifact JSON remains parseable.
function Invoke-TestPhase {
    param(
        [string] $Name,
        [scriptblock] $Action
    )

    $watch = [Diagnostics.Stopwatch]::StartNew()
    Write-Host "Phase: $Name"

    try {
        & $Action
    } finally {
        Write-Host ("Phase completed: {0}; elapsedSeconds={1:F3}" -f $Name, $watch.Elapsed.TotalSeconds)
    }
}

# Readers permit concurrent validation and atomic replacement, including on Windows.
function Open-RustTestInput {
    param([string] $Path)

    [IO.FileStream]::new($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read,
        [IO.FileShare]::Read -bor [IO.FileShare]::Delete)
}

# Stream large executable hashes rather than loading each binary into memory.
function Get-RustTestFileHash {
    param([string] $Path)

    $stream = Open-RustTestInput -Path $Path
    try {
        [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($stream))
    } finally {
        $stream.Dispose()
    }
}

# Bind Cargo's root-to-ancestor configuration search and Cargo home without recording
# configuration text (which can contain credentials). Cargo prefers config over
# config.toml in each directory; workspace member directories are not searched.
function Get-RustCargoConfigurationHash {
    param([string] $RepositoryRoot)

    $homeSelection = [Environment]::GetEnvironmentVariable('CARGO_HOME')
    $cargoHome = if ([string]::IsNullOrEmpty($homeSelection)) {
        # Cargo's default follows the platform home environment, not PowerShell's
        # startup snapshot in $HOME, which can differ after an environment update.
        $userHome = [Environment]::GetEnvironmentVariable($(if ($IsWindows) { 'USERPROFILE' } else { 'HOME' }))
        if ([string]::IsNullOrEmpty($userHome)) { $userHome = $HOME }
        Join-Path $userHome '.cargo'
    } else {
        [IO.Path]::GetFullPath($homeSelection, $RepositoryRoot)
    }
    $directories = [Collections.Generic.List[string]]::new()
    $directory = [IO.DirectoryInfo]::new($RepositoryRoot)
    while ($null -ne $directory) {
        $directories.Add((Join-Path $directory.FullName '.cargo'))
        $directory = $directory.Parent
    }
    $directories.Add($cargoHome)
    $entries = @(
        # Preserve Cargo home selection even if both homes have identical configuration.
        @{ home = $cargoHome; selection = $homeSelection }
        foreach ($path in $directories) {
            $legacy = Join-Path $path 'config'
            $modern = Join-Path $path 'config.toml'
            $selected = if (Test-Path -LiteralPath $legacy -PathType Leaf) { $legacy } else { $modern }
            if (Test-Path -LiteralPath $selected -PathType Leaf) {
                $file = Get-Item -LiteralPath $selected
                @{
                    path = $selected
                    sha256 = Get-RustTestFileHash -Path $selected
                    ticks = $file.LastWriteTimeUtc.Ticks
                }
            } else {
                @{ path = $path; absent = $true }
            }
        }
    )
    # Ordered serialization keeps the aggregate deterministic across independent processes.
    $lines = foreach ($entry in $entries) {
        foreach ($key in @($entry.Keys | Sort-Object)) {
            @($key, $entry[$key]) | ConvertTo-Json -Compress
        }
    }
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData(
        [Text.Encoding]::UTF8.GetBytes(($lines -join "`n"))))
}

# Bind reusable prerequisites to source bytes and Cargo freshness inputs, including embedded files.
function Get-RustTestIdentity {
    param([string] $RepositoryRoot)

    $files = @(
        Get-ChildItem -LiteralPath $RepositoryRoot -File |
            Where-Object Name -In @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml')
        foreach ($directory in @('packages', '.cargo')) {
            $path = Join-Path $RepositoryRoot $directory
            if (Test-Path -LiteralPath $path) {
                Get-ChildItem -LiteralPath $path -Recurse -File |
                    # Rust embeds the GraphQL queries and JSON service fixtures at compile time.
                    Where-Object {
                        $_.Extension -in @('.rs', '.graphql', '.json') -or
                            $_.Name -in @('Cargo.toml', 'config', 'config.toml')
                    }
            }
        }

        # Manifest parser tests embed these seeds; generated fuzz corpora are not build inputs.
        foreach ($seed in @(
            'packages/ss-ai-cargo/fuzz/seeds/dependencies.toml',
            'packages/ss-ai-cargo/fuzz/seeds/malformed.toml'
        )) {
            $path = Join-Path $RepositoryRoot $seed
            if (Test-Path -LiteralPath $path -PathType Leaf) {
                Get-Item -LiteralPath $path
            }
        }

        # Reapproval tests embed the shared policy outside the Cargo package tree.
        $policy = Join-Path $RepositoryRoot 'skills/ss-autoreapprove/significance.md'
        if (Test-Path -LiteralPath $policy -PathType Leaf) {
            Get-Item -LiteralPath $policy
        }
    )

    $files = @($files | Sort-Object FullName)
    $entries = foreach ($file in $files) {
        [IO.Path]::GetRelativePath($RepositoryRoot, $file.FullName) + ':' +
            (Get-RustTestFileHash -Path $file.FullName)
    }
    $source = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData(
        [Text.Encoding]::UTF8.GetBytes(($entries -join "`n"))))

    # Cargo can rebuild byte-identical saves based on mtime; keep that identity separate
    # from content so validation explains why an otherwise unchanged source tree is stale.
    $freshnessEntries = foreach ($file in $files) {
        [IO.Path]::GetRelativePath($RepositoryRoot, $file.FullName) + ':' +
            $file.LastWriteTimeUtc.Ticks.ToString([Globalization.CultureInfo]::InvariantCulture)
    }
    $freshness = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData(
        [Text.Encoding]::UTF8.GetBytes(($freshnessEntries -join "`n"))))

    $rustc = (& rustc -vV) -join "`n"
    if ($LASTEXITCODE -ne 0) {
        throw 'Reading the Rust toolchain identity failed.'
    }

    $cargo = (& cargo --version) -join "`n"
    if ($LASTEXITCODE -ne 0) {
        throw 'Reading the Cargo identity failed.'
    }

    $environment = @{}
    # Preserve null versus empty because both toolchain wrappers and flags affect compatibility.
    foreach ($name in @(
        'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTDOCFLAGS', 'CARGO_BUILD_TARGET',
        'CARGO_PROFILE_DEV_DEBUG', 'CARGO_PROFILE_TEST_DEBUG', 'CARGO_INCREMENTAL',
        'RUSTC', 'RUSTUP_TOOLCHAIN', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'
    )) {
        $environment[$name] = [Environment]::GetEnvironmentVariable($name)
    }
    foreach ($entry in (Get-ChildItem Env: | Where-Object Name -Match '^CARGO_(BUILD|TARGET|PROFILE)_')) {
        $environment[$entry.Name] = $entry.Value
    }

    @{
        repositoryRoot = $RepositoryRoot
        sourceSha256 = $source
        sourceFreshnessSha256 = $freshness
        cargoConfigurationSha256 = Get-RustCargoConfigurationHash -RepositoryRoot $RepositoryRoot
        rustc = $rustc
        cargo = $cargo
        environment = $environment
        profile = 'test'
        features = 'all'
    }
}

# Artifact records retain exact owned outputs rather than guessing Cargo's hashed filenames.
function Get-TestArtifact {
    param([string] $Path)

    if (-not [IO.Path]::IsPathFullyQualified($Path) -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Missing or nonabsolute prepared artifact: $Path"
    }

    @{
        path = $Path
        sha256 = Get-RustTestFileHash -Path $Path
    }
}

# Validate record shapes without PowerShell's usual string/number coercion.
function Assert-RustRecordFields {
    param(
        $Record,
        [string[]] $Fields
    )

    if ($Record -isnot [System.Collections.IDictionary] -or $Record.Count -ne $Fields.Count) {
        throw 'Malformed Rust prerequisite record fields.'
    }
    foreach ($field in $Fields) {
        if (-not $Record.Contains($field)) {
            throw "Missing Rust prerequisite field: $field"
        }
    }
}

# One invocation owns all writable build outputs; publication is atomic and never replaces a run.
function New-RustTestPreparation {
    param(
        [string] $RepositoryRoot,
        [string] $PrerequisitePath,
        [string[]] $TestTarget
    )

    if (-not [IO.Path]::IsPathFullyQualified($RepositoryRoot) -or -not (Test-Path -LiteralPath $RepositoryRoot -PathType Container)) {
        throw 'An existing absolute repository root is required.'
    }
    if (-not [IO.Path]::IsPathFullyQualified($PrerequisitePath)) {
        throw '-PrerequisitePath must be absolute.'
    }
    if (Test-Path -LiteralPath $PrerequisitePath) {
        throw "Prerequisite record already exists: $PrerequisitePath"
    }

    $parent = Split-Path -Parent $PrerequisitePath
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
        throw "Prerequisite parent does not exist: $parent"
    }

    $owned = Join-Path $parent ('rust-tests-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $owned | Out-Null
    try {
        $identity = Get-RustTestIdentity -RepositoryRoot $RepositoryRoot

        $providerSource = Join-Path $RepositoryRoot 'packages/ss-ai/tests/support/provider.rs'
        $provider = Join-Path $owned $(if ($IsWindows) { 'provider.exe' } else { 'provider' })
        $providerFailure = $null
        $requiresProvider = -not $TestTarget -or $TestTarget -contains 'native' -or $TestTarget -contains 'capture_parity'
        if ($requiresProvider) {
            try {
                Invoke-TestPhase -Name 'prepare provider (compilerInvocations=1)' -Action {
                    & rustc --edition 2024 $providerSource -o $provider
                    if ($LASTEXITCODE -ne 0) {
                        throw 'Preparing the native provider failed.'
                    }
                }
            } catch {
                # A provider failure must not suppress compilation or tests with no provider dependency.
                $providerFailure = $_.ToString()
                Write-Host "Provider preparation failed; native tests are blocked: $providerFailure"
            }
        }

        $target = Join-Path $owned 'target'
        $selection = @()
        foreach ($name in $TestTarget) {
            $selection += @('--test', $name)
        }

        $artifacts = @(Invoke-TestPhase -Name 'compile selected Rust tests' -Action {
            & cargo test --workspace --all-features --locked --no-run --message-format=json --target-dir $target @selection |
                ForEach-Object {
                    $message = $_ | ConvertFrom-Json -AsHashtable
                    if ($message.reason -eq 'compiler-message') {
                        Write-Host $message.message.rendered
                    }
                    if ($message.reason -eq 'compiler-artifact') {
                        Write-Host "Cargo artifact: $($message.target.name); fresh=$($message.fresh)"
                        if ($message.executable) {
                            Get-TestArtifact -Path $message.executable
                        }
                    }
                }
            if ($LASTEXITCODE -ne 0) {
                throw 'Compiling Rust test prerequisites failed.'
            }
        })
        if ($artifacts.Count -eq 0) {
            throw 'Cargo returned no executable artifacts.'
        }

        # A build must not publish artifacts against the identity of an earlier source tree.
        $after = Get-RustTestIdentity -RepositoryRoot $RepositoryRoot
        if ($after.sourceSha256 -ne $identity.sourceSha256 -or
            $after.sourceFreshnessSha256 -ne $identity.sourceFreshnessSha256 -or
            $after.cargoConfigurationSha256 -ne $identity.cargoConfigurationSha256) {
            throw 'Rust sources changed during preparation.'
        }

        $record = @{
            # The consumer requires source and effective external configuration identity (DEVELOPMENT.md).
            schemaVersion = 3
            status = $(if ($providerFailure) { 'providerFailed' } else { 'complete' })
            providerFailure = $providerFailure
            identity = $identity
            testTargets = @($TestTarget | Where-Object { $null -ne $_ })
            ownedDirectory = $owned
            targetDirectory = $target
            provider = $(if ($requiresProvider -and -not $providerFailure) {
                @{
                    executable = Get-TestArtifact -Path $provider
                    source = Get-TestArtifact -Path $providerSource
                }
            } else {
                $null
            })
            executables = $artifacts
        }

        $temporary = Join-Path $owned 'prerequisites.json'
        [IO.File]::WriteAllText($temporary, ($record | ConvertTo-Json -Depth 20))

        # Validate the same consumer boundary before publishing a successful prerequisite.
        $null = Read-RustTestPreparation -RepositoryRoot $RepositoryRoot -PrerequisitePath $temporary -TestTarget $TestTarget -AllowProviderFailure

        # Serialize the existence check and atomic publication; competing producers fail explicitly.
        # Retain the sibling inode so another producer cannot lock a different replacement file.
        $publicationLock = [IO.File]::Open("$PrerequisitePath.lock", [IO.FileMode]::OpenOrCreate,
            [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        try {
            if (Test-Path -LiteralPath $PrerequisitePath) {
                throw "Prerequisite record already exists: $PrerequisitePath"
            }
            [IO.File]::Move($temporary, $PrerequisitePath, $false)
        } finally {
            $publicationLock.Dispose()
        }

        return $record
    } catch {
        Remove-Item -LiteralPath $owned -Recurse -Force
        throw
    }
}

# Reject absent, incomplete, changed-source, incompatible, or modified owned outputs before execution.
function Read-RustTestPreparation {
    param(
        [string] $RepositoryRoot,
        [string] $PrerequisitePath,
        [string[]] $TestTarget,
        [switch] $AllowProviderFailure
    )

    if (-not [IO.Path]::IsPathFullyQualified($PrerequisitePath) -or
        -not (Test-Path -LiteralPath $PrerequisitePath -PathType Leaf)) {
        throw 'An existing absolute -PrerequisitePath is required; run -Phase Prepare first.'
    }

    $stream = Open-RustTestInput -Path $PrerequisitePath
    $reader = [IO.StreamReader]::new($stream)
    try {
        $record = $reader.ReadToEnd() | ConvertFrom-Json -AsHashtable
    } finally {
        $reader.Dispose()
    }

    Assert-RustRecordFields -Record $record -Fields @(
        'schemaVersion', 'status', 'providerFailure', 'identity', 'testTargets',
        'ownedDirectory', 'targetDirectory', 'provider', 'executables'
    )
    if ($record -isnot [System.Collections.IDictionary] -or
        $record.schemaVersion -isnot [long] -or $record.schemaVersion -ne 3 -or
        $record.status -isnot [string] -or
        ($record.status -cne 'complete' -and -not ($AllowProviderFailure -and $record.status -ceq 'providerFailed'))) {
        throw 'Incomplete or unsupported Rust prerequisites.'
    }
    if ($record.testTargets -isnot [array] -or $record.executables -isnot [array]) {
        throw 'Rust prerequisite selections and executables must be arrays.'
    }
    foreach ($name in $record.testTargets) {
        if ($name -isnot [string] -or [string]::IsNullOrWhiteSpace($name)) {
            throw 'Malformed Rust test target.'
        }
    }

    $requiresProvider = -not $TestTarget -or $TestTarget -contains 'native' -or $TestTarget -contains 'capture_parity'
    if ($record.status -ceq 'providerFailed') {
        if (-not $requiresProvider -or $null -ne $record.provider -or
            $record.providerFailure -isnot [string] -or [string]::IsNullOrWhiteSpace($record.providerFailure)) {
            throw 'Malformed failed-provider prerequisites.'
        }
    } elseif ($null -ne $record.providerFailure -or ($requiresProvider -and -not $record.provider) -or
        (-not $requiresProvider -and $null -ne $record.provider)) {
        throw 'Incomplete provider prerequisites.'
    }

    # Compare identity before trusting owned artifacts or launching Cargo.
    $identity = Get-RustTestIdentity -RepositoryRoot $RepositoryRoot
    Assert-RustRecordFields -Record $record.identity -Fields @($identity.Keys)
    foreach ($key in @('repositoryRoot', 'sourceSha256', 'sourceFreshnessSha256', 'cargoConfigurationSha256', 'rustc', 'cargo', 'profile', 'features')) {
        if ($record.identity[$key] -isnot [string] -or $record.identity[$key] -cne $identity[$key]) {
            throw "Stale or incompatible Rust prerequisites: $key"
        }
    }
    Assert-RustRecordFields -Record $record.identity.environment -Fields @($identity.environment.Keys)
    foreach ($key in $identity.environment.Keys) {
        $value = $record.identity.environment[$key]
        if (($null -ne $value -and $value -isnot [string]) -or $value -cne $identity.environment[$key]) {
            throw "Incompatible Rust prerequisite environment: $key"
        }
    }
    if (($record.testTargets -join "`n") -cne ($TestTarget -join "`n")) {
        throw 'Prepared Rust test selection does not match -RustTest.'
    }

    # Validate ownership separately from content so an unrelated artifact cannot be borrowed.
    foreach ($directory in @($record.ownedDirectory, $record.targetDirectory)) {
        if (-not [IO.Path]::IsPathFullyQualified($directory) -or -not (Test-Path -LiteralPath $directory -PathType Container)) {
            throw "Missing or nonabsolute prepared directory: $directory"
        }
    }
    if ($record.targetDirectory -cne (Join-Path $record.ownedDirectory 'target')) {
        throw 'Rust prerequisite ownership paths do not match.'
    }

    $artifacts = @($record.executables)
    if ($record.provider) {
        Assert-RustRecordFields -Record $record.provider -Fields @('source', 'executable')
        if ($record.provider.source.path -cne (Join-Path $RepositoryRoot 'packages/ss-ai/tests/support/provider.rs') -or
            (Split-Path -Parent $record.provider.executable.path) -cne $record.ownedDirectory) {
            throw 'Provider prerequisite ownership paths do not match.'
        }
        $artifacts += @($record.provider.executable, $record.provider.source)
    }
    if (@($record.executables).Count -eq 0) {
        throw 'Rust prerequisites contain no test executables.'
    }

    foreach ($artifact in $artifacts) {
        Assert-RustRecordFields -Record $artifact -Fields @('path', 'sha256')
        if ($artifact.path -isnot [string] -or $artifact.sha256 -isnot [string] -or $artifact.sha256 -cnotmatch '^[0-9A-F]{64}$') {
            throw 'Malformed prepared artifact path or SHA-256.'
        }
        $actual = Get-TestArtifact -Path $artifact.path
        if ($actual.sha256 -cne $artifact.sha256) {
            throw "Modified prepared artifact: $($artifact.path)"
        }
    }

    foreach ($artifact in $record.executables) {
        $relative = [IO.Path]::GetRelativePath($record.targetDirectory, $artifact.path)
        if ([IO.Path]::IsPathFullyQualified($relative) -or $relative -eq '..' -or
            $relative.StartsWith('..' + [IO.Path]::DirectorySeparatorChar)) {
            throw "Test executable is outside its owned target directory: $($artifact.path)"
        }
    }

    return $record
}

# Give each Cargo child its own prerequisite record without changing other runspaces' environment.
function New-RustTestCargoStartInfo {
    param([string[]] $ArgumentList, [string] $PrerequisitePath)

    $cargo = Get-Command cargo -CommandType Application | Select-Object -First 1
    $start = [Diagnostics.ProcessStartInfo]::new($cargo.Source)
    $start.UseShellExecute = $false
    $start.WorkingDirectory = (Get-Location).Path
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.Environment['SS_AI_RUST_TEST_PREREQUISITES'] = $PrerequisitePath
    foreach ($argument in $ArgumentList) {
        $start.ArgumentList.Add($argument)
    }
    return $start
}

# Keep native output and exit status separate so execution can aggregate test and doctest failures.
function Invoke-RustTestCargo {
    param([string[]] $ArgumentList, [string] $PrerequisitePath)

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = New-RustTestCargoStartInfo -ArgumentList $ArgumentList -PrerequisitePath $PrerequisitePath
    try {
        if (-not $process.Start()) { throw 'Could not start Cargo tests.' }
        # Drain both pipes concurrently: either stream can exceed the operating system pipe buffer.
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        [Console]::Error.Write($stderr.GetAwaiter().GetResult())
        $output = $stdout.GetAwaiter().GetResult()
        if ($output.Length -gt 0) {
            # Native pipeline output contains lines, not a trailing synthetic empty record.
            ($output -replace '\r?\n$', '') -split '\r?\n'
        }
        $global:LASTEXITCODE = $process.ExitCode
    } finally {
        $process.Dispose()
    }
}

# Cargo retains CARGO_BIN_EXE and doctest semantics; its freshness check must not generate new code.
function Invoke-PreparedRustTests {
    param(
        [hashtable] $Preparation,
        [string] $PrerequisitePath,
        [string[]] $TestTarget,
        [string] $TestFilter
    )

    $selection = @()
    $includeDoctests = $false

    if ($Preparation.status -ceq 'providerFailed') {
        Write-Host "Blocked native tests: $($Preparation.providerFailure)"
        if (-not $TestTarget) {
            $metadata = & cargo metadata --no-deps --format-version 1 --locked | ConvertFrom-Json
            if ($LASTEXITCODE -ne 0) {
                throw 'Locating provider-independent Rust tests failed.'
            }

            $TestTarget = @($metadata.packages |
                Where-Object id -In $metadata.workspace_members |
                ForEach-Object targets | Where-Object { $_.kind -contains 'test' -and $_.name -notin @('native', 'capture_parity') } |
                Select-Object -ExpandProperty name -Unique)
            $selection += @('--lib', '--bins')
            # Explicit target selectors omit doctests, so run those separately below.
            $includeDoctests = $true
        } else {
            $TestTarget = @($TestTarget | Where-Object { $_ -notin @('native', 'capture_parity') })
            if (-not $TestTarget) {
                throw 'All selected Rust tests are blocked by provider preparation failure.'
            }
        }
    }

    foreach ($name in $TestTarget) {
        $selection += @('--test', $name)
    }
    if ($TestFilter) {
        $selection += $TestFilter
    }

    Invoke-RustTestCargo -PrerequisitePath $PrerequisitePath -ArgumentList (@(
        'test', '--workspace', '--all-features', '--locked', '--no-fail-fast', '--target-dir', $Preparation.targetDirectory
    ) + $selection)
    $executionFailed = $LASTEXITCODE -ne 0
    if ($includeDoctests) {
        $filter = @()
        if ($TestFilter) {
            $filter += $TestFilter
        }
        Invoke-RustTestCargo -PrerequisitePath $PrerequisitePath -ArgumentList (@(
            'test', '--workspace', '--all-features', '--locked', '--doc', '--target-dir', $Preparation.targetDirectory
        ) + $filter)
        if ($LASTEXITCODE -ne 0) {
            $executionFailed = $true
        }
    }

    if ($executionFailed -or $Preparation.status -ceq 'providerFailed') {
        throw 'Rust test execution failed.'
    }
}
