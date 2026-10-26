#Requires -Version 7.6

# Keep phase reporting outside command output so Cargo artifact JSON remains parseable.
function Invoke-TestPhase {
    param([string] $Name, [scriptblock] $Action)
    $watch = [Diagnostics.Stopwatch]::StartNew()
    Write-Host "Phase: $Name"
    try { & $Action } finally {
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
    } finally { $stream.Dispose() }
}

# Bind reusable prerequisites to actual source bytes, including untracked Rust inputs.
function Get-RustTestIdentity {
    param([string] $RepositoryRoot)
    $files = @(
        Get-ChildItem -LiteralPath $RepositoryRoot -File |
            Where-Object Name -In @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml')
        foreach ($directory in @('packages', '.cargo')) {
            $path = Join-Path $RepositoryRoot $directory
            if (Test-Path -LiteralPath $path) {
                Get-ChildItem -LiteralPath $path -Recurse -File |
                    Where-Object { $_.Extension -eq '.rs' -or $_.Name -in @('Cargo.toml', 'config', 'config.toml') }
            }
        }
    )
    $entries = foreach ($file in ($files | Sort-Object FullName)) {
        [IO.Path]::GetRelativePath($RepositoryRoot, $file.FullName) + ':' +
            (Get-RustTestFileHash -Path $file.FullName)
    }
    $source = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData(
        [Text.Encoding]::UTF8.GetBytes(($entries -join "`n"))))
    $rustc = (& rustc -vV) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Reading the Rust toolchain identity failed.' }
    $cargo = (& cargo --version) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Reading the Cargo identity failed.' }
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
    @{ path = $Path; sha256 = Get-RustTestFileHash -Path $Path }
}

# Validate record shapes without PowerShell's usual string/number coercion.
function Assert-RustRecordFields {
    param($Record, [string[]] $Fields)
    if ($Record -isnot [System.Collections.IDictionary] -or $Record.Count -ne $Fields.Count) {
        throw 'Malformed Rust prerequisite record fields.'
    }
    foreach ($field in $Fields) {
        if (-not $Record.Contains($field)) { throw "Missing Rust prerequisite field: $field" }
    }
}

# One invocation owns all writable build outputs; publication is atomic and never replaces a run.
function New-RustTestPreparation {
    param([string] $RepositoryRoot, [string] $PrerequisitePath, [string[]] $TestTarget)
    if (-not [IO.Path]::IsPathFullyQualified($RepositoryRoot) -or -not (Test-Path -LiteralPath $RepositoryRoot -PathType Container)) {
        throw 'An existing absolute repository root is required.'
    }
    if (-not [IO.Path]::IsPathFullyQualified($PrerequisitePath)) { throw '-PrerequisitePath must be absolute.' }
    if (Test-Path -LiteralPath $PrerequisitePath) { throw "Prerequisite record already exists: $PrerequisitePath" }
    $parent = Split-Path -Parent $PrerequisitePath
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) { throw "Prerequisite parent does not exist: $parent" }
    $owned = Join-Path $parent ('rust-tests-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $owned | Out-Null
    try {
        $identity = Get-RustTestIdentity -RepositoryRoot $RepositoryRoot
        $providerSource = Join-Path $RepositoryRoot 'packages/ss-skills-util/tests/support/provider.rs'
        $provider = Join-Path $owned $(if ($IsWindows) { 'provider.exe' } else { 'provider' })
        $providerFailure = $null
        $requiresProvider = -not $TestTarget -or $TestTarget -contains 'native'
        if ($requiresProvider) {
            try {
                Invoke-TestPhase -Name 'prepare provider (compilerInvocations=1)' -Action {
                    & rustc --edition 2024 $providerSource -o $provider
                    if ($LASTEXITCODE -ne 0) { throw 'Preparing the native provider failed.' }
                }
            } catch {
                # A provider failure must not suppress compilation or tests with no provider dependency.
                $providerFailure = $_.ToString()
                Write-Host "Provider preparation failed; native tests are blocked: $providerFailure"
            }
        }
        $target = Join-Path $owned 'target'
        $selection = @()
        foreach ($name in $TestTarget) { $selection += @('--test', $name) }
        $artifacts = @(Invoke-TestPhase -Name 'compile selected Rust tests' -Action {
            & cargo test --workspace --all-features --locked --no-run --message-format=json --target-dir $target @selection |
                ForEach-Object {
                    $message = $_ | ConvertFrom-Json -AsHashtable
                    if ($message.reason -eq 'compiler-message') { Write-Host $message.message.rendered }
                    if ($message.reason -eq 'compiler-artifact') {
                        Write-Host "Cargo artifact: $($message.target.name); fresh=$($message.fresh)"
                        if ($message.executable) { Get-TestArtifact -Path $message.executable }
                    }
                }
            if ($LASTEXITCODE -ne 0) { throw 'Compiling Rust test prerequisites failed.' }
        })
        if ($artifacts.Count -eq 0) { throw 'Cargo returned no executable artifacts.' }
        $after = Get-RustTestIdentity -RepositoryRoot $RepositoryRoot
        if ($after.sourceSha256 -ne $identity.sourceSha256) { throw 'Rust sources changed during preparation.' }
        $record = @{
            schemaVersion = 1
            status = $(if ($providerFailure) { 'providerFailed' } else { 'complete' })
            providerFailure = $providerFailure
            identity = $identity
            testTargets = @($TestTarget | Where-Object { $null -ne $_ })
            ownedDirectory = $owned
            targetDirectory = $target
            provider = $(if ($requiresProvider -and -not $providerFailure) { @{
                executable = Get-TestArtifact -Path $provider
                source = Get-TestArtifact -Path $providerSource
            } } else { $null })
            executables = $artifacts
        }
        $temporary = Join-Path $owned 'prerequisites.json'
        [IO.File]::WriteAllText($temporary, ($record | ConvertTo-Json -Depth 20))
        # Validate the same consumer boundary before publishing a successful prerequisite.
        $null = Read-RustTestPreparation -RepositoryRoot $RepositoryRoot -PrerequisitePath $temporary -TestTarget $TestTarget -AllowProviderFailure
        [IO.File]::Move($temporary, $PrerequisitePath, $false)
        return $record
    } catch {
        Remove-Item -LiteralPath $owned -Recurse -Force
        throw
    }
}

# Reject absent, incomplete, changed-source, incompatible, or modified owned outputs before execution.
function Read-RustTestPreparation {
    param([string] $RepositoryRoot, [string] $PrerequisitePath, [string[]] $TestTarget, [switch] $AllowProviderFailure)
    if (-not [IO.Path]::IsPathFullyQualified($PrerequisitePath) -or
        -not (Test-Path -LiteralPath $PrerequisitePath -PathType Leaf)) {
        throw 'An existing absolute -PrerequisitePath is required; run -Phase Prepare first.'
    }
    $stream = Open-RustTestInput -Path $PrerequisitePath
    $reader = [IO.StreamReader]::new($stream)
    try {
        $record = $reader.ReadToEnd() | ConvertFrom-Json -AsHashtable
    } finally { $reader.Dispose() }
    Assert-RustRecordFields -Record $record -Fields @(
        'schemaVersion', 'status', 'providerFailure', 'identity', 'testTargets',
        'ownedDirectory', 'targetDirectory', 'provider', 'executables'
    )
    if ($record -isnot [System.Collections.IDictionary] -or
        $record.schemaVersion -isnot [long] -or $record.schemaVersion -ne 1 -or
        ($record.status -cne 'complete' -and -not ($AllowProviderFailure -and $record.status -ceq 'providerFailed'))) {
        throw 'Incomplete or unsupported Rust prerequisites.'
    }
    if ($record.testTargets -isnot [array] -or $record.executables -isnot [array]) { throw 'Rust prerequisite selections and executables must be arrays.' }
    foreach ($name in $record.testTargets) {
        if ($name -isnot [string] -or [string]::IsNullOrWhiteSpace($name)) { throw 'Malformed Rust test target.' }
    }
    $requiresProvider = -not $TestTarget -or $TestTarget -contains 'native'
    if ($record.status -ceq 'providerFailed') {
        if (-not $requiresProvider -or $null -ne $record.provider -or
            $record.providerFailure -isnot [string] -or [string]::IsNullOrWhiteSpace($record.providerFailure)) {
            throw 'Malformed failed-provider prerequisites.'
        }
    } elseif ($null -ne $record.providerFailure -or ($requiresProvider -and -not $record.provider) -or
        (-not $requiresProvider -and $null -ne $record.provider)) {
        throw 'Incomplete provider prerequisites.'
    }
    $identity = Get-RustTestIdentity -RepositoryRoot $RepositoryRoot
    Assert-RustRecordFields -Record $record.identity -Fields @($identity.Keys)
    foreach ($key in @('repositoryRoot', 'sourceSha256', 'rustc', 'cargo', 'profile', 'features')) {
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
    if (($record.testTargets -join "`n") -cne ($TestTarget -join "`n")) { throw 'Prepared Rust test selection does not match -RustTest.' }
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
        if ($record.provider.source.path -cne (Join-Path $RepositoryRoot 'packages/ss-skills-util/tests/support/provider.rs') -or
            (Split-Path -Parent $record.provider.executable.path) -cne $record.ownedDirectory) {
            throw 'Provider prerequisite ownership paths do not match.'
        }
        $artifacts += @($record.provider.executable, $record.provider.source)
    }
    if (@($record.executables).Count -eq 0) { throw 'Rust prerequisites contain no test executables.' }
    foreach ($artifact in $artifacts) {
        Assert-RustRecordFields -Record $artifact -Fields @('path', 'sha256')
        if ($artifact.path -isnot [string] -or $artifact.sha256 -isnot [string] -or $artifact.sha256 -cnotmatch '^[0-9A-F]{64}$') {
            throw 'Malformed prepared artifact path or SHA-256.'
        }
        $actual = Get-TestArtifact -Path $artifact.path
        if ($actual.sha256 -cne $artifact.sha256) { throw "Modified prepared artifact: $($artifact.path)" }
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

# Cargo retains CARGO_BIN_EXE and doctest semantics; its freshness check must not generate new code.
function Invoke-PreparedRustTests {
    param([hashtable] $Preparation, [string] $PrerequisitePath, [string[]] $TestTarget, [string] $TestFilter)
    $previous = $env:SS_AI_RUST_TEST_PREREQUISITES
    try {
        $env:SS_AI_RUST_TEST_PREREQUISITES = $PrerequisitePath
        $selection = @()
        $includeDoctests = $false
        if ($Preparation.status -ceq 'providerFailed') {
            Write-Host "Blocked native tests: $($Preparation.providerFailure)"
            if (-not $TestTarget) {
                $metadata = & cargo metadata --no-deps --format-version 1 --locked | ConvertFrom-Json
                if ($LASTEXITCODE -ne 0) { throw 'Locating provider-independent Rust tests failed.' }
                $TestTarget = @($metadata.packages |
                    Where-Object id -In $metadata.workspace_members |
                    ForEach-Object targets | Where-Object { $_.kind -contains 'test' -and $_.name -ne 'native' } |
                    Select-Object -ExpandProperty name -Unique)
                $selection += @('--lib', '--bins')
                $includeDoctests = $true
            } else {
                $TestTarget = @($TestTarget | Where-Object { $_ -ne 'native' })
                if (-not $TestTarget) { throw 'All selected Rust tests are blocked by provider preparation failure.' }
            }
        }
        foreach ($name in $TestTarget) { $selection += @('--test', $name) }
        if ($TestFilter) { $selection += $TestFilter }
        & cargo test --workspace --all-features --locked --no-fail-fast --target-dir $Preparation.targetDirectory @selection
        $executionFailed = $LASTEXITCODE -ne 0
        if ($includeDoctests) {
            $filter = @()
            if ($TestFilter) { $filter += $TestFilter }
            & cargo test --workspace --all-features --locked --doc --target-dir $Preparation.targetDirectory @filter
            if ($LASTEXITCODE -ne 0) { $executionFailed = $true }
        }
        if ($executionFailed -or $Preparation.status -ceq 'providerFailed') { throw 'Rust test execution failed.' }
    } finally {
        $env:SS_AI_RUST_TEST_PREREQUISITES = $previous
    }
}
