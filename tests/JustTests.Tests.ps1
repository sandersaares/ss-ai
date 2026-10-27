#Requires -Version 7.6

BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $PSNativeCommandUseErrorActionPreference = $true
    $script:Just = Get-Command just -CommandType Application -ErrorAction SilentlyContinue
    $script:RepositoryRoot = Split-Path -Parent $PSScriptRoot
}

Describe 'Just test argument forwarding' {
    It 'preserves argument boundaries for <Recipe> with extra arguments = <Extra>' -ForEach @(
        foreach ($recipe in @('test', 'test-skill', 'test-rust', 'test-rust-prepare', 'test-rust-execute', 'test-all')) {
            foreach ($extra in @($false, $true)) {
                @{ Recipe = $recipe; Extra = $extra; ExitCode = 0 }
            }
        }
        # A distinct native failure must survive both PowerShell and Just boundaries.
        @{ Recipe = 'test-rust-execute'; Extra = $true; ExitCode = 19 }
    ) {
        if (-not $script:Just) {
            Set-ItResult -Skipped -Because 'Just is not installed.'
            return
        }

        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N') + ' with spaces')
        New-Item -ItemType Directory -Path (Join-Path $root 'scripts/lib') -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $script:RepositoryRoot 'justfile') -Destination $root
        Copy-Item -LiteralPath (Join-Path $script:RepositoryRoot 'scripts/lib/TestRecipeArguments.ps1') -Destination (Join-Path $root 'scripts/lib')
        # Use the canonical runner's actual parameter binder, replacing only its execution
        # body so boundary coverage never builds Rust or recursively launches Pester.
        $runner = Join-Path $script:RepositoryRoot 'scripts/Invoke-Tests.ps1'
        $ast = [Management.Automation.Language.Parser]::ParseFile($runner, [ref]$null, [ref]$null)
        $body = @'
#Requires -Version 7.6
__PARAMETERS__
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
ConvertTo-Json -InputObject $PSBoundParameters -Compress
exit __EXIT_CODE__
'@
        [IO.File]::WriteAllText((Join-Path $root 'scripts/Invoke-Tests.ps1'),
            $body.Replace('__PARAMETERS__', $ast.ParamBlock.Extent.Text).Replace('__EXIT_CODE__', [string]$ExitCode))
        $supplied = @()
        $expected = switch ($Recipe) {
            'test' { @{ Suite = 'Pester' } }
            'test-skill' {
                $supplied += 'skill with spaces'
                @{ Path = @('./tests/skill with spaces/') }
            }
            'test-rust' { @{ Suite = 'Rust' } }
            'test-rust-prepare' { @{ Suite = 'Rust'; Phase = 'Prepare' } }
            'test-rust-execute' { @{ Suite = 'Rust'; Phase = 'Execute' } }
            'test-all' { @{ Suite = 'All' } }
        }
        if ($Extra) {
            # Shell syntax must remain literal data, including quoted paths and runtime filters.
            $extraArguments = @('-PrerequisitePath', (Join-Path $root 'record''s $name; "世界".json'),
                '-RustFilter', 'name with spaces * & $value')
            $supplied += $extraArguments
            $expected.PrerequisitePath = $extraArguments[1]
            $expected.RustFilter = $extraArguments[3]
            $supplied += @('-RustTest', 'native,git', 'other', '-FullNameFilter',
                'first''s "quoted", $value; & *', 'second filter', '-Path', 'first path', 'second''s "quoted", $value; &')
            $expected.RustTest = @('native', 'git', 'other')
            $expected.FullNameFilter = @('first''s "quoted", $value; & *', 'second filter')
            $expected.Path = @('first path', 'second''s "quoted", $value; &')
        }

        $start = [Diagnostics.ProcessStartInfo]::new($script:Just.Source)
        $start.WorkingDirectory = $root
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in (@('--justfile', (Join-Path $root 'justfile'), $Recipe) + $supplied)) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $errorTask = $process.StandardError.ReadToEndAsync()
            $output = $process.StandardOutput.ReadToEnd()
            $process.WaitForExit()
            $process.ExitCode | Should -Be $ExitCode -Because $errorTask.GetAwaiter().GetResult()
            $actual = $output | ConvertFrom-Json -AsHashtable
            $actual.Count | Should -Be $expected.Count
            foreach ($key in $expected.Keys) {
                $actual[$key] | Should -Be $expected[$key]
                if ($key -in @('Path', 'FullNameFilter', 'RustTest')) {
                    ,$actual[$key] | Should -BeOfType [array]
                }
            }
        } finally {
            $process.Dispose()
        }
    }
}
