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
        New-Item -ItemType Directory -Path (Join-Path $root 'scripts') -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $script:RepositoryRoot 'justfile') -Destination $root
        # Capture the actual native argv boundary without compiling Rust or starting nested Pester.
        [IO.File]::WriteAllText((Join-Path $root 'scripts/Invoke-Tests.ps1'), @'
#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
ConvertTo-Json -InputObject @($args) -Compress
exit __EXIT_CODE__
'@.Replace('__EXIT_CODE__', [string]$ExitCode))
        $supplied = @()
        $expected = switch ($Recipe) {
            'test' { @('-Suite', 'Pester') }
            'test-skill' {
                $supplied += 'skill with spaces'
                @('-Path', './tests/skill with spaces/')
            }
            'test-rust' { @('-Suite', 'Rust') }
            'test-rust-prepare' { @('-Suite', 'Rust', '-Phase', 'Prepare') }
            'test-rust-execute' { @('-Suite', 'Rust', '-Phase', 'Execute') }
            'test-all' { @('-Suite', 'All') }
        }
        if ($Extra) {
            # Shell syntax must remain literal data, including quoted paths and runtime filters.
            $extraArguments = @('-PrerequisitePath', (Join-Path $root 'record''s $name; "世界".json'),
                '-RustFilter', 'name with spaces * & $value')
            $supplied += $extraArguments
            $expected += $extraArguments
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
            @($output | ConvertFrom-Json) | Should -Be $expected
        } finally {
            $process.Dispose()
        }
    }
}
