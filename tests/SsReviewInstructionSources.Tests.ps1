BeforeAll {
    $script:Skill = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review'
    . "$script:Skill\scripts\lib\InstructionSources.ps1"
    . "$script:Skill\scripts\lib\FocusAreaCache.ps1"

    # Supplies arbitrary instruction names and exact bytes independently of personal guidance.
    function New-InstructionFixture {
        param([string] $Root, [string] $Relative, [byte[]] $Bytes = @(35, 32, 65, 13, 10))
        $path = Join-Path $Root $Relative
        [void][IO.Directory]::CreateDirectory((Split-Path -Parent $path))
        [IO.File]::WriteAllBytes($path, $Bytes)
        $path
    }

    # Each call exercises the same producer boundary without modifying the process environment.
    function Copy-TestInstructions {
        param([AllowNull()][AllowEmptyString()][string] $Directories)
        Copy-ReviewInstructionSources -SessionStateDirectory $script:Session `
            -SkillRoot $script:Skill -DirectoryList $Directories -BaseDirectory $script:Base
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

    It 'fails explicitly on a missing root and leaves the previous snapshot intact' {
        $first = Copy-TestInstructions -Directories ''
        { Copy-TestInstructions -Directories (Join-Path $script:Base 'missing') } | Should -Throw
        Test-Path -LiteralPath (Join-Path $first.sourceDirectory 'skill\focus-areas.md') | Should -BeTrue
    }

    It 'rejects a file configured as an instruction root' {
        $file = New-InstructionFixture -Root $script:Base -Relative 'a.instructions.md'
        { Copy-TestInstructions -Directories $file } | Should -Throw '*not a directory*'
    }

    It 'propagates unreadable-source failures and preserves the previous snapshot' {
        $source = New-InstructionFixture -Root $script:Base -Relative 'a.instructions.md'
        $first = Copy-TestInstructions -Directories ''
        $locked = [IO.File]::Open($source, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try {
            { Copy-TestInstructions -Directories $script:Base } | Should -Throw
            Test-Path -LiteralPath (Join-Path $first.sourceDirectory 'skill\focus-areas.md') | Should -BeTrue
        } finally { $locked.Dispose() }
        @(Get-ChildItem -LiteralPath $script:Session -Filter '*.tmp').Count | Should -Be 0
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

    It 'keeps repeated snapshots stable and removes deleted sources with nested session state' {
        $script:Session = Join-Path $script:Base 'session'
        [void][IO.Directory]::CreateDirectory($script:Session)
        $source = New-InstructionFixture -Root $script:Base -Relative 'custom.instructions.md'
        (Copy-TestInstructions -Directories $script:Base).externalFileCount | Should -Be 1
        $arguments = @{
            InputDirectory = Join-Path $script:Session 'instruction-set-source'
            RecipePath = Join-Path $script:Skill 'prepare-focus-areas.md'
            GeneratorProfile = 'test'
            FreshnessDays = 7
            NowUtc = [DateTimeOffset]'2026-10-29T00:00:00Z'
        }
        $key = (Get-FocusAreaCacheDescriptor @arguments).Key
        (Copy-TestInstructions -Directories $script:Base).externalFileCount | Should -Be 1
        (Get-FocusAreaCacheDescriptor @arguments).Key | Should -BeExactly $key
        Remove-Item -LiteralPath $source
        (Copy-TestInstructions -Directories $script:Base).externalFileCount | Should -Be 0
        @(Get-ChildItem -LiteralPath $arguments.InputDirectory -Recurse -Filter '*.instructions.md').Count | Should -Be 0
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
        @(Get-ChildItem -LiteralPath $script:Session -Directory).Count | Should -Be 1
    }

    It 'binds the configured instruction bytes and root identity into cache keys' {
        $file = New-InstructionFixture -Root $script:Base -Relative 'custom.instructions.md'
        $result = Copy-TestInstructions -Directories $script:Base
        $arguments = @{
            InputDirectory = $result.sourceDirectory
            RecipePath = Join-Path $script:Skill 'prepare-focus-areas.md'
            GeneratorProfile = 'test'
            FreshnessDays = 7
            NowUtc = [DateTimeOffset]'2026-10-29T00:00:00Z'
        }
        $original = (Get-FocusAreaCacheDescriptor @arguments).Key
        [IO.File]::AppendAllText($file, 'Changed guidance')
        Copy-TestInstructions -Directories $script:Base | Out-Null
        $changed = (Get-FocusAreaCacheDescriptor @arguments).Key
        $changed | Should -Not -BeExactly $original
        $other = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-InstructionFixture -Root $other -Relative 'custom.instructions.md' -Bytes ([IO.File]::ReadAllBytes($file)) | Out-Null
        Copy-TestInstructions -Directories $other | Out-Null
        (Get-FocusAreaCacheDescriptor @arguments).Key | Should -Not -BeExactly $changed
        Copy-TestInstructions -Directories '' | Out-Null
        (Get-FocusAreaCacheDescriptor @arguments).Key | Should -Not -BeExactly $original
    }

    It 'runs the snapshot executable against the child environment with <Count> user files' -ForEach @(
        @{ Count = 0 }
        @{ Count = 1 }
    ) {
        New-InstructionFixture -Root $script:Base -Relative 'own.instructions.md' | Out-Null
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment['COPILOT_CUSTOM_INSTRUCTIONS_DIRS'] = if ($Count -eq 0) { '' } else { $script:Base }
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-File',
            "$script:Skill\scripts\Copy-ReviewInstructionSources.ps1", '-SessionStateDirectory', $script:Session)) {
            $start.ArgumentList.Add($argument)
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
        } finally { $process.Dispose() }
    }
}
