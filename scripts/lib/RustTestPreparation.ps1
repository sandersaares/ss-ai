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
            Where-Object Name -In @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'Install.ps1')
        $installerLibraries = Join-Path $RepositoryRoot 'scripts/lib'
        if (Test-Path -LiteralPath $installerLibraries) {
            Get-ChildItem -LiteralPath $installerLibraries -File -Filter '*.ps1'
        }
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
    # Native release installation requires the preparing host's CPU capabilities, not only its triple.
    $nativeCpu = (& rustc --print cfg -C target-cpu=native) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Reading the native CPU compilation identity failed.' }
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
        nativeCpu = $nativeCpu
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
        $installationExecutables = [Collections.Generic.List[string]]::new()
        $artifacts = @(Invoke-TestPhase -Name 'compile selected Rust tests' -Action {
            & cargo test --workspace --all-features --locked --no-run --message-format=json --target-dir $target @selection |
                ForEach-Object {
                    $message = $_ | ConvertFrom-Json -AsHashtable
                    if ($message.reason -eq 'compiler-message') { Write-Host $message.message.rendered }
                    if ($message.reason -eq 'compiler-artifact') {
                        Write-Host "Cargo artifact: $($message.target.name); fresh=$($message.fresh)"
                        if ($message.target.name -ceq 'installation' -and $message.profile.test -and $message.executable) {
                            $installationExecutables.Add($message.executable)
                        }
                        if ($message.executable) { Get-TestArtifact -Path $message.executable }
                    }
                }
            if ($LASTEXITCODE -ne 0) { throw 'Compiling Rust test prerequisites failed.' }
        })
        if ($artifacts.Count -eq 0) { throw 'Cargo returned no executable artifacts.' }
        $installation = $null
        $installationFailure = $null
        if (-not $TestTarget -or $TestTarget -contains 'installation') {
            try {
                $installation = Invoke-TestPhase -Name 'verified installation preparation (real release/native source rebuild)' -Action {
                    New-RustInstallationPreparation -OwnedDirectory $owned -Executables $installationExecutables.ToArray()
                }
            } catch {
                # Release preparation is independent of already compiled provider and ordinary tests.
                $installationFailure = $_.ToString()
                Write-Host "Installation preparation failed; installation tests are blocked: $installationFailure"
            }
        }
        $after = Get-RustTestIdentity -RepositoryRoot $RepositoryRoot
        if ($after.sourceSha256 -ne $identity.sourceSha256) { throw 'Rust sources changed during preparation.' }
        $record = @{
            schemaVersion = 1
            status = $(if ($providerFailure -and $installationFailure) { 'providerAndInstallationFailed' }
                elseif ($providerFailure) { 'providerFailed' }
                elseif ($installationFailure) { 'installationFailed' }
                else { 'complete' })
            providerFailure = $providerFailure
            installationFailure = $installationFailure
            identity = $identity
            testTargets = @($TestTarget | Where-Object { $null -ne $_ })
            ownedDirectory = $owned
            targetDirectory = $target
            provider = $(if ($requiresProvider -and -not $providerFailure) { @{
                executable = Get-TestArtifact -Path $provider
                source = Get-TestArtifact -Path $providerSource
            } } else { $null })
            executables = $artifacts
            installation = $installation
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
            if (Test-Path -LiteralPath $PrerequisitePath) { throw "Prerequisite record already exists: $PrerequisitePath" }
            [IO.File]::Move($temporary, $PrerequisitePath, $false)
        } finally { $publicationLock.Dispose() }
        return $record
    } catch {
        Remove-Item -LiteralPath $owned -Recurse -Force
        throw
    }
}

# Reject absent, incomplete, changed-source, incompatible, or modified owned outputs before execution.
function Read-RustTestPreparation {
    param([string] $RepositoryRoot, [string] $PrerequisitePath, [string[]] $TestTarget,
        [Alias('AllowPrerequisiteFailure')][switch] $AllowProviderFailure)
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
        'schemaVersion', 'status', 'providerFailure', 'installationFailure', 'identity', 'testTargets',
        'ownedDirectory', 'targetDirectory', 'provider', 'executables', 'installation'
    )
    if ($record -isnot [System.Collections.IDictionary] -or
        $record.schemaVersion -isnot [long] -or $record.schemaVersion -ne 1 -or
        ($record.status -cne 'complete' -and -not ($AllowProviderFailure -and $record.status -cin @(
            'providerFailed', 'installationFailed', 'providerAndInstallationFailed'
        )))) {
        throw 'Incomplete or unsupported Rust prerequisites.'
    }
    if ($record.testTargets -isnot [array] -or $record.executables -isnot [array]) { throw 'Rust prerequisite selections and executables must be arrays.' }
    foreach ($name in $record.testTargets) {
        if ($name -isnot [string] -or [string]::IsNullOrWhiteSpace($name)) { throw 'Malformed Rust test target.' }
    }
    $requiresProvider = -not $TestTarget -or $TestTarget -contains 'native'
    $providerFailed = $record.status -cin @('providerFailed', 'providerAndInstallationFailed')
    $installationFailed = $record.status -cin @('installationFailed', 'providerAndInstallationFailed')
    if ($providerFailed) {
        if (-not $requiresProvider -or $null -ne $record.provider -or
            $record.providerFailure -isnot [string] -or [string]::IsNullOrWhiteSpace($record.providerFailure)) {
            throw 'Malformed failed-provider prerequisites.'
        }
    } elseif ($null -ne $record.providerFailure -or ($requiresProvider -and -not $record.provider) -or
        (-not $requiresProvider -and $null -ne $record.provider)) {
        throw 'Incomplete provider prerequisites.'
    }
    $requiresInstallation = -not $TestTarget -or $TestTarget -contains 'installation'
    if ($installationFailed) {
        if (-not $requiresInstallation -or $null -ne $record.installation -or
            $record.installationFailure -isnot [string] -or [string]::IsNullOrWhiteSpace($record.installationFailure)) {
            throw 'Malformed failed-installation prerequisites.'
        }
    } elseif ($null -ne $record.installationFailure) {
        throw 'Unexpected installation failure in completed prerequisites.'
    }
    $identity = Get-RustTestIdentity -RepositoryRoot $RepositoryRoot
    Assert-RustRecordFields -Record $record.identity -Fields @($identity.Keys)
    foreach ($key in @('repositoryRoot', 'sourceSha256', 'rustc', 'cargo', 'nativeCpu', 'profile', 'features')) {
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
    if (-not $installationFailed) {
        Read-RustInstallationPreparation -Preparation $record -Required:$requiresInstallation
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
        $blockedTargets = @()
        if ($null -ne $Preparation.providerFailure) {
            Write-Host "Blocked native tests: $($Preparation.providerFailure)"
            $blockedTargets += 'native'
        }
        if ($null -ne $Preparation.installationFailure) {
            Write-Host "Blocked installation tests: $($Preparation.installationFailure)"
            $blockedTargets += 'installation'
        }
        if ($blockedTargets) {
            if (-not $TestTarget) {
                $metadata = & cargo metadata --no-deps --format-version 1 --locked | ConvertFrom-Json
                if ($LASTEXITCODE -ne 0) { throw 'Locating provider-independent Rust tests failed.' }
                $TestTarget = @($metadata.packages |
                    Where-Object id -In $metadata.workspace_members |
                    ForEach-Object targets | Where-Object { $_.kind -contains 'test' -and $_.name -notin $blockedTargets } |
                    Select-Object -ExpandProperty name -Unique)
                $selection += @('--lib', '--bins')
                $includeDoctests = $true
            } else {
                $TestTarget = @($TestTarget | Where-Object { $_ -notin $blockedTargets })
                if (-not $TestTarget) { throw 'All selected Rust tests are blocked by prerequisite preparation failure.' }
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
        if ($executionFailed -or $Preparation.status -cne 'complete') { throw 'Rust test execution failed.' }
    } finally {
        $env:SS_AI_RUST_TEST_PREREQUISITES = $previous
    }
}

# The ignored, explicitly named compiler integration owns release outputs separately from test outputs.
function New-RustInstallationPreparation {
    param([string] $OwnedDirectory, [string[]] $Executables)
    if ($Executables.Count -ne 1) { throw 'Cargo must identify exactly one installation test executable.' }
    $directory = Join-Path $OwnedDirectory 'installation'
    New-Item -ItemType Directory -Path $directory | Out-Null
    $previous = $env:SS_AI_INSTALLATION_PREPARATION_DIRECTORY
    try {
        $env:SS_AI_INSTALLATION_PREPARATION_DIRECTORY = $directory
        & $Executables[0] --ignored --exact prepare_real_release_installation --nocapture |
            ForEach-Object { Write-Host $_ }
        if ($LASTEXITCODE -ne 0) { throw 'Verified real release installation preparation failed.' }
    } finally { $env:SS_AI_INSTALLATION_PREPARATION_DIRECTORY = $previous }
    Get-TestArtifact -Path (Join-Path $directory 'evidence.json')
}

# The outer record hashes the complete evidence; every retained input/output is independently bound.
function Read-RustInstallationPreparation {
    param([hashtable] $Preparation, [bool] $Required)
    if (-not $Required) {
        if ($null -ne $Preparation.installation) { throw 'Unexpected installation prerequisites for this selection.' }
        return
    }
    $artifact = $Preparation.installation
    Assert-RustRecordFields -Record $artifact -Fields @('path', 'sha256')
    $directory = Join-Path $Preparation.ownedDirectory 'installation'
    if ($artifact.path -cne (Join-Path $directory 'evidence.json') -or
        $artifact.sha256 -isnot [string] -or $artifact.sha256 -cnotmatch '^[0-9A-F]{64}$' -or
        (Get-TestArtifact -Path $artifact.path).sha256 -cne $artifact.sha256) {
        throw 'Missing, modified, or unowned installation evidence.'
    }
    $stream = Open-RustTestInput -Path $artifact.path
    $reader = [IO.StreamReader]::new($stream)
    try { $evidence = $reader.ReadToEnd() | ConvertFrom-Json -AsHashtable } finally { $reader.Dispose() }
    Assert-RustRecordFields -Record $evidence -Fields @('schemaVersion', 'status', 'artifacts', 'commands', 'nativeFlags', 'baseline', 'changed')
    if ($evidence.schemaVersion -isnot [long] -or $evidence.schemaVersion -ne 1 -or
        $evidence.status -cne 'complete' -or $evidence.artifacts -isnot [array] -or
        $evidence.artifacts.Count -ne 6 -or $evidence.nativeFlags -isnot [string] -or
        -not $evidence.nativeFlags.EndsWith("-C$([char]0x1f)target-cpu=native")) {
        throw 'Incomplete or malformed installation evidence.'
    }
    foreach ($observation in @($evidence.baseline, $evidence.changed)) {
        Assert-RustRecordFields -Record $observation -Fields @('version', 'help')
        if ($observation.version -isnot [string] -or $observation.version -cnotmatch '^ss-skills-util \S+' -or
            $observation.help -isnot [string] -or [string]::IsNullOrWhiteSpace($observation.help)) {
            throw 'Malformed installed executable observation.'
        }
    }
    if ($evidence.baseline.version -cne $evidence.changed.version -or
        $evidence.baseline.help -ceq $evidence.changed.help -or
        -not $evidence.changed.help.Contains('Test-owned replacement from changed local source')) {
        throw 'Installation did not demonstrate same-version changed-source behavior.'
    }
    foreach ($inputArtifact in $evidence.artifacts) {
        Assert-RustRecordFields -Record $inputArtifact -Fields @('path', 'sha256')
        if ($inputArtifact.path -isnot [string] -or $inputArtifact.sha256 -isnot [string] -or
            $inputArtifact.sha256 -cnotmatch '^[0-9A-F]{64}$') { throw 'Malformed installation artifact.' }
        $relative = [IO.Path]::GetRelativePath($directory, $inputArtifact.path)
        if ([IO.Path]::IsPathFullyQualified($relative) -or $relative -eq '..' -or
            $relative.StartsWith('..' + [IO.Path]::DirectorySeparatorChar) -or
            (Get-TestArtifact -Path $inputArtifact.path).sha256 -cne $inputArtifact.sha256) {
            throw 'Modified or unowned installation artifact.'
        }
    }
    $names = @('conflict', 'baseline', 'baselineSetup', 'changed', 'stale', 'shadowed', 'missing', 'finalSetup')
    Assert-RustRecordFields -Record $evidence.commands -Fields $names
    foreach ($name in $names) {
        $command = $evidence.commands[$name]
        Assert-RustRecordFields -Record $command -Fields @('exitCode', 'stdout', 'stderr', 'cargoArtifacts')
        if ($command.exitCode -isnot [long] -or $command.stdout -isnot [string] -or
            $command.stderr -isnot [string] -or $command.cargoArtifacts -isnot [array] -or
            (($command.exitCode -eq 0) -ne ($name -in @('baseline', 'baselineSetup', 'changed', 'finalSetup')))) {
            throw "Unexpected installation outcome: $name"
        }
    }
}
