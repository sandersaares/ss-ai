BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:CacheCliDirectory = Join-Path $script:RepoRoot 'skills\ss-review\scripts'
    $script:Now = [DateTimeOffset]::Parse('2026-08-03T12:00:00Z')

    function New-CacheInputs {
        param(
            [Parameter(Mandatory)]
            [string] $Path
        )

        New-Item -ItemType Directory -Path $Path -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $Path 'current-context.instructions.md') -Value '# Current context'
        $nested = Join-Path $Path 'ss-ai'
        New-Item -ItemType Directory -Path $nested | Out-Null
        Set-Content -LiteralPath (Join-Path $nested 'rust.instructions.md') -Value '# Rust'
    }

    function Invoke-CacheCliProcess {
        param(
            [Parameter(Mandatory)]
            [string] $Script,

            [Parameter(Mandatory)]
            [AllowEmptyCollection()]
            [string[]] $ArgumentList
        )

        $startInfo = [Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = (Get-Process -Id $PID).Path
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.Environment['COPILOT_CUSTOM_INSTRUCTIONS_DIRS'] = $script:InputRoot
        # -NonInteractive turns a missing mandatory parameter into an error rather than a prompt on
        # the inherited console, which would otherwise stall the test run.
        $cliArguments = @(
            '-NoProfile'
            '-NonInteractive'
            '-File'
            (Join-Path $script:CacheCliDirectory $Script)
        )
        $identityArguments = switch ($Script) {
            'Restore-FocusAreaCache.ps1' { @('-SnapshotIdentity', $script:SnapshotIdentity) }
            'Save-FocusAreaCache.ps1' { @('-PreparationIdentity', $script:PreparationIdentity) }
            'Test-FocusAreaArtifacts.ps1' { @('-PreparationIdentity', $script:PreparationIdentity) }
            default { @() }
        }
        foreach ($argument in $cliArguments + $ArgumentList + $identityArguments) {
            $startInfo.ArgumentList.Add($argument)
        }

        $process = [Diagnostics.Process]::Start($startInfo)
        $standardOutput = $process.StandardOutput.ReadToEnd()
        $standardError = $process.StandardError.ReadToEnd()
        $process.WaitForExit()

        $result = [pscustomobject]@{
            ExitCode = $process.ExitCode
            StandardOutput = $standardOutput
            StandardError = $standardError
        }
        if ($Script -eq 'Restore-FocusAreaCache.ps1' -and $result.ExitCode -eq 0) {
            $script:PreparationIdentity = ($result.StandardOutput | ConvertFrom-Json).preparationIdentity
        }
        $result
    }

    # Exercise the same executable and JSON boundary as the review agent.
    function Invoke-CacheCommand {
        param(
            [string] $Script,
            [hashtable] $Parameters
        )

        $arguments = @()
        foreach ($entry in $Parameters.GetEnumerator()) {
            if ($entry.Value -is [switch] -or $entry.Value -is [bool]) {
                if ($entry.Value) { $arguments += "-$($entry.Key)" }
            } else {
                $value = if ($entry.Value -is [DateTimeOffset]) {
                    $entry.Value.ToString('O')
                } else {
                    [string] $entry.Value
                }
                $arguments += "-$($entry.Key)", $value
            }
        }
        $process = Invoke-CacheCliProcess -Script $Script -ArgumentList $arguments
        if ($process.ExitCode -ne 0) {
            throw "$Script failed: $($process.StandardError)"
        }
        $process.StandardError | Should -BeNullOrEmpty
        $lines = @($process.StandardOutput.Trim() -split '\r?\n')
        $lines.Count | Should -Be 1
        $lines[0] | ConvertFrom-Json
    }

    function Save-FocusAreaCache {
        param(
            [string] $SessionStateDirectory, [string] $InputDirectory, [string] $RecipePath,
            [string] $CacheDirectory, [string] $GeneratorProfile, [int] $FreshnessDays,
            [DateTimeOffset] $NowUtc, [string] $InputState = 'Complete'
        )
        if ($InputState -eq 'Degraded') {
            $restore = @{} + $PSBoundParameters
            $restore.Remove('InputState')
            Restore-FocusAreaCache @restore | Out-Null
            Publish-TestGeneration -Degraded
        }
        $parameters = @{} + $PSBoundParameters
        $parameters.Remove('InputState')
        Invoke-CacheCommand -Script 'Save-FocusAreaCache.ps1' -Parameters $parameters
    }

    function Restore-FocusAreaCache {
        param(
            [string] $SessionStateDirectory, [string] $InputDirectory, [string] $RecipePath,
            [string] $CacheDirectory, [string] $GeneratorProfile, [int] $FreshnessDays,
            [DateTimeOffset] $NowUtc, [switch] $Refresh
        )
        $parameters = @{} + $PSBoundParameters
        $result = Invoke-CacheCommand -Script 'Restore-FocusAreaCache.ps1' -Parameters $parameters
        $script:PreparationIdentity = $result.preparationIdentity
        $result
    }

    # Snapshot through the same command that establishes the source identity used by restore.
    function Invoke-TestSnapshot {
        param([string] $RepositoryRoot)
        $parameters = @{
            SessionStateDirectory = $script:Session
        }
        if ($RepositoryRoot) {
            $parameters.RepositoryRoot = $RepositoryRoot
            $parameters.RepositoryInstruction = 'AGENTS.md'
        }
        $result = Invoke-CacheCommand -Script 'Copy-ReviewInstructionSources.ps1' -Parameters $parameters
        $script:Inputs = $result.sourceDirectory
        $script:SnapshotIdentity = $result.snapshotIdentity
        $script:SnapshotExternal = if (@($result.roots).Count) {
            Join-Path $script:Inputs $result.roots[0].prefix
        } else {
            $null
        }
    }

    # Begin and publish are native-only commands; all artifact authoring uses agent wrappers.
    function Invoke-TestGenerationCommand {
        param([string[]] $Arguments, [switch] $AllowFailure)
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Command ss-ai -CommandType Application | Select-Object -First 1).Source)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('workflow') + $Arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $result = [pscustomobject]@{
                ExitCode = $process.ExitCode
                StandardOutput = $stdout.GetAwaiter().GetResult()
                StandardError = $stderr.GetAwaiter().GetResult()
            }
            if (-not $AllowFailure -and $result.ExitCode -ne 0) { throw $result.StandardError }
            $result
        } finally { $process.Dispose() }
    }

    # Build complete coverage from the snapshot itself, then publish the rendered result.
    function Publish-TestGeneration {
        param([switch] $Degraded)
        $begin = Invoke-TestGenerationCommand -Arguments @(
            'begin-focus-area-generation', "--session-state=$script:Session",
            "--preparation-identity=$script:PreparationIdentity"
        )
        $generation = $begin.StandardOutput | ConvertFrom-Json
        $stage = $generation.sessionState
        $null = Invoke-CacheCommand -Script 'Update-FocusAreaExpansion.ps1' -Parameters @{
            SessionStateDirectory = $stage; Operation = 'Initialize'
        }
        if ($Degraded) {
            $reference = @{
                id = 'missing'; uri = 'https://example.invalid/missing'
                referrers = @(@{
                    parentId = $null
                    source = @{ root = 'instruction-set-source'; path = 'skill/focus-areas.md'; startLine = 1; endLine = 1 }
                    scope = 'General guidance.'; declarations = @()
                })
            }
            $inputPath = Join-Path $stage 'reference-input.json'
            $reference | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $inputPath
            $null = Invoke-CacheCommand -Script 'Update-FocusAreaExpansion.ps1' -Parameters @{
                SessionStateDirectory = $stage; Operation = 'Register'; InputPath = $inputPath
            }
            @{ referenceId = 'missing'; outputs = @(); error = 'Unavailable' } |
                ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $inputPath
            $null = Invoke-CacheCommand -Script 'Update-FocusAreaExpansion.ps1' -Parameters @{
                SessionStateDirectory = $stage; Operation = 'Fail'; InputPath = $inputPath
            }
        }
        $null = Invoke-CacheCommand -Script 'Update-FocusAreaExpansion.ps1' -Parameters @{
            SessionStateDirectory = $stage; Operation = 'Finalize'
        }

        $raw = Join-Path $stage 'instruction-set-raw'
        $files = @(Get-ChildItem -LiteralPath $raw -File -Recurse -Force | Sort-Object FullName)
        $chapters = @()
        for ($index = 0; $index -lt $files.Count; $index++) {
            $path = [IO.Path]::GetRelativePath($raw, $files[$index].FullName).Replace('\', '/')
            $lines = @(Get-Content -LiteralPath $files[$index].FullName).Count
            if ($script:ScopeFixture -and $path.EndsWith('/scoped.instructions.md')) {
                foreach ($part in @(
                    @{ id = 'scoped-parameters'; category = 'maintainability'; start = 1; end = 8; scope = 'Agent workflows matching either **/*.ps1 or **/*.psm1.'; declarations = @(1, 4) },
                    @{ id = 'scoped-published'; category = 'intentionally-irrelevant'; start = 9; end = $lines; scope = 'Published agent-workflow scripts matching either **/*.ps1 or **/*.psm1.'; declarations = @(1, 4, 12, 12) }
                )) {
                    $declarations = @()
                    for ($declaration = 0; $declaration -lt $part.declarations.Count; $declaration += 2) {
                        $declarations += @{ root = 'instruction-set-raw'; path = $path; startLine = $part.declarations[$declaration]; endLine = $part.declarations[$declaration + 1] }
                    }
                    $chapters += @{
                        id = $part.id; categoryId = $part.category
                        content = @{ root = 'instruction-set-raw'; path = $path; startLine = $part.start; endLine = $part.end }
                        sources = @(@{ root = 'instruction-set-source'; path = $path; startLine = $part.start; endLine = $part.end })
                        applicability = @(@{ effectiveScope = $part.scope; declarations = $declarations; origins = @(); uncertainties = @() })
                    }
                }
                continue
            }
            if ($files.Count -eq 1) {
                $middle = [int][Math]::Floor($lines / 2)
                foreach ($part in @(
                    @{ id = 'chapter-first'; category = 'maintainability'; start = 1; end = $middle },
                    @{ id = 'chapter-second'; category = 'intentionally-irrelevant'; start = ($middle + 1); end = $lines }
                )) {
                    $chapters += @{
                        id = $part.id; categoryId = $part.category
                        content = @{ root = 'instruction-set-raw'; path = $path; startLine = $part.start; endLine = $part.end }
                        sources = @(@{ root = 'instruction-set-source'; path = $path; startLine = $part.start; endLine = $part.end })
                        applicability = @(@{ effectiveScope = 'No explicit restriction; interpret the guideline in context.'; declarations = @(); origins = @(); uncertainties = @() })
                    }
                }
                continue
            }
            $span = @{ root = 'instruction-set-raw'; path = $path; startLine = 1; endLine = $lines }
            $source = @{ root = 'instruction-set-source'; path = $path; startLine = 1; endLine = $lines }
            $chapters += @{
                id = "chapter-$index"; categoryId = if ($index -eq 0) { 'maintainability' } else { 'intentionally-irrelevant' }
                content = $span; sources = @($source)
                applicability = @(@{ effectiveScope = 'No explicit restriction; interpret the guideline in context.'; declarations = @(); origins = @(); uncertainties = @() })
            }
        }
        $map = @{
            schemaVersion = 1
            categories = @(@{ id = 'maintainability'; title = 'Maintainability' }, @{ id = 'intentionally-irrelevant'; title = 'Still reviewed' })
            chapters = $chapters
        }
        $mapPath = Join-Path $stage 'focus-area-map.json'
        $map | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $mapPath
        $null = Invoke-CacheCommand -Script 'New-FocusAreaArtifacts.ps1' -Parameters @{
            SessionStateDirectory = $stage; MapPath = $mapPath
        }
        $published = Invoke-TestGenerationCommand -Arguments @(
            'publish-focus-area-generation', "--session-state=$script:Session",
            "--generation-id=$($generation.generationId)"
        )
        ($published.StandardOutput | ConvertFrom-Json).status | Should -Be 'Published'
    }
}

Describe 'ss-review focus-area cache' {
    BeforeEach {
        $script:CaseRoot = Join-Path $TestDrive ([guid]::NewGuid())
        $script:InputRoot = Join-Path $script:CaseRoot 'inputs'
        $script:Session = Join-Path $script:CaseRoot 'session'
        $script:Cache = Join-Path $script:CaseRoot 'cache'
        $script:Recipe = Join-Path $script:CaseRoot 'prepare-focus-areas.md'
        $script:ScopeFixture = $false
        New-Item -ItemType Directory -Path $script:CaseRoot | Out-Null
        New-CacheInputs -Path $script:InputRoot
        New-Item -ItemType Directory -Path $script:Session | Out-Null
        Set-Content -LiteralPath $script:Recipe -Value '# Recipe'
        Invoke-TestSnapshot
        $initial = Restore-FocusAreaCache -SessionStateDirectory $script:Session -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe -CacheDirectory $script:Cache -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 -NowUtc $script:Now
        $script:InitialKey = $initial.key
        Publish-TestGeneration
    }

    It 'stores and restores every focus area without relevance filtering' {
        $stored = Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete

        $stored.status | Should -Be 'Stored'
        $stored.focusAreaCount | Should -Be 2

        Remove-Item -LiteralPath (Join-Path $script:Session 'instruction-set-raw') -Recurse
        Remove-Item -LiteralPath (Join-Path $script:Session 'instruction-set-categories.md')
        Remove-Item -LiteralPath (Join-Path $script:Session 'focus-areas') -Recurse

        $restored = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now

        $restored.status | Should -Be 'Hit'
        $restored.focusAreaCount | Should -Be 2
        Test-Path -LiteralPath (Join-Path $script:Session 'focus-areas\focus-intentionally-irrelevant.md') |
            Should -BeTrue
    }

    It 'misses when an input changes' {
        Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete | Out-Null

        Add-Content -LiteralPath (Join-Path $script:InputRoot 'current-context.instructions.md') -Value 'Changed'
        Invoke-TestSnapshot

        $result = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now

        $result.status | Should -Be 'Miss'
        $result.reason | Should -Be 'NotFound'
    }

    It 'rejects store when the snapshot source changes after restore' {
        Add-Content -LiteralPath (Join-Path $script:SnapshotExternal 'current-context.instructions.md') -Value 'Changed after restore'
        {
            Save-FocusAreaCache -SessionStateDirectory $script:Session -InputDirectory $script:Inputs `
                -RecipePath $script:Recipe -CacheDirectory $script:Cache -GeneratorProfile 'gpt-5-mini' `
                -FreshnessDays 7 -NowUtc $script:Now -InputState Complete
        } | Should -Throw '*staleGeneration*'
        Test-Path -LiteralPath (Join-Path $script:Cache $script:InitialKey) | Should -BeFalse
    }

    It 'does not publish artifacts when a separate input copy changes after restore' {
        $inputCopy = Join-Path $script:CaseRoot 'input-copy'
        Copy-Item -LiteralPath $script:Inputs -Destination $inputCopy -Recurse -Force
        $relative = [IO.Path]::GetRelativePath($script:Inputs, $script:SnapshotExternal)
        Add-Content -LiteralPath (Join-Path $inputCopy "$relative/current-context.instructions.md") -Value 'Changed after restore'
        $result = Save-FocusAreaCache -SessionStateDirectory $script:Session -InputDirectory $inputCopy `
            -RecipePath $script:Recipe -CacheDirectory $script:Cache -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 -NowUtc $script:Now -InputState Complete
        $result.status | Should -Be 'Bypassed'
        $result.reason | Should -Be 'InputsChanged'
        Test-Path -LiteralPath (Join-Path $script:Cache $result.key) | Should -BeFalse
    }

    It 'bypasses store when the published artifacts are modified' {
        Add-Content -LiteralPath (Join-Path $script:Session 'instruction-set-categories.md') -Value 'Changed after publication'
        $result = Save-FocusAreaCache -SessionStateDirectory $script:Session -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe -CacheDirectory $script:Cache -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 -NowUtc $script:Now -InputState Complete
        $result.status | Should -Be 'Bypassed'
        $result.reason | Should -Be 'GenerationNotPublished'
        Test-Path -LiteralPath (Join-Path $script:Cache $script:InitialKey) | Should -BeFalse
    }

    It 'rejects publication from a stale generation and cannot store older artifacts under a new key' {
        $arguments = @{
            SessionStateDirectory = $script:Session; InputDirectory = $script:Inputs
            RecipePath = $script:Recipe; CacheDirectory = $script:Cache
            GeneratorProfile = 'gpt-5-mini'; FreshnessDays = 7; NowUtc = $script:Now
        }
        Restore-FocusAreaCache @arguments | Out-Null
        $begin = Invoke-TestGenerationCommand -Arguments @(
            'begin-focus-area-generation', "--session-state=$script:Session",
            "--preparation-identity=$script:PreparationIdentity"
        )
        $generation = $begin.StandardOutput | ConvertFrom-Json
        Add-Content -LiteralPath (Join-Path $script:InputRoot 'current-context.instructions.md') -Value 'New snapshot'
        Invoke-TestSnapshot
        $current = Restore-FocusAreaCache @arguments
        $stale = Invoke-TestGenerationCommand -AllowFailure -Arguments @(
            'publish-focus-area-generation', "--session-state=$script:Session",
            "--generation-id=$($generation.generationId)"
        )
        $stale.ExitCode | Should -Not -Be 0
        $stale.StandardError | Should -Match '(?i)stale|not active'
        $saved = Save-FocusAreaCache @arguments -InputState Complete
        $saved.status | Should -Be 'Bypassed'
        $saved.reason | Should -Be 'GenerationNotPublished'
        Test-Path -LiteralPath (Join-Path $script:Cache $current.key) | Should -BeFalse
    }

    It 'bypasses a store without a restore snapshot' {
        Remove-Item -LiteralPath (Join-Path $script:Session 'focus-area-cache-identity.json')
        $result = Save-FocusAreaCache -SessionStateDirectory $script:Session -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe -CacheDirectory $script:Cache -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 -NowUtc $script:Now -InputState Complete
        $result.status | Should -Be 'Bypassed'
        $result.reason | Should -Be 'SnapshotUnavailable'
    }

    It 'keys every schema, format and assembly-tool dependency' {
        $generationFiles = @(
            'scripts\Copy-ReviewInstructionSources.ps1', 'scripts\lib\InstructionSources.ps1',
            'pwsh-calls\copy-instruction-sources.md', 'focus-area-format.md',
            'schemas\focus-area-map.schema.json', 'scripts\lib\FocusAreaPreparation.ps1',
            'scripts\lib\JsonValue.ps1', 'scripts\lib\PullRequestModel.ps1',
            'scripts\Test-FocusAreaMap.ps1', 'scripts\New-FocusAreaArtifacts.ps1',
            'pwsh-calls\validate-focus-area-map.md', 'pwsh-calls\assemble-focus-areas.md',
            'workflow-data.md', 'data-guides\focus-areas.md', 'schemas\workflow-data.schema.json',
            'scripts\lib\WorkflowData.ps1', 'scripts\New-ReviewWorkerAssignment.ps1',
            'scripts\Read-ReviewWorkerDocument.ps1', 'scripts\lib\NativeWorkflowBridge.ps1',
            'pwsh-calls\prepare-worker-assignment.md', 'scripts\lib\FocusAreaExpansion.ps1',
            'scripts\Update-FocusAreaExpansion.ps1', 'scripts\Update-FocusAreaMap.ps1',
            'pwsh-calls\update-focus-area-expansion.md', 'pwsh-calls\update-focus-area-map.md',
            'scripts\Save-FocusAreaCache.ps1'
        )
        $originalCli = $script:CacheCliDirectory
        $isolatedRoot = Join-Path $script:CaseRoot 'skill'
        Copy-Item -LiteralPath (Split-Path -Parent $originalCli) -Destination $isolatedRoot -Recurse
        $script:CacheCliDirectory = Join-Path $isolatedRoot 'scripts'
        try {
            $arguments = @{
                SessionStateDirectory = $script:Session
                InputDirectory = $script:Inputs
                RecipePath = $script:Recipe
                CacheDirectory = $script:Cache
                GeneratorProfile = 'test'
                FreshnessDays = 7
                NowUtc = $script:Now
                Refresh = $true
            }
            $before = (Restore-FocusAreaCache @arguments).key
            foreach ($relativePath in $generationFiles) {
                # Appending a comment keeps the executable wrappers valid while changing their bytes.
                Add-Content -LiteralPath (Join-Path $isolatedRoot $relativePath) -Value "`n"
                $after = (Restore-FocusAreaCache @arguments).key
                $after | Should -Not -BeExactly $before -Because $relativePath
                $before = $after
            }
        } finally {
            $script:CacheCliDirectory = $originalCli
        }
    }

    It 'reuses an entry when executable packaging changes without changing the native build' {
        $source = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        $bin = Join-Path $script:CaseRoot 'bin'
        [void] [IO.Directory]::CreateDirectory($bin)
        $executable = Join-Path $bin ([IO.Path]::GetFileName($source))
        [IO.File]::Copy($source, $executable)
        if (-not $IsWindows) {
            [IO.File]::SetUnixFileMode($executable, [IO.File]::GetUnixFileMode($source))
        }
        $originalPath = $env:PATH
        try {
            $env:PATH = $bin + [IO.Path]::PathSeparator + $originalPath
            $arguments = @{
                SessionStateDirectory = $script:Session
                InputDirectory = $script:Inputs
                RecipePath = $script:Recipe
                CacheDirectory = $script:Cache
                GeneratorProfile = 'test'
                FreshnessDays = 7
                NowUtc = $script:Now
            }
            Restore-FocusAreaCache @arguments | Out-Null
            Publish-TestGeneration
            $stored = Save-FocusAreaCache @arguments -InputState Complete
            $stored.status | Should -Be 'Stored'
            (Restore-FocusAreaCache @arguments).status | Should -Be 'Hit'
            $manifestPath = Join-Path $script:Cache "$($stored.key)\manifest.json"
            $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
            @($manifest.inputs | Where-Object path -CEQ '__generation__/native-build').Count | Should -Be 1
            # Packaging bytes do not alter the fingerprint compiled into the executable.
            $stream = [IO.File]::Open($executable, [IO.FileMode]::Append, [IO.FileAccess]::Write)
            try { $stream.WriteByte(0) } finally { $stream.Dispose() }
            $result = Restore-FocusAreaCache @arguments
            $result.status | Should -Be 'Hit'
            $result.key | Should -BeExactly $stored.key
        } finally {
            $env:PATH = $originalPath
        }
    }

    It 'preserves original declarations and shared or mixed effective scopes verbatim on reuse' {
        $source = @'
---
applyTo: "**/*.ps1,**/*.psm1"
---
Scope: this instructions file applies only to agent workflows.

# Named parameters

Require named arguments.

# Published scripts

Scope: published scripts only.

Document supported invocation.
'@
        Set-Content -LiteralPath (Join-Path $script:InputRoot 'scoped.instructions.md') -Value $source -NoNewline
        Set-Content -LiteralPath (Join-Path $script:InputRoot 'general.instructions.md') -Value "# General`n`nKeep names clear." -NoNewline
        Invoke-TestSnapshot
        $script:ScopeFixture = $true
        $cacheArguments = @{
            SessionStateDirectory = $script:Session
            InputDirectory = $script:Inputs
            RecipePath = $script:Recipe
            CacheDirectory = $script:Cache
            GeneratorProfile = 'gpt-5-mini'
            FreshnessDays = 7
            NowUtc = $script:Now
        }

        Restore-FocusAreaCache @cacheArguments | Out-Null
        Publish-TestGeneration
        $artifacts = @{}
        foreach ($relativePath in @(
                'instruction-set-categories.md',
                'focus-areas\focus-maintainability.md',
                'focus-areas\focus-intentionally-irrelevant.md')) {
            $artifacts[$relativePath] = [IO.File]::ReadAllText((Join-Path $script:Session $relativePath))
        }
        $rawPath = Join-Path $script:Session ([IO.Path]::GetRelativePath($script:Session, $script:SnapshotExternal).Replace('instruction-set-source', 'instruction-set-raw'))
        $artifacts[[IO.Path]::GetRelativePath($script:Session, (Join-Path $rawPath 'scoped.instructions.md'))] = $source
        $artifacts['focus-areas\focus-maintainability.md'] | Should -Match 'Agent workflows matching either'
        $artifacts['focus-areas\focus-intentionally-irrelevant.md'] | Should -Match 'Published agent-workflow scripts'
        $artifacts['instruction-set-categories.md'] | Should -Match 'scoped\.instructions\.md'
        (Save-FocusAreaCache @cacheArguments -InputState Complete).status | Should -Be 'Stored'
        foreach ($relativePath in $artifacts.Keys) {
            Remove-Item -LiteralPath (Join-Path $script:Session $relativePath)
        }
        (Restore-FocusAreaCache @cacheArguments).status | Should -Be 'Hit'

        foreach ($entry in $artifacts.GetEnumerator()) {
            [IO.File]::ReadAllText((Join-Path $script:Session $entry.Key)) |
                Should -BeExactly $entry.Value
        }
    }

    It 'keys additions, removals, and relative paths' {
        $base = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -CacheDirectory $script:Cache -Refresh `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now

        $addedPath = Join-Path $script:InputRoot 'added.instructions.md'
        Set-Content -LiteralPath $addedPath -Value '# Added'
        Invoke-TestSnapshot
        $added = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -CacheDirectory $script:Cache -Refresh `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now
        $added.Key | Should -Not -Be $base.Key

        Remove-Item -LiteralPath $addedPath
        Invoke-TestSnapshot
        $removed = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -CacheDirectory $script:Cache -Refresh `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now
        $removed.Key | Should -Be $base.Key

        Move-Item `
            -LiteralPath (Join-Path $script:InputRoot 'current-context.instructions.md') `
            -Destination (Join-Path $script:InputRoot 'renamed-context.instructions.md')
        Invoke-TestSnapshot
        $renamed = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -CacheDirectory $script:Cache -Refresh `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now
        $renamed.Key | Should -Not -Be $base.Key
    }

    It 'orders input records ordinally rather than linguistically' {
        foreach ($name in @('ab.instructions.md', 'a_b.instructions.md', 'a-b.instructions.md')) {
            Set-Content -LiteralPath (Join-Path $script:InputRoot $name) -Value "# $name"
        }
        Invoke-TestSnapshot

        $arguments = @{
            SessionStateDirectory = $script:Session; InputDirectory = $script:Inputs
            RecipePath = $script:Recipe; CacheDirectory = $script:Cache
            GeneratorProfile = 'gpt-5-mini'; FreshnessDays = 7; NowUtc = $script:Now
        }
        Restore-FocusAreaCache @arguments | Out-Null
        Publish-TestGeneration
        $stored = Save-FocusAreaCache @arguments -InputState Complete
        $manifest = Get-Content -Raw -LiteralPath (Join-Path $script:Cache "$($stored.key)\manifest.json") | ConvertFrom-Json
        $records = @($manifest.inputs | Where-Object { $_.path -match '/(?:ab|a_b|a-b)\.instructions\.md$' })

        # Linguistic ordering would place 'a_b.md' before 'a-b.md'; ordinal ordering compares the
        # separator characters directly.
        @($records.path | ForEach-Object { Split-Path -Leaf $_ }) |
            Should -Be @('a-b.instructions.md', 'a_b.instructions.md', 'ab.instructions.md')
    }

    It 'misses when the generation recipe changes' {
        Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete | Out-Null

        Set-Content -LiteralPath $script:Recipe -Value '# Different recipe'

        $result = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now

        $result.status | Should -Be 'Miss'
    }

    It 'periodically refreshes unchanged external-reference inputs' {
        Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete | Out-Null

        $result = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now.AddDays(8)

        $result.status | Should -Be 'Miss'
    }

    It 'bypasses restore without changing the deterministic store key when refresh is requested' {
        $stored = Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete

        $result = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -Refresh

        $result.status | Should -Be 'Miss'
        $result.reason | Should -Be 'RefreshRequested'
        $result.key | Should -Be $stored.key
    }

    It 'removes stale destination focus areas on restore' {
        Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete | Out-Null

        Set-Content -LiteralPath (Join-Path $script:Session 'focus-areas\stale.md') -Value '# Stale'

        Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now | Out-Null

        Test-Path -LiteralPath (Join-Path $script:Session 'focus-areas\stale.md') | Should -BeFalse
    }

    It 'rejects a manifest-consistent invalid payload without replacing session artifacts' {
        $arguments = @{
            SessionStateDirectory = $script:Session; InputDirectory = $script:Inputs
            RecipePath = $script:Recipe; CacheDirectory = $script:Cache
            GeneratorProfile = 'gpt-5-mini'; FreshnessDays = 7; NowUtc = $script:Now
        }
        $stored = Save-FocusAreaCache @arguments -InputState Complete
        $entry = Join-Path $script:Cache $stored.key
        $payload = Join-Path $entry 'payload\focus-areas\focus-maintainability.md'
        [IO.File]::WriteAllText($payload, ' ')
        $manifestPath = Join-Path $entry 'manifest.json'
        $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
        $record = $manifest.outputs | Where-Object path -CEQ 'focus-areas/focus-maintainability.md'
        $record.length = (Get-Item -LiteralPath $payload).Length
        $record.sha256 = (Get-FileHash -LiteralPath $payload -Algorithm SHA256).Hash.ToLowerInvariant()
        $manifest | ConvertTo-Json -Depth 20 -Compress | Set-Content -LiteralPath $manifestPath
        Set-Content -LiteralPath (Join-Path $script:Session 'instruction-set-categories.md') -Value '# Original session value'

        $result = Restore-FocusAreaCache @arguments
        $result.status | Should -Be 'Miss'
        $result.reason | Should -Be 'InvalidEntry'
        Get-Content -LiteralPath (Join-Path $script:Session 'instruction-set-categories.md') -Raw |
            Should -BeLike '# Original session value*'
        @(Get-ChildItem -LiteralPath $script:Session -Filter '.focus-area-cache-restore-*' -Force).Count | Should -Be 0
    }

    # Native staged mutation, failed installation, and retained-backup cases use deterministic
    # fault injection in packages/ss-ai-review-workflow/src/focus_cache.rs (focus_cache::tests).

    It 'does not store degraded instruction inputs' {
        $result = Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Degraded

        $result.status | Should -Be 'Bypassed'
        Test-Path -LiteralPath (Join-Path $script:Cache $result.key) | Should -BeFalse
    }

    It 'treats repeated stores as idempotent' {
        $parameters = @{
            SessionStateDirectory = $script:Session
            InputDirectory = $script:Inputs
            RecipePath = $script:Recipe
            CacheDirectory = $script:Cache
            GeneratorProfile = 'gpt-5-mini'
            FreshnessDays = 7
            NowUtc = $script:Now
            InputState = 'Complete'
        }

        $first = Save-FocusAreaCache @parameters
        $second = Save-FocusAreaCache @parameters

        $first.status | Should -Be 'Stored'
        $second.status | Should -Be 'Existing'
        $second.key | Should -Be $first.key
    }

    It 'prunes superseded payload entries while preserving the current entry' {
        $arguments = @{
            SessionStateDirectory = $script:Session; InputDirectory = $script:Inputs
            RecipePath = $script:Recipe; CacheDirectory = $script:Cache
            GeneratorProfile = 'gpt-5-mini'; FreshnessDays = 7; NowUtc = $script:Now
        }
        $current = Save-FocusAreaCache @arguments -InputState Complete
        $manifestPath = Join-Path $script:Cache "$($current.key)\manifest.json"
        $template = Get-Content -LiteralPath $manifestPath -Raw
        # Populate the actual retention limit with distinct, recent entries, ordered by creation.
        foreach ($index in 1..64) {
            $key = '{0:x64}' -f $index
            $entry = Join-Path $script:Cache $key
            New-Item -ItemType Directory -Path $entry | Out-Null
            $manifest = $template | ConvertFrom-Json
            $manifest.key = $key
            $manifest.createdAtUnix = $script:Now.ToUnixTimeSeconds() - (65 - $index)
            $manifest | ConvertTo-Json -Depth 20 -Compress |
                Set-Content -LiteralPath (Join-Path $entry 'manifest.json')
        }
        (Save-FocusAreaCache @arguments -InputState Complete).status | Should -Be 'Existing'
        Test-Path -LiteralPath (Join-Path $script:Cache ('{0:x64}' -f 1)) | Should -BeFalse
        Test-Path -LiteralPath (Join-Path $script:Cache ('{0:x64}' -f 64)) | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $script:Cache $current.key) | Should -BeTrue
    }

    It 'removes abandoned staging directories after their retention window' {
        New-Item -ItemType Directory -Path $script:Cache -Force | Out-Null
        $staleKey = 'a' * 64

        # A native staging name contains the writer process ID and an allocation counter.
        $stagingPath = Join-Path $script:Cache ".$staleKey.1-1.tmp"
        $staging = New-Item -ItemType Directory -Path $stagingPath
        $staging.LastWriteTimeUtc = $script:Now.UtcDateTime.AddDays(-2)

        Save-FocusAreaCache -SessionStateDirectory $script:Session -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe -CacheDirectory $script:Cache -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 -NowUtc $script:Now -InputState Complete | Out-Null

        Test-Path -LiteralPath $stagingPath | Should -BeFalse
    }

    It 'retains a staging directory that is still within its retention window' {
        New-Item -ItemType Directory -Path $script:Cache -Force | Out-Null
        $stagingPath = Join-Path $script:Cache ".$('a' * 64).1-1.tmp"
        $staging = New-Item -ItemType Directory -Path $stagingPath
        $staging.LastWriteTimeUtc = $script:Now.UtcDateTime.AddHours(-1)

        Save-FocusAreaCache -SessionStateDirectory $script:Session -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe -CacheDirectory $script:Cache -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 -NowUtc $script:Now -InputState Complete | Out-Null

        Test-Path -LiteralPath $stagingPath | Should -BeTrue
    }

    It 'waits for another operation before storing under the cache key' {
        $arguments = @{
            SessionStateDirectory = $script:Session; InputDirectory = $script:Inputs
            RecipePath = $script:Recipe; CacheDirectory = $script:Cache
            GeneratorProfile = 'gpt-5-mini'; FreshnessDays = 7; NowUtc = $script:Now
        }
        $key = (Restore-FocusAreaCache @arguments).key
        Publish-TestGeneration
        $childScript = Join-Path $script:CaseRoot 'contended-save.ps1'
        Set-Content -LiteralPath $childScript -Value @'
#Requires -Version 7.6
param([string] $Wrapper, [string] $Session, [string] $Inputs, [string] $Recipe, [string] $Cache, [string] $Now, [string] $PreparationIdentity)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
[Console]::Out.WriteLine('ready')
[Console]::Out.Flush()
if ([Console]::In.ReadLine() -ne 'start') { throw 'Missing start signal' }
& $Wrapper -SessionStateDirectory $Session -InputDirectory $Inputs -RecipePath $Recipe -CacheDirectory $Cache -GeneratorProfile 'gpt-5-mini' -FreshnessDays 7 -NowUtc $Now -PreparationIdentity $PreparationIdentity
'@
        $start = [Diagnostics.ProcessStartInfo]::new()
        $start.FileName = (Get-Process -Id $PID).Path
        $start.UseShellExecute = $false
        $start.RedirectStandardInput = $true
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($arg in @('-NoProfile', '-NonInteractive', '-File', $childScript,
                '-Wrapper', (Join-Path $script:CacheCliDirectory 'Save-FocusAreaCache.ps1'),
                '-Session', $script:Session, '-Inputs', $script:Inputs, '-Recipe', $script:Recipe,
                '-Cache', $script:Cache, '-Now', $script:Now.ToString('O'),
                '-PreparationIdentity', $script:PreparationIdentity)) {
            $start.ArgumentList.Add($arg)
        }
        # Start the child before opening the lock: it must not inherit the held handle.
        $child = [Diagnostics.Process]::Start($start)
        $lock = $null
        try {
            $child.StandardOutput.ReadLine() | Should -Be 'ready'
            $lockPath = Join-Path $script:Cache ".locks\$key.lock"
            $lock = [IO.File]::Open($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
            $child.StandardInput.WriteLine('start')
            $child.StandardInput.Flush()
            $operationLock = Join-Path $script:Session 'focus-area-cache-operation.lock'
            $observed = $false
            while (-not $child.HasExited) {
                try {
                    $probe = [IO.File]::Open($operationLock, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
                    $probe.Dispose()
                } catch [IO.IOException] {
                    $observed = $true
                    break
                }
                [Threading.Thread]::Yield() | Out-Null
            }
            $observed | Should -BeTrue -Because 'the save must hold the operation lock while waiting for the key'
            $lock.Dispose()
            $lock = $null
            $stdout = $child.StandardOutput.ReadToEnd()
            $stderr = $child.StandardError.ReadToEnd()
            $child.WaitForExit()
            $child.ExitCode | Should -Be 0 -Because $stderr
            $result = $stdout | ConvertFrom-Json
            $result.status | Should -Be 'Stored'
            $result.reason | Should -BeNullOrEmpty
            $result.focusAreaCount | Should -Be @(Get-ChildItem -LiteralPath (Join-Path $script:Session 'focus-areas') -Filter '*.md' -File).Count
        } finally {
            if ($lock) { $lock.Dispose() }
            $child.StandardInput.Close()
            if (-not $child.HasExited) { $child.Kill($true); $child.WaitForExit() }
            $child.Dispose()
        }
    }

    It 'reports an unusable cache location distinctly from contention' {
        $cacheFile = Join-Path $script:CaseRoot 'not-a-directory'
        Set-Content -LiteralPath $cacheFile -Value 'not a directory'
        $result = Save-FocusAreaCache `
            -SessionStateDirectory $script:Session -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe -CacheDirectory $cacheFile -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 -NowUtc $script:Now -InputState Complete
        $result.status | Should -Be 'Bypassed'
        $result.reason | Should -Be 'CacheUnavailable'
    }

    It 'compares manifest records ordinally rather than linguistically' {
        # Soft hyphens, zero-width spaces and joiners are ignorable to PowerShell's -ceq, so a
        # linguistic comparison would accept a payload whose bytes differ from the manifest.
        $needsGeneration = $false
        foreach ($ignorable in @(0x00AD, 0x200B, 0x200D)) {
            foreach ($field in @('path', 'sha256')) {
                $arguments = @{
                    SessionStateDirectory = $script:Session; InputDirectory = $script:Inputs
                    RecipePath = $script:Recipe; CacheDirectory = $script:Cache
                    GeneratorProfile = 'gpt-5-mini'; FreshnessDays = 7; NowUtc = $script:Now
                }
                if ($needsGeneration) { Publish-TestGeneration }
                $stored = Save-FocusAreaCache @arguments -InputState Complete
                $manifestPath = Join-Path $script:Cache "$($stored.key)\manifest.json"
                $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
                $record = $manifest.outputs | Where-Object path -CEQ 'focus-areas/focus-maintainability.md'
                $record.$field = $record.$field.Insert(1, [string][char]$ignorable)
                $manifest | ConvertTo-Json -Depth 20 -Compress | Set-Content -LiteralPath $manifestPath
                $result = Restore-FocusAreaCache @arguments
                $result.status | Should -Be 'Miss'
                $result.reason | Should -Be 'InvalidEntry'
                $needsGeneration = $true
            }
        }
    }

    It 'treats an altered payload as a cache miss' {
        $stored = Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete

        Set-Content -LiteralPath (
            Join-Path $script:Cache "$($stored.key)\payload\instruction-set-categories.md"
        ) -Value '# Corrupted'

        $result = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now

        $result.status | Should -Be 'Miss'
        $result.reason | Should -Be 'InvalidEntry'
    }

    It 'uses the focus-area assembly model by default for both cache CLIs' {
        # The expected profile follows final assembly in prepare-focus-areas.md.
        $expected = Restore-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -CacheDirectory $script:Cache `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-6-luna' `
            -FreshnessDays 7 `
            -NowUtc $script:Now
        Publish-TestGeneration
        $arguments = @(
            '-SessionStateDirectory', $script:Session,
            '-InputDirectory', $script:Inputs,
            '-RecipePath', $script:Recipe,
            '-CacheDirectory', $script:Cache,
            '-FreshnessDays', '7',
            '-NowUtc', $script:Now.ToString('O')
        )

        $storeResult = Invoke-CacheCliProcess `
            -Script 'Save-FocusAreaCache.ps1' `
            -ArgumentList $arguments

        $storeResult.ExitCode | Should -Be 0
        $storeResult.StandardError | Should -BeNullOrEmpty
        $stored = $storeResult.StandardOutput | ConvertFrom-Json
        $stored.status | Should -Be 'Stored'
        $stored.key | Should -Be $expected.key

        $restoreResult = Invoke-CacheCliProcess `
            -Script 'Restore-FocusAreaCache.ps1' `
            -ArgumentList $arguments

        $restoreResult.ExitCode | Should -Be 0
        $restoreResult.StandardError | Should -BeNullOrEmpty
        $restored = $restoreResult.StandardOutput | ConvertFrom-Json
        $restored.status | Should -Be 'Hit'
        $restored.key | Should -Be $expected.key
    }

    It 'emits exactly one JSON document when a corrupt cache entry is restored through the CLI' {
        $stored = Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete
        Set-Content `
            -LiteralPath (Join-Path $script:Cache "$($stored.key)\manifest.json") `
            -Value '{not-json'

        $processResult = Invoke-CacheCliProcess -Script 'Restore-FocusAreaCache.ps1' -ArgumentList @(
            '-SessionStateDirectory', $script:Session,
            '-InputDirectory', $script:Inputs,
            '-RecipePath', $script:Recipe,
            '-CacheDirectory', $script:Cache,
            '-GeneratorProfile', 'gpt-5-mini',
            '-FreshnessDays', '7',
            '-NowUtc', $script:Now.ToString('O')
        )

        $processResult.ExitCode | Should -Be 0
        $processResult.StandardError | Should -BeNullOrEmpty
        $lines = @($processResult.StandardOutput.Trim() -split '\r?\n')
        $lines.Count | Should -Be 1
        ($lines[0] | ConvertFrom-Json).reason | Should -Be 'InvalidEntry'
        $diagnostics = Get-Content `
            -LiteralPath (Join-Path $script:Session 'focus-area-cache-diagnostics.json') `
            -Raw |
            ConvertFrom-Json
        $diagnostics.cache | Should -Be 'Miss'
        $diagnostics.reason | Should -Be 'InvalidEntry'
    }

    It 'reports <Count> user instruction files on cache misses, hits and stores' -ForEach @(
        @{ Count = 0 }
        @{ Count = 2 }
    ) {
        Get-ChildItem -LiteralPath $script:InputRoot -Force | Remove-Item -Recurse -Force
        for ($index = 0; $index -lt $Count; $index++) {
            $path = Join-Path $script:InputRoot ".github\instructions\custom-$index.instructions.md"
            [void][IO.Directory]::CreateDirectory((Split-Path -Parent $path))
            [IO.File]::WriteAllText($path, '# Custom guidance')
        }
        $repository = Join-Path $script:CaseRoot 'review-repository'
        New-Item -ItemType Directory -Path $repository | Out-Null
        [IO.File]::WriteAllText((Join-Path $repository 'AGENTS.md'), '# Repository guidance')
        Invoke-TestSnapshot -RepositoryRoot $repository
        $arguments = @(
            '-SessionStateDirectory', $script:Session,
            '-InputDirectory', $script:Inputs,
            '-RecipePath', $script:Recipe,
            '-CacheDirectory', $script:Cache,
            '-GeneratorProfile', 'gpt-5-mini',
            '-FreshnessDays', '7',
            '-NowUtc', $script:Now.ToString('O')
        )
        $first = Invoke-CacheCliProcess -Script 'Restore-FocusAreaCache.ps1' -ArgumentList $arguments
        $first.ExitCode | Should -Be 0 -Because $first.StandardError
        ($first.StandardOutput | ConvertFrom-Json).status | Should -Be 'Miss'
        ($first.StandardOutput | ConvertFrom-Json).userInstructionFileCount | Should -Be $Count

        Publish-TestGeneration
        $store = Invoke-CacheCliProcess -Script 'Save-FocusAreaCache.ps1' -ArgumentList $arguments
        $store.ExitCode | Should -Be 0 -Because $store.StandardError
        $diagnosticsPath = Join-Path $script:Session 'focus-area-cache-diagnostics.json'
        (Get-Content -LiteralPath $diagnosticsPath -Raw | ConvertFrom-Json).userInstructionFileCount | Should -Be $Count

        $hit = Invoke-CacheCliProcess -Script 'Restore-FocusAreaCache.ps1' -ArgumentList $arguments
        $hit.ExitCode | Should -Be 0 -Because $hit.StandardError
        ($hit.StandardOutput | ConvertFrom-Json).status | Should -Be 'Hit'
        ($hit.StandardOutput | ConvertFrom-Json).userInstructionFileCount | Should -Be $Count
        $diagnostics = Get-Content -LiteralPath $diagnosticsPath -Raw | ConvertFrom-Json
        $diagnostics.cache | Should -Be 'Hit'
        $diagnostics.userInstructionFileCount | Should -Be $Count

        Get-ChildItem -LiteralPath (Join-Path $script:Session 'focus-areas') -Filter '*.md' -File | Remove-Item
        $invalid = Invoke-CacheCliProcess -Script 'Test-FocusAreaArtifacts.ps1' -ArgumentList @(
            '-SessionStateDirectory', $script:Session
        )
        $invalid.ExitCode | Should -Not -Be 0
        $diagnostics = Get-Content -LiteralPath $diagnosticsPath -Raw | ConvertFrom-Json
        $diagnostics.cache | Should -Be 'Miss'
        $diagnostics.reason | Should -Be 'ValidationFailed'
        $diagnostics.userInstructionFileCount | Should -Be $Count
    }

    It 'requires a finalized expansion manifest for CLI store operations' {
        Set-Content -LiteralPath (Join-Path $script:Session 'focus-area-expansion-status.json') -Value '{"schemaVersion":1,"state":"Unknown","references":[]}'
        $processResult = Invoke-CacheCliProcess -Script 'Save-FocusAreaCache.ps1' -ArgumentList @(
            '-SessionStateDirectory', $script:Session,
            '-InputDirectory', $script:Inputs,
            '-RecipePath', $script:Recipe,
            '-CacheDirectory', $script:Cache,
            '-GeneratorProfile', 'gpt-5-mini',
            '-FreshnessDays', '7',
            '-NowUtc', $script:Now.ToString('O')
        )

        $processResult.ExitCode | Should -Not -Be 0
        $processResult.StandardError | Should -BeLike '*finalized*'
    }

    It 'records cache and degraded expansion diagnostics across CLI operations' {
        $restoreResult = Invoke-CacheCliProcess -Script 'Restore-FocusAreaCache.ps1' -ArgumentList @(
            '-SessionStateDirectory', $script:Session,
            '-InputDirectory', $script:Inputs,
            '-RecipePath', $script:Recipe,
            '-CacheDirectory', $script:Cache,
            '-GeneratorProfile', 'gpt-5-mini',
            '-FreshnessDays', '7',
            '-NowUtc', $script:Now.ToString('O')
        )
        $restoreResult.ExitCode | Should -Be 0

        Publish-TestGeneration -Degraded
        $storeResult = Invoke-CacheCliProcess -Script 'Save-FocusAreaCache.ps1' -ArgumentList @(
            '-SessionStateDirectory', $script:Session,
            '-InputDirectory', $script:Inputs,
            '-RecipePath', $script:Recipe,
            '-CacheDirectory', $script:Cache,
            '-GeneratorProfile', 'gpt-5-mini',
            '-FreshnessDays', '7',
            '-NowUtc', $script:Now.ToString('O')
        )
        $storeResult.ExitCode | Should -Be 0

        $diagnostics = Get-Content `
            -LiteralPath (Join-Path $script:Session 'focus-area-cache-diagnostics.json') `
            -Raw |
            ConvertFrom-Json
        $diagnostics.cache | Should -Be 'Miss'
        $diagnostics.reason | Should -Be 'NotFound'
        $diagnostics.inputState | Should -Be 'Degraded'
        $diagnostics.store | Should -Be 'Bypassed'
        $diagnostics.storeReason | Should -Be 'DegradedInputs'
    }

    It 'clears stale degraded diagnostics on a later restore' {
        Set-Content `
            -LiteralPath (Join-Path $script:Session 'focus-area-cache-diagnostics.json') `
            -Value '{"cache":"Miss","inputState":"Degraded","store":"Bypassed"}'

        $restoreResult = Invoke-CacheCliProcess -Script 'Restore-FocusAreaCache.ps1' -ArgumentList @(
            '-SessionStateDirectory', $script:Session,
            '-InputDirectory', $script:Inputs,
            '-RecipePath', $script:Recipe,
            '-CacheDirectory', $script:Cache,
            '-GeneratorProfile', 'gpt-5-mini',
            '-FreshnessDays', '7',
            '-NowUtc', $script:Now.ToString('O')
        )

        $restoreResult.ExitCode | Should -Be 0
        $diagnostics = Get-Content `
            -LiteralPath (Join-Path $script:Session 'focus-area-cache-diagnostics.json') `
            -Raw |
            ConvertFrom-Json
        $diagnostics.cache | Should -Be 'Miss'
        $diagnostics.PSObject.Properties.Name | Should -Not -Contain 'inputState'
        $diagnostics.PSObject.Properties.Name | Should -Not -Contain 'store'
    }

    It 'records a cache miss when restored artifact validation fails' {
        Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete | Out-Null

        $restoreResult = Invoke-CacheCliProcess -Script 'Restore-FocusAreaCache.ps1' -ArgumentList @(
            '-SessionStateDirectory', $script:Session,
            '-InputDirectory', $script:Inputs,
            '-RecipePath', $script:Recipe,
            '-CacheDirectory', $script:Cache,
            '-GeneratorProfile', 'gpt-5-mini',
            '-FreshnessDays', '7',
            '-NowUtc', $script:Now.ToString('O')
        )
        $restoreResult.ExitCode | Should -Be 0
        Get-ChildItem `
            -LiteralPath (Join-Path $script:Session 'focus-areas') `
            -Filter '*.md' |
            Remove-Item

        $validateResult = Invoke-CacheCliProcess -Script 'Test-FocusAreaArtifacts.ps1' -ArgumentList @(
            '-SessionStateDirectory', $script:Session
        )

        $validateResult.ExitCode | Should -Not -Be 0
        $diagnostics = Get-Content `
            -LiteralPath (Join-Path $script:Session 'focus-area-cache-diagnostics.json') `
            -Raw |
            ConvertFrom-Json
        $diagnostics.cache | Should -Be 'Miss'
        $diagnostics.reason | Should -Be 'ValidationFailed'
        $diagnostics.userInstructionFileCount | Should -Be 2
        $diagnostics.PSObject.Properties.Name | Should -Not -Contain 'inputState'
    }

    It 'documents every cache command as non-interactive' {
        # A missing mandatory parameter prompts rather than failing when stdin is an open pipe,
        # which an agent shell provides, so a documented command without -NonInteractive stalls the
        # review indefinitely instead of reporting the mistake.
        $skillRoot = Join-Path $script:RepoRoot 'skills\ss-review'

        # Match the executable rather than the start of the line so that an invocation prefixed by
        # the call operator, spelled pwsh.exe, or written inline in prose is still held to the rule.
        # Requiring a following switch keeps prose mentions of the host itself out of the results.
        $invocations = @(
            Get-ChildItem -LiteralPath $skillRoot -Recurse -Filter '*.md' -File |
                Select-String -Pattern '(?<![\w.-])pwsh(\.exe)?\s+-'
        )

        $invocations | Should -Not -BeNullOrEmpty
        foreach ($invocation in $invocations) {
            # A trailing comment is prose rather than an argument, so it cannot satisfy the rule.
            # A word boundary rather than trailing whitespace accepts the switch as the final
            # argument while still rejecting a longer switch that merely starts the same way.
            ($invocation.Line -split '\s#')[0] |
                Should -Match '\s-NonInteractive\b' -Because "$($invocation.Filename) invokes pwsh"
        }

    }

    It 'returns a single JSON result for valid focus-area artifacts' {
        $result = Invoke-CacheCommand -Script 'Test-FocusAreaArtifacts.ps1' -Parameters @{
            SessionStateDirectory = $script:Session
        }
        $result.status | Should -Be 'Valid'
        $result.focusAreaCount | Should -Be 2
    }

    It 'rejects empty focus-area files' {
        [IO.File]::WriteAllBytes(
            (Join-Path $script:Session 'focus-areas\focus-maintainability.md'),
            [byte[]]::new(0)
        )

        $result = Invoke-CacheCliProcess -Script 'Test-FocusAreaArtifacts.ps1' -ArgumentList @(
            '-SessionStateDirectory', $script:Session
        )
        $result.ExitCode | Should -Not -Be 0
        $result.StandardError | Should -Match '(?i)empty'
    }

    It 'rejects whitespace-only focus-area files' {
        Set-Content `
            -LiteralPath (Join-Path $script:Session 'focus-areas\focus-maintainability.md') `
            -Value " `t`r`n"

        $result = Invoke-CacheCliProcess -Script 'Test-FocusAreaArtifacts.ps1' -ArgumentList @(
            '-SessionStateDirectory', $script:Session
        )
        $result.ExitCode | Should -Not -Be 0
        $result.StandardError | Should -Match '(?i)empty'
    }
}
