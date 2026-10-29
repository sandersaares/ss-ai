BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:CacheLibrary = Join-Path $script:RepoRoot 'skills\ss-review\scripts\lib\FocusAreaCache.ps1'
    $script:CacheCliDirectory = Join-Path $script:RepoRoot 'skills\ss-review\scripts'
    $script:MoveItemCommand = Get-Command Move-Item -CommandType Cmdlet
    . (Join-Path $script:CacheCliDirectory 'lib\PullRequestModel.ps1')
    . $script:CacheLibrary

    $script:Now = [DateTimeOffset]::Parse('2026-08-03T12:00:00Z')

    function New-CacheInputs {
        param(
            [Parameter(Mandatory)]
            [string] $Path
        )

        New-Item -ItemType Directory -Path $Path -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $Path 'current-context.md') -Value '# Current context'
        $nested = Join-Path $Path 'ss-ai'
        New-Item -ItemType Directory -Path $nested | Out-Null
        Set-Content -LiteralPath (Join-Path $nested 'rust.instructions.md') -Value '# Rust'
    }

    function New-FocusAreaArtifacts {
        param(
            [Parameter(Mandatory)]
            [string] $Path
        )

        $raw = Join-Path $Path 'instruction-set-raw'
        $focusAreas = Join-Path $Path 'focus-areas'
        New-Item -ItemType Directory -Path $raw -Force | Out-Null
        New-Item -ItemType Directory -Path $focusAreas -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $raw 'guidelines.md') -Value '# Expanded guidelines'
        Set-Content -LiteralPath (Join-Path $Path 'instruction-set-categories.md') -Value '# Categories'
        Set-Content -LiteralPath (Join-Path $focusAreas 'maintainability.md') -Value '# Maintainability'
        Set-Content -LiteralPath (Join-Path $focusAreas 'intentionally-irrelevant.md') -Value '# Still reviewed'
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
        # -NonInteractive turns a missing mandatory parameter into an error rather than a prompt on
        # the inherited console, which would otherwise stall the test run.
        $cliArguments = @(
            '-NoProfile'
            '-NonInteractive'
            '-File'
            (Join-Path $script:CacheCliDirectory $Script)
        )
        foreach ($argument in $cliArguments + $ArgumentList) {
            $startInfo.ArgumentList.Add($argument)
        }

        $process = [Diagnostics.Process]::Start($startInfo)
        $standardOutput = $process.StandardOutput.ReadToEnd()
        $standardError = $process.StandardError.ReadToEnd()
        $process.WaitForExit()

        [pscustomobject]@{
            ExitCode = $process.ExitCode
            StandardOutput = $standardOutput
            StandardError = $standardError
        }
    }
}

Describe 'ss-review focus-area cache' {
    BeforeEach {
        $script:CaseRoot = Join-Path $TestDrive ([guid]::NewGuid())
        $script:Inputs = Join-Path $script:CaseRoot 'inputs'
        $script:Session = Join-Path $script:CaseRoot 'session'
        $script:Cache = Join-Path $script:CaseRoot 'cache'
        $script:Recipe = Join-Path $script:CaseRoot 'prepare-focus-areas.md'
        New-Item -ItemType Directory -Path $script:CaseRoot | Out-Null
        New-CacheInputs -Path $script:Inputs
        New-FocusAreaArtifacts -Path $script:Session
        Set-Content -LiteralPath $script:Recipe -Value '# Recipe'
        New-Item -ItemType Directory -Path (Join-Path $script:Session 'instruction-set-source') | Out-Null
        Set-Content -LiteralPath (Join-Path $script:Session 'focus-area-expansion-status.json') -Value '{"schemaVersion":1,"state":"Complete","references":[]}'
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
        Test-Path -LiteralPath (Join-Path $script:Session 'focus-areas\intentionally-irrelevant.md') |
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

        Add-Content -LiteralPath (Join-Path $script:Inputs 'current-context.md') -Value 'Changed'

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

    It 'keys every schema, format and assembly-tool dependency' {
        $script:FocusAreaGenerationFiles | Should -Contain 'data-guides\focus-areas.md'
        $originalRoot = $script:FocusAreaGenerationRoot
        $isolatedRoot = Join-Path $script:CaseRoot 'generation'
        foreach ($relativePath in $script:FocusAreaGenerationFiles) {
            $destination = Join-Path $isolatedRoot $relativePath
            New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
            Copy-Item -LiteralPath (Join-Path $originalRoot $relativePath) -Destination $destination
        }
        $script:FocusAreaGenerationRoot = $isolatedRoot
        try {
            $arguments = @{
                InputDirectory = $script:Inputs
                RecipePath = $script:Recipe
                GeneratorProfile = 'test'
                FreshnessDays = 7
                NowUtc = $script:Now
            }
            $before = (Get-FocusAreaCacheDescriptor @arguments).Key
            foreach ($relativePath in $script:FocusAreaGenerationFiles) {
                Add-Content -LiteralPath (Join-Path $isolatedRoot $relativePath) -Value 'Changed generation input'
                $after = (Get-FocusAreaCacheDescriptor @arguments).Key
                $after | Should -Not -BeExactly $before -Because $relativePath
                $before = $after
            }
        } finally {
            $script:FocusAreaGenerationRoot = $originalRoot
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
        $artifacts = @{
            'instruction-set-raw\scoped.instructions.md' = $source
            'instruction-set-raw\general.md' = "# General`n`nKeep names clear."
            'instruction-set-categories.md' = @'
# Parameters
Source: instruction-set-source/scoped.instructions.md, "Named parameters"
Scope declarations: applyTo: "**/*.ps1,**/*.psm1"; Scope: this instructions file applies only to agent workflows.
Effective scope: Agent workflows matching either **/*.ps1 or **/*.psm1.

# Documentation
Source: instruction-set-source/scoped.instructions.md, "Published scripts"
Scope declarations: applyTo: "**/*.ps1,**/*.psm1"; Scope: this instructions file applies only to agent workflows.; Scope: published scripts only.
Effective scope: Published agent-workflow scripts matching either **/*.ps1 or **/*.psm1.
Source: instruction-set-source/general.md, "General"
Scope declarations: None.
Effective scope: No explicit restriction; interpret the guideline in context.
'@
            'focus-areas\maintainability.md' = @'
Effective scope: Agent workflows matching either **/*.ps1 or **/*.psm1.
Scope declarations: applyTo: "**/*.ps1,**/*.psm1"; Scope: this instructions file applies only to agent workflows.

# Named parameters
Source: instruction-set-source/scoped.instructions.md, "Named parameters"

Require named arguments.
'@
            'focus-areas\intentionally-irrelevant.md' = @'
# Published scripts
Effective scope: Published agent-workflow scripts matching either **/*.ps1 or **/*.psm1.
Scope declarations: applyTo: "**/*.ps1,**/*.psm1"; Scope: this instructions file applies only to agent workflows.; Scope: published scripts only.
Source: instruction-set-source/scoped.instructions.md, "Published scripts"

Scope: published scripts only.

Document supported invocation.

# General
Effective scope: No explicit restriction; interpret the guideline in context.
Scope declarations: None.
Source: instruction-set-source/general.md, "General"

Keep names clear.
'@
        }
        Set-Content -LiteralPath (Join-Path $script:Inputs 'scoped.instructions.md') -Value $source -NoNewline
        Set-Content -LiteralPath (Join-Path $script:Inputs 'general.md') `
            -Value $artifacts['instruction-set-raw\general.md'] -NoNewline
        foreach ($entry in $artifacts.GetEnumerator()) {
            Set-Content -LiteralPath (Join-Path $script:Session $entry.Key) -Value $entry.Value -NoNewline
        }
        $cacheArguments = @{
            SessionStateDirectory = $script:Session
            InputDirectory = $script:Inputs
            RecipePath = $script:Recipe
            CacheDirectory = $script:Cache
            GeneratorProfile = 'gpt-5-mini'
            FreshnessDays = 7
            NowUtc = $script:Now
        }

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
        $base = Get-FocusAreaCacheDescriptor `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now

        $addedPath = Join-Path $script:Inputs 'added.md'
        Set-Content -LiteralPath $addedPath -Value '# Added'
        $added = Get-FocusAreaCacheDescriptor `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now
        $added.Key | Should -Not -Be $base.Key

        Remove-Item -LiteralPath $addedPath
        $removed = Get-FocusAreaCacheDescriptor `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now
        $removed.Key | Should -Be $base.Key

        Move-Item `
            -LiteralPath (Join-Path $script:Inputs 'current-context.md') `
            -Destination (Join-Path $script:Inputs 'renamed-context.md')
        $renamed = Get-FocusAreaCacheDescriptor `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now
        $renamed.Key | Should -Not -Be $base.Key
    }

    It 'orders input records ordinally rather than linguistically' {
        $ordinalRoot = Join-Path $script:CaseRoot 'ordinal'
        New-Item -ItemType Directory -Path $ordinalRoot | Out-Null
        foreach ($name in @('ab.md', 'a_b.md', 'a-b.md')) {
            Set-Content -LiteralPath (Join-Path $ordinalRoot $name) -Value "# $name"
        }

        $records = @(Get-FileRecords -RootPath $ordinalRoot)

        # Linguistic ordering would place 'a_b.md' before 'a-b.md'; ordinal ordering compares the
        # separator characters directly.
        $records.path | Should -Be @('a-b.md', 'a_b.md', 'ab.md')
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

    It 'rejects a staged copy that no longer matches the cache manifest' {
        Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete | Out-Null
        Set-Content `
            -LiteralPath (Join-Path $script:Session 'instruction-set-categories.md') `
            -Value '# Original session value'

        Mock Copy-FocusAreaArtifacts {
            param($SourceRoot, $DestinationRoot)

            New-Item -ItemType Directory -Path $DestinationRoot -Force | Out-Null
            foreach ($artifact in $script:FocusAreaArtifacts) {
                Microsoft.PowerShell.Management\Copy-Item `
                    -LiteralPath (Join-Path $SourceRoot $artifact.Path) `
                    -Destination $DestinationRoot `
                    -Recurse
            }
            Set-Content `
                -LiteralPath (Join-Path $DestinationRoot 'focus-areas\maintainability.md') `
                -Value '# Changed during copy'
        }

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
        Get-Content -LiteralPath (Join-Path $script:Session 'instruction-set-categories.md') -Raw |
            Should -BeLike '# Original session value*'
    }

    It 'rolls back prior artifacts when installation fails' {
        Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete | Out-Null
        Set-Content `
            -LiteralPath (Join-Path $script:Session 'instruction-set-categories.md') `
            -Value '# Original session value'

        Mock Move-Item {
            param($LiteralPath, $Destination)

            if ((Split-Path -Leaf (Split-Path -Parent $LiteralPath)) -like '.focus-area-cache-restore-*' -and
                (Split-Path -Leaf $LiteralPath) -ceq 'instruction-set-categories.md') {
                throw 'Injected installation failure.'
            }
            & $script:MoveItemCommand `
                -LiteralPath $LiteralPath `
                -Destination $Destination
        }

        {
            Restore-FocusAreaCache `
                -SessionStateDirectory $script:Session `
                -InputDirectory $script:Inputs `
                -RecipePath $script:Recipe `
                -CacheDirectory $script:Cache `
                -GeneratorProfile 'gpt-5-mini' `
                -FreshnessDays 7 `
                -NowUtc $script:Now
        } | Should -Throw '*Injected installation failure*'

        Get-Content -LiteralPath (Join-Path $script:Session 'instruction-set-categories.md') -Raw |
            Should -BeLike '# Original session value*'
        @(Get-ChildItem -LiteralPath $script:Session -Filter '.focus-area-cache-backup-*' -Force).Count |
            Should -Be 0
    }

    It 'retains the backup when rollback cannot restore every prior artifact' {
        Save-FocusAreaCache `
            -SessionStateDirectory $script:Session `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -CacheDirectory $script:Cache `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now `
            -InputState Complete | Out-Null
        Set-Content `
            -LiteralPath (Join-Path $script:Session 'instruction-set-categories.md') `
            -Value '# Original session value'

        Mock Move-Item {
            param($LiteralPath, $Destination)

            if ((Split-Path -Leaf (Split-Path -Parent $LiteralPath)) -like '.focus-area-cache-restore-*' -and
                (Split-Path -Leaf $LiteralPath) -ceq 'instruction-set-categories.md') {
                throw 'Injected installation failure.'
            }
            if ((Split-Path -Leaf (Split-Path -Parent $LiteralPath)) -like '.focus-area-cache-backup-*' -and
                (Split-Path -Leaf $LiteralPath) -ceq 'instruction-set-categories.md') {
                throw 'Injected rollback failure.'
            }
            & $script:MoveItemCommand `
                -LiteralPath $LiteralPath `
                -Destination $Destination
        }

        {
            Restore-FocusAreaCache `
                -SessionStateDirectory $script:Session `
                -InputDirectory $script:Inputs `
                -RecipePath $script:Recipe `
                -CacheDirectory $script:Cache `
                -GeneratorProfile 'gpt-5-mini' `
                -FreshnessDays 7 `
                -NowUtc $script:Now
        } | Should -Throw '*Rollback was incomplete*Backup retained*'

        $backup = Get-ChildItem `
            -LiteralPath $script:Session `
            -Filter '.focus-area-cache-backup-*' `
            -Directory -Force
        @($backup).Count | Should -Be 1
        Get-Content -LiteralPath (Join-Path $backup.FullName 'instruction-set-categories.md') -Raw |
            Should -BeLike '# Original session value*'
        Test-Path -LiteralPath (Join-Path $script:Session 'instruction-set-raw') |
            Should -BeTrue
        Test-Path -LiteralPath (Join-Path $script:Session 'focus-areas') |
            Should -BeTrue
    }

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
        Test-Path -LiteralPath $script:Cache | Should -BeFalse
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
        $originalMaximumEntries = $script:FocusAreaCacheMaximumEntries
        $script:FocusAreaCacheMaximumEntries = 2

        try {
            $storedKeys = @(
                foreach ($index in 1..3) {
                    Set-Content `
                        -LiteralPath (Join-Path $script:Inputs 'current-context.md') `
                        -Value "# Current context $index"
                    (
                        Save-FocusAreaCache `
                            -SessionStateDirectory $script:Session `
                            -InputDirectory $script:Inputs `
                            -RecipePath $script:Recipe `
                            -CacheDirectory $script:Cache `
                            -GeneratorProfile 'gpt-5-mini' `
                            -FreshnessDays 7 `
                            -NowUtc $script:Now.AddDays($index) `
                            -InputState Complete
                    ).key
                }
            )

            Test-Path -LiteralPath (Join-Path $script:Cache $storedKeys[0]) |
                Should -BeFalse
            Test-Path -LiteralPath (Join-Path $script:Cache $storedKeys[1]) |
                Should -BeTrue
            Test-Path -LiteralPath (Join-Path $script:Cache $storedKeys[2]) |
                Should -BeTrue
        } finally {
            $script:FocusAreaCacheMaximumEntries = $originalMaximumEntries
        }
    }

    It 'removes abandoned staging directories after their retention window' {
        New-Item -ItemType Directory -Path $script:Cache -Force | Out-Null
        $staleKey = 'a' * 64

        # Built exactly as Save-FocusAreaCache builds it, so this test fails if the name and the
        # pattern that recognizes it ever diverge.
        $stagingPath = Join-Path $script:Cache ".$staleKey.$([guid]::NewGuid()).tmp"
        $staging = New-Item -ItemType Directory -Path $stagingPath
        $staging.LastWriteTimeUtc = $script:Now.UtcDateTime.AddDays(-2)

        Invoke-FocusAreaCacheMaintenance `
            -CacheDirectory $script:Cache `
            -NowUtc $script:Now

        Test-Path -LiteralPath $stagingPath | Should -BeFalse
    }

    It 'retains a staging directory that is still within its retention window' {
        New-Item -ItemType Directory -Path $script:Cache -Force | Out-Null
        $stagingPath = Join-Path $script:Cache ".$('a' * 64).$([guid]::NewGuid()).tmp"
        $staging = New-Item -ItemType Directory -Path $stagingPath
        $staging.LastWriteTimeUtc = $script:Now.UtcDateTime.AddHours(-1)

        Invoke-FocusAreaCacheMaintenance `
            -CacheDirectory $script:Cache `
            -NowUtc $script:Now

        Test-Path -LiteralPath $stagingPath | Should -BeTrue
    }

    It 'waits for another operation before storing under the cache key' {
        $descriptor = Get-FocusAreaCacheDescriptor `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now
        $lockPath = Get-FocusAreaCacheLockPath `
            -CacheDirectory $script:Cache `
            -Key $descriptor.Key
        $script:HeldCacheLock = (Enter-FocusAreaCacheLock -LockPath $lockPath).Lock
        Mock Start-Sleep {
            $script:HeldCacheLock.Dispose()
            Write-PullRequestOutputFile -Path (Join-Path $script:Session 'focus-areas\additional.md') -Content '# Additional focus area'
        }

        try {
            $result = Save-FocusAreaCache `
                -SessionStateDirectory $script:Session `
                -InputDirectory $script:Inputs `
                -RecipePath $script:Recipe `
                -CacheDirectory $script:Cache `
                -GeneratorProfile 'gpt-5-mini' `
                -FreshnessDays 7 `
                -NowUtc $script:Now `
                -InputState Complete

            $result.status | Should -Be 'Stored'
            $result.reason | Should -BeNullOrEmpty
            $result.focusAreaCount | Should -Be @(Get-ChildItem -LiteralPath (Join-Path $script:Session 'focus-areas') -Filter '*.md' -File).Count
            Should -Invoke Start-Sleep -Times 1 -Exactly
        } finally {
            $script:HeldCacheLock.Dispose()
        }
    }

    It 'reports an unusable cache location distinctly from contention' {
        $descriptor = Get-FocusAreaCacheDescriptor `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-5-mini' `
            -FreshnessDays 7 `
            -NowUtc $script:Now
        $lockPath = Get-FocusAreaCacheLockPath `
            -CacheDirectory $script:Cache `
            -Key $descriptor.Key
        New-Item -ItemType Directory -Path (Split-Path -Parent $lockPath) -Force | Out-Null
        New-Item -ItemType File -Path $lockPath -Force | Out-Null
        $lockFile = Get-Item -LiteralPath $lockPath
        $lockFile.IsReadOnly = $true

        try {
            $result = Save-FocusAreaCache `
                -SessionStateDirectory $script:Session `
                -InputDirectory $script:Inputs `
                -RecipePath $script:Recipe `
                -CacheDirectory $script:Cache `
                -GeneratorProfile 'gpt-5-mini' `
                -FreshnessDays 7 `
                -NowUtc $script:Now `
                -InputState Complete

            $result.status | Should -Be 'Bypassed'
            $result.reason | Should -Be 'CacheUnavailable'
        } finally {
            $lockFile.IsReadOnly = $false
        }
    }

    It 'compares manifest records ordinally rather than linguistically' {
        # Soft hyphens, zero-width spaces and joiners are ignorable to PowerShell's -ceq, so a
        # linguistic comparison would accept a payload whose bytes differ from the manifest.
        foreach ($ignorable in @(0x00AD, 0x200B, 0x200D)) {
            $expected = @([pscustomobject]@{ path = 'focus-areas/a.md'; sha256 = 'aa'; length = 1 })
            $actual = @(
                [pscustomobject]@{
                    path = "focus-areas/a$([char]$ignorable).md"
                    sha256 = 'aa'
                    length = 1
                }
            )
            Test-FileRecordEquality -Expected $expected -Actual $actual | Should -BeFalse

            $expected = @([pscustomobject]@{ path = 'focus-areas/a.md'; sha256 = 'aa'; length = 1 })
            $actual = @(
                [pscustomobject]@{
                    path = 'focus-areas/a.md'
                    sha256 = "a$([char]$ignorable)a"
                    length = 1
                }
            )
            Test-FileRecordEquality -Expected $expected -Actual $actual | Should -BeFalse
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
        $expected = Get-FocusAreaCacheDescriptor `
            -InputDirectory $script:Inputs `
            -RecipePath $script:Recipe `
            -GeneratorProfile 'gpt-6-luna' `
            -FreshnessDays 7 `
            -NowUtc $script:Now
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
        $stored.key | Should -Be $expected.Key

        $restoreResult = Invoke-CacheCliProcess `
            -Script 'Restore-FocusAreaCache.ps1' `
            -ArgumentList $arguments

        $restoreResult.ExitCode | Should -Be 0
        $restoreResult.StandardError | Should -BeNullOrEmpty
        $restored = $restoreResult.StandardOutput | ConvertFrom-Json
        $restored.status | Should -Be 'Hit'
        $restored.key | Should -Be $expected.Key
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
        for ($index = 0; $index -lt $Count; $index++) {
            $path = Join-Path $script:Inputs "external\configured\.github\instructions\custom-$index.instructions.md"
            [void][IO.Directory]::CreateDirectory((Split-Path -Parent $path))
            [IO.File]::WriteAllText($path, '# Custom guidance')
        }
        foreach ($relative in @('skill\focus-areas.md', 'review-repository\AGENTS.md')) {
            $path = Join-Path $script:Inputs $relative
            [void][IO.Directory]::CreateDirectory((Split-Path -Parent $path))
            [IO.File]::WriteAllText($path, '# Not user-scoped')
        }
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
        Set-Content -LiteralPath (Join-Path $script:Session 'instruction-set-source\source.md') -Value 'Read the unavailable guidance.'
        Set-Content -LiteralPath (Join-Path $script:Session 'focus-area-expansion-status.json') -Value @'
{"schemaVersion":1,"state":"Degraded","references":[{"id":"missing","uri":"https://example.invalid/missing","referrers":[{"parentId":null,"source":{"root":"instruction-set-source","path":"source.md","startLine":1,"endLine":1},"scope":"General guidance.","declarations":[]}],"state":"Failed","outputs":[],"error":"Unavailable"}]}
'@
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
        $diagnostics.userInstructionFileCount | Should -Be 0
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

    It 'rejects empty focus-area files' {
        [IO.File]::WriteAllBytes(
            (Join-Path $script:Session 'focus-areas\maintainability.md'),
            [byte[]]::new(0)
        )

        {
            Test-FocusAreaArtifacts -SessionStateDirectory $script:Session
        } | Should -Throw '*Focus-area file is empty*'
    }

    It 'rejects whitespace-only focus-area files' {
        Set-Content `
            -LiteralPath (Join-Path $script:Session 'focus-areas\maintainability.md') `
            -Value " `t`r`n"

        {
            Test-FocusAreaArtifacts -SessionStateDirectory $script:Session
        } | Should -Throw '*Focus-area file is empty*'
    }
}
