#Requires -Version 7.6

BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $PSNativeCommandUseErrorActionPreference = $true
    $script:Skill = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review'
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source

    # Supplies arbitrary instruction names and exact bytes independently of personal guidance.
    function New-InstructionFixture {
        param([string] $Root, [string] $Relative, [byte[]] $Bytes = @(35, 32, 65, 13, 10))
        $path = Join-Path $Root $Relative
        [void][IO.Directory]::CreateDirectory((Split-Path -Parent $path))
        [IO.File]::WriteAllBytes($path, $Bytes)
        $path
    }

    # Isolate each producer's environment and working directory from the test process.
    function Copy-TestInstructions {
        param(
            [AllowNull()][AllowEmptyString()] $Directories,
            [string] $SkillRoot = $script:Skill,
            [string] $RepositoryRoot,
            [string[]] $RepositoryInstruction
        )
        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.WorkingDirectory = $script:Base
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        if ($null -eq $Directories) {
            [void]$start.Environment.Remove('COPILOT_CUSTOM_INSTRUCTIONS_DIRS')
        } else {
            $start.Environment['COPILOT_CUSTOM_INSTRUCTIONS_DIRS'] = $Directories
        }
        foreach ($argument in @('workflow', 'snapshot-instructions', "--session-state=$script:Session", "--skill-root=$SkillRoot")) {
            $start.ArgumentList.Add($argument)
        }
        if ($RepositoryRoot) { $start.ArgumentList.Add("--repository-root=$RepositoryRoot") }
        foreach ($path in $RepositoryInstruction) { $start.ArgumentList.Add("--repository-instruction=$path") }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $output = $stdout.GetAwaiter().GetResult()
            $errorText = $stderr.GetAwaiter().GetResult()
            if ($process.ExitCode -ne 0) { throw "Instruction snapshot failed: $errorText" }
            $output | ConvertFrom-Json
        } finally { $process.Dispose() }
    }

    # Exercise the actual cache consumer with an isolated cache and fixed freshness bucket.
    function Get-TestCacheResult {
        param([Parameter(Mandatory)] $Snapshot)
        $json = & "$script:Skill\scripts\Restore-FocusAreaCache.ps1" `
            -SessionStateDirectory $script:Session -InputDirectory $Snapshot.sourceDirectory `
            -SnapshotIdentity $Snapshot.snapshotIdentity `
            -CacheDirectory (Join-Path $TestDrive 'cache') -GeneratorProfile 'test' `
            -FreshnessDays 7 -NowUtc ([DateTimeOffset]'2026-10-29T00:00:00Z')
        $json | ConvertFrom-Json
    }

    # Captures the complete corpus so failed refreshes cannot discard or rewrite prior guidance.
    function Get-TestInstructionSnapshot {
        param([string] $Directory)
        @(Get-ChildItem -LiteralPath $Directory -Recurse -File | ForEach-Object {
            $relative = [IO.Path]::GetRelativePath($Directory, $_.FullName)
            "$relative`0$([Convert]::ToBase64String([IO.File]::ReadAllBytes($_.FullName)))"
        } | Sort-Object -CaseSensitive)
    }
}

Describe 'ss-review configured instruction discovery' {
    BeforeEach {
        $script:Base = Join-Path $TestDrive ([guid]::NewGuid().ToString('N') + ' with spaces')
        $script:Session = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        [void][IO.Directory]::CreateDirectory($script:Base)
        [void][IO.Directory]::CreateDirectory($script:Session)
    }

    It 'uses only built-in guidance for an <Case> configuration' -ForEach @(
        @{ Case = 'unset'; Value = $null }
        @{ Case = 'empty'; Value = '' }
        @{ Case = 'whitespace and empty entries'; Value = ' ,  , ' }
    ) {
        $result = Copy-TestInstructions -Directories $Value
        $result.schemaVersion | Should -Be 1
        $result.roots.Count | Should -Be 0
        $result.externalFileCount | Should -Be 0
        $result.snapshotIdentity | Should -Not -BeNullOrEmpty
        @(Get-ChildItem -LiteralPath $result.sourceDirectory -Recurse -File).Count | Should -Be 1
        [IO.File]::ReadAllBytes((Join-Path $result.sourceDirectory 'skill\focus-areas.md')) |
            Should -Be ([IO.File]::ReadAllBytes((Join-Path $script:Skill 'focus-areas.md')))
    }

    It 'preserves bytes and paths from multiple roots without requiring familiar filenames' {
        $first = Join-Path $script:Base 'first root'
        $second = Join-Path $script:Base 'second root'
        $source = New-InstructionFixture -Root $first -Relative '.github\instructions\nested\bespoke.instructions.md' `
            -Bytes @(239, 187, 191, 35, 32, 195, 164, 13, 10)
        New-InstructionFixture -Root $first -Relative 'AGENTS.md' | Out-Null
        New-InstructionFixture -Root $first -Relative 'nested\AGENTS.md' | Out-Null
        New-InstructionFixture -Root $first -Relative 'ignored.md' | Out-Null
        New-InstructionFixture -Root $second -Relative '.github\instructions\nested\bespoke.instructions.md' | Out-Null

        $result = Copy-TestInstructions -Directories " $first, ,$second,$first "
        $result.roots.Count | Should -Be 2
        $result.externalFileCount | Should -Be 4
        $result.roots[0].fileCount | Should -Be 3
        $result.roots[0].prefix | Should -Not -BeExactly $result.roots[1].prefix
        $copy = Join-Path $result.sourceDirectory "$($result.roots[0].prefix)\.github\instructions\nested\bespoke.instructions.md"
        [IO.File]::ReadAllBytes($copy) | Should -Be ([IO.File]::ReadAllBytes($source))
        @(Get-ChildItem -LiteralPath $result.sourceDirectory -Recurse -Filter ignored.md).Count | Should -Be 0
    }

    It 'resolves relative roots and accepts an empty existing directory' {
        [void][IO.Directory]::CreateDirectory((Join-Path $script:Base 'empty'))
        $result = Copy-TestInstructions -Directories 'empty'
        $result.roots.Count | Should -Be 1
        $result.roots[0].path | Should -BeExactly (Join-Path $script:Base 'empty')
        $result.roots[0].fileCount | Should -Be 0
        $result.externalFileCount | Should -Be 0
    }

    It 'distinguishes Unicode path identities during <Case>' -ForEach @(
        @{ Case = 'root exclusion' }
        @{ Case = 'linked-target exclusion' }
        @{ Case = 'linked ancestry checks' }
    ) {
        # Canonically equivalent spellings expose linguistic equality without relying on culture.
        $composed = Join-Path $script:Base ([string][char]0x00E9)
        $decomposed = Join-Path $script:Base ('e' + [char]0x0301)
        [void][IO.Directory]::CreateDirectory($composed)
        [void][IO.Directory]::CreateDirectory($decomposed)
        [IO.File]::WriteAllText((Join-Path $composed 'identity-probe'), 'Distinct directory probe.')
        if ([IO.File]::Exists((Join-Path $decomposed 'identity-probe'))) {
            Set-ItResult -Skipped -Because 'The fixture filesystem identifies these Unicode spellings as one directory.'
            return
        }
        $source = New-InstructionFixture -Root $decomposed -Relative 'custom.instructions.md'
        $root = $decomposed
        $relative = 'custom.instructions.md'
        $link = $null
        if ($Case -ne 'linked ancestry checks') { $script:Session = $composed }
        if ($Case -ne 'root exclusion') {
            $root = if ($Case -eq 'linked ancestry checks') { $composed } else { Join-Path $script:Base 'configured' }
            [void][IO.Directory]::CreateDirectory($root)
            $link = Join-Path $root 'linked'
            $kind = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
            New-Item -ItemType $kind -Path $link -Target $decomposed | Out-Null
            $relative = 'linked\custom.instructions.md'
        }
        try {
            $result = Copy-TestInstructions -Directories $root
            $result.externalFileCount | Should -Be 1
            $copy = Join-Path $result.sourceDirectory "$($result.roots[0].prefix)\$relative"
            [IO.File]::ReadAllBytes($copy) | Should -Be ([IO.File]::ReadAllBytes($source))
        } finally {
            if ($null -ne $link) { [IO.Directory]::Delete($link) }
        }
    }

    It 'fails explicitly on a missing root and leaves the previous snapshot intact' {
        New-InstructionFixture -Root $script:Base -Relative 'previous.instructions.md' | Out-Null
        $first = Copy-TestInstructions -Directories $script:Base
        $before = @(Get-TestInstructionSnapshot -Directory $first.sourceDirectory)
        { Copy-TestInstructions -Directories (Join-Path $script:Base 'missing') } | Should -Throw
        @(Get-TestInstructionSnapshot -Directory $first.sourceDirectory) | Should -BeExactly $before
    }

    It 'rejects a file configured as an instruction root' {
        $file = New-InstructionFixture -Root $script:Base -Relative 'a.instructions.md'
        { Copy-TestInstructions -Directories $file } | Should -Throw '*not a directory*'
    }

    It 'propagates unreadable-source failures and preserves the previous snapshot' {
        $source = New-InstructionFixture -Root $script:Base -Relative 'a.instructions.md'
        $first = Copy-TestInstructions -Directories $script:Base
        $before = @(Get-TestInstructionSnapshot -Directory $first.sourceDirectory)
        [IO.File]::WriteAllText($source, 'Replacement guidance must not replace the prior corpus on failure.')
        $originalAcl = $null
        $mode = $null
        try {
            if ($IsWindows) {
                # Access denial exercises a source read failure without waiting for contention retries.
                $originalAcl = Get-Acl -LiteralPath $source
                $deniedAcl = Get-Acl -LiteralPath $source
                $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
                try {
                    $rule = [Security.AccessControl.FileSystemAccessRule]::new(
                        $identity.User,
                        [Security.AccessControl.FileSystemRights]::ReadData,
                        [Security.AccessControl.AccessControlType]::Deny
                    )
                    $deniedAcl.AddAccessRule($rule)
                    Set-Acl -LiteralPath $source -AclObject $deniedAcl
                } finally { $identity.Dispose() }
                try {
                    $probe = [IO.File]::OpenRead($source)
                    $probe.Dispose()
                    throw 'The current account can read the denied source.'
                } catch [UnauthorizedAccessException] {}
            } else {
                # Unix sharing modes do not prevent an independent native reader.
                $mode = [IO.File]::GetUnixFileMode($source)
                [IO.File]::SetUnixFileMode($source, [IO.UnixFileMode]::None)
                try {
                    $probe = [IO.File]::OpenRead($source)
                    $probe.Dispose()
                    Set-ItResult -Skipped -Because 'The current account can read a file without read permissions.'
                    return
                } catch [UnauthorizedAccessException] {}
            }
            { Copy-TestInstructions -Directories $script:Base } | Should -Throw
            @(Get-TestInstructionSnapshot -Directory $first.sourceDirectory) | Should -BeExactly $before
        } finally {
            if ($null -ne $originalAcl) { Set-Acl -LiteralPath $source -AclObject $originalAcl }
            if ($null -ne $mode) { [IO.File]::SetUnixFileMode($source, $mode) }
        }
        @(Get-ChildItem -LiteralPath $script:Session -Filter '*.tmp').Count | Should -Be 0
    }

    It 'preserves the prior snapshot if staging cannot read the built-in source' {
        New-InstructionFixture -Root $script:Base -Relative 'a.instructions.md' | Out-Null
        $first = Copy-TestInstructions -Directories $script:Base
        $before = @(Get-TestInstructionSnapshot -Directory $first.sourceDirectory)
        $missingSkill = Join-Path $TestDrive 'missing-skill'
        [void][IO.Directory]::CreateDirectory($missingSkill)
        { Copy-TestInstructions -Directories $script:Base -SkillRoot $missingSkill } | Should -Throw
        @(Get-TestInstructionSnapshot -Directory $first.sourceDirectory) | Should -BeExactly $before
        @(Get-ChildItem -LiteralPath $script:Session -Force -Filter '*.tmp').Count | Should -Be 0
    }

    It 'follows installed linked instruction directories' {
        $target = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-InstructionFixture -Root $target -Relative 'custom.instructions.md' | Out-Null
        $parent = Join-Path $script:Base '.github'
        [void][IO.Directory]::CreateDirectory($parent)
        $kind = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        $link = Join-Path $parent 'instructions'
        New-Item -ItemType $kind -Path $link -Target $target | Out-Null
        try {
            $result = Copy-TestInstructions -Directories $script:Base
            $result.externalFileCount | Should -Be 1
            Test-Path -LiteralPath (Join-Path $result.sourceDirectory "$($result.roots[0].prefix)\.github\instructions\custom.instructions.md") |
                Should -BeTrue
        } finally { [IO.Directory]::Delete($link) }
    }

    It 'rejects directory-link cycles without waiting for a timeout' {
        $kind = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        $link = Join-Path $script:Base 'loop'
        New-Item -ItemType $kind -Path $link -Target $script:Base | Out-Null
        try {
            { Copy-TestInstructions -Directories $script:Base } | Should -Throw '*link cycle*'
        } finally { [IO.Directory]::Delete($link) }
    }

    It 'reports a dangling instruction-directory link instead of omitting guidance' {
        New-InstructionFixture -Root $script:Base -Relative 'previous.instructions.md' | Out-Null
        $previous = Copy-TestInstructions -Directories $script:Base
        $before = @(Get-TestInstructionSnapshot -Directory $previous.sourceDirectory)
        $target = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        [void][IO.Directory]::CreateDirectory($target)
        $parent = Join-Path $script:Base '.github'
        [void][IO.Directory]::CreateDirectory($parent)
        $kind = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        $link = Join-Path $parent 'instructions'
        New-Item -ItemType $kind -Path $link -Target $target | Out-Null
        [IO.Directory]::Delete($target)
        try {
            { Copy-TestInstructions -Directories $script:Base } | Should -Throw '*link target*'
            @(Get-TestInstructionSnapshot -Directory $previous.sourceDirectory) | Should -BeExactly $before
        } finally {
            if ($IsWindows) { [IO.Directory]::Delete($link) } else { [IO.File]::Delete($link) }
        }
    }

    It 'keeps repeated snapshots stable and removes deleted sources with nested session state' {
        $script:Session = Join-Path $script:Base 'session'
        [void][IO.Directory]::CreateDirectory($script:Session)
        $source = New-InstructionFixture -Root $script:Base -Relative 'custom.instructions.md'
        $first = Copy-TestInstructions -Directories $script:Base
        $first.externalFileCount | Should -Be 1
        $inputDirectory = Join-Path $script:Session 'instruction-set-source'
        $key = (Get-TestCacheResult -Snapshot $first).key
        $second = Copy-TestInstructions -Directories $script:Base
        $second.externalFileCount | Should -Be 1
        $second.snapshotIdentity | Should -Not -BeExactly $first.snapshotIdentity
        { Get-TestCacheResult -Snapshot $first } | Should -Throw '*snapshot identity*'
        (Get-TestCacheResult -Snapshot $second).key | Should -BeExactly $key
        Remove-Item -LiteralPath $source
        (Copy-TestInstructions -Directories $script:Base).externalFileCount | Should -Be 0
        @(Get-ChildItem -LiteralPath $inputDirectory -Recurse -Filter '*.instructions.md').Count | Should -Be 0
    }

    It 'excludes links directly into generated session subtrees' {
        $source = New-InstructionFixture -Root $script:Base -Relative 'custom.instructions.md'
        $first = Copy-TestInstructions -Directories $script:Base
        $target = Join-Path $first.sourceDirectory $first.roots[0].prefix
        $kind = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        $link = Join-Path $script:Base 'old-snapshot'
        New-Item -ItemType $kind -Path $link -Target $target | Out-Null
        try {
            (Copy-TestInstructions -Directories $script:Base).externalFileCount | Should -Be 1
            Remove-Item -LiteralPath $source
            (Copy-TestInstructions -Directories $script:Base).externalFileCount | Should -Be 0
            # The next pass encounters a dangling alias after the empty snapshot removes its target.
            (Copy-TestInstructions -Directories $script:Base).externalFileCount | Should -Be 0
        } finally {
            # The empty replacement removes the target; Unix treats a dangling
            # directory symlink as a file, while Windows junctions remain directories.
            if ($IsWindows) { [IO.Directory]::Delete($link) } else { [IO.File]::Delete($link) }
        }
    }

    It 'excludes session state reached through a linked parent directory' {
        $sessions = Join-Path $script:Base 'sessions'
        [void][IO.Directory]::CreateDirectory((Join-Path $sessions 'current'))
        $kind = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        $link = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-Item -ItemType $kind -Path $link -Target $sessions | Out-Null
        try {
            $script:Session = Join-Path $link 'current'
            $source = New-InstructionFixture -Root $script:Base -Relative 'custom.instructions.md'
            (Copy-TestInstructions -Directories $script:Base).externalFileCount | Should -Be 1
            (Copy-TestInstructions -Directories $script:Base).externalFileCount | Should -Be 1
            Remove-Item -LiteralPath $source
            (Copy-TestInstructions -Directories $script:Base).externalFileCount | Should -Be 0
        } finally { [IO.Directory]::Delete($link) }
    }

    It 'replaces a prior snapshot without retaining removed guidance' {
        $file = New-InstructionFixture -Root $script:Base -Relative 'removed.instructions.md'
        Copy-TestInstructions -Directories $script:Base | Out-Null
        Remove-Item -LiteralPath $file
        $second = Copy-TestInstructions -Directories ''
        @(Get-ChildItem -LiteralPath $second.sourceDirectory -Recurse -File).Count | Should -Be 1
        # Reserved generation identities remain beside the current snapshot; staging directories do not.
        $directories = @(Get-ChildItem -LiteralPath $script:Session -Directory -Force)
        $directories.Count | Should -Be 2
        $directories.Name | Should -Contain '.focus-area-generations'
        $directories.Name | Should -Contain (Split-Path -Leaf $second.sourceDirectory)
    }

    It 'binds the configured instruction bytes and root identity into cache keys' {
        $file = New-InstructionFixture -Root $script:Base -Relative 'custom.instructions.md'
        $result = Copy-TestInstructions -Directories $script:Base
        $originalResult = Get-TestCacheResult -Snapshot $result
        $originalResult.userInstructionFileCount | Should -Be 1
        $original = $originalResult.key
        [IO.File]::AppendAllText($file, 'Changed guidance')
        $result = Copy-TestInstructions -Directories $script:Base
        $changed = (Get-TestCacheResult -Snapshot $result).key
        $changed | Should -Not -BeExactly $original
        $other = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-InstructionFixture -Root $other -Relative 'custom.instructions.md' -Bytes ([IO.File]::ReadAllBytes($file)) | Out-Null
        $result = Copy-TestInstructions -Directories $other
        (Get-TestCacheResult -Snapshot $result).key | Should -Not -BeExactly $changed
        $result = Copy-TestInstructions -Directories ''
        $empty = Get-TestCacheResult -Snapshot $result
        $empty.userInstructionFileCount | Should -Be 0
        $empty.key | Should -Not -BeExactly $original
    }

    It 'includes only explicitly selected repository instructions and passes them to the cache consumer' {
        $repository = Join-Path $TestDrive 'reviewed repository'
        $firstFile = New-InstructionFixture -Root $repository -Relative 'AGENTS.md' `
            -Bytes @(239, 187, 191, 35, 32, 195, 164, 13, 10)
        $secondFile = New-InstructionFixture -Root $repository -Relative 'nested\other.instructions.md'
        New-InstructionFixture -Root $repository -Relative 'nested\unselected.instructions.md' | Out-Null

        $without = Copy-TestInstructions -Directories ''
        Test-Path -LiteralPath (Join-Path $without.sourceDirectory 'review-repository') | Should -BeFalse
        $withoutCache = Get-TestCacheResult -Snapshot $without
        $withoutCache.userInstructionFileCount | Should -Be 0

        $selected = Copy-TestInstructions -Directories '' -RepositoryRoot $repository `
            -RepositoryInstruction @('AGENTS.md', 'nested/other.instructions.md')
        $selected.snapshotIdentity | Should -Not -BeExactly $without.snapshotIdentity
        $selected.externalFileCount | Should -Be 0
        $selected.roots.Count | Should -Be 0
        $selected.repositoryRoot.path | Should -BeExactly $repository
        $selected.repositoryRoot.fileCount | Should -Be 2
        $selectedRoot = Join-Path $selected.sourceDirectory $selected.repositoryRoot.prefix
        [IO.File]::ReadAllBytes((Join-Path $selectedRoot 'AGENTS.md')) |
            Should -Be ([IO.File]::ReadAllBytes($firstFile))
        [IO.File]::ReadAllBytes((Join-Path $selectedRoot 'nested\other.instructions.md')) |
            Should -Be ([IO.File]::ReadAllBytes($secondFile))
        Test-Path -LiteralPath (Join-Path $selectedRoot 'nested\unselected.instructions.md') |
            Should -BeFalse
        $selectedCache = Get-TestCacheResult -Snapshot $selected
        $selectedCache.userInstructionFileCount | Should -Be 0
        $selectedCache.key | Should -Not -BeExactly $withoutCache.key

        $withoutAgain = Copy-TestInstructions -Directories ''
        Test-Path -LiteralPath (Join-Path $withoutAgain.sourceDirectory 'review-repository') | Should -BeFalse
        (Get-TestCacheResult -Snapshot $withoutAgain).key | Should -BeExactly $withoutCache.key
    }

    It 'rejects invalid repository selections and invalidates the previous identity' {
        $repository = Join-Path $TestDrive 'reviewed repository'
        New-InstructionFixture -Root $repository -Relative 'AGENTS.md' | Out-Null
        $previous = Copy-TestInstructions -Directories '' -RepositoryRoot $repository -RepositoryInstruction @('AGENTS.md')
        $before = @(Get-TestInstructionSnapshot -Directory $previous.sourceDirectory)
        { Copy-TestInstructions -Directories '' -RepositoryRoot $repository -RepositoryInstruction @('../outside.instructions.md') } |
            Should -Throw
        @(Get-TestInstructionSnapshot -Directory $previous.sourceDirectory) | Should -BeExactly $before
        { Get-TestCacheResult -Snapshot $previous } | Should -Throw '*snapshot identity*'
        { Copy-TestInstructions -Directories '' -RepositoryRoot $repository -RepositoryInstruction @('AGENTS.md', 'AGENTS.md') } |
            Should -Throw
        { Copy-TestInstructions -Directories '' -RepositoryRoot $repository } | Should -Throw
        { Copy-TestInstructions -Directories '' -RepositoryInstruction @('AGENTS.md') } | Should -Throw
    }

    It 'rejects a selected repository instruction reached through an escaping directory link' {
        $repository = Join-Path $TestDrive 'linked repository'
        $outside = Join-Path $TestDrive 'outside repository'
        [void][IO.Directory]::CreateDirectory($repository)
        New-InstructionFixture -Root $outside -Relative 'secret.instructions.md' | Out-Null
        $link = Join-Path $repository 'outside'
        $kind = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        New-Item -ItemType $kind -Path $link -Target $outside | Out-Null
        try {
            { Copy-TestInstructions -Directories '' -RepositoryRoot $repository `
                -RepositoryInstruction @('outside/secret.instructions.md') } | Should -Throw
        } finally { [IO.Directory]::Delete($link) }
    }

    It 'runs the snapshot executable against the child environment with <Count> user files' -ForEach @(
        @{ Count = 0 }
        @{ Count = 1 }
    ) {
        New-InstructionFixture -Root $script:Base -Relative 'own.instructions.md' | Out-Null
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        $start.WorkingDirectory = $script:Base
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment['COPILOT_CUSTOM_INSTRUCTIONS_DIRS'] = if ($Count -eq 0) { '' } else { '.' }
        $repository = Join-Path $TestDrive 'wrapper repository'
        New-InstructionFixture -Root $repository -Relative 'chosen\AGENTS.md' | Out-Null
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-File',
            "$script:Skill\scripts\Copy-ReviewInstructionSources.ps1", '-SessionStateDirectory', $script:Session)) {
            $start.ArgumentList.Add($argument)
        }
        if ($Count -gt 0) {
            foreach ($argument in @('-RepositoryRoot', $repository, '-RepositoryInstruction', 'chosen/AGENTS.md')) {
                $start.ArgumentList.Add($argument)
            }
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $process.ExitCode | Should -Be 0 -Because ($stderr.GetAwaiter().GetResult())
            $result = $stdout.GetAwaiter().GetResult() | ConvertFrom-Json
            $result.schemaVersion | Should -Be 1
            $result.externalFileCount | Should -Be $Count
            $result.roots.Count | Should -Be $Count
            $result.snapshotIdentity | Should -Not -BeNullOrEmpty
            if ($Count -gt 0) { $result.roots[0].path | Should -BeExactly $script:Base }
            if ($Count -gt 0) {
                Test-Path -LiteralPath (Join-Path $result.sourceDirectory "$($result.repositoryRoot.prefix)\chosen\AGENTS.md") |
                    Should -BeTrue
            } else {
                Test-Path -LiteralPath (Join-Path $result.sourceDirectory 'review-repository') | Should -BeFalse
            }
        } finally { $process.Dispose() }
    }
}
